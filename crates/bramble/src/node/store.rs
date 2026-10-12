//! The pet's own state (not the bank's books), persisted as JSON documents in SQLite.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

pub struct PetStore {
    conn: Connection,
}

impl PetStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?)
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

    pub fn save<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO documents (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
}
