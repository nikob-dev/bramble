use super::*;

fn bank() -> Ledger {
    let ledger = Ledger::open_in_memory().expect("in-memory ledger");
    ledger
        .ensure_account("reserves", AccountKind::Asset)
        .expect("reserves");
    ledger
        .ensure_account("customer:you", AccountKind::Liability)
        .expect("customer");
    ledger
}

fn deposit(amount: i64, key: Option<&str>) -> NewEntry {
    NewEntry {
        memo: "deposit".into(),
        idempotency_key: key.map(str::to_owned),
        postings: vec![
            Posting::new("reserves", amount),
            Posting::new("customer:you", -amount),
        ],
    }
}

#[test]
fn ledger_post_balanced_entry_updates_both_balances() {
    let mut ledger = bank();
    ledger.post(&deposit(10_000, None)).expect("post");
    assert_eq!(ledger.balance("reserves").expect("balance"), 10_000);
    assert_eq!(ledger.balance("customer:you").expect("balance"), -10_000);
}

#[test]
fn ledger_post_unbalanced_entry_is_rejected_in_code() {
    let mut ledger = bank();
    let mut entry = deposit(10_000, None);
    entry.postings[1].amount_minor = -9_000;
    assert!(matches!(
        ledger.post(&entry),
        Err(LedgerError::Unbalanced(1_000))
    ));
    assert_eq!(ledger.entry_count().expect("count"), 0);
}

#[test]
fn ledger_trigger_rejects_unbalanced_entry_written_with_raw_sql() {
    let ledger = bank();
    ledger
        .conn
        .execute_batch(
            "INSERT INTO journal_entries (id, memo) VALUES (1, 'sneaky');
             INSERT INTO postings (entry_id, account_id, amount_minor) VALUES (1, 'reserves', 500);
             INSERT INTO postings (entry_id, account_id, amount_minor) VALUES (1, 'customer:you', -400);",
        )
        .expect("pending rows are allowed");
    let err = ledger
        .conn
        .execute(
            "UPDATE journal_entries SET status = 'posted' WHERE id = 1",
            [],
        )
        .expect_err("trigger must abort");
    assert!(err.to_string().contains("unbalanced entry"), "{err}");
    assert_eq!(
        ledger.balance("reserves").expect("balance"),
        0,
        "pending postings never count"
    );
}

#[test]
fn ledger_post_too_few_postings_is_rejected() {
    let mut ledger = bank();
    let entry = NewEntry {
        memo: "one-legged".into(),
        idempotency_key: None,
        postings: vec![Posting::new("reserves", 100)],
    };
    assert!(matches!(
        ledger.post(&entry),
        Err(LedgerError::TooFewPostings(1))
    ));
}

#[test]
fn ledger_post_unknown_account_is_rejected() {
    let mut ledger = bank();
    let mut entry = deposit(100, None);
    entry.postings[1].account = "customer:ghost".into();
    assert!(
        matches!(ledger.post(&entry), Err(LedgerError::UnknownAccount(a)) if a == "customer:ghost")
    );
}

#[test]
fn ledger_post_retry_with_same_idempotency_key_writes_once() {
    let mut ledger = bank();
    let first = ledger
        .post(&deposit(2_500, Some("pay-123")))
        .expect("first");
    let retry = ledger
        .post(&deposit(2_500, Some("pay-123")))
        .expect("retry");
    assert!(matches!(first, PostOutcome::Created(_)));
    assert_eq!(retry, PostOutcome::Replayed(first.entry_id()));
    assert_eq!(ledger.entry_count().expect("count"), 1);
    assert_eq!(ledger.balance("reserves").expect("balance"), 2_500);
}

#[test]
fn ledger_post_reused_key_with_different_payload_conflicts() {
    let mut ledger = bank();
    ledger
        .post(&deposit(2_500, Some("pay-123")))
        .expect("first");
    let err = ledger
        .post(&deposit(9_999, Some("pay-123")))
        .expect_err("conflict");
    assert!(matches!(err, LedgerError::IdempotencyConflict(k) if k == "pay-123"));
}

