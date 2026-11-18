use crate::models::*;
use anyhow::{Context, Result, anyhow, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, sync::mpsc};
use tokio::sync::oneshot;

type Operation = Box<dyn FnOnce(&mut Connection, &mut SessionMirror) + Send + 'static>;
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Operation>,
    data_dir: std::sync::Arc<std::path::PathBuf>,
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
        db.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('config',?1)",
            [serde_json::to_string(&AppConfig::default())?],
        )?;
        crate::autonomy_store::migrate(&mut db)?;
        crate::message_queue::migrate(&mut db)?;
        crate::quality_store::migrate(&mut db)?;
        recover_interrupted(&mut db)?;
        let mut mirror = SessionMirror::open(
            &db,
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .join("sessions"),
        )?;
        let (tx, rx) = mpsc::channel::<Operation>();
        std::thread::Builder::new()
            .name("justai-sqlite".into())
            .spawn(move || {
                for op in rx {
                    op(&mut db, &mut mirror);
                }
            })?;
        let data_dir = std::sync::Arc::new(std::fs::canonicalize(
            path.parent().unwrap_or_else(|| Path::new(".")),
        )?);
        Ok(Self { tx, data_dir })
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
                    None => return Ok(format!("justai:v1:{}", session.id)),
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
            let s = Session { id:new_id(),title,cwd,selection,reasoning_effort:None,status:"idle".into(),created_at:now(),updated_at:now(),parent_id,depth,context_epoch:0 };
            db.execute("INSERT INTO sessions(id,title,cwd,selection,status,created_at,updated_at,parent_id,depth,context_epoch) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![s.id,s.title,s.cwd,serde_json::to_string(&s.selection)?,s.status,s.created_at,s.updated_at,s.parent_id,s.depth,s.context_epoch])?;
            Ok(s)
        }).await
    }
    pub async fn session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();
        self.call(move |db| load_session(db, &id)).await
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
            let mut stmt=db.prepare("SELECT seq,session_id,kind,data,created_at FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq")?;
            let rows=stmt.query_map(params![id,after],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?)))?;
            rows.map(|r| { let (seq,session_id,kind,data,created_at)=r?;Ok(Event{seq,session_id,kind,data:serde_json::from_str(&data)?,created_at}) }).collect()
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
            let mut stmt=db.prepare("SELECT item FROM context_items WHERE session_id=?1 AND epoch=(SELECT context_epoch FROM sessions WHERE id=?1) ORDER BY id")?;
            let rows=stmt.query_map([id],|r|r.get::<_,String>(0))?;
            rows.map(|r|Ok(serde_json::from_str(&r?)?)).collect()
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
    ) -> Result<()> {
        let id = id.to_owned();
        let attempt_id = attempt_id.to_owned();
        let text = text.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            context_items(&tx, &id, items)?;
            event(
                &tx,
                &id,
                "assistant_message",
                json!({"attempt_id":attempt_id,"id":new_id(),"text":text}),
            )?;
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
            if let Some((n,a))=prior {ensure!(n==name && serde_json::from_str::<Value>(&a)?==args,"Tool call identity collision: arguments differ");return Ok(false);}
            tx.execute("INSERT INTO executions(session_id,call_id,name,args,status) VALUES(?1,?2,?3,?4,'running')",params![id,call_id,name,serde_json::to_string(&args)?])?;
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
            tx.execute(
                "UPDATE executions SET status=?3,output=?4 WHERE session_id=?1 AND call_id=?2",
                params![
                    id,
                    call_id,
                    if is_error { "failed" } else { "succeeded" },
                    output
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
            Ok(db
                .query_row(
                    "SELECT status,output FROM executions WHERE session_id=?1 AND call_id=?2",
                    params![id, call_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
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
            mirror.replace_log(&id, session_log_text(db, &id)?.as_bytes())?;
            mirror.known.insert(id);
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
        // Write a versioned header before any events of a newly created session.
        for id in all_session_ids(db)? {
            if !self.known.contains(&id) {
                let header =
                    json!({"type":"session","schema_version":1,"session":load_session(db,&id)?});
                self.replace_log(
                    &id,
                    format!("{}\n", serde_json::to_string(&header)?).as_bytes(),
                )?;
                self.known.insert(id);
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
        let event = Event {
            seq,
            session_id: id.to_owned(),
            kind,
            data: portable_log_value(&serde_json::from_str::<Value>(&data)?),
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
    let raw=db.query_row("SELECT id,title,cwd,selection,status,created_at,updated_at,parent_id,depth,context_epoch,reasoning_effort FROM sessions WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get::<_,Option<String>>(10)?))).optional()?.ok_or_else(||anyhow!("Session not found"))?;
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
    })
}
pub(crate) fn event(tx: &Transaction<'_>, id: &str, kind: &str, data: Value) -> Result<Event> {
    let created_at = now();
    tx.execute(
        "INSERT INTO events(session_id,kind,data,created_at) VALUES(?1,?2,?3,?4)",
        params![id, kind, serde_json::to_string(&data)?, created_at],
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
        tx.execute(
            "INSERT INTO context_items(session_id,epoch,item) VALUES(?1,?2,?3)",
            params![id, epoch, serde_json::to_string(&item)?],
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
        let items = {
            let mut s=tx.prepare("SELECT item FROM context_items WHERE session_id=?1 AND epoch=(SELECT context_epoch FROM sessions WHERE id=?1) ORDER BY id")?;
            s.query_map([&id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut calls = Vec::new();
        let mut results = HashSet::new();
        for item in items {
            let item: Value = serde_json::from_str(&item)?;
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
        let expected = format!("justai:v1:{}", parent.id);
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
}
