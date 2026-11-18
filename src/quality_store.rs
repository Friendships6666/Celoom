//! Durable provider-quality checks, independent of chat sessions and never auto-retried.
use crate::{
    models::{Selection, now},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const MAX_PENDING: i64 = 4;
const MAX_REQUESTS: usize = 40;
const MAX_INITIAL_BYTES: usize = 512 * 1024;
const MAX_DATA_BYTES: usize = 4 * 1024 * 1024;
const RESTART_ERROR: &str = "Service restarted; checks were not retried.";

pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS quality_runs(
        id TEXT PRIMARY KEY NOT NULL,
        selection TEXT NOT NULL CHECK(json_valid(selection)),
        data TEXT NOT NULL CHECK(json_valid(data)),
        status TEXT NOT NULL CHECK(status IN ('queued','running','completed','cancelled','failed','interrupted')),
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS quality_runs_route_created ON quality_runs(
        json_extract(selection,'$.site_id'),json_extract(selection,'$.key_id'),
        json_extract(selection,'$.model_id'),created_at DESC,id DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS quality_runs_one_active_route ON quality_runs(
        json_extract(selection,'$.site_id'),json_extract(selection,'$.key_id'),
        json_extract(selection,'$.model_id')) WHERE status IN ('queued','running');")?;
    let pending = {
        let mut statement =
            tx.prepare("SELECT id,data FROM quality_runs WHERE status IN ('queued','running')")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, encoded) in pending {
        let data: Value = serde_json::from_str(&encoded)?;
        let timestamp = now();
        let data = interrupted_data(data, &timestamp);
        tx.execute(
            "UPDATE quality_runs SET status='interrupted',data=?2,updated_at=?3 WHERE id=?1",
            params![id, serde_json::to_string(&data)?, timestamp],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn interrupted_data(mut data: Value, timestamp: &str) -> Value {
    data["status"] = json!("interrupted");
    data["error"] = json!(RESTART_ERROR);
    data["updated_at"] = json!(timestamp);
    if !data["summary"].is_object() {
        data["summary"] = json!({});
    }
    data["summary"]["recovery"] = json!({"interrupted":true,"in_flight_usage":"unknown","remaining_probes":"cancelled","automatically_retried":false});
    if !data["summary"]["totals"].is_object() {
        data["summary"]["totals"] = json!({});
    }
    data["summary"]["totals"]["usage_complete"] = json!(false);
    data["summary"]["totals"]["estimated_cost"] = Value::Null;
    data
}

fn checked_id(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 128 && !value.contains('\0'),
        "Invalid quality check id"
    );
    Ok(())
}
fn checked_selection(value: &Value) -> Result<Selection> {
    let selection: Selection =
        serde_json::from_value(value.clone()).context("Invalid quality check selection")?;
    for id in [&selection.site_id, &selection.key_id, &selection.model_id] {
        checked_id(id)?;
    }
    Ok(selection)
}
fn active(status: &str) -> bool {
    matches!(status, "queued" | "running")
}
fn status_valid(status: &str) -> bool {
    matches!(
        status,
        "queued" | "running" | "completed" | "cancelled" | "failed" | "interrupted"
    )
}
fn safe_fields(value: &Value, depth: usize) -> Result<()> {
    ensure!(depth <= 96, "Quality report nesting limit exceeded");
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                let normalized = name.to_ascii_lowercase().replace(['-', '_'], "");
                ensure!(
                    !matches!(
                        normalized.as_str(),
                        "secret"
                            | "apikey"
                            | "authorization"
                            | "proxyauthorization"
                            | "cookie"
                            | "setcookie"
                            | "password"
                            | "accesstoken"
                            | "refreshtoken"
                            | "encryptedcontent"
                    ),
                    "Credential or opaque reasoning field is forbidden in quality report"
                );
                safe_fields(value, depth + 1)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                safe_fields(item, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn encoded_data(data: &Value, limit: usize) -> Result<String> {
    safe_fields(data, 0)?;
    let encoded = serde_json::to_string(data)?;
    ensure!(encoded.len() <= limit, "Quality report size limit exceeded");
    Ok(encoded)
}
fn load(db: &Connection, id: &str) -> Result<Value> {
    let encoded: String = db
        .query_row("SELECT data FROM quality_runs WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?
        .context("Quality check not found")?;
    Ok(serde_json::from_str(&encoded)?)
}

impl Store {
    pub async fn create_quality_run(&self, data: Value) -> Result<Value> {
        let object = data.as_object().context("Quality run must be an object")?;
        let keys = [
            "id",
            "selection",
            "source",
            "request",
            "plan",
            "status",
            "created_at",
            "updated_at",
            "completed_probes",
            "results",
            "summary",
            "error",
        ];
        ensure!(
            object.len() == keys.len() && keys.iter().all(|name| object.contains_key(*name)),
            "Invalid quality run fields"
        );
        let id = data["id"]
            .as_str()
            .context("Quality check id missing")?
            .to_owned();
        checked_id(&id)?;
        let selection = checked_selection(&data["selection"])?;
        ensure!(
            data["status"] == "queued",
            "New quality check must be queued"
        );
        ensure!(
            data["completed_probes"].as_u64() == Some(0)
                && data["results"].as_array().is_some_and(Vec::is_empty),
            "New quality check must have no results"
        );
        ensure!(
            data["source"].is_object()
                && data["request"].is_object()
                && data["plan"].is_object()
                && data["summary"].is_object()
                && data["error"].is_null(),
            "Invalid quality run payload"
        );
        ensure!(
            checked_selection(&data["request"]["selection"])? == selection
                && checked_selection(&data["plan"]["selection"])? == selection,
            "Quality check selections must match"
        );
        let count = data["plan"]["request_count"]
            .as_u64()
            .context("Quality request count missing")?;
        ensure!(
            (1..=MAX_REQUESTS as u64).contains(&count),
            "Quality check request limit exceeded (40)"
        );
        let created = data["created_at"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 64)
            .context("Quality creation time missing")?
            .to_owned();
        let updated = data["updated_at"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 64)
            .context("Quality update time missing")?
            .to_owned();
        let selection_json = serde_json::to_string(&selection)?;
        let encoded = encoded_data(&data, MAX_INITIAL_BYTES)?;
        self.call(move |db| {
            let tx = db.transaction()?;
            let pending:i64 = tx.query_row("SELECT COUNT(*) FROM quality_runs WHERE status IN ('queued','running')",[],|row|row.get(0))?;
            ensure!(pending<MAX_PENDING,"Quality check queue limit reached (4 active checks)");
            let exists:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM quality_runs WHERE status IN ('queued','running') AND json_extract(selection,'$.site_id')=?1 AND json_extract(selection,'$.key_id')=?2 AND json_extract(selection,'$.model_id')=?3)",params![selection.site_id,selection.key_id,selection.model_id],|row|row.get(0))?;
            ensure!(!exists,"A quality check is already active for this model selection");
            tx.execute("INSERT INTO quality_runs(id,selection,data,status,created_at,updated_at) VALUES(?1,?2,?3,'queued',?4,?5)",params![id,selection_json,encoded,created,updated])?;
            tx.commit()?;Ok(data)
        }).await
    }
    pub async fn quality_run(&self, id: &str) -> Result<Value> {
        checked_id(id)?;
        let id = id.to_owned();
        self.call(move |db| load(db, &id)).await
    }
    pub async fn quality_runs(&self, selection: Selection) -> Result<Vec<Value>> {
        checked_selection(&serde_json::to_value(&selection)?)?;
        self.call(move |db| {
            let mut statement=db.prepare("SELECT data FROM quality_runs WHERE json_extract(selection,'$.site_id')=?1 AND json_extract(selection,'$.key_id')=?2 AND json_extract(selection,'$.model_id')=?3 ORDER BY created_at DESC,id DESC LIMIT 10")?;
            let rows=statement.query_map(params![selection.site_id,selection.key_id,selection.model_id],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(|encoded|Ok(serde_json::from_str(&encoded)?)).collect()
        }).await
    }
    pub async fn update_quality_run(
        &self,
        id: &str,
        status: &str,
        completed_probes: usize,
        results: Vec<Value>,
        summary: Value,
        error: Option<String>,
    ) -> Result<Value> {
        checked_id(id)?;
        ensure!(status_valid(status), "Invalid quality check status");
        ensure!(
            completed_probes == results.len() && completed_probes <= MAX_REQUESTS,
            "Quality progress count does not match results"
        );
        ensure!(summary.is_object(), "Quality summary must be an object");
        ensure!(
            error.as_ref().is_none_or(|value| value.len() <= 16384),
            "Quality error size limit exceeded"
        );
        ensure!(
            results.iter().all(Value::is_object),
            "Quality results must be objects"
        );
        let id = id.to_owned();
        let status = status.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            let old = load(&tx, &id)?;
            let maximum = old["plan"]["request_count"]
                .as_u64()
                .context("Invalid stored quality plan")? as usize;
            ensure!(
                completed_probes <= maximum,
                "Quality results exceed planned request count"
            );
            let previous = old["results"]
                .as_array()
                .context("Invalid stored quality results")?;
            ensure!(
                results.len() >= previous.len() && results[..previous.len()] == previous[..],
                "Quality results must append without replacing old results"
            );
            let same = old["status"] == status
                && old["completed_probes"].as_u64() == Some(completed_probes as u64)
                && old["results"] == json!(results)
                && old["summary"] == summary
                && old["error"] == json!(error);
            if same {
                return Ok(old);
            }
            let old_status = old["status"]
                .as_str()
                .context("Invalid stored quality status")?;
            ensure!(active(old_status), "Terminal quality check is immutable");
            ensure!(
                old_status != "running" || status != "queued",
                "Running quality check cannot be queued again"
            );
            let mut updated = old;
            updated["status"] = json!(status);
            updated["completed_probes"] = json!(completed_probes);
            updated["results"] = json!(results);
            updated["summary"] = summary;
            updated["error"] = json!(error);
            let timestamp = now();
            updated["updated_at"] = json!(timestamp);
            let encoded = encoded_data(&updated, MAX_DATA_BYTES)?;
            if active(&status) {
                // Reserve enough report space for startup interruption metadata,
                // without truncating previously committed probe observations.
                encoded_data(
                    &interrupted_data(updated.clone(), &timestamp),
                    MAX_DATA_BYTES,
                )?;
            }
            tx.execute(
                "UPDATE quality_runs SET data=?2,status=?3,updated_at=?4 WHERE id=?1",
                params![id, encoded, status, timestamp],
            )?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection(site: &str, key: &str, model: &str) -> Selection {
        Selection {
            site_id: site.into(),
            key_id: key.into(),
            model_id: model.into(),
        }
    }
    fn run(id: &str, selection: Selection, count: usize) -> Value {
        json!({"id":id,"selection":selection,"source":{"site_name":"Synthetic","model_name":"Fixture","model":"same-model-name","endpoint":"http://127.0.0.1:1/v1/responses","api":"responses","config_revision":1},"request":{"selection":selection,"groups":["routing"]},"plan":{"selection":selection,"request_count":count,"probes":[]},"status":"queued","created_at":now(),"updated_at":now(),"completed_probes":0,"results":[],"summary":{},"error":null})
    }
    fn result(id: &str) -> Value {
        json!({"id":id,"label":"Synthetic","group":"routing","variant":"control","repeat":1,"status":"ok","observation":{"response_model":"fixture","usage":{"input_tokens":12}},"analysis":{"model_match":true},"cost":{"currency":"USD","estimated_cost":null,"complete":false,"notes":[]}})
    }
    async fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("quality.sqlite3")).unwrap();
        store.call(migrate).await.unwrap();
        (dir, store)
    }
    #[tokio::test]
    async fn independent_runs_need_no_session_and_round_trip() {
        let (_dir, store) = fixture().await;
        let route = selection("s", "k", "m");
        let data = run("check-one", route.clone(), 3);
        assert_eq!(store.create_quality_run(data.clone()).await.unwrap(), data);
        assert_eq!(store.quality_run("check-one").await.unwrap(), data);
        assert_eq!(store.quality_runs(route).await.unwrap(), vec![data]);
        assert!(store.quality_run("missing").await.is_err());
        assert_eq!(
            store
                .call(
                    |db| Ok(db.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                        .get::<_, usize>(0))?)
                )
                .await
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn route_identity_uses_all_three_ids_and_global_cap() {
        let (_dir, store) = fixture().await;
        for (i, route) in [
            selection("s", "k", "m"),
            selection("s", "other", "m"),
            selection("other", "k", "m"),
            selection("s", "k", "other"),
        ]
        .into_iter()
        .enumerate()
        {
            store
                .create_quality_run(run(&format!("r{i}"), route, 2))
                .await
                .unwrap();
        }
        assert!(
            store
                .create_quality_run(run("fifth", selection("fifth", "k", "m"), 2))
                .await
                .is_err()
        );
        assert!(
            store
                .create_quality_run(run("duplicate", selection("s", "k", "m"), 2))
                .await
                .is_err()
        );
        store
            .update_quality_run("r0", "cancelled", 0, vec![], json!({}), None)
            .await
            .unwrap();
        store
            .create_quality_run(run("new-route", selection("s", "k", "m"), 2))
            .await
            .unwrap();
        assert_eq!(
            store
                .quality_runs(selection("other", "k", "m"))
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn append_preserves_original_identity_source_plan_and_results() {
        let (_dir, store) = fixture().await;
        let data = run("append", selection("s", "k", "m"), 3);
        store.create_quality_run(data.clone()).await.unwrap();
        let first = store
            .update_quality_run(
                "append",
                "running",
                1,
                vec![result("p1")],
                json!({"totals":{"input_tokens":12}}),
                None,
            )
            .await
            .unwrap();
        let second = store
            .update_quality_run(
                "append",
                "completed",
                2,
                vec![result("p1"), result("p2")],
                json!({"ok":true}),
                None,
            )
            .await
            .unwrap();
        for field in ["id", "selection", "source", "request", "plan", "created_at"] {
            assert_eq!(second[field], data[field]);
        }
        assert_eq!(second["results"][0], first["results"][0]);
        assert_eq!(second["completed_probes"], 2);
    }
    #[tokio::test]
    async fn terminal_rows_are_immutable_but_exact_retry_keeps_timestamp() {
        let (_dir, store) = fixture().await;
        store
            .create_quality_run(run("terminal", selection("s", "k", "m"), 2))
            .await
            .unwrap();
        let done = store
            .update_quality_run(
                "terminal",
                "failed",
                1,
                vec![result("p1")],
                json!({"partial":true}),
                Some("bounded failure".into()),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .update_quality_run(
                    "terminal",
                    "failed",
                    1,
                    vec![result("p1")],
                    json!({"partial":true}),
                    Some("bounded failure".into())
                )
                .await
                .unwrap(),
            done
        );
        assert!(
            store
                .update_quality_run(
                    "terminal",
                    "running",
                    1,
                    vec![result("p1")],
                    json!({}),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            store
                .update_quality_run(
                    "terminal",
                    "failed",
                    2,
                    vec![result("p1"), result("p2")],
                    json!({"partial":true}),
                    Some("bounded failure".into())
                )
                .await
                .is_err()
        );
        assert_eq!(store.quality_run("terminal").await.unwrap(), done);
    }
    #[tokio::test]
    async fn bad_progress_or_replacement_does_not_overwrite() {
        let (_dir, store) = fixture().await;
        store
            .create_quality_run(run("progress", selection("s", "k", "m"), 2))
            .await
            .unwrap();
        let first = store
            .update_quality_run(
                "progress",
                "running",
                1,
                vec![result("p1")],
                json!({}),
                None,
            )
            .await
            .unwrap();
        assert!(
            store
                .update_quality_run("progress", "running", 0, vec![], json!({}), None)
                .await
                .is_err()
        );
        assert!(
            store
                .update_quality_run(
                    "progress",
                    "running",
                    1,
                    vec![result("different")],
                    json!({}),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            store
                .update_quality_run(
                    "progress",
                    "running",
                    2,
                    vec![result("p1")],
                    json!({}),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            store
                .update_quality_run(
                    "progress",
                    "completed",
                    3,
                    vec![result("p1"), result("p2"), result("p3")],
                    json!({}),
                    None
                )
                .await
                .is_err()
        );
        assert!(
            store
                .update_quality_run("progress", "queued", 1, vec![result("p1")], json!({}), None)
                .await
                .is_err()
        );
        assert_eq!(store.quality_run("progress").await.unwrap(), first);
    }
    #[tokio::test]
    async fn invalid_create_counts_and_payload_caps_are_atomic() {
        let (_dir, store) = fixture().await;
        let route = selection("s", "k", "m");
        for count in [0, 41] {
            assert!(
                store
                    .create_quality_run(run("bad-count", route.clone(), count))
                    .await
                    .is_err()
            );
        }
        let mut data = run("bad-size", route.clone(), 2);
        data["source"]["oversize"] = json!("x".repeat(MAX_INITIAL_BYTES));
        assert!(store.create_quality_run(data).await.is_err());
        let mut data = run("bad-utf8-size", route.clone(), 2);
        data["source"]["oversize"] = json!("界".repeat(MAX_INITIAL_BYTES / 2));
        assert!(store.create_quality_run(data).await.is_err());
        let mut data = run("bad-results", route.clone(), 2);
        data["results"] = json!([result("p1")]);
        assert!(store.create_quality_run(data).await.is_err());
        let mut data = run("bad-route", route.clone(), 2);
        data["plan"]["selection"]["key_id"] = json!("other");
        assert!(store.create_quality_run(data).await.is_err());
        assert!(store.quality_runs(route.clone()).await.unwrap().is_empty());
        store
            .create_quality_run(run("valid", route, 40))
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn oversize_update_and_trigger_failure_roll_back() {
        let (_dir, store) = fixture().await;
        let data = run("atomic", selection("s", "k", "m"), 2);
        store.create_quality_run(data.clone()).await.unwrap();
        assert!(
            store
                .update_quality_run(
                    "atomic",
                    "running",
                    1,
                    vec![result("p1")],
                    json!({"large":"x".repeat(MAX_DATA_BYTES)}),
                    None
                )
                .await
                .is_err()
        );
        assert_eq!(store.quality_run("atomic").await.unwrap(), data);
        store.call(|db|{db.execute_batch("CREATE TRIGGER reject_quality_update BEFORE UPDATE ON quality_runs BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;")?;Ok(())}).await.unwrap();
        assert!(
            store
                .update_quality_run("atomic", "running", 1, vec![result("p1")], json!({}), None)
                .await
                .is_err()
        );
        assert_eq!(store.quality_run("atomic").await.unwrap(), data);
    }
    #[tokio::test]
    async fn credential_fields_are_rejected_without_scanning_arbitrary_text() {
        let (_dir, store) = fixture().await;
        let route = selection("s", "k", "m");
        for field in [
            "secret",
            "api_key",
            "Authorization",
            "set-cookie",
            "encrypted_content",
        ] {
            let mut data = run("bad", route.clone(), 2);
            data["source"][field] = json!("not-a-real-secret");
            assert!(store.create_quality_run(data).await.is_err());
        }
        store
            .create_quality_run(run("safe", route, 2))
            .await
            .unwrap();
        let mut probe = result("p1");
        probe["observation"]["text_sample"] =
            json!("The word authorization appears in this legitimate answer.");
        store
            .update_quality_run("safe", "running", 1, vec![probe.clone()], json!({}), None)
            .await
            .unwrap();
        probe["observation"]["headers"]["authorization"] = json!("dummy");
        assert!(
            store
                .update_quality_run(
                    "safe",
                    "running",
                    2,
                    vec![result("p1"), probe],
                    json!({}),
                    None
                )
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn latest_ten_filter_exact_tuple_and_terminal_does_not_block_new_run() {
        let (_dir, store) = fixture().await;
        let route = selection("s", "k", "m");
        for n in 0..12 {
            let id = format!("r{n:02}");
            let mut data = run(&id, route.clone(), 1);
            data["created_at"] = json!(format!("2026-11-16T00:00:{n:02}.000Z"));
            store.create_quality_run(data).await.unwrap();
            store
                .update_quality_run(&id, "completed", 1, vec![result("p1")], json!({}), None)
                .await
                .unwrap();
        }
        store
            .create_quality_run(run("other", selection("s", "other", "m"), 1))
            .await
            .unwrap();
        let list = store.quality_runs(route).await.unwrap();
        assert_eq!(list.len(), 10);
        assert_eq!(list[0]["id"], "r11");
        assert_eq!(list[9]["id"], "r02");
    }
    #[tokio::test]
    async fn migration_interrupts_partial_checks_preserves_data_and_is_idempotent() {
        let (dir, store) = fixture().await;
        let data = run("restart", selection("s", "k", "m"), 3);
        store.create_quality_run(data.clone()).await.unwrap();
        let partial=store.update_quality_run("restart","running",1,vec![result("p1")],json!({"totals":{"requests_completed":1,"input_tokens":12,"usage_complete":true,"estimated_cost":0.1}}),None).await.unwrap();
        store.call(migrate).await.unwrap();
        let recovered = store.quality_run("restart").await.unwrap();
        assert_eq!(recovered["status"], "interrupted");
        assert_eq!(recovered["error"], RESTART_ERROR);
        for field in [
            "id",
            "selection",
            "source",
            "request",
            "plan",
            "created_at",
            "results",
            "completed_probes",
        ] {
            assert_eq!(recovered[field], partial[field]);
        }
        assert_eq!(recovered["summary"]["totals"]["input_tokens"], 12);
        assert_eq!(recovered["summary"]["totals"]["usage_complete"], false);
        assert!(recovered["summary"]["totals"]["estimated_cost"].is_null());
        assert_eq!(
            recovered["summary"]["recovery"]["in_flight_usage"],
            "unknown"
        );
        store.call(migrate).await.unwrap();
        assert_eq!(store.quality_run("restart").await.unwrap(), recovered);
        drop(store);
        let reopened = Store::open(&dir.path().join("quality.sqlite3")).unwrap();
        reopened.call(migrate).await.unwrap();
        assert_eq!(reopened.quality_run("restart").await.unwrap(), recovered);
    }
    #[tokio::test]
    async fn reopening_store_interrupts_active_partial_run_without_starting_a_session() {
        let (dir, store) = fixture().await;
        store
            .create_quality_run(run("actual-restart", selection("s", "k", "m"), 3))
            .await
            .unwrap();
        let partial=store.update_quality_run("actual-restart","running",1,vec![result("p1")],json!({"model_identity":{"requested":"fixture","reported":["fixture"],"mismatches":0}}),None).await.unwrap();
        drop(store);
        let reopened = Store::open(&dir.path().join("quality.sqlite3")).unwrap();
        let restored = reopened.quality_run("actual-restart").await.unwrap();
        assert_eq!(restored["status"], "interrupted");
        assert_eq!(restored["results"], partial["results"]);
        assert_eq!(restored["source"], partial["source"]);
        assert_eq!(
            restored["summary"]["model_identity"],
            partial["summary"]["model_identity"]
        );
        assert_eq!(restored["summary"]["totals"]["usage_complete"], false);
        assert_eq!(
            reopened
                .call(
                    |db| Ok(db.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                        .get::<_, usize>(0))?)
                )
                .await
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn queued_checks_interrupt_without_results_or_paid_retries() {
        let (_dir, store) = fixture().await;
        store
            .create_quality_run(run("queued", selection("s", "k", "m"), 1))
            .await
            .unwrap();
        store.call(migrate).await.unwrap();
        let recovered = store.quality_run("queued").await.unwrap();
        assert_eq!(recovered["status"], "interrupted");
        assert_eq!(recovered["results"], json!([]));
        assert_eq!(recovered["completed_probes"], 0);
        assert_eq!(
            recovered["summary"]["recovery"]["automatically_retried"],
            false
        );
        store
            .create_quality_run(run("new", selection("s", "k", "m"), 1))
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn migration_failure_rolls_back_every_interruption_and_terminal_rows_stay_unchanged() {
        let (_dir, store) = fixture().await;
        let first = run("first", selection("s", "k", "first"), 2);
        let second = run("second", selection("s", "k", "second"), 2);
        store.create_quality_run(first.clone()).await.unwrap();
        store.create_quality_run(second.clone()).await.unwrap();
        store
            .create_quality_run(run("done", selection("s", "k", "done"), 1))
            .await
            .unwrap();
        let terminal = store
            .update_quality_run(
                "done",
                "completed",
                1,
                vec![result("p1")],
                json!({"kept":true}),
                None,
            )
            .await
            .unwrap();
        store.call(|db|{db.execute_batch("CREATE TRIGGER fail_second_recovery BEFORE UPDATE ON quality_runs WHEN OLD.id='second' BEGIN SELECT RAISE(ABORT,'synthetic recovery failure'); END;")?;Ok(())}).await.unwrap();
        assert!(store.call(migrate).await.is_err());
        assert_eq!(store.quality_run("first").await.unwrap(), first);
        assert_eq!(store.quality_run("second").await.unwrap(), second);
        assert_eq!(store.quality_run("done").await.unwrap(), terminal);
        store
            .call(|db| {
                db.execute_batch("DROP TRIGGER fail_second_recovery")?;
                Ok(())
            })
            .await
            .unwrap();
        store.call(migrate).await.unwrap();
        assert_eq!(
            store.quality_run("first").await.unwrap()["status"],
            "interrupted"
        );
        assert_eq!(
            store.quality_run("second").await.unwrap()["status"],
            "interrupted"
        );
        assert_eq!(store.quality_run("done").await.unwrap(), terminal);
    }
    #[tokio::test]
    async fn simultaneous_creates_cannot_bypass_global_or_route_limits() {
        let (_dir, store) = fixture().await;
        let route = selection("s", "k", "m");
        let (a, b) = tokio::join!(
            store.create_quality_run(run("a", route.clone(), 1)),
            store.create_quality_run(run("b", route.clone(), 1))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(store.quality_runs(route).await.unwrap().len(), 1);
        for n in 0..3 {
            store
                .create_quality_run(run(
                    &format!("seed{n}"),
                    selection("s", "k", &format!("m{n}")),
                    1,
                ))
                .await
                .unwrap();
        }
        let (a, b) = tokio::join!(
            store.create_quality_run(run("fifth", selection("s", "k", "fifth"), 1)),
            store.create_quality_run(run("sixth", selection("s", "k", "sixth"), 1))
        );
        assert!(a.is_err() && b.is_err());
    }
}
