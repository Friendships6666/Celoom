//! Durable questions, explicitly started goals, and boundary-delivered input.
//!
//! Every mutation runs on Store's existing SQLite worker. Inbox entries are the
//! only bridge from these APIs into model context: the runner must call
//! `drain_inbox` only after all outstanding tool calls have matching results.

use crate::models::{Goal, Question, new_id, now};
use crate::store::{Store, context_items, event, load_session};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::json;
use std::collections::HashSet;

const MAX_ID_CHARS: usize = 512;
const MAX_QUESTION_CHARS: usize = 16_000;
const MAX_OPTIONS: usize = 32;
const MAX_OPTION_CHARS: usize = 2_000;
const MAX_ANSWER_CHARS: usize = 64_000;
const MAX_GOAL_TITLE_CHARS: usize = 240;
const MAX_GOAL_OBJECTIVE_CHARS: usize = 32_000;
const MAX_PROGRESS_CHARS: usize = 64_000;
const MAX_PAUSE_REASON_CHARS: usize = 8_000;
const RESTART_REASON: &str = "Backend restarted; explicitly resume this goal to continue.";

const QUESTION_COLUMNS: &str =
    "id,session_id,call_id,question,blocking,options,status,answer,delivered,created_at,updated_at";
const GOAL_COLUMNS: &str =
    "id,session_id,title,objective,status,progress,rounds,created_at,updated_at";

/// Called once by Store::open, before ordinary interrupted-run recovery.
///
/// Schema creation and restart recovery commit together. A partial unique index
/// enforces the active-goal invariant even if a future caller bypasses this API.
/// No queued goal instruction may survive a backend restart and execute before
/// the user explicitly resumes its goal. Question answers do survive restarts.
pub(crate) fn migrate(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_questions(
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            call_id TEXT NOT NULL,
            question TEXT NOT NULL CHECK(length(trim(question))>0),
            blocking INTEGER NOT NULL CHECK(blocking IN(0,1)),
            options TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN('pending','answered','withdrawn')),
            answer TEXT,
            delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN(0,1)),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(session_id,call_id),
            UNIQUE(session_id,id),
            CHECK((status='answered' AND answer IS NOT NULL) OR
                  (status IN('pending','withdrawn') AND answer IS NULL)),
            CHECK(delivered=0 OR status='answered'),
            CHECK(status!='withdrawn' OR blocking=0)
        );
        CREATE INDEX IF NOT EXISTS agent_questions_session
            ON agent_questions(session_id,created_at DESC);
        CREATE TABLE IF NOT EXISTS agent_goals(
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            title TEXT NOT NULL CHECK(length(trim(title))>0),
            objective TEXT NOT NULL CHECK(length(trim(objective))>0),
            status TEXT NOT NULL CHECK(status IN('pending','active','paused','completed','cancelled')),
            progress TEXT NOT NULL DEFAULT '',
            rounds INTEGER NOT NULL DEFAULT 0 CHECK(typeof(rounds)='integer' AND rounds>=0),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(session_id,id)
        );
        CREATE INDEX IF NOT EXISTS agent_goals_session
            ON agent_goals(session_id,created_at DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS agent_one_active_goal
            ON agent_goals(session_id) WHERE status='active';
        CREATE TABLE IF NOT EXISTS agent_inbox(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            kind TEXT NOT NULL CHECK(kind IN('question_answer','goal_start','goal_resume')),
            question_id TEXT UNIQUE,
            goal_id TEXT UNIQUE,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(session_id,question_id) REFERENCES agent_questions(session_id,id),
            FOREIGN KEY(session_id,goal_id) REFERENCES agent_goals(session_id,id),
            CHECK((kind='question_answer' AND question_id IS NOT NULL AND goal_id IS NULL) OR
                  (kind IN('goal_start','goal_resume') AND question_id IS NULL AND goal_id IS NOT NULL))
        );
        CREATE INDEX IF NOT EXISTS agent_inbox_session ON agent_inbox(session_id,id);",
    )?;
    let active = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {GOAL_COLUMNS} FROM agent_goals WHERE status='active' ORDER BY session_id,id"
        ))?;
        stmt.query_map([], goal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for mut goal in active {
        goal.status = "paused".into();
        goal.progress = paused_progress(&goal.progress, RESTART_REASON);
        goal.updated_at = now();
        save_goal(&tx, &goal)?;
        goal_event(&tx, &goal)?;
    }
    tx.execute("DELETE FROM agent_inbox WHERE goal_id IS NOT NULL", [])?;
    tx.commit()?;
    Ok(())
}

