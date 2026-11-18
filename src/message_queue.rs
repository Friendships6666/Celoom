//! Durable live-message admission. Only the run owner may drain at a closed boundary.
use crate::{
    models::now,
    store::{Store, context_items, event, load_session},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const MAX_QUEUED_MESSAGES: i64 = 64;
const MAX_QUEUED_BYTES: i64 = 32 * 1024 * 1024;

pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_queue(
            position INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            client_id TEXT NOT NULL,
            items TEXT NOT NULL,
            event_data TEXT NOT NULL,
            mode TEXT NOT NULL CHECK(mode IN ('after_call','immediate')),
            created_at TEXT NOT NULL,
            UNIQUE(session_id,client_id),
            FOREIGN KEY(session_id,client_id) REFERENCES accepted_messages(session_id,client_id)
        );
        CREATE TABLE IF NOT EXISTS message_escalations(
            session_id TEXT NOT NULL REFERENCES sessions(id),
            client_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY(session_id,client_id)
        );
        CREATE INDEX IF NOT EXISTS message_queue_session_position
            ON message_queue(session_id,position);
        CREATE INDEX IF NOT EXISTS events_user_message_client
            ON events(session_id,json_extract(data,'$.client_message_id'),seq)
            WHERE kind='user_message';",
    )?;
    Ok(())
}

