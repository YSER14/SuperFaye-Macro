use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::assistant::model::estimate_text_tokens;

const SUMMARY_PREFIX: &str = "较早对话的续聊摘要，供参考：\n";

#[derive(Debug, Clone)]
pub struct HistoryTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct CompactionInput {
    pub previous: i64,
    pub through: i64,
    pub used: usize,
    pub messages: Vec<HistoryTurn>,
}

/// Persistent SQLite conversation & preference store matching `core/assistant_history.py`.
pub struct AssistantHistory {
    pub path: PathBuf,
}

impl AssistantHistory {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn connect(&self) -> Result<Connection, String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(&self.path).map_err(|e| e.to_string())?;
        db.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS chats (
                id TEXT PRIMARY KEY, mode TEXT NOT NULL, title TEXT NOT NULL, updated INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS turns (
                id INTEGER PRIMARY KEY, chat TEXT NOT NULL REFERENCES chats(id),
                prompt TEXT NOT NULL, model TEXT NOT NULL, reply TEXT,
                phases TEXT NOT NULL DEFAULT '[]', seconds INTEGER,
                effects TEXT NOT NULL DEFAULT '[]'
            );
            CREATE INDEX IF NOT EXISTS turns_chat ON turns(chat, id);
            CREATE TABLE IF NOT EXISTS preferences (name TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS context_summaries (
                chat TEXT PRIMARY KEY REFERENCES chats(id), through_turn INTEGER NOT NULL, summary TEXT NOT NULL
            );
            ",
        )
        .map_err(|e| e.to_string())?;
        Ok(db)
    }