impl Store {
    pub async fn questions(&self, session_id: &str) -> Result<Vec<Question>> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            load_session(db, &session_id)?;
            list_questions(db, &session_id)
        })
        .await
    }

    pub async fn create_question(
        &self,
        session_id: &str,
        call_id: &str,
        text: String,
        blocking: bool,
        options: Vec<String>,
    ) -> Result<Question> {
        validate_text(call_id, MAX_ID_CHARS, "Question call id")?;
        validate_text(&text, MAX_QUESTION_CHARS, "Question")?;
        ensure!(options.len() <= MAX_OPTIONS, "Too many question options");
        let mut seen = HashSet::new();
        for option in &options {
            validate_text(option, MAX_OPTION_CHARS, "Question option")?;
            ensure!(seen.insert(option), "Question options must be distinct");
        }
        let session_id = session_id.to_owned();
        let call_id = call_id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            if let Some(existing) = question_by_call(&tx, &session_id, &call_id)? {
                ensure!(
                    existing.question == text
                        && existing.blocking == blocking
                        && existing.options == options,
                    "Question call id already exists with different arguments"
                );
                return Ok(existing);
            }
            let timestamp = now();
            let question = Question {
                id: new_id(),
                session_id,
                call_id,
                question: text,
                blocking,
                options,
                status: "pending".into(),
                answer: None,
                delivered: false,
                created_at: timestamp.clone(),
                updated_at: timestamp,
            };
            tx.execute(
                "INSERT INTO agent_questions
                 (id,session_id,call_id,question,blocking,options,status,answer,delivered,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,'pending',NULL,0,?7,?7)",
                params![
                    question.id, question.session_id, question.call_id, question.question,
                    question.blocking, serde_json::to_string(&question.options)?, question.created_at
                ],
            )?;
            question_event(&tx, "question", &question)?;
            tx.commit()?;
            Ok(question)
        })
        .await
    }

    pub async fn answer_question(
        &self,
        session_id: &str,
        id: &str,
        answer: String,
    ) -> Result<Question> {
        validate_text(&answer, MAX_ANSWER_CHARS, "Question answer")?;
        let session_id = session_id.to_owned();
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let mut question = load_question(&tx, &session_id, &id)?;
            match question.status.as_str() {
                "answered" => {
                    ensure!(
                        question.answer.as_deref() == Some(answer.as_str()),
                        "Question was already answered differently"
                    );
                    return Ok(question);
                }
                "pending" => {}
                _ => bail!("Withdrawn questions cannot be answered"),
            }
            question.status = "answered".into();
            question.answer = Some(answer);
            question.updated_at = now();
            tx.execute(
                "UPDATE agent_questions SET status='answered',answer=?3,updated_at=?4
                 WHERE session_id=?1 AND id=?2 AND status='pending'",
                params![session_id, id, question.answer, question.updated_at],
            )?;
            let content = question_answer_text(&question)?;
            tx.execute(
                "INSERT INTO agent_inbox(session_id,kind,question_id,content,created_at)
                 VALUES(?1,'question_answer',?2,?3,?4)",
                params![session_id, id, content, question.updated_at],
            )?;
            question_event(&tx, "question_answer", &question)?;
            tx.commit()?;
            Ok(question)
        })
        .await
    }

    pub async fn withdraw_question(&self, session_id: &str, id: &str) -> Result<Question> {
        let session_id = session_id.to_owned();
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let mut question = load_question(&tx, &session_id, &id)?;
            if question.status == "withdrawn" {
                return Ok(question);
            }
            ensure!(
                question.status == "pending",
                "Answered questions cannot be withdrawn"
            );
            ensure!(
                !question.blocking,
                "Pending blocking questions cannot be withdrawn"
            );
            question.status = "withdrawn".into();
            question.updated_at = now();
            tx.execute(
                "UPDATE agent_questions SET status='withdrawn',updated_at=?3
                 WHERE session_id=?1 AND id=?2",
                params![session_id, id, question.updated_at],
            )?;
            question_event(&tx, "question_withdrawn", &question)?;
            tx.commit()?;
            Ok(question)
        })
        .await
    }

    pub async fn pending_blocking(&self, session_id: &str) -> Result<bool> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            load_session(db, &session_id)?;
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_questions
                 WHERE session_id=?1 AND status='pending' AND blocking=1)",
                [session_id],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Deliver all currently queued input in insertion order, exactly once.
    /// The runner, rather than an HTTP answer handler, owns this safe boundary.
    pub async fn drain_inbox(&self, session_id: &str) -> Result<usize> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let entries = {
                let mut stmt = tx.prepare(
                    "SELECT id,question_id,goal_id,content FROM agent_inbox
                     WHERE session_id=?1 ORDER BY id",
                )?;
                stmt.query_map([&session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut delivered = 0;
            for (inbox_id, question_id, goal_id, content) in entries {
                if let Some(goal_id) = goal_id {
                    // Defend against a stale instruction even if another code
                    // path changed its goal without using change_goal.
                    if load_goal(&tx, &session_id, &goal_id)?.status != "active" {
                        tx.execute("DELETE FROM agent_inbox WHERE id=?1", [inbox_id])?;
                        continue;
                    }
                }
                let mut question = question_id
                    .as_deref()
                    .map(|id| load_question(&tx, &session_id, id))
                    .transpose()?;
                if let Some(question) = question.as_ref() {
                    ensure!(
                        question.status == "answered",
                        "Inbox question is not answered"
                    );
                    if question.delivered {
                        tx.execute("DELETE FROM agent_inbox WHERE id=?1", [inbox_id])?;
                        continue;
                    }
                }
                let item = json!({"role":"user","content":content});
                context_items(&tx, &session_id, vec![item.clone()])?;
                event(&tx, &session_id, "context_note", json!({"items":[item]}))?;
                if let Some(question) = question.as_mut() {
                    question.delivered = true;
                    question.updated_at = now();
                    tx.execute(
                        "UPDATE agent_questions SET delivered=1,updated_at=?3
                         WHERE session_id=?1 AND id=?2",
                        params![session_id, question.id, question.updated_at],
                    )?;
                    question_event(&tx, "question_answer", question)?;
                }
                tx.execute("DELETE FROM agent_inbox WHERE id=?1", [inbox_id])?;
                delivered += 1;
            }
            tx.commit()?;
            Ok(delivered)
        })
        .await
    }

    pub async fn goals(&self, session_id: &str) -> Result<Vec<Goal>> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            load_session(db, &session_id)?;
            list_goals(db, &session_id)
        })
        .await
    }

    pub async fn create_goal(
        &self,
        session_id: &str,
        title: String,
        objective: String,
    ) -> Result<Goal> {
        validate_text(&title, MAX_GOAL_TITLE_CHARS, "Goal title")?;
        validate_text(&objective, MAX_GOAL_OBJECTIVE_CHARS, "Goal objective")?;
        let session_id = session_id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let timestamp = now();
            let goal = Goal {
                id: new_id(),
                session_id,
                title,
                objective,
                status: "pending".into(),
                progress: String::new(),
                rounds: 0,
                created_at: timestamp.clone(),
                updated_at: timestamp,
            };
            tx.execute(
                "INSERT INTO agent_goals
                 (id,session_id,title,objective,status,progress,rounds,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,'pending','',0,?5,?5)",
                params![
                    goal.id,
                    goal.session_id,
                    goal.title,
                    goal.objective,
                    goal.created_at
                ],
            )?;
            goal_event(&tx, &goal)?;
            tx.commit()?;
            Ok(goal)
        })
        .await
    }

    pub async fn change_goal(
        &self,
        session_id: &str,
        id: &str,
        action: &str,
        title: Option<String>,
        objective: Option<String>,
    ) -> Result<Goal> {
        ensure!(
            matches!(action, "start" | "pause" | "resume" | "cancel"),
            "Unknown goal action"
        );
        if let Some(title) = title.as_deref() {
            validate_text(title, MAX_GOAL_TITLE_CHARS, "Goal title")?;
        }
        if let Some(objective) = objective.as_deref() {
            validate_text(objective, MAX_GOAL_OBJECTIVE_CHARS, "Goal objective")?;
        }
        let session_id = session_id.to_owned();
        let id = id.to_owned();
        let action = action.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let mut goal = load_goal(&tx, &session_id, &id)?;
            let editing = title.as_ref().is_some_and(|v| v != &goal.title)
                || objective.as_ref().is_some_and(|v| v != &goal.objective);
            ensure!(
                !editing || goal.status != "active",
                "Active goals cannot be edited"
            );
            ensure!(
                !editing || !matches!(goal.status.as_str(), "completed" | "cancelled"),
                "Terminal goals cannot be edited"
            );
            let previous_status = goal.status.clone();
            let next_status = match (action.as_str(), goal.status.as_str()) {
                ("start", "pending") | ("resume", "paused") => "active",
                ("start" | "resume", "active") => "active",
                ("pause", "active" | "paused") => "paused",
                ("cancel", "pending" | "active" | "paused" | "cancelled") => "cancelled",
                _ => bail!("Goal action is not valid for its current status"),
            };
            if next_status == "active" {
                let other_active: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agent_goals
                     WHERE session_id=?1 AND status='active' AND id!=?2)",
                    params![session_id, id],
                    |row| row.get(0),
                )?;
                ensure!(
                    !other_active,
                    "Another goal is already active in this session"
                );
            }
            if let Some(title) = title {
                goal.title = title;
            }
            if let Some(objective) = objective {
                goal.objective = objective;
            }
            if previous_status == next_status && !editing {
                return Ok(goal);
            }
            goal.status = next_status.into();
            goal.updated_at = now();
            save_goal(&tx, &goal)?;
            if goal.status == "active" && previous_status != "active" {
                tx.execute(
                    "INSERT INTO agent_inbox(session_id,kind,goal_id,content,created_at)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![
                        session_id,
                        format!("goal_{action}"),
                        id,
                        goal_instruction_text(&goal, &action)?,
                        goal.updated_at
                    ],
                )?;
            } else if goal.status != "active" {
                tx.execute(
                    "DELETE FROM agent_inbox WHERE session_id=?1 AND goal_id=?2",
                    params![session_id, id],
                )?;
            }
            goal_event(&tx, &goal)?;
            tx.commit()?;
            Ok(goal)
        })
        .await
    }

    pub async fn active_goal(&self, session_id: &str) -> Result<Option<Goal>> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            load_session(db, &session_id)?;
            Ok(db.query_row(
                &format!("SELECT {GOAL_COLUMNS} FROM agent_goals WHERE session_id=?1 AND status='active'"),
                [session_id], goal_row,
            ).optional()?)
        })
        .await
    }

    pub async fn update_goal_progress(
        &self,
        session_id: &str,
        id: &str,
        progress: String,
        complete: bool,
    ) -> Result<Goal> {
        validate_text(&progress, MAX_PROGRESS_CHARS, "Goal progress")?;
        let session_id = session_id.to_owned();
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let mut goal = load_goal(&tx, &session_id, &id)?;
            // Retrying a committed completion is safe; it cannot overwrite it.
            if complete && goal.status == "completed" && goal.progress == progress {
                return Ok(goal);
            }
            ensure!(
                goal.status == "active",
                "Only active goals can report progress or complete"
            );
            if !complete && goal.progress == progress {
                return Ok(goal);
            }
            goal.progress = progress;
            if complete {
                goal.status = "completed".into();
            }
            goal.updated_at = now();
            save_goal(&tx, &goal)?;
            if complete {
                tx.execute(
                    "DELETE FROM agent_inbox WHERE session_id=?1 AND goal_id=?2",
                    params![session_id, id],
                )?;
            }
            goal_event(&tx, &goal)?;
            tx.commit()?;
            Ok(goal)
        })
        .await
    }

    pub async fn advance_goal_round(&self, session_id: &str, id: &str) -> Result<Goal> {
        let session_id = session_id.to_owned();
        let id = id.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let mut goal = load_goal(&tx, &session_id, &id)?;
            ensure!(
                goal.status == "active",
                "Only active goals can advance rounds"
            );
            ensure!(
                goal.rounds < i64::MAX as u64,
                "Goal round counter exhausted"
            );
            goal.rounds += 1;
            goal.updated_at = now();
            save_goal(&tx, &goal)?;
            goal_event(&tx, &goal)?;
            tx.commit()?;
            Ok(goal)
        })
        .await
    }

    pub async fn pause_active_goals(&self, session_id: &str, reason: &str) -> Result<()> {
        validate_text(reason, MAX_PAUSE_REASON_CHARS, "Goal pause reason")?;
        let session_id = session_id.to_owned();
        let reason = reason.to_owned();
        self.call(move |db| {
            let tx = db.transaction()?;
            load_session(&tx, &session_id)?;
            let active = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {GOAL_COLUMNS} FROM agent_goals WHERE session_id=?1 AND status='active'"
                ))?;
                stmt.query_map([&session_id], goal_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for mut goal in active {
                goal.status = "paused".into();
                goal.progress = paused_progress(&goal.progress, &reason);
                goal.updated_at = now();
                save_goal(&tx, &goal)?;
                tx.execute(
                    "DELETE FROM agent_inbox WHERE session_id=?1 AND goal_id=?2",
                    params![session_id, goal.id],
                )?;
                goal_event(&tx, &goal)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Local deterministic text for a compaction prompt, never a model call.
    /// JSON string values keep embedded newlines and labels unambiguous.
    pub async fn autonomy_summary(&self, session_id: &str) -> Result<String> {
        let session_id = session_id.to_owned();
        self.call(move |db| {
            // One worker closure gives a coherent view of questions and goals.
            load_session(db, &session_id)?;
            let questions = list_questions(db, &session_id)?;
            let goals = list_goals(db, &session_id)?;
            let mut lines = Vec::new();
            for question in questions.into_iter().rev() {
                if question.status == "pending" {
                    lines.push(format!("Pending question: {}", serde_json::to_string(&json!({
                        "id":question.id, "blocking":question.blocking,
                        "question":question.question, "options":question.options
                    }))?));
                } else if question.status == "answered" && !question.delivered {
                    lines.push(format!("Answered question awaiting safe-boundary delivery: {}",
                        serde_json::to_string(&json!({"id":question.id,
                            "question":question.question,"answer":question.answer}))?));
                }
            }
            for goal in goals.into_iter().rev() {
                if matches!(goal.status.as_str(), "pending" | "active" | "paused") {
                    lines.push(format!("Persistent goal: {}", serde_json::to_string(&json!({
                        "id":goal.id,"status":goal.status,"title":goal.title,
                        "objective":goal.objective,"progress":goal.progress,"rounds":goal.rounds
                    }))?));
                }
            }
            if lines.is_empty() { return Ok(String::new()); }
            Ok(format!("Durable autonomy state (pending/paused goals require explicit user start/resume):\n{}", lines.join("\n")))
        })
        .await
    }
}

fn validate_text(value: &str, max_chars: usize, label: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label} cannot be empty");
    ensure!(value.chars().count() <= max_chars, "{label} is too long");
    ensure!(
        !value.contains('\0'),
        "{label} cannot contain NUL characters"
    );
    Ok(())
}

fn question_row(row: &Row<'_>) -> rusqlite::Result<Question> {
    let options: String = row.get(5)?;
    let options = serde_json::from_str(&options).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Question {
        id: row.get(0)?,
        session_id: row.get(1)?,
        call_id: row.get(2)?,
        question: row.get(3)?,
        blocking: row.get(4)?,
        options,
        status: row.get(6)?,
        answer: row.get(7)?,
        delivered: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn goal_row(row: &Row<'_>) -> rusqlite::Result<Goal> {
    Ok(Goal {
        id: row.get(0)?,
        session_id: row.get(1)?,
        title: row.get(2)?,
        objective: row.get(3)?,
        status: row.get(4)?,
        progress: row.get(5)?,
        rounds: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn load_question(db: &Connection, session_id: &str, id: &str) -> Result<Question> {
    db.query_row(
        &format!("SELECT {QUESTION_COLUMNS} FROM agent_questions WHERE session_id=?1 AND id=?2"),
        params![session_id, id],
        question_row,
    )
    .optional()?
    .context("Question not found in this session")
}

fn question_by_call(db: &Connection, session_id: &str, call_id: &str) -> Result<Option<Question>> {
    Ok(db
        .query_row(
            &format!(
                "SELECT {QUESTION_COLUMNS} FROM agent_questions WHERE session_id=?1 AND call_id=?2"
            ),
            params![session_id, call_id],
            question_row,
        )
        .optional()?)
}

fn list_questions(db: &Connection, session_id: &str) -> Result<Vec<Question>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {QUESTION_COLUMNS} FROM agent_questions WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC"
    ))?;
    Ok(stmt
        .query_map([session_id], question_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_goal(db: &Connection, session_id: &str, id: &str) -> Result<Goal> {
    db.query_row(
        &format!("SELECT {GOAL_COLUMNS} FROM agent_goals WHERE session_id=?1 AND id=?2"),
        params![session_id, id],
        goal_row,
    )
    .optional()?
    .context("Goal not found in this session")
}

fn list_goals(db: &Connection, session_id: &str) -> Result<Vec<Goal>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {GOAL_COLUMNS} FROM agent_goals WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC"
    ))?;
    Ok(stmt
        .query_map([session_id], goal_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn save_goal(tx: &Transaction<'_>, goal: &Goal) -> Result<()> {
    ensure!(tx.execute(
        "UPDATE agent_goals SET title=?3,objective=?4,status=?5,progress=?6,rounds=?7,updated_at=?8
         WHERE session_id=?1 AND id=?2",
        params![goal.session_id, goal.id, goal.title, goal.objective,
            goal.status, goal.progress, goal.rounds, goal.updated_at],
    )? == 1, "Goal not found in this session");
    Ok(())
}

fn question_event(tx: &Transaction<'_>, kind: &str, question: &Question) -> Result<()> {
    event(tx, &question.session_id, kind, json!({"question":question}))?;
    Ok(())
}

fn goal_event(tx: &Transaction<'_>, goal: &Goal) -> Result<()> {
    event(tx, &goal.session_id, "goal", json!({"goal":goal}))?;
    Ok(())
}

fn question_answer_text(question: &Question) -> Result<String> {
    Ok(format!(
        "User answer to a previously asked question (retain the original task):\n{}",
        serde_json::to_string(&json!({
            "question_id":question.id,"question":question.question,"answer":question.answer
        }))?
    ))
}

fn goal_instruction_text(goal: &Goal, action: &str) -> Result<String> {
    Ok(format!(
        "The user explicitly requested a persistent goal {action}. Preserve any unrelated current task. \
         At the next safe boundary, work toward this objective, report concrete progress, and continue \
         until verified complete or explicitly paused/cancelled. Do not claim completion merely because \
         a turn ended.\n{}",
        serde_json::to_string(&json!({
            "goal_id":goal.id,"title":goal.title,"objective":goal.objective,
            "progress":goal.progress,"rounds":goal.rounds
        }))?
    ))
}

fn paused_progress(progress: &str, reason: &str) -> String {
    if progress.is_empty() {
        format!("Paused: {reason}")
    } else {
        format!("{progress}\n\nPaused: {reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Selection;

    async fn fixture() -> (tempfile::TempDir, Store, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("autonomy.db")).unwrap();
        let id = session(&store).await;
        (dir, store, id)
    }

    async fn session(store: &Store) -> String {
        store
            .create_session(
                "Local test".into(),
                "/tmp".into(),
                Selection {
                    site_id: "no-network-site".into(),
                    key_id: "no-secret".into(),
                    model_id: "no-model".into(),
                },
                None,
                0,
            )
            .await
            .unwrap()
            .id
    }

    async fn ask(store: &Store, sid: &str, call: &str, blocking: bool) -> Question {
        store
            .create_question(
                sid,
                call,
                format!("Question for {call}?"),
                blocking,
                vec!["Yes".into(), "No".into()],
            )
            .await
            .unwrap()
    }

    async fn goal(store: &Store, sid: &str, title: &str) -> Goal {
        store
            .create_goal(sid, title.into(), format!("Verify {title} locally"))
            .await
            .unwrap()
    }

    async fn action(store: &Store, sid: &str, id: &str, action: &str) -> Result<Goal> {
        store.change_goal(sid, id, action, None, None).await
    }

    async fn inbox_count(store: &Store) -> i64 {
        store
            .call(|db| Ok(db.query_row("SELECT count(*) FROM agent_inbox", [], |r| r.get(0))?))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn autonomy_store_question_creation_is_idempotent_and_collision_checked() {
        let (_dir, store, sid) = fixture().await;
        let first = ask(&store, &sid, "call-a", false).await;
        let same = ask(&store, &sid, "call-a", false).await;
        assert_eq!(first.id, same.id);
        assert_eq!(first.updated_at, same.updated_at);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), 1);
        assert!(
            store
                .create_question(
                    &sid,
                    "call-a",
                    "Different".into(),
                    false,
                    vec!["Yes".into(), "No".into()]
                )
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(
                    &sid,
                    "call-a",
                    first.question.clone(),
                    true,
                    first.options.clone()
                )
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(&sid, "call-a", first.question, false, vec![])
                .await
                .is_err()
        );
        assert_eq!(store.questions(&sid).await.unwrap().len(), 1);
        assert!(store.context(&sid).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn autonomy_store_answer_retry_is_exact_and_context_is_deferred() {
        let (_dir, store, sid) = fixture().await;
        let q = ask(&store, &sid, "answer", true).await;
        let answered = store
            .answer_question(&sid, &q.id, "Yes, continue".into())
            .await
            .unwrap();
        assert_eq!(answered.status, "answered");
        assert!(!answered.delivered);
        assert!(!store.pending_blocking(&sid).await.unwrap());
        assert!(store.context(&sid).await.unwrap().is_empty());
        let events = store.events(&sid, 0).await.unwrap();
        let retry = store
            .answer_question(&sid, &q.id, "Yes, continue".into())
            .await
            .unwrap();
        assert_eq!(answered.updated_at, retry.updated_at);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), events.len());
        assert_eq!(inbox_count(&store).await, 1);
        assert!(
            store
                .answer_question(&sid, &q.id, "No".into())
                .await
                .is_err()
        );
        assert!(store.withdraw_question(&sid, &q.id).await.is_err());
    }

    #[tokio::test]
    async fn autonomy_store_cross_session_ownership_is_enforced() {
        let (_dir, store, sid) = fixture().await;
        let other = session(&store).await;
        let q = ask(&store, &sid, "scoped-call", false).await;
        let g = goal(&store, &sid, "Scoped goal").await;
        assert!(
            store
                .answer_question(&other, &q.id, "private".into())
                .await
                .is_err()
        );
        assert!(store.withdraw_question(&other, &q.id).await.is_err());
        assert!(action(&store, &other, &g.id, "start").await.is_err());
        assert!(
            store
                .update_goal_progress(&other, &g.id, "private".into(), false)
                .await
                .is_err()
        );
        assert!(store.advance_goal_round(&other, &g.id).await.is_err());
        assert!(store.questions(&other).await.unwrap().is_empty());
        assert!(store.goals(&other).await.unwrap().is_empty());
        let other_q = ask(&store, &other, "scoped-call", false).await;
        assert_ne!(q.id, other_q.id);
        assert_eq!(store.questions(&sid).await.unwrap()[0].status, "pending");
    }

    #[tokio::test]
    async fn autonomy_store_only_pending_optional_questions_can_be_withdrawn() {
        let (_dir, store, sid) = fixture().await;
        let blocking = ask(&store, &sid, "blocking", true).await;
        let optional = ask(&store, &sid, "optional", false).await;
        assert!(store.pending_blocking(&sid).await.unwrap());
        assert!(store.withdraw_question(&sid, &blocking.id).await.is_err());
        let withdrawn = store.withdraw_question(&sid, &optional.id).await.unwrap();
        let n = store.events(&sid, 0).await.unwrap().len();
        let again = store.withdraw_question(&sid, &optional.id).await.unwrap();
        assert_eq!(withdrawn.updated_at, again.updated_at);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), n);
        assert!(
            store
                .answer_question(&sid, &optional.id, "too late".into())
                .await
                .is_err()
        );
        assert_eq!(inbox_count(&store).await, 0);
        assert!(store.pending_blocking(&sid).await.unwrap());
    }

    #[tokio::test]
    async fn autonomy_store_inbox_delivers_once_in_order_with_full_snapshots() {
        let (_dir, store, sid) = fixture().await;
        let first = ask(&store, &sid, "first", false).await;
        let second = ask(&store, &sid, "second", true).await;
        store
            .answer_question(&sid, &second.id, "second first".into())
            .await
            .unwrap();
        store
            .answer_question(&sid, &first.id, "first second".into())
            .await
            .unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 2);
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
        let items = store.context(&sid).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["role"], "user");
        assert!(items[0]["content"].as_str().unwrap().contains(&second.id));
        assert!(
            items[1]["content"]
                .as_str()
                .unwrap()
                .contains(&first.question)
        );
        let questions = store.questions(&sid).await.unwrap();
        assert!(
            questions
                .iter()
                .all(|q| q.delivered && q.status == "answered")
        );
        let events = store.events(&sid, 0).await.unwrap();
        assert_eq!(
            events.iter().filter(|e| e.kind == "context_note").count(),
            2
        );
        let snapshot = &events
            .iter()
            .rfind(|e| e.kind == "question_answer")
            .unwrap()
            .data["question"];
        assert_eq!(snapshot["delivered"], true);
        assert!(snapshot.get("options").is_some());
        assert!(snapshot.get("created_at").is_some());
        store
            .answer_question(&sid, &first.id, "first second".into())
            .await
            .unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn autonomy_store_drain_rolls_back_context_events_delivery_and_deletion_together() {
        let (_dir, store, sid) = fixture().await;
        let q = ask(&store, &sid, "rollback", false).await;
        store
            .answer_question(&sid, &q.id, "durable".into())
            .await
            .unwrap();
        store
            .call(|db| {
                db.execute_batch(
                    "CREATE TRIGGER autonomy_fail_context BEFORE INSERT ON events
                WHEN NEW.kind='context_note' BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(store.drain_inbox(&sid).await.is_err());
        assert_eq!(inbox_count(&store).await, 1);
        assert!(store.context(&sid).await.unwrap().is_empty());
        assert!(!store.questions(&sid).await.unwrap()[0].delivered);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), 2);
        store
            .call(|db| {
                db.execute_batch("DROP TRIGGER autonomy_fail_context;")?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 1);
        assert_eq!(store.context(&sid).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn autonomy_store_answer_and_event_failure_leave_question_pending() {
        let (_dir, store, sid) = fixture().await;
        let q = ask(&store, &sid, "atomic-answer", true).await;
        store
            .call(|db| {
                db.execute_batch(
                    "CREATE TRIGGER autonomy_fail_answer BEFORE INSERT ON events
                WHEN NEW.kind='question_answer' BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            store
                .answer_question(&sid, &q.id, "not committed".into())
                .await
                .is_err()
        );
        assert_eq!(inbox_count(&store).await, 0);
        assert!(store.pending_blocking(&sid).await.unwrap());
        let q = store.questions(&sid).await.unwrap().remove(0);
        assert_eq!(q.status, "pending");
        assert!(q.answer.is_none());
    }

    #[tokio::test]
    async fn autonomy_store_goal_creation_is_pending_without_model_input() {
        let (_dir, store, sid) = fixture().await;
        let g = goal(&store, &sid, "Pending").await;
        assert_eq!(g.status, "pending");
        assert_eq!(g.rounds, 0);
        assert!(g.progress.is_empty());
        assert!(store.active_goal(&sid).await.unwrap().is_none());
        assert_eq!(inbox_count(&store).await, 0);
        assert!(store.context(&sid).await.unwrap().is_empty());
        let events = store.events(&sid, 0).await.unwrap();
        assert_eq!(events[0].kind, "goal");
        assert_eq!(events[0].data["goal"]["objective"], g.objective);
        assert!(
            store
                .update_goal_progress(&sid, &g.id, "not started".into(), true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn autonomy_store_one_active_goal_per_session_and_start_is_idempotent() {
        let (_dir, store, sid) = fixture().await;
        let other = session(&store).await;
        let a = goal(&store, &sid, "A").await;
        let b = goal(&store, &sid, "B").await;
        let c = goal(&store, &other, "C").await;
        let active = action(&store, &sid, &a.id, "start").await.unwrap();
        let n = store.events(&sid, 0).await.unwrap().len();
        let again = action(&store, &sid, &a.id, "start").await.unwrap();
        assert_eq!(active.updated_at, again.updated_at);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), n);
        assert!(action(&store, &sid, &b.id, "start").await.is_err());
        action(&store, &other, &c.id, "start").await.unwrap();
        assert_eq!(inbox_count(&store).await, 2);
        assert_eq!(store.active_goal(&sid).await.unwrap().unwrap().id, a.id);
        assert!(store.context(&sid).await.unwrap().is_empty());
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 1);
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
        assert!(
            store.context(&sid).await.unwrap()[0]["content"]
                .as_str()
                .unwrap()
                .contains(&a.objective)
        );
    }

    #[tokio::test]
    async fn autonomy_store_goal_transitions_are_strict_and_remove_stale_instructions() {
        let (_dir, store, sid) = fixture().await;
        let g = goal(&store, &sid, "Transitions").await;
        assert!(action(&store, &sid, &g.id, "resume").await.is_err());
        assert!(action(&store, &sid, &g.id, "pause").await.is_err());
        assert!(action(&store, &sid, &g.id, "complete").await.is_err());
        action(&store, &sid, &g.id, "start").await.unwrap();
        assert!(
            store
                .change_goal(&sid, &g.id, "pause", Some("Changed".into()), None)
                .await
                .is_err()
        );
        action(&store, &sid, &g.id, "pause").await.unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
        assert!(action(&store, &sid, &g.id, "start").await.is_err());
        let resumed = store
            .change_goal(
                &sid,
                &g.id,
                "resume",
                Some("Edited while paused".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(resumed.title, "Edited while paused");
        assert_eq!(inbox_count(&store).await, 1);
        action(&store, &sid, &g.id, "cancel").await.unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
        let n = store.events(&sid, 0).await.unwrap().len();
        action(&store, &sid, &g.id, "cancel").await.unwrap();
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), n);
        assert!(action(&store, &sid, &g.id, "resume").await.is_err());
    }

    #[tokio::test]
    async fn autonomy_store_rounds_and_completion_are_durable_and_not_context_events() {
        let (_dir, store, sid) = fixture().await;
        let g = goal(&store, &sid, "Complete").await;
        assert!(store.advance_goal_round(&sid, &g.id).await.is_err());
        action(&store, &sid, &g.id, "start").await.unwrap();
        assert_eq!(
            store.advance_goal_round(&sid, &g.id).await.unwrap().rounds,
            1
        );
        assert_eq!(
            store.advance_goal_round(&sid, &g.id).await.unwrap().rounds,
            2
        );
        let progress = store
            .update_goal_progress(&sid, &g.id, "Local tests passed".into(), false)
            .await
            .unwrap();
        assert_eq!(progress.status, "active");
        assert!(
            store
                .update_goal_progress(&sid, &g.id, "  ".into(), true)
                .await
                .is_err()
        );
        let done = store
            .update_goal_progress(
                &sid,
                &g.id,
                "Verified all requirements with local tests".into(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(done.status, "completed");
        assert_eq!(done.rounds, 2);
        assert!(store.active_goal(&sid).await.unwrap().is_none());
        assert_eq!(inbox_count(&store).await, 0);
        assert!(store.context(&sid).await.unwrap().is_empty());
        let n = store.events(&sid, 0).await.unwrap().len();
        store
            .update_goal_progress(&sid, &g.id, done.progress.clone(), true)
            .await
            .unwrap();
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), n);
        assert!(
            store
                .update_goal_progress(&sid, &g.id, "overwrite".into(), true)
                .await
                .is_err()
        );
        assert!(store.advance_goal_round(&sid, &g.id).await.is_err());
        assert!(action(&store, &sid, &g.id, "cancel").await.is_err());
    }

    #[tokio::test]
    async fn autonomy_store_pause_preserves_progress_and_reason_once() {
        let (_dir, store, sid) = fixture().await;
        let g = goal(&store, &sid, "Pause").await;
        action(&store, &sid, &g.id, "start").await.unwrap();
        store
            .update_goal_progress(&sid, &g.id, "First file verified".into(), false)
            .await
            .unwrap();
        store
            .pause_active_goals(&sid, "User stopped this session")
            .await
            .unwrap();
        let first = store.goals(&sid).await.unwrap().remove(0);
        assert_eq!(first.status, "paused");
        assert!(first.progress.contains("First file verified"));
        assert!(first.progress.contains("User stopped this session"));
        let n = store.events(&sid, 0).await.unwrap().len();
        store
            .pause_active_goals(&sid, "User stopped this session")
            .await
            .unwrap();
        let second = store.goals(&sid).await.unwrap().remove(0);
        assert_eq!(first.progress, second.progress);
        assert_eq!(first.updated_at, second.updated_at);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), n);
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn autonomy_store_restart_pauses_goals_preserves_questions_and_answers() {
        let (dir, store, sid) = fixture().await;
        let blocking = ask(&store, &sid, "pending-restart", true).await;
        let answered = ask(&store, &sid, "answered-restart", false).await;
        store
            .answer_question(&sid, &answered.id, "survives restart".into())
            .await
            .unwrap();
        let g = goal(&store, &sid, "Restart").await;
        action(&store, &sid, &g.id, "start").await.unwrap();
        store
            .update_goal_progress(&sid, &g.id, "Existing progress".into(), false)
            .await
            .unwrap();
        store.advance_goal_round(&sid, &g.id).await.unwrap();
        drop(store);
        let reopened = Store::open(&dir.path().join("autonomy.db")).unwrap();
        let recovered = reopened.goals(&sid).await.unwrap().remove(0);
        assert_eq!(recovered.status, "paused");
        assert_eq!(recovered.rounds, 1);
        assert!(recovered.progress.contains("Existing progress"));
        assert!(recovered.progress.contains(RESTART_REASON));
        assert!(reopened.active_goal(&sid).await.unwrap().is_none());
        assert!(reopened.pending_blocking(&sid).await.unwrap());
        assert!(
            reopened
                .questions(&sid)
                .await
                .unwrap()
                .iter()
                .any(|q| q.id == blocking.id && q.status == "pending")
        );
        assert_eq!(inbox_count(&reopened).await, 1);
        assert_eq!(reopened.drain_inbox(&sid).await.unwrap(), 1);
        assert!(
            reopened.context(&sid).await.unwrap()[0]["content"]
                .as_str()
                .unwrap()
                .contains("survives restart")
        );
        let events = reopened.events(&sid, 0).await.unwrap();
        assert_eq!(
            events.iter().rfind(|e| e.kind == "goal").unwrap().data["goal"]["status"],
            "paused"
        );
        let n = events.len();
        reopened.call(migrate).await.unwrap();
        assert_eq!(reopened.events(&sid, 0).await.unwrap().len(), n);
    }

    #[tokio::test]
    async fn autonomy_store_validation_rejects_blank_oversized_and_nul_input() {
        let (_dir, store, sid) = fixture().await;
        assert!(
            store
                .create_question(&sid, "", "Question".into(), false, vec![])
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(&sid, "a", " \n".into(), false, vec![])
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(&sid, "b", "Question".into(), false, vec!["".into()])
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(
                    &sid,
                    "c",
                    "Question".into(),
                    false,
                    vec!["same".into(), "same".into()]
                )
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(
                    &sid,
                    "d",
                    "Question".into(),
                    false,
                    vec!["a".into(); MAX_OPTIONS + 1]
                )
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(&sid, "e", "x".repeat(MAX_QUESTION_CHARS + 1), false, vec![])
                .await
                .is_err()
        );
        assert!(
            store
                .create_goal(&sid, " ".into(), "Objective".into())
                .await
                .is_err()
        );
        assert!(
            store
                .create_goal(
                    &sid,
                    "x".repeat(MAX_GOAL_TITLE_CHARS + 1),
                    "Objective".into()
                )
                .await
                .is_err()
        );
        assert!(
            store
                .create_goal(&sid, "Title".into(), "\0".into())
                .await
                .is_err()
        );
        let q = ask(&store, &sid, "valid", false).await;
        assert!(
            store
                .answer_question(&sid, &q.id, "\n".into())
                .await
                .is_err()
        );
        assert!(
            store
                .answer_question(&sid, &q.id, "x".repeat(MAX_ANSWER_CHARS + 1))
                .await
                .is_err()
        );
        assert_eq!(store.questions(&sid).await.unwrap().len(), 1);
        assert!(store.goals(&sid).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn autonomy_store_all_operations_reject_missing_sessions() {
        let (_dir, store, _sid) = fixture().await;
        let sid = "absent-session";
        assert!(store.questions(sid).await.is_err());
        assert!(store.goals(sid).await.is_err());
        assert!(store.active_goal(sid).await.is_err());
        assert!(store.pending_blocking(sid).await.is_err());
        assert!(store.drain_inbox(sid).await.is_err());
        assert!(store.autonomy_summary(sid).await.is_err());
        assert!(store.pause_active_goals(sid, "stopped").await.is_err());
        assert!(
            store
                .create_goal(sid, "Title".into(), "Objective".into())
                .await
                .is_err()
        );
        assert!(
            store
                .create_question(sid, "call", "Question".into(), false, vec![])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn autonomy_store_summary_is_deterministic_and_retains_open_state() {
        let (_dir, store, sid) = fixture().await;
        assert!(store.autonomy_summary(&sid).await.unwrap().is_empty());
        let pending = ask(&store, &sid, "summary-pending", true).await;
        let withdrawn = ask(&store, &sid, "summary-withdrawn", false).await;
        store.withdraw_question(&sid, &withdrawn.id).await.unwrap();
        let answered = ask(&store, &sid, "summary-answer", false).await;
        store
            .answer_question(&sid, &answered.id, "answer for later".into())
            .await
            .unwrap();
        let g = goal(&store, &sid, "Open objective").await;
        let cancelled = goal(&store, &sid, "Cancelled objective").await;
        action(&store, &sid, &cancelled.id, "cancel").await.unwrap();
        let summary = store.autonomy_summary(&sid).await.unwrap();
        assert_eq!(summary, store.autonomy_summary(&sid).await.unwrap());
        assert!(summary.contains(&pending.id));
        assert!(summary.contains(&g.objective));
        assert!(summary.contains("answer for later"));
        assert!(!summary.contains(&withdrawn.id));
        assert!(!summary.contains(&cancelled.id));
        assert!(store.context(&sid).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn autonomy_store_database_enforces_unique_active_goal_and_round_limit() {
        let (_dir, store, sid) = fixture().await;
        let a = goal(&store, &sid, "DB A").await;
        let b = goal(&store, &sid, "DB B").await;
        action(&store, &sid, &a.id, "start").await.unwrap();
        let bid = b.id.clone();
        assert!(
            store
                .call(move |db| {
                    db.execute("UPDATE agent_goals SET status='active' WHERE id=?1", [bid])?;
                    Ok(())
                })
                .await
                .is_err()
        );
        let aid = a.id.clone();
        store
            .call(move |db| {
                db.execute(
                    "UPDATE agent_goals SET rounds=?2 WHERE id=?1",
                    params![aid, i64::MAX],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(store.advance_goal_round(&sid, &a.id).await.is_err());
        assert_eq!(
            store.active_goal(&sid).await.unwrap().unwrap().rounds,
            i64::MAX as u64
        );
    }

    #[tokio::test]
    async fn autonomy_store_concurrent_answers_have_one_winner_and_one_delivery() {
        let (_dir, store, sid) = fixture().await;
        let q = ask(&store, &sid, "concurrent", false).await;
        let (a, b) = tokio::join!(
            store.answer_question(&sid, &q.id, "A".into()),
            store.answer_question(&sid, &q.id, "B".into())
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(inbox_count(&store).await, 1);
        let (a, b) = tokio::join!(store.drain_inbox(&sid), store.drain_inbox(&sid));
        assert_eq!(a.unwrap() + b.unwrap(), 1);
        assert_eq!(store.context(&sid).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn autonomy_store_batch_drain_failure_rolls_back_previously_delivered_entries() {
        let (_dir, store, sid) = fixture().await;
        let a = ask(&store, &sid, "batch-a", false).await;
        let b = ask(&store, &sid, "batch-b", false).await;
        store
            .answer_question(&sid, &a.id, "A".into())
            .await
            .unwrap();
        store
            .answer_question(&sid, &b.id, "B".into())
            .await
            .unwrap();
        store
            .call(|db| {
                db.execute_batch(
                    "CREATE TRIGGER autonomy_fail_second_note BEFORE INSERT ON events
                WHEN NEW.kind='context_note' AND
                     (SELECT count(*) FROM events WHERE kind='context_note')>0
                BEGIN SELECT RAISE(ABORT,'second entry failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(store.drain_inbox(&sid).await.is_err());
        assert_eq!(inbox_count(&store).await, 2);
        assert!(store.context(&sid).await.unwrap().is_empty());
        assert!(
            store
                .questions(&sid)
                .await
                .unwrap()
                .iter()
                .all(|q| !q.delivered)
        );
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), 4);
        store
            .call(|db| {
                db.execute_batch("DROP TRIGGER autonomy_fail_second_note;")?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 2);
        assert_eq!(store.context(&sid).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn autonomy_store_goal_event_failure_rolls_back_activation_and_instruction() {
        let (_dir, store, sid) = fixture().await;
        let g = goal(&store, &sid, "Atomic activation").await;
        store
            .call(|db| {
                db.execute_batch(
                    "CREATE TRIGGER autonomy_fail_goal BEFORE INSERT ON events
                WHEN NEW.kind='goal' BEGIN SELECT RAISE(ABORT,'goal event failure'); END;",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(action(&store, &sid, &g.id, "start").await.is_err());
        assert!(store.active_goal(&sid).await.unwrap().is_none());
        assert_eq!(store.goals(&sid).await.unwrap()[0].status, "pending");
        assert_eq!(inbox_count(&store).await, 0);
        assert_eq!(store.events(&sid, 0).await.unwrap().len(), 1);
        store
            .call(|db| {
                db.execute_batch("DROP TRIGGER autonomy_fail_goal;")?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            action(&store, &sid, &g.id, "start").await.unwrap().status,
            "active"
        );
    }

    #[tokio::test]
    async fn autonomy_store_answers_enter_current_epoch_only_when_drained() {
        let (_dir, store, sid) = fixture().await;
        let q = ask(&store, &sid, "epoch", false).await;
        store
            .answer_question(&sid, &q.id, "after compaction".into())
            .await
            .unwrap();
        store
            .rebase_context(
                &sid,
                vec![json!({"role":"user","content":"Compacted original task"})],
                "test-route",
            )
            .await
            .unwrap();
        assert_eq!(store.context(&sid).await.unwrap().len(), 1);
        assert_eq!(store.drain_inbox(&sid).await.unwrap(), 1);
        let context = store.context(&sid).await.unwrap();
        assert_eq!(context.len(), 2);
        assert!(
            context[1]["content"]
                .as_str()
                .unwrap()
                .contains("after compaction")
        );
        let query_sid = sid.clone();
        let old_epoch_answers: i64 = store
            .call(move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM context_items WHERE session_id=?1 AND epoch=0",
                    [query_sid],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(old_epoch_answers, 0);
    }

    #[tokio::test]
    async fn autonomy_store_question_order_and_multiple_blocking_gate_are_explicit() {
        let (_dir, store, sid) = fixture().await;
        let a = ask(&store, &sid, "blocking-a", true).await;
        let b = ask(&store, &sid, "blocking-b", true).await;
        let c = ask(&store, &sid, "optional-c", false).await;
        let questions = store.questions(&sid).await.unwrap();
        assert_eq!(
            questions.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(),
            vec![c.id.as_str(), b.id.as_str(), a.id.as_str()]
        );
        store
            .answer_question(&sid, &a.id, "A".into())
            .await
            .unwrap();
        assert!(store.pending_blocking(&sid).await.unwrap());
        store
            .answer_question(&sid, &b.id, "B".into())
            .await
            .unwrap();
        assert!(!store.pending_blocking(&sid).await.unwrap());
        assert_eq!(store.questions(&sid).await.unwrap()[0].status, "pending");
    }

    #[tokio::test]
    async fn autonomy_store_snapshots_do_not_include_application_credentials() {
        let (_dir, store, sid) = fixture().await;
        let mut config = store.config().await.unwrap();
        config.search.provider = "brave".into();
        config.search.api_key = Some("private-search-credential-never-in-autonomy-events".into());
        store.save_config(config).await.unwrap();
        let q = ask(&store, &sid, "public-snapshot", false).await;
        store
            .answer_question(&sid, &q.id, "ordinary answer".into())
            .await
            .unwrap();
        store.drain_inbox(&sid).await.unwrap();
        let g = goal(&store, &sid, "Public goal snapshot").await;
        action(&store, &sid, &g.id, "start").await.unwrap();
        store.advance_goal_round(&sid, &g.id).await.unwrap();
        store
            .pause_active_goals(&sid, "User requested pause")
            .await
            .unwrap();
        let encoded = serde_json::to_string(&store.events(&sid, 0).await.unwrap()).unwrap();
        assert!(!encoded.contains("private-search-credential"));
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("selection"));
        assert!(
            !store
                .autonomy_summary(&sid)
                .await
                .unwrap()
                .contains("private-search-credential")
        );
    }

    #[tokio::test]
    async fn autonomy_store_concurrent_goal_starts_have_one_winner() {
        let (_dir, store, sid) = fixture().await;
        let a = goal(&store, &sid, "Concurrent A").await;
        let b = goal(&store, &sid, "Concurrent B").await;
        let (ra, rb) = tokio::join!(
            action(&store, &sid, &a.id, "start"),
            action(&store, &sid, &b.id, "start")
        );
        assert_ne!(ra.is_ok(), rb.is_ok());
        assert_eq!(inbox_count(&store).await, 1);
        assert_eq!(
            store
                .goals(&sid)
                .await
                .unwrap()
                .iter()
                .filter(|g| g.status == "active")
                .count(),
            1
        );
    }
}