#[test]
fn ledger_posted_history_is_append_only() {
    let mut ledger = bank();
    let id = ledger.post(&deposit(100, None)).expect("post").entry_id();
    for sql in [
        "UPDATE postings SET amount_minor = 1",
        "DELETE FROM postings",
        "UPDATE journal_entries SET memo = 'edited'",
        "DELETE FROM journal_entries",
    ] {
        assert!(
            ledger.conn.execute(sql, []).is_err(),
            "{sql} should be rejected"
        );
    }
    let add_leg = ledger.conn.execute(
        "INSERT INTO postings (entry_id, account_id, amount_minor) VALUES (?1, 'reserves', 1)",
        [id],
    );
    assert!(add_leg.is_err());
}

#[test]
fn ledger_read_only_handle_cannot_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ledger.db");
    {
        let mut ledger = Ledger::open(&path).expect("open rw");
        ledger
            .ensure_account("reserves", AccountKind::Asset)
            .expect("acct");
        ledger
            .ensure_account("customer:you", AccountKind::Liability)
            .expect("acct");
        ledger.post(&deposit(700, None)).expect("post");
    }
    let mut ro = Ledger::open_read_only(&path).expect("open ro");
    assert_eq!(ro.balance("reserves").expect("read"), 700);
    assert!(ro.post(&deposit(1, None)).is_err());
}

#[test]
fn ledger_accounts_and_postings_report_posted_rows() {
    let mut ledger = bank();
    ledger.post(&deposit(300, None)).expect("post");
    ledger.post(&deposit(200, None)).expect("post");
    let accounts = ledger.accounts().expect("accounts");
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].account, "customer:you");
    assert_eq!(accounts[0].balance_minor, -500);
    let rows = ledger.postings(Some("reserves")).expect("postings");
    assert_eq!(
        rows.iter().map(|r| r.amount_minor).collect::<Vec<_>>(),
        vec![200, 300]
    );
    assert_eq!(ledger.postings(None).expect("all").len(), 4);
}

#[test]
fn ledger_max_entry_id_and_key_lookup() {
    let mut ledger = bank();
    assert_eq!(ledger.max_entry_id().expect("max"), 0);
    let id = ledger
        .post(&deposit(100, Some("k1")))
        .expect("post")
        .entry_id();
    assert_eq!(ledger.max_entry_id().expect("max"), id);
    assert_eq!(
        ledger.entry_id_by_idempotency_key("k1").expect("lookup"),
        Some(id)
    );
    assert_eq!(
        ledger.entry_id_by_idempotency_key("nope").expect("lookup"),
        None
    );
}

#[test]
fn ledger_find_entry_since_matches_exact_postings_after_watermark() {
    let mut ledger = bank();
    let first = ledger.post(&deposit(100, None)).expect("post").entry_id();
    let second = ledger.post(&deposit(250, None)).expect("post").entry_id();
    let wanted = [
        Posting::new("customer:you", -100),
        Posting::new("reserves", 100),
    ];
    assert_eq!(
        ledger.find_entry_since(0, &wanted).expect("find"),
        Some(first)
    );
    assert_eq!(
        ledger.find_entry_since(first, &wanted).expect("find"),
        None,
        "watermark excludes older entries"
    );
    let later = [
        Posting::new("reserves", 250),
        Posting::new("customer:you", -250),
    ];
    assert_eq!(
        ledger.find_entry_since(first, &later).expect("find"),
        Some(second)
    );
    let partial = [Posting::new("reserves", 250)];
    assert_eq!(
        ledger.find_entry_since(0, &partial).expect("find"),
        None,
        "all legs must match"
    );
}

#[test]
fn ledger_recent_entries_groups_postings_newest_first() {
    let mut ledger = bank();
    for amount in [100, 200, 300] {
        ledger.post(&deposit(amount, None)).expect("post");
    }
    let recent = ledger.recent_entries(2).expect("recent");
    assert_eq!(recent.len(), 2);
    assert_eq!(
        recent[0].postings,
        vec![
            Posting::new("reserves", 300),
            Posting::new("customer:you", -300)
        ]
    );
    assert_eq!(recent[1].postings[0].amount_minor, 200);
}
