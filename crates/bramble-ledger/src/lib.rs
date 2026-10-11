//! Double-entry ledger for a pet-bank, stored in SQLite.
//!
//! Invariants are enforced twice: in Rust (fail fast with a clear error) and by
//! triggers in `schema.sql`, so a learner poking at the database with `sqlite3`
//! hits the same rules the code does.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

pub use bramble_core::money::format_minor;

pub mod nacha;
pub mod rails;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRow {
    pub entry_id: EntryId,
    pub memo: String,
    pub postings: Vec<Posting>,
}

/// What an ACH origination queues for the next file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub originator: String,
    pub receiving_bank: String,
    pub receiving_account: String,
    pub receiver_name: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRow {
    pub trace_seq: i64,
    pub entry_id: EntryId,
    pub item: OutboxItem,
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
        let tx = self.conn.transaction()?;
        let outcome = post_in(&tx, entry)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Books an ACH payment and queues it for the next file, atomically (see
    /// `ach_outbox` in schema.sql). A replayed idempotency key queues nothing new.
    pub fn originate_ach(&mut self, entry: &NewEntry, item: &OutboxItem) -> Result<PostOutcome> {
        let tx = self.conn.transaction()?;
        let outcome = post_in(&tx, entry)?;
        if let PostOutcome::Created(entry_id) = outcome {
            tx.execute(
                "INSERT INTO ach_outbox
                     (entry_id, originator, receiving_bank, receiving_account, receiver_name, amount_minor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry_id,
                    item.originator,
                    item.receiving_bank,
                    item.receiving_account,
                    item.receiver_name,
                    item.amount_minor
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// ACH payments waiting for the next cutoff, oldest first.
    pub fn pending_ach(&self) -> Result<Vec<OutboxRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, originator, receiving_bank, receiving_account, receiver_name, amount_minor
             FROM ach_outbox
             WHERE file_id IS NULL
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(OutboxRow {
                trace_seq: row.get(0)?,
                entry_id: row.get(1)?,
                item: OutboxItem {
                    originator: row.get(2)?,
                    receiving_bank: row.get(3)?,
                    receiving_account: row.get(4)?,
                    receiver_name: row.get(5)?,
                    amount_minor: row.get(6)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
        let mut stmt = self
            .conn
            .prepare("SELECT account_id, kind, balance_minor FROM balances ORDER BY account_id")?;
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

    /// Highest posted entry id (0 for an empty ledger): a high-water mark for "entries since".
    pub fn max_entry_id(&self) -> Result<EntryId> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM journal_entries WHERE status = 'posted'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn entry_id_by_idempotency_key(&self, key: &str) -> Result<Option<EntryId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM journal_entries WHERE idempotency_key = ?1 AND status = 'posted'",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// First posted entry after `since` whose postings are exactly `postings` (order-insensitive).
    pub fn find_entry_since(
        &self,
        since: EntryId,
        postings: &[Posting],
    ) -> Result<Option<EntryId>> {
        let mut wanted: Vec<(&str, i64)> = postings
            .iter()
            .map(|p| (p.account.as_str(), p.amount_minor))
            .collect();
        wanted.sort_unstable();

        let mut stmt = self.conn.prepare(
            "SELECT e.id, p.account_id, p.amount_minor
             FROM journal_entries AS e
             JOIN postings AS p ON p.entry_id = e.id
             WHERE e.status = 'posted' AND e.id > ?1
             ORDER BY e.id",
        )?;
        let rows = stmt.query_map([since], |row| {
            Ok((
                row.get::<_, EntryId>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        let mut current: Option<(EntryId, Vec<(String, i64)>)> = None;
        let matches = |legs: &mut Vec<(String, i64)>| {
            legs.sort_unstable();
            legs.len() == wanted.len()
                && legs
                    .iter()
                    .zip(&wanted)
                    .all(|((a, x), (b, y))| a == b && x == y)
        };
        for row in rows {
            let (id, account, amount) = row?;
            match &mut current {
                Some((current_id, legs)) if *current_id == id => legs.push((account, amount)),
                _ => {
                    if let Some((done_id, mut legs)) = current.take()
                        && matches(&mut legs)
                    {
                        return Ok(Some(done_id));
                    }
                    current = Some((id, vec![(account, amount)]));
                }
            }
        }
        Ok(current.and_then(|(id, mut legs)| matches(&mut legs).then_some(id)))
    }

    /// The newest `limit` posted entries with their postings, newest first.
    pub fn recent_entries(&self, limit: usize) -> Result<Vec<EntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.memo, p.account_id, p.amount_minor
             FROM journal_entries AS e
             JOIN postings AS p ON p.entry_id = e.id
             WHERE e.id IN (
                 SELECT id FROM journal_entries WHERE status = 'posted' ORDER BY id DESC LIMIT ?1
             )
             ORDER BY e.id DESC, p.id",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map([limit], |row| {
            Ok((
                row.get::<_, EntryId>(0)?,
                row.get::<_, String>(1)?,
                Posting::new(row.get::<_, String>(2)?, row.get::<_, i64>(3)?),
            ))
        })?;
        let mut entries: Vec<EntryRow> = Vec::new();
        for row in rows {
            let (entry_id, memo, posting) = row?;
            match entries.last_mut() {
                Some(last) if last.entry_id == entry_id => last.postings.push(posting),
                _ => entries.push(EntryRow {
                    entry_id,
                    memo,
                    postings: vec![posting],
                }),
            }
        }
        Ok(entries)
    }

    pub fn entry_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM journal_entries WHERE status = 'posted'",
            [],
            |row| row.get(0),
        )?)
    }
}

/// Posts `entry` inside the caller's transaction; the caller commits.
fn post_in(tx: &Transaction<'_>, entry: &NewEntry) -> Result<PostOutcome> {
    validate(entry)?;
    let fingerprint = fingerprint(entry);

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
    Ok(PostOutcome::Created(entry_id))
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

#[cfg(test)]
mod tests;
