//! The pet's own state (not the bank's books), persisted as JSON documents in SQLite.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub struct PetStore {
    conn: Connection,
}

impl PetStore {
    /// Opens the store readable only by the node's user: it holds the signing key.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let store = Self::init(Connection::open(&path)?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            -- Raw learning signal for tuning the Teacher later (e.g. a format bandit).
            CREATE TABLE IF NOT EXISTS learning_events (
                id         INTEGER PRIMARY KEY,
                ts         INTEGER NOT NULL,
                concept    TEXT NOT NULL,
                lesson_id  TEXT NOT NULL,
                task_id    TEXT,
                outcome    TEXT NOT NULL,
                elapsed_ms INTEGER NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn load<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT value FROM documents WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    pub fn log_event(&self, event: &LearningEvent<'_>) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO learning_events (ts, concept, lesson_id, task_id, outcome, elapsed_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.ts,
                event.concept,
                event.lesson_id,
                event.task_id,
                event.outcome,
                event.elapsed_ms
            ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn event_outcomes(&self) -> anyhow::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT outcome FROM learning_events ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO documents (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
}

pub struct LearningEvent<'a> {
    pub ts: i64,
    pub concept: &'a str,
    pub lesson_id: &'a str,
    pub task_id: Option<&'a str>,
    pub outcome: &'a str,
    pub elapsed_ms: i64,
}