impl Store {
    /// Accept once, sharing the initial-message path's deduplication transaction.
    /// No context is exposed until `drain_messages` is explicitly called.
    pub async fn queue_message(
        &self,
        id: &str,
        client_id: &str,
        items: Vec<Value>,
        mut data: Value,
    ) -> Result<bool> {
        let id = id.to_owned();
        let client_id = client_id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &id)?;
            if tx.execute(
                "INSERT OR IGNORE INTO accepted_messages(session_id,client_id) VALUES(?1,?2)",
                params![id, client_id],
            )? == 0
            {
                // An accepted UUID is immutable, even if a retry has another payload.
                return Ok(false);
            }
            let mode = data.get("delivery_mode").and_then(Value::as_str);
            ensure!(
                matches!(mode, Some("after_call" | "immediate")),
                "Invalid message delivery mode"
            );
            let promoted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM message_escalations WHERE session_id=?1 AND client_id=?2)",params![id,client_id],|row|row.get(0))?;
            let mode = if promoted{"immediate".to_owned()}else{mode.unwrap().to_owned()};
            let object = data.as_object_mut().expect("a mode requires an object");
            object.insert("queued".into(), Value::Bool(true));
            object.insert("delivery_mode".into(),json!(mode));
            object.insert("client_message_id".into(), Value::String(client_id.clone()));
            let encoded_items = serde_json::to_string(&items)?;
            let encoded_data = serde_json::to_string(&data)?;
            // Count UTF-8 bytes actually persisted, including both context and event copies.
            let bytes = encoded_items.len() as i64 + encoded_data.len() as i64;
            let (count, queued_bytes): (i64, i64) = tx.query_row(
                "SELECT count(*),coalesce(sum(length(CAST(items AS BLOB)) +
                    length(CAST(event_data AS BLOB))),0)
                 FROM message_queue WHERE session_id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            ensure!(
                count < MAX_QUEUED_MESSAGES,
                "Message queue limit reached (64 messages)"
            );
            ensure!(
                bytes <= MAX_QUEUED_BYTES && queued_bytes <= MAX_QUEUED_BYTES - bytes,
                "Message queue size limit reached (32 MiB)"
            );
            tx.execute(
                "INSERT INTO message_queue(session_id,client_id,items,event_data,mode,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, client_id, encoded_items, encoded_data, mode, now()],
            )?;
            event(&tx, &id, "user_message", data)?;
            if promoted{event(&tx,&id,"message_interrupted",json!({"client_message_id":client_id,"delivery_mode":"immediate"}))?;}
            tx.execute("DELETE FROM message_escalations WHERE session_id=?1 AND client_id=?2",params![id,client_id])?;
            tx.execute(
                "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                params![id, now()],
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    /// Atomically admit the whole FIFO, in the current epoch, exactly once.
    /// Immediate messages never overtake older after-call messages.
    pub async fn drain_messages(&self, id: &str) -> Result<usize> {
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &id)?;
            let pending = {
                let mut statement = tx.prepare(
                    "SELECT client_id,items FROM message_queue WHERE session_id=?1 ORDER BY position",
                )?;
                statement
                    .query_map([&id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (client_id, items) in &pending {
                context_items(&tx, &id, serde_json::from_str::<Vec<Value>>(items)?)?;
                event(
                    &tx,
                    &id,
                    "message_delivered",
                    json!({"client_message_id":client_id}),
                )?;
            }
            if !pending.is_empty() {
                tx.execute("DELETE FROM message_queue WHERE session_id=?1", [&id])?;
                tx.execute(
                    "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                    params![id, now()],
                )?;
            }
            tx.commit()?;
            Ok(pending.len())
        })
        .await
    }

    /// Esc promotes the SAME accepted message. If its HTTP acceptance is still
    /// in flight, record a bounded durable marker so the race cannot lose intent.
    pub async fn interrupt_message(&self, id: &str, client_id: &str) -> Result<Value> {
        let id = id.to_owned();
        let client_id = client_id.to_owned();
        ensure!(
            uuid::Uuid::parse_str(&client_id).is_ok(),
            "Invalid client_message_id UUID"
        );
        self.call(move|db|{
            let tx=db.transaction()?;load_session(&tx,&id)?;
            let row:Option<(String,String)>=tx.query_row("SELECT mode,event_data FROM message_queue WHERE session_id=?1 AND client_id=?2",params![id,client_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            let (promoted,delivered,pending)=if let Some((mode,event_data))=row{
                if mode!="immediate"{
                    let mut data:Value=serde_json::from_str(&event_data)?;data["delivery_mode"]=json!("immediate");
                    tx.execute("UPDATE message_queue SET mode='immediate',event_data=?3 WHERE session_id=?1 AND client_id=?2",params![id,client_id,serde_json::to_string(&data)?])?;
                    event(&tx,&id,"message_interrupted",json!({"client_message_id":client_id,"delivery_mode":"immediate"}))?;
                }
                (true,false,false)
            }else{
                let accepted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM accepted_messages WHERE session_id=?1 AND client_id=?2)",params![id,client_id],|row|row.get(0))?;
                if accepted{(false,true,false)}else{
                    let cutoff=(chrono::Utc::now()-chrono::Duration::days(7)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                    tx.execute("DELETE FROM message_escalations WHERE created_at<?1",[cutoff])?;
                    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM message_escalations WHERE session_id=?1 AND client_id=?2)",params![id,client_id],|row|row.get(0))?;
                    if !exists{
                        let count:i64=tx.query_row("SELECT COUNT(*) FROM message_escalations WHERE session_id=?1",[&id],|row|row.get(0))?;
                        ensure!(count<MAX_QUEUED_MESSAGES,"Too many pending message interruptions (64)");
                        tx.execute("INSERT INTO message_escalations(session_id,client_id,created_at) VALUES(?1,?2,?3)",params![id,client_id,now()])?;
                    }
                    (false,false,true)
                }
            };
            if delivered||promoted{tx.execute("DELETE FROM message_escalations WHERE session_id=?1 AND client_id=?2",params![id,client_id])?;}
            tx.commit()?;Ok(json!({"client_message_id":client_id,"promoted":promoted,"delivered":delivered,"pending_acceptance":pending}))
        }).await
    }
    pub async fn has_immediate_messages(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.call(move |db| {
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_queue WHERE session_id=?1 AND mode='immediate')",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn queued_messages(&self, id: &str) -> Result<usize> {
        let id = id.to_owned();
        self.call(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM message_queue WHERE session_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn accepted_message_run(&self, id: &str, client_id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        let client_id = client_id.to_owned();
        self.call(move |db| {
            Ok(db
                .query_row(
                    "SELECT json_extract(data,'$.run_id') FROM events
                     WHERE session_id=?1 AND kind='user_message'
                       AND json_extract(data,'$.client_message_id')=?2
                     ORDER BY seq LIMIT 1",
                    params![id, client_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Selection;

    async fn fixture() -> (tempfile::TempDir, Store, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.sqlite")).unwrap();
        let id = session(&store).await;
        (dir, store, id)
    }

    async fn session(store: &Store) -> String {
        store
            .create_session(
                "Queue test".into(),
                "/tmp".into(),
                Selection {
                    site_id: "site".into(),
                    key_id: "key".into(),
                    model_id: "model".into(),
                },
                None,
                0,
            )
            .await
            .unwrap()
            .id
    }

    fn items(text: &str) -> Vec<Value> {
        vec![json!({"role":"user","content":[{"type":"input_text","text":text}]})]
    }

    fn data(client: &str, mode: &str) -> Value {
        json!({"id":client,"text":client,"images":[],"client_message_id":client,
               "run_id":format!("run-{client}"),"delivery_mode":mode})
    }

    async fn enqueue(store: &Store, id: &str, client: &str, mode: &str) -> bool {
        store
            .queue_message(id, client, items(client), data(client, mode))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn esc_promotes_same_queued_message_once_without_rewriting_items() {
        let (_dir, store, id) = fixture().await;
        let client = crate::models::new_id();
        assert!(enqueue(&store, &id, &client, "after_call").await);
        let before = store.events(&id, 0).await.unwrap().len();
        let promoted = store.interrupt_message(&id, &client).await.unwrap();
        assert_eq!(promoted["promoted"], true);
        assert!(store.has_immediate_messages(&id).await.unwrap());
        assert_eq!(store.queued_messages(&id).await.unwrap(), 1);
        assert_eq!(store.context(&id).await.unwrap().len(), 0);
        assert_eq!(
            store.interrupt_message(&id, &client).await.unwrap(),
            promoted
        );
        assert_eq!(store.events(&id, 0).await.unwrap().len(), before + 1);
        store.drain_messages(&id).await.unwrap();
        assert_eq!(store.context(&id).await.unwrap(), items(&client));
        let delivered = store.interrupt_message(&id, &client).await.unwrap();
        assert_eq!(delivered["delivered"], true);
        assert_eq!(delivered["promoted"], false);
        assert!(!store.has_immediate_messages(&id).await.unwrap());
    }
    #[tokio::test]
    async fn early_esc_survives_accept_race_and_initial_message_is_already_delivered() {
        let (_dir, store, id) = fixture().await;
        let client = crate::models::new_id();
        assert_eq!(
            store.interrupt_message(&id, &client).await.unwrap()["pending_acceptance"],
            true
        );
        assert_eq!(
            store.interrupt_message(&id, &client).await.unwrap()["pending_acceptance"],
            true
        );
        assert!(enqueue(&store, &id, &client, "after_call").await);
        assert!(store.has_immediate_messages(&id).await.unwrap());
        let events = store.events(&id, 0).await.unwrap();
        assert_eq!(
            events.iter().filter(|e| e.kind == "user_message").count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "message_interrupted")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .find(|e| e.kind == "user_message")
                .unwrap()
                .data["delivery_mode"],
            "immediate"
        );
        let initial = crate::models::new_id();
        store.interrupt_message(&id, &initial).await.unwrap();
        store
            .accept_message(
                &id,
                &initial,
                items("initial"),
                data(&initial, "after_call"),
            )
            .await
            .unwrap();
        assert_eq!(
            store.interrupt_message(&id, &initial).await.unwrap()["delivered"],
            true
        );
    }
    #[tokio::test]
    async fn esc_promotion_event_failure_rolls_back_and_checks_ownership() {
        let (_dir, store, id) = fixture().await;
        let client = crate::models::new_id();
        assert!(enqueue(&store, &id, &client, "after_call").await);
        store.call(|db|{db.execute_batch("CREATE TRIGGER fail_interrupt BEFORE INSERT ON events WHEN NEW.kind='message_interrupted' BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;Ok(())}).await.unwrap();
        assert!(store.interrupt_message(&id, &client).await.is_err());
        assert!(!store.has_immediate_messages(&id).await.unwrap());
        let other = session(&store).await;
        assert_eq!(
            store.interrupt_message(&other, &client).await.unwrap()["pending_acceptance"],
            true
        );
        assert!(!store.has_immediate_messages(&id).await.unwrap());
        assert!(store.interrupt_message(&id, "not-a-uuid").await.is_err());
    }
    #[tokio::test]
    async fn early_escalation_markers_survive_restart_without_starting_tasks() {
        let (dir, store, id) = fixture().await;
        let client = crate::models::new_id();
        store.interrupt_message(&id, &client).await.unwrap();
        drop(store);
        let store = Store::open(&dir.path().join("test.sqlite")).unwrap();
        assert!(enqueue(&store, &id, &client, "after_call").await);
        assert!(store.has_immediate_messages(&id).await.unwrap());
        assert_eq!(store.session(&id).await.unwrap().status, "idle");
    }
    #[tokio::test]
    async fn initial_accept_then_queue_retry_is_one_acceptance() {
        let (_dir, store, id) = fixture().await;
        assert!(
            store
                .accept_message(&id, "a", items("original"), data("a", "after_call"))
                .await
                .unwrap()
        );
        assert!(!enqueue(&store, &id, "a", "immediate").await);
        assert_eq!(store.context(&id).await.unwrap(), items("original"));
        assert_eq!(store.queued_messages(&id).await.unwrap(), 0);
        assert_eq!(store.events(&id, 0).await.unwrap().len(), 1);
        assert_eq!(
            store
                .accepted_message_run(&id, "a")
                .await
                .unwrap()
                .as_deref(),
            Some("run-a")
        );
    }

    #[tokio::test]
    async fn concurrent_initial_and_queued_accept_share_one_winner() {
        let (_dir, store, id) = fixture().await;
        let (initial, queued) = tokio::join!(
            store.accept_message(&id, "race", items("initial"), data("race", "after_call")),
            store.queue_message(&id, "race", items("queued"), data("race", "immediate")),
        );
        let initial = initial.unwrap();
        let queued = queued.unwrap();
        assert_ne!(initial, queued);
        assert_eq!(store.events(&id, 0).await.unwrap().len(), 1);
        assert_eq!(
            store.queued_messages(&id).await.unwrap(),
            usize::from(queued)
        );
        assert_eq!(
            store.drain_messages(&id).await.unwrap(),
            usize::from(queued)
        );
        assert_eq!(
            store.context(&id).await.unwrap(),
            items(if initial { "initial" } else { "queued" })
        );
    }

    #[tokio::test]
    async fn queue_accept_collision_and_changed_retry_preserve_original() {
        let (_dir, store, id) = fixture().await;
        assert!(enqueue(&store, &id, "a", "after_call").await);
        assert!(
            !store
                .accept_message(&id, "a", items("collision"), data("a", "immediate"))
                .await
                .unwrap()
        );
        assert!(
            !store
                .queue_message(
                    &id,
                    "a",
                    items("changed"),
                    json!({"delivery_mode":"invalid","run_id":"wrong"})
                )
                .await
                .unwrap()
        );
        assert!(store.context(&id).await.unwrap().is_empty());
        assert_eq!(store.queued_messages(&id).await.unwrap(), 1);
        assert!(!store.has_immediate_messages(&id).await.unwrap());
        assert_eq!(
            store
                .accepted_message_run(&id, "a")
                .await
                .unwrap()
                .as_deref(),
            Some("run-a")
        );
        assert_eq!(store.drain_messages(&id).await.unwrap(), 1);
        assert_eq!(store.drain_messages(&id).await.unwrap(), 0);
        assert_eq!(store.context(&id).await.unwrap(), items("a"));
        let events = store.events(&id, 0).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "user_message");
        assert_eq!(events[0].data["queued"], true);
        assert_eq!(events[0].data["delivery_mode"], "after_call");
        assert_eq!(events[1].kind, "message_delivered");
        assert_eq!(events[1].data, json!({"client_message_id":"a"}));
    }

    #[tokio::test]
    async fn fifo_immediate_does_not_overtake_and_source_items_are_unchanged() {
        let (_dir, store, id) = fixture().await;
        let source = vec![
            json!({"type":"message","role":"user","content":[
                {"type":"input_text","text":"first"},
                {"type":"input_image","image_url":"data:image/png;base64,fixture"}
            ]}),
            json!({"role":"user","content":"second item"}),
        ];
        assert!(
            store
                .queue_message(&id, "a", source.clone(), data("a", "after_call"))
                .await
                .unwrap()
        );
        assert!(enqueue(&store, &id, "b", "immediate").await);
        assert!(enqueue(&store, &id, "c", "after_call").await);
        assert!(store.has_immediate_messages(&id).await.unwrap());
        assert_eq!(store.drain_messages(&id).await.unwrap(), 3);
        let mut expected = source;
        expected.extend(items("b"));
        expected.extend(items("c"));
        assert_eq!(store.context(&id).await.unwrap(), expected);
        let delivered: Vec<_> = store
            .events(&id, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "message_delivered")
            .map(|event| event.data["client_message_id"].clone())
            .collect();
        assert_eq!(delivered, vec![json!("a"), json!("b"), json!("c")]);
        assert!(!store.has_immediate_messages(&id).await.unwrap());
        assert_eq!(store.queued_messages(&id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn count_cap_rolls_back_uuid_and_is_per_session() {
        let (_dir, store, id) = fixture().await;
        for n in 0..MAX_QUEUED_MESSAGES {
            assert!(enqueue(&store, &id, &n.to_string(), "after_call").await);
        }
        assert!(!enqueue(&store, &id, "0", "after_call").await);
        assert!(
            store
                .queue_message(
                    &id,
                    "overflow",
                    items("retry"),
                    data("overflow", "immediate")
                )
                .await
                .is_err()
        );
        assert_eq!(
            store.accepted_message_run(&id, "overflow").await.unwrap(),
            None
        );
        assert_eq!(store.events(&id, 0).await.unwrap().len(), 64);
        assert!(!store.has_immediate_messages(&id).await.unwrap());
        let other = session(&store).await;
        assert!(enqueue(&store, &other, "overflow", "after_call").await);
        assert_eq!(store.drain_messages(&id).await.unwrap(), 64);
        assert!(enqueue(&store, &id, "overflow", "immediate").await);
    }

    #[tokio::test]
    async fn byte_cap_counts_utf8_and_both_persisted_payloads() {
        let (_dir, store, id) = fixture().await;
        // Each payload alone fits, but their persisted sum crosses the session cap.
        let large = "界".repeat(6 * 1024 * 1024);
        assert!(
            store
                .queue_message(&id, "large", items(&large), data("large", "after_call"))
                .await
                .unwrap()
        );
        assert!(
            store
                .queue_message(
                    &id,
                    "overflow",
                    vec![],
                    json!({"delivery_mode":"immediate", "text":large})
                )
                .await
                .is_err()
        );
        assert_eq!(store.queued_messages(&id).await.unwrap(), 1);
        assert_eq!(store.events(&id, 0).await.unwrap().len(), 1);
        assert_eq!(
            store.accepted_message_run(&id, "overflow").await.unwrap(),
            None
        );
        assert!(enqueue(&store, &id, "overflow", "after_call").await);
    }

    #[tokio::test]
    async fn single_oversize_and_invalid_mode_do_not_consume_uuid() {
        let (_dir, store, id) = fixture().await;
        assert!(
            store
                .queue_message(&id, "a", vec![], json!({"delivery_mode":"later"}))
                .await
                .is_err()
        );
        assert!(
            store
                .queue_message(
                    &id,
                    "a",
                    items(&"x".repeat(MAX_QUEUED_BYTES as usize)),
                    data("a", "after_call")
                )
                .await
                .is_err()
        );
        assert!(store.events(&id, 0).await.unwrap().is_empty());
        assert_eq!(store.queued_messages(&id).await.unwrap(), 0);
        assert!(enqueue(&store, &id, "a", "after_call").await);
    }

    #[tokio::test]
    async fn unknown_session_cannot_reserve_a_client_uuid() {
        let (_dir, store, id) = fixture().await;
        assert!(
            store
                .queue_message("missing", "a", items("a"), data("a", "after_call"))
                .await
                .is_err()
        );
        let reserved = store
            .call(|db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM accepted_messages WHERE session_id='missing'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(reserved, 0);
        assert!(store.drain_messages("missing").await.is_err());
        assert!(enqueue(&store, &id, "a", "after_call").await);
    }

    #[tokio::test]
    async fn drain_all_or_nothing_on_context_event_and_delete_failure() {
        let (_dir, store, id) = fixture().await;
        assert!(enqueue(&store, &id, "a", "after_call").await);
        assert!(enqueue(&store, &id, "b", "immediate").await);
        // Fail after earlier rows have been inserted, including a delivered event.
        for (name, trigger) in [
            (
                "fail_context",
                "CREATE TRIGGER fail_context BEFORE INSERT ON context_items WHEN NEW.item LIKE '%\"b\"%' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            ),
            (
                "fail_event",
                "CREATE TRIGGER fail_event BEFORE INSERT ON events WHEN NEW.kind='message_delivered' AND json_extract(NEW.data,'$.client_message_id')='b' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            ),
            (
                "fail_delete",
                "CREATE TRIGGER fail_delete BEFORE DELETE ON message_queue BEGIN SELECT RAISE(ABORT,'injected'); END;",
            ),
        ] {
            store
                .call(move |db| {
                    db.execute_batch(trigger)?;
                    Ok(())
                })
                .await
                .unwrap();
            assert!(store.drain_messages(&id).await.is_err(), "{name}");
            assert!(store.context(&id).await.unwrap().is_empty());
            assert_eq!(store.events(&id, 0).await.unwrap().len(), 2);
            assert_eq!(store.queued_messages(&id).await.unwrap(), 2);
            assert!(store.has_immediate_messages(&id).await.unwrap());
            store
                .call(move |db| {
                    db.execute_batch(&format!("DROP TRIGGER {name}"))?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        assert_eq!(store.drain_messages(&id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn acceptance_event_failure_rolls_back_queue_and_dedup() {
        let (_dir, store, id) = fixture().await;
        store.call(|db| {
            db.execute_batch("CREATE TRIGGER fail_accept BEFORE INSERT ON events WHEN NEW.kind='user_message' BEGIN SELECT RAISE(ABORT,'injected'); END;")?;
            Ok(())
        }).await.unwrap();
        assert!(
            store
                .queue_message(&id, "a", items("a"), data("a", "after_call"))
                .await
                .is_err()
        );
        assert_eq!(store.queued_messages(&id).await.unwrap(), 0);
        assert!(store.events(&id, 0).await.unwrap().is_empty());
        store
            .call(|db| {
                db.execute_batch("DROP TRIGGER fail_accept")?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(enqueue(&store, &id, "a", "after_call").await);
    }

    #[tokio::test]
    async fn drain_uses_current_epoch_not_acceptance_epoch() {
        let (_dir, store, id) = fixture().await;
        store.append_context(&id, items("old epoch")).await.unwrap();
        assert!(enqueue(&store, &id, "a", "after_call").await);
        let moved = id.clone();
        store
            .call(move |db| {
                db.execute("UPDATE sessions SET context_epoch=7 WHERE id=?1", [moved])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.drain_messages(&id).await.unwrap(), 1);
        assert_eq!(store.context(&id).await.unwrap(), items("a"));
        let moved = id.clone();
        let epochs = store
            .call(move |db| {
                let mut statement =
                    db.prepare("SELECT epoch FROM context_items WHERE session_id=?1 ORDER BY id")?;
                Ok(statement
                    .query_map([moved], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
            .unwrap();
        assert_eq!(epochs, vec![0, 7]);
    }

    #[tokio::test]
    async fn same_uuid_is_isolated_across_sessions_and_run_lookup_is_exact() {
        let (_dir, store, first) = fixture().await;
        let second = session(&store).await;
        let client = "same-'\"-uuid";
        assert!(enqueue(&store, &first, client, "immediate").await);
        let mut second_data = data(client, "after_call");
        second_data["run_id"] = json!("second-run");
        assert!(
            store
                .queue_message(&second, client, items("second"), second_data)
                .await
                .unwrap()
        );
        store
            .append_event(
                &first,
                "message_delivered",
                json!({"client_message_id":"missing","run_id":"not-a-user-event"}),
            )
            .await
            .unwrap();
        assert_eq!(
            store.accepted_message_run(&first, client).await.unwrap(),
            Some(format!("run-{client}"))
        );
        assert_eq!(
            store
                .accepted_message_run(&second, client)
                .await
                .unwrap()
                .as_deref(),
            Some("second-run")
        );
        assert_eq!(
            store.accepted_message_run(&first, "missing").await.unwrap(),
            None
        );
        assert_eq!(store.drain_messages(&first).await.unwrap(), 1);
        assert_eq!(store.queued_messages(&second).await.unwrap(), 1);
        assert!(!store.has_immediate_messages(&second).await.unwrap());
    }

    #[tokio::test]
    async fn restart_preserves_queue_and_mirrors_without_auto_execution() {
        let (dir, store, id) = fixture().await;
        store.set_status(&id, "running").await.unwrap();
        assert!(enqueue(&store, &id, "保留明文", "immediate").await);
        drop(store);
        let store = Store::open(&dir.path().join("test.sqlite")).unwrap();
        assert_eq!(store.queued_messages(&id).await.unwrap(), 1);
        assert!(store.has_immediate_messages(&id).await.unwrap());
        assert!(store.context(&id).await.unwrap().is_empty());
        assert_eq!(store.session(&id).await.unwrap().status, "interrupted");
        assert!(!enqueue(&store, &id, "保留明文", "immediate").await);
        let mirror_path = dir.path().join("sessions").join(format!("{id}.jsonl"));
        let before = std::fs::read_to_string(&mirror_path).unwrap();
        assert!(before.contains("保留明文"));
        assert!(!before.contains("message_delivered"));
        assert_eq!(store.drain_messages(&id).await.unwrap(), 1);
        let after = std::fs::read_to_string(mirror_path).unwrap();
        let events: Vec<Value> = after
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "user_message")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "message_delivered")
                .count(),
            1
        );
        assert_eq!(store.context(&id).await.unwrap(), items("保留明文"));
        // Draining is storage only: it never starts/resumes the interrupted run.
        assert_eq!(store.session(&id).await.unwrap().status, "interrupted");
    }
}
