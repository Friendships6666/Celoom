use crate::{models::*, session_crypto};
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashSet,
    path::Path,
    sync::{Arc, mpsc},
};
use tokio::sync::oneshot;

thread_local! {
    static WORKER_VAULT: RefCell<Option<Arc<session_crypto::KeyVault>>> = const { RefCell::new(None) };
}

fn with_vault<T>(f: impl FnOnce(&session_crypto::KeyVault) -> Result<T>) -> Result<T> {
    WORKER_VAULT.with(|cell| {
        let borrow = cell.borrow();
        let vault = borrow
            .as_deref()
            .ok_or_else(|| anyhow!("Session key vault unavailable"))?;
        f(vault)
    })
}

fn session_key(db: &Connection, id: &str) -> Result<Option<[u8; 32]>> {
    let enc: String = db
        .query_row("SELECT encryption FROM sessions WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| anyhow!("Session not found"))?;
    match enc.as_str() {
        "none" => Ok(None),
        "half" => with_vault(|v| session_crypto::ensure_half_key(db, v, id).map(Some)),
        "full" => with_vault(|v| {
            v.get_full(id)
                .map(Some)
                .ok_or_else(|| anyhow!("SESSION_LOCKED: 会话已全加密，请输入密码解锁"))
        }),
        other => bail!("Unsupported session encryption mode: {other}"),
    }
}

fn enc_str(db: &Connection, id: &str, text: &str) -> Result<String> {
    match session_key(db, id)? {
        Some(key) => session_crypto::encrypt_text(&key, id, text),
        None => Ok(text.to_owned()),
    }
}

fn dec_str(db: &Connection, id: &str, text: &str) -> Result<String> {
    if !session_crypto::is_encrypted(text) {
        return Ok(text.to_owned());
    }
    match session_key(db, id)? {
        Some(key) => session_crypto::decrypt_text(&key, id, text),
        None => bail!("SESSION_LOCKED: 会话数据已加密，无法直接读取"),
    }
}

type Operation = Box<dyn FnOnce(&mut Connection, &mut SessionMirror) + Send + 'static>;
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Operation>,
    data_dir: Arc<std::path::PathBuf>,
    vault: Arc<session_crypto::KeyVault>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut db = Connection::open(path).context("Cannot open private application database")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,title TEXT NOT NULL,cwd TEXT NOT NULL,selection TEXT NOT NULL,status TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,parent_id TEXT REFERENCES sessions(id),depth INTEGER NOT NULL,context_epoch INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL REFERENCES sessions(id),kind TEXT NOT NULL,data TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS events_session ON events(session_id,seq);
            CREATE TABLE IF NOT EXISTS context_items(id INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT NOT NULL REFERENCES sessions(id),epoch INTEGER NOT NULL,item TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS context_session ON context_items(session_id,epoch,id);
            CREATE TABLE IF NOT EXISTS executions(session_id TEXT NOT NULL REFERENCES sessions(id),call_id TEXT NOT NULL,name TEXT NOT NULL,args TEXT NOT NULL,status TEXT NOT NULL,output TEXT NOT NULL DEFAULT '',PRIMARY KEY(session_id,call_id));
            CREATE TABLE IF NOT EXISTS accepted_messages(session_id TEXT NOT NULL,client_id TEXT NOT NULL,PRIMARY KEY(session_id,client_id));
            CREATE TABLE IF NOT EXISTS auth_sessions(token_hash TEXT PRIMARY KEY,expires_at INTEGER NOT NULL);")?;
        let columns = {
            let mut statement = db.prepare("PRAGMA table_info(sessions)")?;
            statement
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !columns.iter().any(|c| c == "reasoning_effort") {
            db.execute("ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT", [])?;
        }
        if !columns.iter().any(|c| c == "encryption") {
            db.execute(
                "ALTER TABLE sessions ADD COLUMN encryption TEXT NOT NULL DEFAULT 'none'",
                [],
            )?;
        }
        if !columns.iter().any(|c| c == "enc_meta") {
            db.execute("ALTER TABLE sessions ADD COLUMN enc_meta TEXT", [])?;
        }
        db.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('config',?1)",
            [serde_json::to_string(&AppConfig::default())?],
        )?;
        session_crypto::migrate(&mut db)?;
        crate::autonomy_store::migrate(&mut db)?;
        crate::message_queue::migrate(&mut db)?;
        crate::quality_store::migrate(&mut db)?;
        let vault = Arc::new(session_crypto::KeyVault::default());
        WORKER_VAULT.with(|cell| {
            *cell.borrow_mut() = Some(vault.clone());
        });
        recover_interrupted(&mut db)?;
        let mut mirror = SessionMirror::open(
            &db,
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join("sessions"),
        )?;
        let (tx, rx) = mpsc::channel::<Operation>();
        let worker_vault = vault.clone();
        std::thread::Builder::new()
            .name("celoom-sqlite".into())
            .spawn(move || {
                WORKER_VAULT.with(|cell| {
                    *cell.borrow_mut() = Some(worker_vault);
                });
                for op in rx {
                    op(&mut db, &mut mirror);
                }
            })?;
        let data_dir = Arc::new(std::fs::canonicalize(
            path.parent().unwrap_or_else(|| Path::new(".")),
        )?);
        Ok(Self {
            tx,
            data_dir,
            vault,
        })
    }
    pub fn data_dir(&self) -> &Path {
        self.data_dir.as_path()
    }
    pub(crate) async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |db, mirror| {
                let result = f(db);
                let result = match mirror.sync(db) {
                    Ok(()) => result,
                    Err(error) => {
                        Err(error.context("Plaintext session log could not be synchronized"))
                    }
                };
                let _ = tx.send(result);
            }))
            .map_err(|_| anyhow!("Database worker unavailable"))?;
        rx.await
            .map_err(|_| anyhow!("Database worker disconnected"))?
    }
    pub async fn request_audit_key(&self) -> Result<[u8; 32]> {
        self.call(|db| {
            use rand::RngCore;
            let existing: Option<String> = db
                .query_row(
                    "SELECT value FROM meta WHERE key='request_audit_key'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(hex) = existing {
                ensure!(
                    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
                    "Invalid request audit key"
                );
                let mut key = [0u8; 32];
                for (i, byte) in key.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                        .context("Invalid request audit key encoding")?;
                }
                return Ok(key);
            }
            let mut key = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            let hex = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
            db.execute(
                "INSERT INTO meta(key,value) VALUES('request_audit_key',?1)",
                [hex],
            )?;
            Ok(key)
        })
        .await
    }
    pub async fn session_cache_key(&self, id: &str) -> Result<String> {
        let id = id.to_owned();
        self.call(move |db| {
            let mut session = load_session(db, &id)?;
            let mut visited = HashSet::new();
            loop {
                ensure!(
                    visited.insert(session.id.clone()) && visited.len() <= 32,
                    "Invalid session ancestry"
                );
                match session.parent_id {
                    Some(parent) => session = load_session(db, &parent)?,
                    None => return Ok(format!("celoom:v1:{}", session.id)),
                }
            }
        })
        .await
    }
    pub async fn computer_host_enabled(&self) -> Result<bool> {
        self.call(|db| {
            let value: Option<String> = db
                .query_row(
                    "SELECT value FROM meta WHERE key='computer_host_enabled'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(value.as_deref() != Some("false"))
        })
        .await
    }
    pub async fn set_computer_host_enabled(&self, enabled: bool) -> Result<()> {
        self.call(move|db|{db.execute("INSERT INTO meta(key,value) VALUES('computer_host_enabled',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[if enabled{"true"}else{"false"}])?;Ok(())}).await
    }
    pub async fn config(&self) -> Result<AppConfig> {
        self.call(|db| load_config(db)).await
    }
    pub async fn save_config(&self, mut proposed: AppConfig) -> Result<AppConfig> {
        self.call(move |db| {
            let tx = db.transaction()?;
            let old = load_config(&tx)?;
            ensure!(
                proposed.revision == old.revision,
                "CONFIG_CONFLICT: Configuration changed on another device. Reload before saving."
            );
            for site in &mut proposed.sites {
                let previous = old.sites.iter().find(|s| {
                    s.id == site.id
                        && s.base_url.trim_end_matches('/') == site.base_url.trim_end_matches('/')
                });
                for key in &mut site.keys {
                    if key.secret.as_ref().is_none_or(|s| s.trim().is_empty()) {
                        key.secret = previous
                            .and_then(|s| s.keys.iter().find(|k| k.id == key.id))
                            .and_then(|k| k.secret.clone());
                    } else {
                        key.secret = key.secret.take().map(|s| s.trim().to_owned());
                    }
                    key.has_secret = key.secret.as_ref().is_some_and(|s| !s.is_empty());
                }
                site.base_url = site.base_url.trim_end_matches('/').to_owned();
            }
            if proposed
                .search
                .api_key
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
            {
                proposed.search.api_key = if proposed.search.provider == old.search.provider
                    && proposed.search.base_url.trim_end_matches('/')
                        == old.search.base_url.trim_end_matches('/')
                {
                    old.search.api_key.clone()
                } else {
                    None
                };
            } else {
                proposed.search.api_key =
                    proposed.search.api_key.take().map(|s| s.trim().to_owned());
            }
            proposed.search.has_api_key = proposed
                .search
                .api_key
                .as_ref()
                .is_some_and(|s| !s.is_empty());
            validate_config(&proposed)?;
            proposed.revision = old
                .revision
                .checked_add(1)
                .ok_or_else(|| anyhow!("Configuration revision exhausted"))?;
            tx.execute(
                "UPDATE meta SET value=?1 WHERE key='config'",
                [serde_json::to_string(&proposed)?],
            )?;
            tx.commit()?;
            Ok(proposed)
        })
        .await
    }
    pub async fn create_session(
        &self,
        title: String,
        cwd: String,
        selection: Selection,
        parent_id: Option<String>,
        depth: u32,
    ) -> Result<Session> {
        self.call(move |db| {
            let s = Session { id:new_id(),title,cwd,selection,reasoning_effort:None,status:"idle".into(),created_at:now(),updated_at:now(),parent_id,depth,context_epoch:0,encryption:"none".into(),locked:false };
            db.execute("INSERT INTO sessions(id,title,cwd,selection,status,created_at,updated_at,parent_id,depth,context_epoch) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![s.id,s.title,s.cwd,serde_json::to_string(&s.selection)?,s.status,s.created_at,s.updated_at,s.parent_id,s.depth,s.context_epoch])?;
            Ok(s)
        }).await
    }
    pub async fn session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();
        self.call(move |db| load_session(db, &id)).await
    }
    pub async fn rename_session(&self, id: &str, title: &str) -> Result<Session> {
        let id = id.to_owned();
        let title = title.trim().to_owned();
        ensure!(
            !title.is_empty() && title.chars().count() <= 256,
            "Session title must contain 1-256 characters"
        );
        self.call(move |db| {
            ensure!(
                db.execute(
                    "UPDATE sessions SET title=?2,updated_at=?3 WHERE id=?1",
                    params![id, title, now()]
                )? == 1,
                "Session not found"
            );
            load_session(db, &id)
        })
        .await
    }
    pub async fn unlock_session(&self, id: &str, password: &str) -> Result<Session> {
        let id = id.to_owned();
        let password = password.to_owned();
        self.call(move |db| {
            let (enc, meta): (String, Option<String>) = db
                .query_row(
                    "SELECT encryption, enc_meta FROM sessions WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow!("Session not found"))?;
            ensure!(enc == "full", "Session is not full-encrypted");
            let meta = meta.ok_or_else(|| anyhow!("Missing session encryption metadata"))?;
            let key = session_crypto::verify_full_password(&meta, &id, &password)?;
            with_vault(|v| {
                v.set_full(&id, key);
                Ok(())
            })?;
            load_session(db, &id)
        })
        .await
    }
    pub async fn lock_session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();
        self.call(move |db| {
            let enc: String = db
                .query_row("SELECT encryption FROM sessions WHERE id=?1", [&id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| anyhow!("Session not found"))?;
            ensure!(enc == "full", "Only full-encrypted sessions can be locked");
            with_vault(|v| {
                v.lock_full(&id);
                Ok(())
            })?;
            load_session(db, &id)
        })
        .await
    }
    pub async fn set_session_encryption(
        &self,
        id: &str,
        mode: &str,
        password: Option<String>,
    ) -> Result<Session> {
        ensure!(
            matches!(mode, "none" | "half" | "full"),
            "Unsupported encryption mode"
        );
        let id = id.to_owned();
        let mode = mode.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            let (current, enc_meta, status): (String, Option<String>, String) = tx
                .query_row(
                    "SELECT encryption, enc_meta, status FROM sessions WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow!("Session not found"))?;
            ensure!(
                !matches!(
                    status.as_str(),
                    "running" | "compacting" | "auto_compacting" | "waiting_for_answer" | "waiting_for_network"
                ),
                "Cannot change encryption while session is active"
            );
            if current == mode {
                return load_session(&tx, &id);
            }
            let old_key: Option<[u8; 32]> = match current.as_str() {
                "none" => None,
                "half" => Some(with_vault(|v| session_crypto::ensure_half_key(&tx, v, &id))?),
                "full" => {
                    let cached = with_vault(|v| Ok(v.get_full(&id)))?;
                    if let Some(k) = cached {
                        Some(k)
                    } else {
                        let pw = password
                            .as_deref()
                            .filter(|p| !p.is_empty())
                            .ok_or_else(|| anyhow!("PASSWORD_REQUIRED: 请输入会话密码以解除或更改全加密"))?;
                        let meta = enc_meta.as_deref().ok_or_else(|| anyhow!("Missing encryption metadata"))?;
                        Some(session_crypto::verify_full_password(meta, &id, pw)?)
                    }
                }
                _ => None,
            };
            let (new_key, new_meta): (Option<[u8; 32]>, Option<String>) = match mode.as_str() {
                "none" => (None, None),
                "half" => (
                    Some(with_vault(|v| session_crypto::ensure_half_key(&tx, v, &id))?),
                    None,
                ),
                "full" => {
                    let pw = password
                        .as_deref()
                        .filter(|p| !p.is_empty())
                        .ok_or_else(|| anyhow!("PASSWORD_REQUIRED: 全加密必须设置会话密码"))?;
                    let (meta, key) = session_crypto::create_full_meta(pw, &id)?;
                    (Some(key), Some(meta))
                }
                _ => unreachable!(),
            };
            let transform = |text: &str| -> Result<String> {
                let plain = if session_crypto::is_encrypted(text) {
                    let k = old_key.as_ref().ok_or_else(|| anyhow!("Missing decryption key"))?;
                    session_crypto::decrypt_text(k, &id, text)?
                } else {
                    text.to_owned()
                };
                match &new_key {
                    Some(k) => session_crypto::encrypt_text(k, &id, &plain),
                    None => Ok(plain),
                }
            };
            {
                let mut stmt = tx.prepare("SELECT seq, data FROM events WHERE session_id=?1")?;
                let rows = stmt
                    .query_map([&id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (seq, data) in rows {
                    let updated = transform(&data)?;
                    tx.execute("UPDATE events SET data=?2 WHERE seq=?1", params![seq, updated])?;
                }
            }
            {
                let mut stmt = tx.prepare("SELECT id, item FROM context_items WHERE session_id=?1")?;
                let rows = stmt
                    .query_map([&id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (row_id, item) in rows {
                    let updated = transform(&item)?;
                    tx.execute(
                        "UPDATE context_items SET item=?2 WHERE id=?1",
                        params![row_id, updated],
                    )?;
                }
            }
            {
                let mut stmt = tx.prepare("SELECT call_id, args, output FROM executions WHERE session_id=?1")?;
                let rows = stmt
                    .query_map([&id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for (call_id, args, output) in rows {
                    let updated_args = transform(&args)?;
                    let updated_output = if output.is_empty() {
                        String::new()
                    } else {
                        transform(&output)?
                    };
                    tx.execute(
                        "UPDATE executions SET args=?3, output=?4 WHERE session_id=?1 AND call_id=?2",
                        params![id, call_id, updated_args, updated_output],
                    )?;
                }
            }
            tx.execute(
                "UPDATE sessions SET encryption=?2, enc_meta=?3, updated_at=?4 WHERE id=?1",
                params![id, mode, new_meta, now()],
            )?;
            if mode != "half" {
                let _ = session_crypto::remove_half_slot(&tx, &id);
            }
            tx.commit()?;
            with_vault(|v| {
                if mode == "full" {
                    if let Some(k) = new_key {
                        v.set_full(&id, k);
                    }
                } else {
                    v.lock_full(&id);
                }
                if mode != "half" {
                    v.remove_all(&id);
                    if mode == "full"
                        && let Some(k) = new_key
                    {
                        v.set_full(&id, k);
                    }
                }
                Ok(())
            })?;
            load_session(db, &id)
        })
        .await
    }
    pub async fn delete_session(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        let id_fs = id.clone();
        let sessions_dir = self.data_dir.join("sessions");
        self.call(move |db| {
            let tx = db.transaction()?;
            let status: Option<String> = tx
                .query_row("SELECT status FROM sessions WHERE id=?1", [&id], |r| r.get(0))
                .optional()?;
            let Some(status) = status else {
                bail!("Session not found");
            };
            ensure!(
                !matches!(
                    status.as_str(),
                    "running" | "compacting" | "auto_compacting" | "waiting_for_answer" | "waiting_for_network"
                ),
                "Cannot delete an active session; stop it first"
            );
            tx.execute("UPDATE sessions SET parent_id=NULL WHERE parent_id=?1", [&id])?;
            for table in [
                "agent_inbox",
                "agent_goals",
                "agent_questions",
                "message_queue",
                "message_escalations",
                "accepted_messages",
                "executions",
                "context_items",
                "events",
            ] {
                tx.execute(&format!("DELETE FROM {table} WHERE session_id=?1"), [&id])?;
            }
            tx.execute("DELETE FROM sessions WHERE id=?1", [&id])?;
            let _ = session_crypto::remove_half_slot(&tx, &id);
            tx.commit()?;
            with_vault(|v| {
                v.remove_all(&id);
                Ok(())
            })?;
            Ok(())
        })
        .await?;
        let jsonl = sessions_dir.join(format!("{id_fs}.jsonl"));
        if jsonl.exists() {
            let _ = std::fs::remove_file(jsonl);
        }
        let dir = sessions_dir.join(&id_fs);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }
    pub async fn update_session_settings(
        &self,
        id: &str,
        selection: Selection,
        reasoning_effort: Option<String>,
    ) -> Result<Session> {
        let id = id.to_owned();
        self.call(move|db|{
            let tx=db.transaction()?;let config=load_config(&tx)?;
            let site=config.sites.iter().find(|s|s.id==selection.site_id).context("Selected site no longer exists")?;
            let key=site.keys.iter().find(|k|k.id==selection.key_id).context("Selected key no longer exists")?;
            ensure!(key.secret.as_ref().is_some_and(|s|!s.is_empty()),"Selected key has no secret");
            let model=key.models.iter().find(|m|m.id==selection.model_id).context("Selected model no longer exists")?;
            if let Some(effort)=&reasoning_effort{ensure!(effort.is_empty()||model.reasoning_efforts.contains(effort),"Reasoning effort is not supported by this model");}
            ensure!(tx.execute("UPDATE sessions SET selection=?2,reasoning_effort=?3,updated_at=?4 WHERE id=?1",params![id,serde_json::to_string(&selection)?,reasoning_effort,now()])?==1,"Session not found");
            event(&tx,&id,"session_settings",json!({"selection":selection,"reasoning_effort":reasoning_effort,"site_name":site.name,"model_name":model.name,"model":model.model}))?;
            let session=load_session(&tx,&id)?;tx.commit()?;Ok(session)
        }).await
    }
    pub async fn append_subagent_catalog(
        &self,
        id: &str,
        fingerprint: &str,
        catalog: Value,
        message: String,
    ) -> Result<()> {
        let id = id.to_owned();
        let fingerprint = fingerprint.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            context_items(&tx, &id, vec![json!({"role":"user","content":message})])?;
            event(
                &tx,
                &id,
                "subagent_catalog",
                json!({"fingerprint":fingerprint,"catalog":catalog}),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn rebase_context(
        &self,
        id: &str,
        items: Vec<Value>,
        fingerprint: &str,
    ) -> Result<()> {
        let id = id.to_owned();
        let fingerprint = fingerprint.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            ensure!(
                tx.execute(
                    "UPDATE sessions SET context_epoch=context_epoch+1,updated_at=?2 WHERE id=?1",
                    params![id, now()]
                )? == 1,
                "Session not found"
            );
            context_items(&tx, &id, items)?;
            event(
                &tx,
                &id,
                "context_rebased",
                json!({"reason":"route_changed","route_fingerprint":fingerprint}),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn session_jsonl(&self, id: &str) -> Result<String> {
        let id = id.to_owned();
        self.call(move |db| session_log_text(db, &id)).await
    }
    pub async fn sessions(&self) -> Result<Vec<Session>> {
        self.call(|db| {
            let mut stmt = db.prepare("SELECT id FROM sessions ORDER BY updated_at DESC")?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.iter().map(|id| load_session(db, id)).collect()
        })
        .await
    }
    pub async fn set_status(&self, id: &str, status: &str) -> Result<()> {
        let id = id.to_owned();
        let status = status.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            ensure!(
                tx.execute(
                    "UPDATE sessions SET status=?2,updated_at=?3 WHERE id=?1",
                    params![id, status, now()]
                )? == 1,
                "Session not found"
            );
            event(&tx, &id, "status", json!({"status":status}))?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn events(&self, id: &str, after: i64) -> Result<Vec<Event>> {
        let id = id.to_owned();
        self.call(move|db|{
            let _ = session_key(db, &id)?;
            let mut stmt=db.prepare("SELECT seq,session_id,kind,data,created_at FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq")?;
            let rows=stmt.query_map(params![id,after],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?)))?;
            rows.map(|r| {
                let (seq, session_id, kind, data, created_at): (i64, String, String, String, String) = r?;
                let plain = dec_str(db, &session_id, &data)?;
                Ok(Event{seq,session_id,kind,data:serde_json::from_str(&plain)?,created_at})
            }).collect()
        }).await
    }
    pub async fn append_event(&self, id: &str, kind: &str, data: Value) -> Result<Event> {
        let id = id.to_owned();
        let kind = kind.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            let e = event(&tx, &id, &kind, data)?;
            tx.commit()?;
            Ok(e)
        })
        .await
    }
    pub async fn context(&self, id: &str) -> Result<Vec<Value>> {
        let id = id.to_owned();
        self.call(move|db|{
            let _ = session_key(db, &id)?;
            let mut stmt=db.prepare("SELECT item FROM context_items WHERE session_id=?1 AND epoch=(SELECT context_epoch FROM sessions WHERE id=?1) ORDER BY id")?;
            let rows=stmt.query_map([&id],|r|r.get::<_,String>(0))?;
            rows.map(|r|{
                let plain = dec_str(db, &id, &r?)?;
                Ok(serde_json::from_str(&plain)?)
            }).collect()
        }).await
    }
    pub async fn append_context(&self, id: &str, items: Vec<Value>) -> Result<()> {
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            let portable = portable_log_value(&json!(items));
            context_items(&tx, &id, items)?;
            event(&tx, &id, "context_note", json!({"items":portable}))?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn commit_response(
        &self,
        id: &str,
        attempt_id: &str,
        items: Vec<Value>,
        text: &str,
        reasoning: &str,
    ) -> Result<()> {
        let id = id.to_owned();
        let attempt_id = attempt_id.to_owned();
        let text = text.to_owned();
        let reasoning = reasoning.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            context_items(&tx, &id, items)?;
            let mut payload = json!({"attempt_id":attempt_id,"id":new_id(),"text":text});
            if !reasoning.is_empty() {
                payload["reasoning"] = json!(reasoning);
            }
            event(&tx, &id, "assistant_message", payload)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn replace_context(&self, id: &str, items: Vec<Value>, summary: &str) -> Result<u32> {
        let id = id.to_owned();
        let summary = summary.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            ensure!(
                tx.execute(
                    "UPDATE sessions SET context_epoch=context_epoch+1,updated_at=?2 WHERE id=?1",
                    params![id, now()]
                )? == 1,
                "Session not found"
            );
            let epoch: u32 = tx.query_row(
                "SELECT context_epoch FROM sessions WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            context_items(&tx, &id, items)?;
            event(
                &tx,
                &id,
                "compacted",
                json!({"summary":summary,"context_epoch":epoch}),
            )?;
            tx.commit()?;
            Ok(epoch)
        })
        .await
    }
    pub async fn tool_started(
        &self,
        id: &str,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<bool> {
        let id = id.to_owned();
        let call_id = call_id.to_owned();
        let name = name.to_owned();
        let args = args.clone();
        self.call(move|db|{
            let tx=db.transaction()?;
            let prior:Option<(String,String)>=tx.query_row("SELECT name,args FROM executions WHERE session_id=?1 AND call_id=?2",params![id,call_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((n,a))=prior {
                let plain_a = dec_str(&tx, &id, &a)?;
                ensure!(n==name && serde_json::from_str::<Value>(&plain_a)?==args,"Tool call identity collision: arguments differ");
                return Ok(false);
            }
            let enc_args = enc_str(&tx, &id, &serde_json::to_string(&args)?)?;
            tx.execute("INSERT INTO executions(session_id,call_id,name,args,status) VALUES(?1,?2,?3,?4,'running')",params![id,call_id,name,enc_args])?;
            event(&tx,&id,"tool_start",json!({"call_id":call_id,"name":name,"args":args}))?;tx.commit()?;Ok(true)
        }).await
    }
    pub async fn tool_finished(
        &self,
        id: &str,
        call_id: &str,
        name: &str,
        output: &str,
        is_error: bool,
    ) -> Result<()> {
        self.tool_finished_with_context(id, call_id, name, output, is_error, Vec::new())
            .await
    }
    pub async fn tool_finished_with_context(
        &self,
        id: &str,
        call_id: &str,
        name: &str,
        output: &str,
        is_error: bool,
        additional: Vec<Value>,
    ) -> Result<()> {
        let id = id.to_owned();
        let call_id = call_id.to_owned();
        let name = name.to_owned();
        let output = output.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM executions WHERE session_id=?1 AND call_id=?2",
                    params![id, call_id],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(status.is_some(), "Tool was not durably started");
            if matches!(status.as_deref(), Some("succeeded" | "failed")) {
                return Ok(());
            }
            let enc_output = enc_str(&tx, &id, &output)?;
            tx.execute(
                "UPDATE executions SET status=?3,output=?4 WHERE session_id=?1 AND call_id=?2",
                params![
                    id,
                    call_id,
                    if is_error { "failed" } else { "succeeded" },
                    enc_output
                ],
            )?;
            context_items(
                &tx,
                &id,
                vec![json!({"type":"function_call_output","call_id":call_id,"output":output})],
            )?;
            if !additional.is_empty() {
                ensure!(!is_error, "A failed tool cannot attach vision input");
                let portable = portable_log_value(&json!(additional));
                context_items(&tx, &id, additional)?;
                event(
                    &tx,
                    &id,
                    "context_note",
                    json!({"source":name,"items":portable}),
                )?;
            }
            event(
                &tx,
                &id,
                "tool_end",
                json!({"call_id":call_id,"name":name,"output":output,"is_error":is_error}),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn tool_result(&self, id: &str, call_id: &str) -> Result<Option<(String, String)>> {
        let id = id.to_owned();
        let call_id = call_id.to_owned();
        self.call(move |db| {
            let row = db
                .query_row(
                    "SELECT status,output FROM executions WHERE session_id=?1 AND call_id=?2",
                    params![id, call_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?;
            match row {
                Some((status, output)) => Ok(Some((status, dec_str(db, &id, &output)?))),
                None => Ok(None),
            }
        })
        .await
    }
    pub async fn accept_message(
        &self,
        id: &str,
        client_id: &str,
        items: Vec<Value>,
        data: Value,
    ) -> Result<bool> {
        let id = id.to_owned();
        let client_id = client_id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            if tx.execute(
                "INSERT OR IGNORE INTO accepted_messages(session_id,client_id) VALUES(?1,?2)",
                params![id, client_id],
            )? == 0
            {
                return Ok(false);
            }
            context_items(&tx, &id, items)?;
            event(&tx, &id, "user_message", data)?;
            tx.execute(
                "DELETE FROM message_escalations WHERE session_id=?1 AND client_id=?2",
                params![id, client_id],
            )?;
            tx.execute(
                "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                params![id, now()],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
    pub async fn password_hash(&self) -> Result<Option<String>> {
        self.call(|db| {
            Ok(db
                .query_row(
                    "SELECT value FROM meta WHERE key='password_hash'",
                    [],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
    }
    pub async fn set_password_hash(&self, hash: String) -> Result<()> {
        self.call(move|db|{let tx=db.transaction()?;tx.execute("INSERT INTO meta(key,value) VALUES('password_hash',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[hash])?;tx.execute("DELETE FROM auth_sessions",[])?;tx.commit()?;Ok(())}).await
    }
    pub async fn create_auth_session_checked(
        &self,
        hash: String,
        expires_at: i64,
        expected_password_hash: String,
    ) -> Result<bool> {
        self.call(move |db| {
            let tx = db.transaction()?;
            let current: Option<String> = tx
                .query_row(
                    "SELECT value FROM meta WHERE key='password_hash'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if current.as_deref() != Some(&expected_password_hash) {
                return Ok(false);
            }
            tx.execute(
                "DELETE FROM auth_sessions WHERE expires_at<?1",
                [chrono::Utc::now().timestamp()],
            )?;
            tx.execute(
                "INSERT INTO auth_sessions(token_hash,expires_at) VALUES(?1,?2)",
                params![hash, expires_at],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }
    pub async fn initialize_password(&self, hash: String) -> Result<bool> {
        self.call(move |db| {
            Ok(db.execute(
                "INSERT OR IGNORE INTO meta(key,value) VALUES('password_hash',?1)",
                [hash],
            )? == 1)
        })
        .await
    }
    pub async fn auth_session(&self, hash: String) -> Result<bool> {
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT 1 FROM auth_sessions WHERE token_hash=?1 AND expires_at>?2",
                    params![hash, chrono::Utc::now().timestamp()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await
    }
    pub async fn delete_auth_session(&self, hash: String) -> Result<()> {
        self.call(move |db| {
            db.execute("DELETE FROM auth_sessions WHERE token_hash=?1", [hash])?;
            Ok(())
        })
        .await
    }
    pub async fn revoke_auth_sessions(&self) -> Result<()> {
        self.call(|db| {
            db.execute("DELETE FROM auth_sessions", [])?;
            Ok(())
        })
        .await
    }
}

// SQLite is an unencrypted transactional source of truth. These plaintext JSONL
// mirrors are deliberately readable by other local agent tools, without a key.
struct SessionMirror {
    root: std::path::PathBuf,
    known: HashSet<String>,
    cursor: i64,
    changes: u64,
}
impl SessionMirror {
    fn open(db: &Connection, root: std::path::PathBuf) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&root)?;
        ensure!(
            !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "Session log directory must not be a symlink"
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let root = std::fs::canonicalize(root)?;
        let mut mirror = Self {
            root,
            known: HashSet::new(),
            cursor: 0,
            changes: 0,
        };
        for id in all_session_ids(db)? {
            let enc: String = db
                .query_row("SELECT encryption FROM sessions WHERE id=?1", [&id], |r| r.get(0))
                .unwrap_or_else(|_| "none".into());
            if enc == "none" {
                mirror.replace_log(&id, session_log_text(db, &id)?.as_bytes())?;
                mirror.known.insert(id);
            } else {
                let _ = std::fs::remove_file(mirror.path(&id)?);
            }
        }
        mirror.cursor =
            db.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
        mirror.changes = db.total_changes();
        Ok(mirror)
    }
    fn path(&self, id: &str) -> Result<std::path::PathBuf> {
        ensure!(
            uuid::Uuid::parse_str(id).is_ok(),
            "Session log ID must be a UUID"
        );
        Ok(self.root.join(format!("{id}.jsonl")))
    }
    fn replace_log(&self, id: &str, text: &[u8]) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let destination = self.path(id)?;
        let temporary = self.root.join(format!(".session-{}.tmp", new_id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text)?;
        file.sync_all()?;
        ensure!(
            temporary.parent() == Some(self.root.as_path())
                && destination.parent() == Some(self.root.as_path()),
            "Session log publication must stay inside its directory"
        );
        std::fs::rename(&temporary, &destination)?;
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn sync(&mut self, db: &Connection) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        if self.changes == db.total_changes() {
            return Ok(());
        }
        let active_ids: HashSet<String> = all_session_ids(db)?.into_iter().collect();
        let removed: Vec<String> = self
            .known
            .iter()
            .filter(|id| !active_ids.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Ok(path) = self.path(&id) {
                let _ = std::fs::remove_file(path);
            }
            self.known.remove(&id);
        }
        // Write a versioned header before any events of a newly created unencrypted session.
        for id in &active_ids {
            let enc: String = db
                .query_row("SELECT encryption FROM sessions WHERE id=?1", [id], |r| r.get(0))
                .unwrap_or_else(|_| "none".into());
            if enc != "none" {
                if let Ok(path) = self.path(id) {
                    let _ = std::fs::remove_file(path);
                }
                self.known.remove(id);
                continue;
            }
            if !self.known.contains(id) {
                let header =
                    json!({"type":"session","schema_version":1,"session":load_session(db,id)?});
                self.replace_log(
                    id,
                    format!("{}\n", serde_json::to_string(&header)?).as_bytes(),
                )?;
                self.known.insert(id.clone());
            }
        }
        let mut stmt = db.prepare(
            "SELECT seq,session_id,kind,data,created_at FROM events WHERE seq>?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([self.cursor], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (seq, id, kind, data, created_at) = row?;
            let enc: String = db
                .query_row("SELECT encryption FROM sessions WHERE id=?1", [&id], |r| r.get(0))
                .unwrap_or_else(|_| "none".into());
            if enc != "none" {
                self.cursor = seq;
                continue;
            }
            let data = portable_log_value(&serde_json::from_str::<Value>(&data)?);
            let event = Event {
                seq,
                session_id: id.clone(),
                kind,
                data,
                created_at,
            };
            let path = self.path(&id)?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(path)?;
            let length = file.metadata()?.len();
            let line = format!("{}\n", serde_json::to_string(&event)?);
            if let Err(error) = file
                .write_all(line.as_bytes())
                .and_then(|_| file.sync_all())
            {
                let _ = file.set_len(length);
                let _ = file.sync_all();
                return Err(error.into());
            }
            self.cursor = seq;
        }
        self.changes = db.total_changes();
        Ok(())
    }
}
fn all_session_ids(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT id FROM sessions ORDER BY created_at,id")?;
    Ok(stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn portable_log_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| key.as_str() != "encrypted_content")
                .map(|(key, value)| (key.clone(), portable_log_value(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(portable_log_value).collect()),
        other => other.clone(),
    }
}
fn session_log_text(db: &Connection, id: &str) -> Result<String> {
    let session = load_session(db, id)?;
    let mut text =
        serde_json::to_string(&json!({"type":"session","schema_version":1,"session":session}))?;
    text.push('\n');
    let mut stmt =
        db.prepare("SELECT seq,kind,data,created_at FROM events WHERE session_id=?1 ORDER BY seq")?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (seq, kind, data, created_at) = row?;
        let plain = dec_str(db, id, &data)?;
        let event = Event {
            seq,
            session_id: id.to_owned(),
            kind,
            data: portable_log_value(&serde_json::from_str::<Value>(&plain)?),
            created_at,
        };
        text.push_str(&serde_json::to_string(&event)?);
        text.push('\n');
    }
    Ok(text)
}

fn load_config(db: &Connection) -> Result<AppConfig> {
    let s: String = db.query_row("SELECT value FROM meta WHERE key='config'", [], |r| {
        r.get(0)
    })?;
    Ok(serde_json::from_str(&s)?)
}
pub(crate) fn load_session(db: &Connection, id: &str) -> Result<Session> {
    let raw: (String, String, String, String, String, String, String, Option<String>, u32, u32, Option<String>, String) = db.query_row("SELECT id,title,cwd,selection,status,created_at,updated_at,parent_id,depth,context_epoch,reasoning_effort,encryption FROM sessions WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?))).optional()?.ok_or_else(||anyhow!("Session not found"))?;
    let locked = raw.11 == "full" && with_vault(|v| Ok(v.get_full(&raw.0).is_none())).unwrap_or(true);
    Ok(Session {
        id: raw.0,
        title: raw.1,
        cwd: raw.2,
        selection: serde_json::from_str(&raw.3)?,
        status: raw.4,
        created_at: raw.5,
        updated_at: raw.6,
        parent_id: raw.7,
        depth: raw.8,
        context_epoch: raw.9,
        reasoning_effort: raw.10,
        encryption: raw.11,
        locked,
    })
}
pub(crate) fn event(tx: &Transaction<'_>, id: &str, kind: &str, data: Value) -> Result<Event> {
    let created_at = now();
    let stored = enc_str(tx, id, &serde_json::to_string(&data)?)?;
    tx.execute(
        "INSERT INTO events(session_id,kind,data,created_at) VALUES(?1,?2,?3,?4)",
        params![id, kind, stored, created_at],
    )?;
    Ok(Event {
        seq: tx.last_insert_rowid(),
        session_id: id.to_owned(),
        kind: kind.to_owned(),
        data,
        created_at,
    })
}
pub(crate) fn context_items(tx: &Transaction<'_>, id: &str, items: Vec<Value>) -> Result<()> {
    let epoch: u32 = tx.query_row(
        "SELECT context_epoch FROM sessions WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    for item in items {
        let stored = enc_str(tx, id, &serde_json::to_string(&item)?)?;
        tx.execute(
            "INSERT INTO context_items(session_id,epoch,item) VALUES(?1,?2,?3)",
            params![id, epoch, stored],
        )?;
    }
    Ok(())
}
fn recover_interrupted(db: &mut Connection) -> Result<()> {
    let ids = {
        let mut s =
            db.prepare("SELECT id FROM sessions WHERE status IN('running','compacting','auto_compacting','waiting_for_answer','waiting_for_network')")?;
        s.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in ids {
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE sessions SET status='interrupted',updated_at=?2 WHERE id=?1",
            params![id, now()],
        )?;
        tx.execute(
            "UPDATE executions SET status='unknown' WHERE session_id=?1 AND status='running'",
            [&id],
        )?;
        let enc: String = tx.query_row("SELECT encryption FROM sessions WHERE id=?1", [&id], |r| r.get(0))?;
        if enc == "full" {
            tx.commit()?;
            continue;
        }
        let items = {
            let mut s=tx.prepare("SELECT item FROM context_items WHERE session_id=?1 AND epoch=(SELECT context_epoch FROM sessions WHERE id=?1) ORDER BY id")?;
            s.query_map([&id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut calls = Vec::new();
        let mut results = HashSet::new();
        for item in items {
            let plain = dec_str(&tx, &id, &item)?;
            let item: Value = serde_json::from_str(&plain)?;
            match item["type"].as_str() {
                Some("function_call") => {
                    if let Some(cid) = item["call_id"].as_str() {
                        calls.push((
                            cid.to_owned(),
                            item["name"].as_str().unwrap_or("unknown").to_owned(),
                        ));
                    }
                }
                Some("function_call_output") => {
                    if let Some(cid) = item["call_id"].as_str() {
                        results.insert(cid.to_owned());
                    }
                }
                _ => {}
            }
        }
        for (cid, name) in calls {
            if results.contains(&cid) {
                continue;
            }
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM executions WHERE session_id=?1 AND call_id=?2",
                    params![id, cid],
                    |r| r.get(0),
                )
                .optional()?;
            let output = if status.as_deref() == Some("unknown") {
                "Tool execution outcome is UNKNOWN after backend restart. It may have changed the system. Inspect actual state before deciding any further action. Do not blindly repeat."
            } else {
                "Tool was NOT executed: backend stopped before dispatch. Re-issue only if still needed."
            };
            context_items(
                &tx,
                &id,
                vec![json!({"type":"function_call_output","call_id":cid,"output":output})],
            )?;
            event(
                &tx,
                &id,
                "tool_end",
                json!({"call_id":cid,"name":name,"output":output,"is_error":true}),
            )?;
        }
        event(
            &tx,
            &id,
            "error",
            json!({"kind":"backend_restarted","message":"Backend restarted. In-flight generation stopped; uncertain tool operations were not retried."}),
        )?;
        event(&tx, &id, "status", json!({"status":"interrupted"}))?;
        tx.commit()?;
    }
    Ok(())
}

pub fn validate_config(c: &AppConfig) -> Result<()> {
    ensure!(c.sites.len() <= 64, "Too many sites");
    let mut ids = HashSet::new();
    for s in &c.sites {
        validate_id(&s.id, &mut ids)?;
        ensure!(
            !s.name.trim().is_empty() && s.name.len() <= 128,
            "Site name is required (max 128 bytes)"
        );
        let u = url::Url::parse(&s.base_url).context("Invalid site base URL")?;
        ensure!(
            matches!(u.scheme(), "https" | "http") && u.host_str().is_some(),
            "Site URL must use HTTP(S)"
        );
        ensure!(
            u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none(),
            "Site URL must not contain credentials, query or fragment"
        );
        ensure!(s.keys.len() <= 64, "Too many keys for one site");
        for k in &s.keys {
            validate_id(&k.id, &mut ids)?;
            ensure!(
                !k.name.trim().is_empty() && k.name.len() <= 128,
                "Key name is required"
            );
            if let Some(secret) = &k.secret {
                ensure!(
                    secret.len() <= 8192 && !secret.contains(['\r', '\n']) && !secret.contains('•'),
                    "Invalid key format"
                );
            }
            ensure!(
                k.models.is_empty() || k.secret.as_ref().is_some_and(|s| !s.is_empty()),
                "A key must be entered for configured models; re-enter it when changing the endpoint"
            );
            ensure!(k.models.len() <= 128, "Too many models for one key");
            for m in &k.models {
                validate_id(&m.id, &mut ids)?;
                ensure!(
                    !m.model.trim().is_empty() && m.model.len() <= 256,
                    "Model identifier is required"
                );
                ensure!(
                    !m.name.trim().is_empty() && m.name.len() <= 128,
                    "Model display name is required"
                );
                ensure!(
                    (1024..=4_000_000).contains(&m.context_window),
                    "Context window must be 1024–4000000 tokens"
                );
                ensure!(
                    m.max_output_tokens > 0 && m.max_output_tokens < m.context_window,
                    "Maximum output must be positive and below context window"
                );
                ensure!(
                    (20..=100).contains(&m.auto_compact_percent),
                    "Auto-compaction threshold must be 20–100%"
                );
                const EFFORTS: [&str; 7] =
                    ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
                let unique: HashSet<_> = m.reasoning_efforts.iter().collect();
                ensure!(
                    unique.len() == m.reasoning_efforts.len()
                        && m.reasoning_efforts
                            .iter()
                            .all(|s| EFFORTS.contains(&s.as_str())),
                    "Invalid reasoning effort choices"
                );
                ensure!(
                    m.reasoning_effort.is_empty()
                        || m.reasoning_efforts.contains(&m.reasoning_effort),
                    "Default reasoning effort must be one of the configured choices"
                );
            }
        }
    }
    ensure!(
        c.subagent_models.len() <= 32,
        "At most 32 subagent model choices are allowed"
    );
    for preset in &c.subagent_models {
        validate_id(&preset.id, &mut ids)?;
        ensure!(
            preset.id != "inherit",
            "Subagent choice ID 'inherit' is reserved"
        );
        ensure!(
            !preset.name.trim().is_empty()
                && preset.name.len() <= 128
                && !preset.name.contains('\0'),
            "Subagent choice needs a name of at most 128 bytes"
        );
        let site=c.sites.iter().find(|site|site.id==preset.selection.site_id).with_context(||format!("Subagent choice '{}' references a removed site; update the subagent model pool",preset.name))?;
        let key = site
            .keys
            .iter()
            .find(|key| key.id == preset.selection.key_id)
            .with_context(|| {
                format!(
                    "Subagent choice '{}' references a removed key; update the subagent model pool",
                    preset.name
                )
            })?;
        ensure!(
            key.secret.as_ref().is_some_and(|secret| !secret.is_empty()),
            "Subagent choice '{}' requires a configured key",
            preset.name
        );
        let model=key.models.iter().find(|model|model.id==preset.selection.model_id).with_context(||format!("Subagent choice '{}' references a removed model; update the subagent model pool",preset.name))?;
        ensure!(
            preset
                .reasoning_effort
                .as_ref()
                .is_none_or(|effort| effort.is_empty() || model.reasoning_efforts.contains(effort)),
            "Subagent choice '{}' has an unsupported reasoning effort",
            preset.name
        );
    }
    ensure!(
        matches!(
            c.search.provider.as_str(),
            "auto"
                | "duckduckgo"
                | "bing"
                | "wikipedia"
                | "github"
                | "arxiv"
                | "hackernews"
                | "stackexchange"
                | "brave"
                | "searxng"
        ),
        "Unsupported built-in search provider"
    );
    if !c.search.base_url.trim().is_empty() {
        let u = url::Url::parse(&c.search.base_url).context("Invalid search endpoint URL")?;
        ensure!(
            matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none(),
            "Search endpoint must be a credential-free HTTP(S) URL without query/fragment"
        );
    }
    if c.search.provider == "searxng" {
        ensure!(
            !c.search.base_url.trim().is_empty(),
            "SearXNG requires a search server URL"
        );
    }
    if let Some(secret) = &c.search.api_key {
        ensure!(
            secret.len() <= 8192 && !secret.contains(['\r', '\n']),
            "Invalid search API key"
        );
    }
    let n = &c.network;
    ensure!(
        matches!(n.proxy_mode.as_str(), "environment" | "manual" | "direct"),
        "Invalid proxy mode"
    );
    if n.proxy_mode == "manual" {
        let u = url::Url::parse(&n.proxy_url).context("Invalid manual proxy URL")?;
        ensure!(
            matches!(u.scheme(), "http" | "https" | "socks5" | "socks5h") && u.host_str().is_some(),
            "Proxy must be HTTP(S) or SOCKS5"
        );
    }
    ensure!(n.no_proxy.len() <= 4096, "Proxy exclusion list is too long");
    for t in [
        n.connect_timeout_secs,
        n.headers_timeout_secs,
        n.idle_timeout_secs,
        n.request_timeout_secs,
    ] {
        ensure!(
            (1..=86400).contains(&t),
            "Network timeouts must be 1–86400 seconds"
        );
    }
    ensure!(
        (1..=5).contains(&n.max_attempts),
        "Request attempts must be 1–5"
    );
    Ok(())
}
fn validate_id(id: &str, ids: &mut HashSet<String>) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "Invalid configuration ID"
    );
    if !ids.insert(id.to_owned()) {
        bail!("Configuration IDs must be unique");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configured() -> AppConfig {
        let mut c = AppConfig::default();
        let m = ModelConfig {
            id: "model".into(),
            name: "Test".into(),
            model: "test".into(),
            ..Default::default()
        };
        c.sites.push(Site {
            id: "site".into(),
            name: "Site".into(),
            base_url: "http://127.0.0.1:9999/v1".into(),
            keys: vec![ApiKey {
                id: "key".into(),
                name: "Key".into(),
                secret: Some("test-secret".into()),
                has_secret: true,
                models: vec![m],
            }],
        });
        c
    }
    #[tokio::test]
    async fn session_settings_are_independent_and_logs_are_readable_plaintext() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("db")).unwrap();
        let config = s.save_config(configured()).await.unwrap();
        let selection = Selection {
            site_id: "site".into(),
            key_id: "key".into(),
            model_id: "model".into(),
        };
        let session = s
            .create_session(
                "Plain session".into(),
                "/tmp".into(),
                selection.clone(),
                None,
                0,
            )
            .await
            .unwrap();
        let changed = s
            .update_session_settings(&session.id, selection, Some("high".into()))
            .await
            .unwrap();
        assert_eq!(changed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(s.config().await.unwrap().revision, config.revision);
        assert_eq!(
            s.config().await.unwrap().sites[0].keys[0].models[0].reasoning_effort,
            "medium"
        );
        s.accept_message(
            &session.id,
            "plain-message",
            vec![json!({"role":"user","content":"明文消息"})],
            json!({"text":"明文消息"}),
        )
        .await
        .unwrap();
        s.append_context(&session.id,vec![json!({"type":"reasoning","encrypted_content":"opaque-provider-ciphertext","summary":[]})]).await.unwrap();
        let text = std::fs::read_to_string(
            d.path()
                .join("sessions")
                .join(format!("{}.jsonl", session.id)),
        )
        .unwrap();
        assert!(text.contains("明文消息"));
        assert!(!text.contains("test-secret"));
        assert!(!text.contains("opaque-provider-ciphertext"));
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
        let export = s.session_jsonl(&session.id).await.unwrap();
        assert!(export.contains("明文消息"));
        assert!(
            s.context(&session.id)
                .await
                .unwrap()
                .iter()
                .any(|item| item["encrypted_content"] == "opaque-provider-ciphertext")
        );
    }
    #[tokio::test]
    async fn request_audit_key_and_family_routing_survive_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open(&path).unwrap();
        let key = store.request_audit_key().await.unwrap();
        assert_eq!(key, store.request_audit_key().await.unwrap());
        let selection = Selection {
            site_id: "site".into(),
            key_id: "key".into(),
            model_id: "model".into(),
        };
        let parent = store
            .create_session("P".into(), "/tmp".into(), selection.clone(), None, 0)
            .await
            .unwrap();
        let child = store
            .create_session(
                "C".into(),
                "/tmp".into(),
                selection,
                Some(parent.id.clone()),
                1,
            )
            .await
            .unwrap();
        let expected = format!("celoom:v1:{}", parent.id);
        assert_eq!(store.session_cache_key(&parent.id).await.unwrap(), expected);
        assert_eq!(store.session_cache_key(&child.id).await.unwrap(), expected);
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.request_audit_key().await.unwrap(), key);
        assert_eq!(
            reopened.session_cache_key(&child.id).await.unwrap(),
            expected
        );
        let text = reopened.session_jsonl(&parent.id).await.unwrap();
        assert!(!text.contains("request_audit_key"));
    }
    #[tokio::test]
    async fn host_takeover_preference_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open(&path).unwrap();
        assert!(store.computer_host_enabled().await.unwrap());
        store.set_computer_host_enabled(false).await.unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert!(!reopened.computer_host_enabled().await.unwrap());
        reopened.set_computer_host_enabled(true).await.unwrap();
        assert!(reopened.computer_host_enabled().await.unwrap());
    }
    #[tokio::test]
    async fn search_credentials_are_separate_redacted_and_endpoint_bound() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::open(&d.path().join("db")).unwrap();
        let mut config = AppConfig::default();
        config.search.provider = "brave".into();
        config.search.api_key = Some("separate-search-secret".into());
        let saved = store.save_config(config).await.unwrap();
        let redacted = saved.sanitized();
        assert!(redacted.search.api_key.is_none());
        assert!(redacted.search.has_api_key);
        let preserved = store.save_config(redacted).await.unwrap();
        assert_eq!(
            preserved.search.api_key.as_deref(),
            Some("separate-search-secret")
        );
        let mut changed = preserved.sanitized();
        changed.search.base_url = "https://other-search.example/search".into();
        let changed = store.save_config(changed).await.unwrap();
        assert!(changed.search.api_key.is_none());
        assert!(!changed.search.has_api_key);
    }
    #[tokio::test]
    async fn subagent_pool_config_validates_routes_efforts_and_unique_ids() {
        let mut config = configured();
        let selection = Selection {
            site_id: "site".into(),
            key_id: "key".into(),
            model_id: "model".into(),
        };
        config.subagent_models = vec![SubagentModel {
            id: "review".into(),
            name: "Reviewer".into(),
            selection: selection.clone(),
            reasoning_effort: Some("high".into()),
        }];
        validate_config(&config).unwrap();
        config.subagent_models[0].reasoning_effort = Some("unsupported".into());
        assert!(validate_config(&config).is_err());
        config.subagent_models[0].reasoning_effort = None;
        validate_config(&config).unwrap();
        config.subagent_models[0].reasoning_effort = Some("".into());
        validate_config(&config).unwrap();
        config.subagent_models[0].selection.model_id = "removed".into();
        assert!(
            validate_config(&config)
                .unwrap_err()
                .to_string()
                .contains("removed model")
        );
        config.subagent_models[0].selection = selection;
        config.subagent_models[0].id = "model".into();
        assert!(validate_config(&config).is_err());
        config.subagent_models[0].id = "inherit".into();
        assert!(validate_config(&config).is_err());
    }
    #[tokio::test]
    async fn config_revision_and_secret_preservation() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("db")).unwrap();
        let c = s.save_config(configured()).await.unwrap();
        assert_eq!(c.revision, 1);
        assert!(c.sanitized().sites[0].keys[0].secret.is_none());
        let saved = s.save_config(c.sanitized()).await.unwrap();
        assert_eq!(
            saved.sites[0].keys[0].secret.as_deref(),
            Some("test-secret")
        );
        assert!(
            s.save_config(c)
                .await
                .unwrap_err()
                .to_string()
                .contains("CONFIG_CONFLICT")
        );
        let mut changed = saved.sanitized();
        changed.sites[0].base_url = "https://other.example/v1".into();
        assert!(s.save_config(changed).await.is_err());
    }
    #[tokio::test]
    async fn messages_deduplicate_and_context_epochs_are_atomic() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("db")).unwrap();
        let selection = Selection {
            site_id: "s".into(),
            key_id: "k".into(),
            model_id: "m".into(),
        };
        let session = s
            .create_session("Test".into(), "/tmp".into(), selection, None, 0)
            .await
            .unwrap();
        let id = &session.id;
        assert!(
            s.accept_message(
                id,
                "msg",
                vec![json!({"role":"user","content":"hello"})],
                json!({"text":"hello"})
            )
            .await
            .unwrap()
        );
        assert!(
            !s.accept_message(id, "msg", vec![], json!({}))
                .await
                .unwrap()
        );
        assert_eq!(s.context(id).await.unwrap().len(), 1);
        s.replace_context(
            id,
            vec![json!({"role":"user","content":"summary"})],
            "summary",
        )
        .await
        .unwrap();
        assert_eq!(s.session(id).await.unwrap().context_epoch, 1);
        assert_eq!(s.context(id).await.unwrap()[0]["content"], "summary");
        assert_eq!(s.events(id, 0).await.unwrap().len(), 2);
    }
    #[tokio::test]
    async fn password_rotation_revokes_sessions_and_stale_verification() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("db")).unwrap();
        assert!(s.initialize_password("first".into()).await.unwrap());
        assert!(!s.initialize_password("overwrite".into()).await.unwrap());
        let expires = chrono::Utc::now().timestamp() + 3600;
        assert!(
            s.create_auth_session_checked("token".into(), expires, "first".into())
                .await
                .unwrap()
        );
        assert!(s.auth_session("token".into()).await.unwrap());
        s.set_password_hash("second".into()).await.unwrap();
        assert!(!s.auth_session("token".into()).await.unwrap());
        assert!(
            !s.create_auth_session_checked("stale".into(), expires, "first".into())
                .await
                .unwrap()
        );
        assert!(
            s.create_auth_session_checked("fresh".into(), expires, "second".into())
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    async fn restart_closes_pending_calls_without_repeating_side_effects() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("db");
        let s = Store::open(&path).unwrap();
        let session = s
            .create_session(
                "Crash".into(),
                "/tmp".into(),
                Selection {
                    site_id: "s".into(),
                    key_id: "k".into(),
                    model_id: "m".into(),
                },
                None,
                0,
            )
            .await
            .unwrap();
        s.set_status(&session.id, "running").await.unwrap();
        s.append_context(&session.id,vec![json!({"type":"function_call","call_id":"started","name":"bash","arguments":"{}"}),json!({"type":"function_call","call_id":"not-dispatched","name":"write","arguments":"{}"})]).await.unwrap();
        s.tool_started(&session.id, "started", "bash", &json!({}))
            .await
            .unwrap();
        drop(s);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(
            reopened.session(&session.id).await.unwrap().status,
            "interrupted"
        );
        assert_eq!(
            reopened
                .tool_result(&session.id, "started")
                .await
                .unwrap()
                .unwrap()
                .0,
            "unknown"
        );
        let context = reopened.context(&session.id).await.unwrap();
        assert_eq!(context.len(), 4);
        assert!(context[2]["output"].as_str().unwrap().contains("UNKNOWN"));
        assert!(
            context[3]["output"]
                .as_str()
                .unwrap()
                .contains("NOT executed")
        );
    }
    #[tokio::test]
    async fn tool_result_commits_once() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("db")).unwrap();
        let session = s
            .create_session(
                "T".into(),
                "/tmp".into(),
                Selection {
                    site_id: "s".into(),
                    key_id: "k".into(),
                    model_id: "m".into(),
                },
                None,
                0,
            )
            .await
            .unwrap();
        let id = &session.id;
        assert!(
            s.tool_started(id, "call", "read", &json!({"path":"a"}))
                .await
                .unwrap()
        );
        assert!(
            !s.tool_started(id, "call", "read", &json!({"path":"a"}))
                .await
                .unwrap()
        );
        assert!(
            s.tool_started(id, "call", "read", &json!({"path":"b"}))
                .await
                .is_err()
        );
        s.tool_finished(id, "call", "read", "ok", false)
            .await
            .unwrap();
        s.tool_finished(id, "call", "read", "duplicate", false)
            .await
            .unwrap();
        assert_eq!(s.context(id).await.unwrap().len(), 1);
        assert_eq!(s.tool_result(id, "call").await.unwrap().unwrap().1, "ok");
    }
    #[tokio::test]
    async fn session_encryption_half_full_unlock_decrypt_and_delete() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("db");
        let s = Store::open(&path).unwrap();
        let session = s
            .create_session(
                "Secret Chat".into(),
                "/tmp".into(),
                Selection {
                    site_id: "s".into(),
                    key_id: "k".into(),
                    model_id: "m".into(),
                },
                None,
                0,
            )
            .await
            .unwrap();
        let id = &session.id;
        s.append_event(id, "user_message", json!({"text": "top-secret-msg"}))
            .await
            .unwrap();
        assert!(d.path().join("sessions").join(format!("{id}.jsonl")).exists());

        // Half encryption: AES-256-GCM in SQLite, no plaintext .jsonl, seamless read
        let half = s.set_session_encryption(id, "half", None).await.unwrap();
        assert_eq!(half.encryption, "half");
        assert!(!half.locked);
        assert!(!d.path().join("sessions").join(format!("{id}.jsonl")).exists());
        let raw_db = Connection::open(&path).unwrap();
        let raw_data: String = raw_db
            .query_row("SELECT data FROM events WHERE session_id=?1", [id], |r| r.get(0))
            .unwrap();
        assert!(session_crypto::is_encrypted(&raw_data));
        assert!(!raw_data.contains("top-secret-msg"));
        assert_eq!(s.events(id, 0).await.unwrap()[0].data["text"], "top-secret-msg");

        // Full encryption with password, lock, wrong/right unlock, decrypt, delete
        let full = s
            .set_session_encryption(id, "full", Some("my-pass".into()))
            .await
            .unwrap();
        assert_eq!(full.encryption, "full");
        assert!(!full.locked);
        let locked = s.lock_session(id).await.unwrap();
        assert!(locked.locked);
        assert!(s.events(id, 0).await.is_err());
        assert!(s.unlock_session(id, "wrong").await.is_err());
        let unlocked = s.unlock_session(id, "my-pass").await.unwrap();
        assert!(!unlocked.locked);
        assert_eq!(s.events(id, 0).await.unwrap()[0].data["text"], "top-secret-msg");

        let plain = s
            .set_session_encryption(id, "none", Some("my-pass".into()))
            .await
            .unwrap();
        assert_eq!(plain.encryption, "none");
        assert!(d.path().join("sessions").join(format!("{id}.jsonl")).exists());

        s.delete_session(id).await.unwrap();
        assert!(!d.path().join("sessions").join(format!("{id}.jsonl")).exists());
        assert!(s.session(id).await.is_err());
    }
}
