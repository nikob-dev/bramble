//! Double-entry ledger for a pet-bank, stored in SQLite.
//!
//! Invariants are enforced twice: in Rust (fail fast with a clear error) and by
//! triggers in `schema.sql`, so a learner poking at the database with `sqlite3`
//! hits the same rules the code does.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = include_str!("schema.sql");

pub type EntryId = i64;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("an entry needs at least two postings, got {0}")]
    TooFewPostings(usize),
    #[error("postings must sum to zero, but they sum to {0}")]
    Unbalanced(i64),
    #[error("posting amounts must be non-zero")]
    ZeroAmount,
    #[error("unknown account: {0}")]
    UnknownAccount(String),
    #[error("idempotency key {0:?} was already used for a different request")]
    IdempotencyConflict(String),
    #[error("unknown account kind: {0}")]
    UnknownKind(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "asset",
            Self::Liability => "liability",
            Self::Equity => "equity",
            Self::Revenue => "revenue",
            Self::Expense => "expense",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "asset" => Ok(Self::Asset),
            "liability" => Ok(Self::Liability),
            "equity" => Ok(Self::Equity),
            "revenue" => Ok(Self::Revenue),
            "expense" => Ok(Self::Expense),
            other => Err(LedgerError::UnknownKind(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub account: String,
    pub amount_minor: i64,
}

impl Posting {
    pub fn new(account: impl Into<String>, amount_minor: i64) -> Self {
        Self {
            account: account.into(),
            amount_minor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEntry {
    pub memo: String,
    pub idempotency_key: Option<String>,
    pub postings: Vec<Posting>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostOutcome {
    Created(EntryId),
    /// The idempotency key matched an earlier identical request; nothing new was written.
    Replayed(EntryId),
}

impl PostOutcome {
    pub fn entry_id(self) -> EntryId {
        match self {
            Self::Created(id) | Self::Replayed(id) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalance {
    pub account: String,
    pub kind: AccountKind,
    pub balance_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingRow {
    pub entry_id: EntryId,
    pub memo: String,
    pub account: String,
    pub amount_minor: i64,
    pub created_at: i64,
}

pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// Opens (creating if needed) a writable ledger. Only the node process should do this.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Opens an existing ledger for reading only, as the lab's `learner` user does.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // Rollback-journal mode (not WAL) so read-only users need no write access to -shm files.
        conn.pragma_update(None, "journal_mode", "DELETE")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn ensure_account(&self, id: &str, kind: AccountKind) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (id, kind) VALUES (?1, ?2) ON CONFLICT (id) DO NOTHING",
            params![id, kind.as_str()],
        )?;
        Ok(())
    }

    pub fn post(&mut self, entry: &NewEntry) -> Result<PostOutcome> {
        validate(entry)?;
        let fingerprint = fingerprint(entry);

        let tx = self.conn.transaction()?;

        if let Some(key) = &entry.idempotency_key {
            let existing: Option<(EntryId, Option<String>)> = tx
                .query_row(
                    "SELECT id, request_fingerprint FROM journal_entries WHERE idempotency_key = ?1",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((id, stored)) = existing {
                return if stored.as_deref() == Some(fingerprint.as_str()) {
                    Ok(PostOutcome::Replayed(id))
                } else {
                    Err(LedgerError::IdempotencyConflict(key.clone()))
                };
            }
        }

        for posting in &entry.postings {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ?1)",
                [&posting.account],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(LedgerError::UnknownAccount(posting.account.clone()));
            }
        }

        tx.execute(
            "INSERT INTO journal_entries (memo, idempotency_key, request_fingerprint) VALUES (?1, ?2, ?3)",
            params![entry.memo, entry.idempotency_key, fingerprint],
        )?;
        let entry_id = tx.last_insert_rowid();
        for posting in &entry.postings {
            tx.execute(
                "INSERT INTO postings (entry_id, account_id, amount_minor) VALUES (?1, ?2, ?3)",
                params![entry_id, posting.account, posting.amount_minor],
            )?;
        }
        tx.execute(
            "UPDATE journal_entries SET status = 'posted' WHERE id = ?1",
            [entry_id],
        )?;
        tx.commit()?;
        Ok(PostOutcome::Created(entry_id))
    }

    pub fn balance(&self, account: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT balance_minor FROM balances WHERE account_id = ?1",
                [account],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| LedgerError::UnknownAccount(account.to_owned()))
    }

    pub fn accounts(&self) -> Result<Vec<AccountBalance>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, kind, balance_minor FROM balances ORDER BY account_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (account, kind, balance_minor) = row?;
            Ok(AccountBalance {
                account,
                kind: AccountKind::parse(&kind)?,
                balance_minor,
            })
        })
        .collect()
    }

    /// Posted postings, newest first. `account = None` returns postings for every account.
    pub fn postings(&self, account: Option<&str>) -> Result<Vec<PostingRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.memo, p.account_id, p.amount_minor, e.created_at
             FROM postings AS p
             JOIN journal_entries AS e ON e.id = p.entry_id
             WHERE e.status = 'posted'
               AND (?1 IS NULL OR p.account_id = ?1)
             ORDER BY e.id DESC, p.id",
        )?;
        let rows = stmt.query_map([account], |row| {
            Ok(PostingRow {
                entry_id: row.get(0)?,
                memo: row.get(1)?,
                account: row.get(2)?,
                amount_minor: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn entry_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM journal_entries WHERE status = 'posted'",
            [],
            |row| row.get(0),
        )?)
    }
}

fn validate(entry: &NewEntry) -> Result<()> {
    if entry.postings.len() < 2 {
        return Err(LedgerError::TooFewPostings(entry.postings.len()));
    }
    if entry.postings.iter().any(|p| p.amount_minor == 0) {
        return Err(LedgerError::ZeroAmount);
    }
    let sum: i64 = entry.postings.iter().map(|p| p.amount_minor).sum();
    if sum != 0 {
        return Err(LedgerError::Unbalanced(sum));
    }
    Ok(())
}

/// What makes two requests "the same" for idempotency purposes.
fn fingerprint(entry: &NewEntry) -> String {
    serde_json::json!({ "memo": entry.memo, "postings": entry.postings }).to_string()
}

/// Formats minor units as a currency string, e.g. `-7150` → `-$71.50`.
pub fn format_minor(amount_minor: i64) -> String {
    let sign = if amount_minor < 0 { "-" } else { "" };
    let abs = amount_minor.unsigned_abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests;
