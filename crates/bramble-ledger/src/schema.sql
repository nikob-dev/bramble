-- Bramble pet-bank ledger. Amounts are signed integers in minor units (cents):
-- debits are positive, credits are negative, and every entry sums to zero.

CREATE TABLE IF NOT EXISTS accounts (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('asset', 'liability', 'equity', 'revenue', 'expense')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS journal_entries (
    id                  INTEGER PRIMARY KEY,
    memo                TEXT NOT NULL,
    idempotency_key     TEXT UNIQUE,
    request_fingerprint TEXT,
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'posted')),
    created_at          INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS postings (
    id           INTEGER PRIMARY KEY,
    entry_id     INTEGER NOT NULL REFERENCES journal_entries (id),
    account_id   TEXT NOT NULL REFERENCES accounts (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor <> 0)
);

CREATE INDEX IF NOT EXISTS postings_by_account ON postings (account_id);
CREATE INDEX IF NOT EXISTS postings_by_entry ON postings (entry_id);

-- An entry may only be posted once its postings balance to zero.
CREATE TRIGGER IF NOT EXISTS entry_must_balance
BEFORE UPDATE OF status ON journal_entries
WHEN NEW.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'unbalanced entry: postings must sum to zero across at least two accounts')
    WHERE (SELECT COUNT(*) FROM postings WHERE entry_id = NEW.id) < 2
       OR (SELECT SUM(amount_minor) FROM postings WHERE entry_id = NEW.id) <> 0;
END;

-- Posted history is append-only: mistakes are fixed with a reversing entry, never an edit.
CREATE TRIGGER IF NOT EXISTS posted_entries_are_immutable
BEFORE UPDATE ON journal_entries
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'posted entries are immutable; post a reversing entry instead');
END;

CREATE TRIGGER IF NOT EXISTS posted_entries_cannot_be_deleted
BEFORE DELETE ON journal_entries
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'posted entries are immutable; post a reversing entry instead');
END;

CREATE TRIGGER IF NOT EXISTS postings_cannot_be_updated
BEFORE UPDATE ON postings
BEGIN
    SELECT RAISE(ABORT, 'postings are append-only');
END;

CREATE TRIGGER IF NOT EXISTS postings_cannot_be_deleted
BEFORE DELETE ON postings
WHEN (SELECT status FROM journal_entries WHERE id = OLD.entry_id) = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'postings are append-only');
END;

CREATE TRIGGER IF NOT EXISTS no_postings_on_posted_entries
BEFORE INSERT ON postings
WHEN (SELECT status FROM journal_entries WHERE id = NEW.entry_id) = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'cannot add postings to a posted entry');
END;

CREATE VIEW IF NOT EXISTS balances AS
SELECT
    a.id                            AS account_id,
    a.kind                          AS kind,
    COALESCE(SUM(pp.amount_minor), 0) AS balance_minor
FROM accounts AS a
LEFT JOIN (
    SELECT p.account_id, p.amount_minor
    FROM postings AS p
    JOIN journal_entries AS e ON e.id = p.entry_id
    WHERE e.status = 'posted'
) AS pp ON pp.account_id = a.id
GROUP BY a.id, a.kind;

-- ACH payments booked but not yet sent to the operator in a file. A transactional
-- outbox: each row is written in the same transaction as the entry that debited the
-- customer, so the books and the next file can never disagree.
CREATE TABLE IF NOT EXISTS ach_outbox (
    id                INTEGER PRIMARY KEY, -- also the entry's trace sequence in the file
    entry_id          INTEGER NOT NULL UNIQUE REFERENCES journal_entries (id),
    originator        TEXT NOT NULL,       -- our customer account that pays
    receiving_bank    TEXT NOT NULL,
    receiving_account TEXT NOT NULL,
    receiver_name     TEXT NOT NULL,
    amount_minor      INTEGER NOT NULL CHECK (amount_minor > 0),
    file_id           TEXT,                -- NULL until a cutoff sends it
    created_at        INTEGER NOT NULL DEFAULT (unixepoch())
);