    pub fn permission(&self) -> Result<String, String> {
        let db = self.connect()?;
        let row: Option<String> = db
            .query_row(
                "SELECT value FROM preferences WHERE name = 'permission'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(row.unwrap_or_else(|| "ask".to_string()))
    }

    pub fn set_permission(&self, permission: &str) -> Result<(), String> {
        let db = self.connect()?;
        db.execute(
            "INSERT OR REPLACE INTO preferences VALUES ('permission', ?1)",
            params![permission],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn summary(db: &Connection, chat: &str) -> Result<(i64, String), String> {
        let row: Option<(i64, String)> = db
            .query_row(
                "SELECT through_turn, summary FROM context_summaries WHERE chat = ?1",
                params![chat],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(match row {
            Some((through, text)) => (through, format!("{SUMMARY_PREFIX}{text}")),
            None => (0, String::new()),
        })
    }

    fn turn_tokens(prompt: &str, reply: &str) -> usize {
        estimate_text_tokens(prompt) + estimate_text_tokens(reply)
    }

    fn context_tokens(through: i64, summary: &str, turns: &[(i64, String, Option<String>)]) -> usize {
        let mut total = if summary.is_empty() {
            0
        } else {
            estimate_text_tokens(summary)
        };
        for (id, prompt, reply_opt) in turns {
            if *id > through {
                if let Some(reply) = reply_opt {
                    total += Self::turn_tokens(prompt, reply);
                }
            }
        }
        total
    }

    pub fn history(&self, mode: &str, chat_arg: Option<&str>) -> Result<Value, String> {
        let db = self.connect()?;
        let mut stmt = db
            .prepare(
                "SELECT id, title FROM chats WHERE mode = ?1
                 AND EXISTS (SELECT 1 FROM turns WHERE turns.chat = chats.id)
                 ORDER BY updated DESC, rowid DESC",
            )
            .map_err(|e| e.to_string())?;
        let chats: Vec<(String, String)> = stmt
            .query_map(params![mode], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let pref_name = format!("active_chat:{mode}");
        let explicit = chat_arg.is_some();
        let mut chat = match chat_arg {
            Some(c) => c.to_string(),
            None => {
                let last: Option<String> = db
                    .query_row(
                        "SELECT value FROM preferences WHERE name = ?1",
                        params![&pref_name],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                last.unwrap_or_else(|| chats.first().map(|(id, _)| id.clone()).unwrap_or_default())
            }
        };

        if !chat.is_empty() && !chats.iter().any(|(id, _)| id == &chat) {
            if explicit {
                return Err("找不到这段对话".to_string());
            }
            chat = chats.first().map(|(id, _)| id.clone()).unwrap_or_default();
        }
        if explicit {
            db.execute(
                "INSERT OR REPLACE INTO preferences VALUES (?1, ?2)",
                params![&pref_name, &chat],
            )
            .map_err(|e| e.to_string())?;
        }

        let mut turn_stmt = db
            .prepare(
                "SELECT id, prompt, model, reply, phases, seconds, effects FROM turns WHERE chat = ?1 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        struct TurnRow {
            id: i64,
            prompt: String,
            model: String,
            reply: Option<String>,
            phases: String,
            seconds: Option<i64>,
            effects: String,
        }
        let rows: Vec<TurnRow> = turn_stmt
            .query_map(params![&chat], |r| {
                Ok(TurnRow {
                    id: r.get(0)?,
                    prompt: r.get(1)?,
                    model: r.get(2)?,
                    reply: r.get(3)?,
                    phases: r.get(4)?,
                    seconds: r.get(5)?,
                    effects: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let (through, summary) = Self::summary(&db, &chat)?;
        let raw_tokens: Vec<(i64, String, Option<String>)> = rows
            .iter()
            .map(|r| (r.id, r.prompt.clone(), r.reply.clone()))
            .collect();
        let context_tokens = Self::context_tokens(through, &summary, &raw_tokens);

        let turns_json: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let phases: Value = serde_json::from_str(&r.phases).unwrap_or_else(|_| json!([]));
                let effects: Value = serde_json::from_str(&r.effects).unwrap_or_else(|_| json!([]));
                json!({
                    "prompt": r.prompt,
                    "model": r.model,
                    "reply": r.reply,
                    "phases": phases,
                    "seconds": r.seconds,
                    "effects": effects,
                })
            })
            .collect();

        let chats_json: Vec<Value> = chats
            .into_iter()
            .map(|(id, title)| json!({"id": id, "title": title}))
            .collect();

        Ok(json!({
            "chats": chats_json,
            "chat": chat,
            "turns": turns_json,
            "context_tokens": context_tokens,
        }))
    }

    pub fn begin(
        &self,
        mode: &str,
        chat_arg: &str,
        prompt: &str,
        model: &str,
    ) -> Result<(String, i64), String> {
        let mut db = self.connect()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let updated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        let chat = if !chat_arg.is_empty() {
            let changed = tx
                .execute(
                    "UPDATE chats SET updated = ?1 WHERE id = ?2 AND mode = ?3",
                    params![updated, chat_arg, mode],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("找不到这段对话".to_string());
            }
            chat_arg.to_string()
        } else {
            let new_id = format!("{:032x}", rand::random::<u128>());
            let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
            let title: String = collapsed.chars().take(52).collect();
            tx.execute(
                "INSERT INTO chats VALUES (?1, ?2, ?3, ?4)",
                params![&new_id, mode, title, updated],
            )
            .map_err(|e| e.to_string())?;
            new_id
        };
        tx.execute(
            "INSERT INTO turns(chat, prompt, model) VALUES (?1, ?2, ?3)",
            params![&chat, prompt, model],
        )
        .map_err(|e| e.to_string())?;
        let turn_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT OR REPLACE INTO preferences VALUES (?1, ?2)",
            params![format!("active_chat:{mode}"), &chat],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok((chat, turn_id))
    }

    pub fn turn_boundary(&self, mode: &str, chat: &str) -> Result<i64, String> {
        let db = self.connect()?;
        let row: Option<i64> = db
            .query_row(
                "SELECT COALESCE((SELECT MAX(id) FROM turns WHERE chat = chats.id), 0) + 1
                 FROM chats WHERE mode = ?1 AND id = ?2",
                params![mode, chat],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        row.ok_or_else(|| "找不到这段对话".to_string())
    }

    pub fn recent(&self, chat: &str, before: i64, mut budget: isize) -> Result<Vec<HistoryTurn>, String> {
        let db = self.connect()?;
        let (through, mut summary) = Self::summary(&db, chat)?;
        let mut summary_cost = if summary.is_empty() {
            0
        } else {
            estimate_text_tokens(&summary)
        };
        if summary_cost as isize > budget {
            summary.clear();
            summary_cost = 0;
        }
        budget -= summary_cost as isize;

        let mut stmt = db
            .prepare(
                "SELECT prompt, reply FROM turns WHERE chat = ?1 AND id > ?2 AND id < ?3
                 AND reply IS NOT NULL ORDER BY id DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![chat, through, before], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let mut messages = Vec::new();
        for (prompt, reply) in rows {
            budget -= Self::turn_tokens(&prompt, &reply) as isize;
            if budget < 0 {
                break;
            }
            messages.push(HistoryTurn {
                role: "assistant".to_string(),
                content: reply,
            });
            messages.push(HistoryTurn {
                role: "user".to_string(),
                content: prompt,
            });
        }
        messages.reverse();
        if !summary.is_empty() {
            messages.insert(
                0,
                HistoryTurn {
                    role: "user".to_string(),
                    content: summary,
                },
            );
        }
        Ok(messages)
    }

    pub fn compaction_input(
        &self,
        chat: &str,
        before: i64,
        budget: usize,
    ) -> Result<CompactionInput, String> {
        let db = self.connect()?;
        let (previous, summary) = Self::summary(&db, chat)?;
        let mut messages = Vec::new();
        let mut used = 0usize;
        if !summary.is_empty() {
            used += estimate_text_tokens(&summary);
            messages.push(HistoryTurn {
                role: "user".to_string(),
                content: summary,
            });
        }
        let mut through = previous;
        let mut stmt = db
            .prepare(
                "SELECT id, prompt, reply FROM turns WHERE chat = ?1 AND id > ?2 AND id < ?3
                 AND reply IS NOT NULL ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String)> = stmt
            .query_map(params![chat, previous, before], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for (id, prompt, reply) in rows {
            let cost = Self::turn_tokens(&prompt, &reply);
            if used + cost > budget {
                break;
            }
            used += cost;
            through = id;
            messages.push(HistoryTurn {
                role: "user".to_string(),
                content: prompt,
            });
            messages.push(HistoryTurn {
                role: "assistant".to_string(),
                content: reply,
            });
        }
        Ok(CompactionInput {
            previous,
            through,
            used,
            messages,
        })
    }

    pub fn save_summary(
        &self,
        chat: &str,
        previous: i64,
        through: i64,
        summary_text: &str,
    ) -> Result<usize, String> {
        let mut db = self.connect()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let (latest, _) = Self::summary(&tx, chat)?;
        if latest != previous {
            return Err("上下文已在另一窗口压缩，请刷新对话后重试".to_string());
        }
        tx.execute(
            "INSERT OR REPLACE INTO context_summaries VALUES (?1, ?2, ?3)",
            params![chat, through, summary_text],
        )
        .map_err(|e| e.to_string())?;
        let (_, stored) = Self::summary(&tx, chat)?;
        let turns: Vec<(i64, String, Option<String>)> = {
            let mut stmt = tx
                .prepare("SELECT id, prompt, reply FROM turns WHERE chat = ?1 AND id > ?2")
                .map_err(|e| e.to_string())?;
            stmt.query_map(params![chat, through], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Self::context_tokens(through, &stored, &turns))
    }

    pub fn record_effects(&self, turn: i64, effects: &[Value]) -> Result<(), String> {
        let db = self.connect()?;
        let encoded = serde_json::to_string(effects).map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE turns SET effects = ?1 WHERE id = ?2",
            params![encoded, turn],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn finish(
        &self,
        turn: i64,
        reply: Option<&str>,
        phases: &[String],
        seconds: i64,
        had_effects: bool,
    ) -> Result<(), String> {
        let db = self.connect()?;
        if reply.is_none() && !had_effects {
            db.execute("DELETE FROM turns WHERE id = ?1", params![turn])
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        let phases_json = serde_json::to_string(phases).map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE turns SET reply = ?1, phases = ?2, seconds = ?3 WHERE id = ?4",
            params![reply, phases_json, seconds, turn],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}
