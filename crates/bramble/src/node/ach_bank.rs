//! A bank's part in an ACH settlement window (decisions 013, 014):
//!
//! 1. **Cutoff:** the operator asks for this window's file; we put every pending
//!    payment in it (marking the outbox rows) and send it.
//! 2. **Incoming file:** the operator forwards the entries addressed to us; we credit
//!    each payee from `clearing:ach-in`. Money for an account we don't have is held in
//!    suspense rather than lost.
//! 3. **Settlement notice:** the operator moved our reserves by our net position; we
//!    book that and empty both clearing accounts of this window's gross amounts.

use bramble_core::net::{NetMsg, Rail as RailKind};
use bramble_ledger::nacha::{self, EntryKind};
use bramble_ledger::rails::{
    ACH_INCOMING, ACH_SUSPENSE, Ach, Position, Rail, Transfer, bank_ach_settlement,
};
use bramble_ledger::{Ledger, LedgerError, NewEntry, OutboxItem, PostOutcome, Posting};

use super::rtp::{PaymentRequest, check_outgoing};
use super::{Shared, now_unix};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Invalid(String),
    /// The idempotency key was already used for a different payment.
    Conflict(String),
}

/// Queues an ACH payment: debits the customer into `clearing:ach-out` and adds the
/// entry to the outbox, in one transaction. Nothing crosses banks until cutoff.
pub async fn originate(
    shared: &Shared,
    request: PaymentRequest,
    idempotency_key: Option<String>,
) -> Result<PostOutcome, Refusal> {
    let key = idempotency_key.unwrap_or_else(bramble_net::new_nonce);
    let transfer = Transfer {
        debtor_bank: shared.net.id.clone(),
        debtor_account: request.from_account.clone(),
        creditor_bank: request.to_bank.clone(),
        creditor_account: request.to_account.clone(),
        amount_minor: request.amount_minor,
    };
    if nacha::routing_number(&transfer.creditor_bank).is_none()
        || transfer.creditor_bank == shared.operator_id
    {
        return Err(Refusal::Invalid(format!(
            "{:?} is not a bank on this network",
            transfer.creditor_bank
        )));
    }

    let mut world = shared.world.lock().await;
    let booked_key = format!("ach:{}-{key}", shared.net.id);
    // A retry of a payment that's already queued replays, even if funds have since dropped.
    let already_queued = world
        .ledger
        .entry_id_by_idempotency_key(&booked_key)
        .ok()
        .flatten()
        .is_some();
    if !already_queued {
        check_outgoing(&world.ledger, &transfer).map_err(Refusal::Invalid)?;
    }
    let receiver_name = request
        .receiver_name
        .unwrap_or_else(|| crate::ach::file_account(&transfer.creditor_account).to_owned());
    let entry = NewEntry {
        memo: format!(
            "ACH to {}/{}: {}",
            transfer.creditor_bank, transfer.creditor_account, request.memo
        ),
        idempotency_key: Some(booked_key),
        postings: Ach.debtor_postings(&transfer),
    };
    let item = OutboxItem {
        originator: transfer.debtor_account.clone(),
        receiving_bank: transfer.creditor_bank.clone(),
        receiving_account: transfer.creditor_account.clone(),
        receiver_name,
        amount_minor: transfer.amount_minor,
    };
    match world.ledger.originate_ach(&entry, &item) {
        Ok(outcome) => {
            if matches!(outcome, PostOutcome::Created(_)) {
                let _ = shared.updates.send(world.snapshot(None, now_unix()));
            }
            Ok(outcome)
        }
        Err(error @ LedgerError::IdempotencyConflict(_)) => {
            Err(Refusal::Conflict(error.to_string()))
        }
        Err(error) => Err(Refusal::Invalid(error.to_string())),
    }
}

pub fn file_id(bank: &str, window: u32) -> String {
    format!("{bank}-w{window}")
}

pub async fn on_cutoff(shared: &Shared, window: u32) {
    let me = &shared.net.id;
    let id = file_id(me, window);
    let contents = {
        let mut world = shared.world.lock().await;
        match world.ledger.submit_ach(&id) {
            Ok(rows) if rows.is_empty() => Ok(String::new()),
            Ok(rows) => {
                let file = crate::ach::pending_file(me, &shared.operator_id, &rows, now_unix());
                file.to_nacha().map_err(|error| error.to_string())
            }
            Err(error) => Err(error.to_string()),
        }
    };
    let contents = match contents {
        Ok(contents) => contents,
        Err(error) => {
            tracing::error!(%error, window, "could not assemble our ACH file");
            return;
        }
    };
    tracing::info!(window, file_id = %id, bytes = contents.len(), "sending ACH file for cutoff");
    let msg = NetMsg::AchFile {
        file_id: id,
        contents,
    };
    if let Err(error) = shared.net.send(&shared.operator_id, &msg).await {
        tracing::error!(%error, window, "could not send our ACH file");
    }
}

fn credit_postings(ledger: &Ledger, account: &str, amount_minor: i64) -> Vec<Posting> {
    let customer = format!("customer:{account}");
    let to = if ledger.balance(&customer).is_ok() {
        customer
    } else {
        tracing::warn!(
            account,
            "ACH credit for an unknown account; held in suspense"
        );
        ACH_SUSPENSE.to_owned()
    };
    vec![
        Posting::new(ACH_INCOMING, amount_minor),
        Posting::new(to, -amount_minor),
    ]
}

pub async fn on_incoming_file(shared: &Shared, contents: &str) {
    let file = match nacha::parse(contents) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, "rejected a malformed ACH file from the operator");
            return;
        }
    };
    let me = &shared.net.id;
    let mut world = shared.world.lock().await;
    let mut booked = 0;
    for batch in &file.batches {
        for entry in batch.entries.iter().filter(|e| &e.receiving_bank == me) {
            if entry.kind != EntryKind::Credit {
                tracing::warn!(
                    trace = entry.trace_seq,
                    "ACH debits are not supported; skipped"
                );
                continue;
            }
            let postings = credit_postings(&world.ledger, &entry.account, entry.amount_minor);
            let posted = world.ledger.post(&NewEntry {
                memo: format!(
                    "ACH from {}/{}: {}",
                    batch.originating_bank, entry.individual_id, entry.individual_name
                ),
                // A trace number is unique per originating bank.
                idempotency_key: Some(format!(
                    "ach:in:{}:{}",
                    batch.originating_bank, entry.trace_seq
                )),
                postings,
            });
            match posted {
                Ok(PostOutcome::Created(_)) => booked += 1,
                Ok(PostOutcome::Replayed(_)) => {}
                Err(error) => {
                    tracing::error!(%error, trace = entry.trace_seq, "ACH credit not booked")
                }
            }
        }
    }
    if booked > 0 {
        tracing::info!(booked, "ACH credits booked");
        let _ = shared.updates.send(world.snapshot(None, now_unix()));
    }
}

pub async fn on_settlement(shared: &Shared, msg: &NetMsg) {
    let NetMsg::SettlementNotice {
        settlement_id,
        rail: RailKind::Ach,
        bank,
        amount_minor: net,
        reference: our_file,
    } = msg
    else {
        return;
    };
    if bank != &shared.net.id {
        tracing::warn!(%bank, "settlement notice for another bank; ignored");
        return;
    }
    let mut world = shared.world.lock().await;
    let sent: i64 = match world.ledger.ach_file_rows(our_file) {
        Ok(rows) => rows.iter().map(|r| r.item.amount_minor).sum(),
        Err(error) => {
            tracing::error!(%error, %our_file, "reading our ACH file for settlement");
            return;
        }
    };
    // net = received − sent, so what we received this window follows from what we sent.
    let position = Position {
        sent_minor: sent,
        received_minor: net + sent,
    };
    if position.received_minor < 0 {
        tracing::error!(net, sent, %settlement_id, "settlement disagrees with our file; not booked");
        return;
    }
    let postings = bank_ach_settlement(&position);
    if postings.len() < 2 {
        return;
    }
    let entry = NewEntry {
        memo: format!(
            "ACH settlement {settlement_id}: sent {}, received {}, net {}",
            bramble_ledger::format_minor(position.sent_minor),
            bramble_ledger::format_minor(position.received_minor),
            bramble_ledger::format_minor(net.to_owned())
        ),
        idempotency_key: Some(format!("ach:settle:{settlement_id}")),
        postings,
    };
    match world.ledger.post(&entry) {
        Ok(PostOutcome::Created(_)) => {
            tracing::info!(%settlement_id, net, "ACH settlement booked");
            let _ = shared.updates.send(world.snapshot(None, now_unix()));
        }
        Ok(PostOutcome::Replayed(_)) => {}
        Err(error) => tracing::error!(%error, %settlement_id, "ACH settlement not booked"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bramble_ledger::rails::{
        ACH_OUTGOING, OPENING_RESERVES, RESERVES_AT_OPERATOR, operator_account,
    };

    use super::*;
    use crate::node::operator::Operator;
    use crate::node::test_support::network;

    fn pay(to_bank: &str, to_account: &str, amount_minor: i64) -> PaymentRequest {
        PaymentRequest {
            from_account: "customer:you".into(),
            to_bank: to_bank.into(),
            to_account: to_account.into(),
            amount_minor,
            memo: "test".into(),
            receiver_name: None,
        }
    }

    async fn balance(bank: &Shared, account: &str) -> i64 {
        bank.world
            .lock()
            .await
            .ledger
            .balance(account)
            .expect("balance")
    }

    async fn operator_balance(op: &Operator, account: &str) -> i64 {
        op.ledger.lock().await.balance(account).expect("balance")
    }

    /// Banks book incoming files and settlement notices just after the operator
    /// reports; wait until every bank has emptied its clearing accounts.
    async fn settled(banks: &[Arc<Shared>]) {
        for _ in 0..60 {
            let mut clear = true;
            for bank in banks {
                clear &= balance(bank, ACH_OUTGOING).await == 0
                    && balance(bank, ACH_INCOMING).await == 0;
            }
            if clear {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("clearing accounts never emptied");
    }

    /// Six payments: a→b 10.00, a→c 5.00, a→b 2.50, b→a 7.00, b→c 3.00, c→a 20.00.
    async fn six_payments(banks: &[Arc<Shared>]) {
        let (a, b, c) = (&banks[0], &banks[1], &banks[2]);
        let payments = [
            (a, pay("pet-b", "customer:alex", 1_000)),
            (a, pay("pet-c", "customer:alex", 500)),
            (a, pay("pet-b", "customer:alex", 250)),
            (b, pay("pet-a", "customer:alex", 700)),
            (b, pay("pet-c", "customer:alex", 300)),
            (c, pay("pet-a", "customer:alex", 2_000)),
        ];
        for (i, (bank, request)) in payments.into_iter().enumerate() {
            let outcome = originate(bank, request, Some(format!("p{i}"))).await;
            assert!(
                matches!(outcome, Ok(PostOutcome::Created(_))),
                "{outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn ach_cutoff_nets_six_payments_into_one_settlement_and_every_ledger_reconciles() {
        let (banks, op) = network(3).await;
        six_payments(&banks).await;
        // Queued: customers debited, nothing has moved between banks.
        assert_eq!(
            balance(&banks[0], RESERVES_AT_OPERATOR).await,
            OPENING_RESERVES
        );

        let report = op.cutoff().await.expect("cutoff");
        assert!(report.settled);
        assert_eq!((report.window, report.payments), (1, 6));
        let nets: Vec<i64> = report.positions.iter().map(|p| p.net_minor).collect();
        assert_eq!(nets, [950, 250, -1_200], "net = received − sent, per bank");

        settled(&banks).await;
        let ids = ["pet-a", "pet-b", "pet-c"];
        for (bank, (id, net)) in banks.iter().zip(ids.iter().zip(nets)) {
            assert_eq!(
                balance(bank, RESERVES_AT_OPERATOR).await,
                OPENING_RESERVES + net,
                "{id}"
            );
            assert_eq!(
                balance(bank, RESERVES_AT_OPERATOR).await,
                -operator_balance(&op, &operator_account(id)).await,
                "{id} reconciles with the operator"
            );
            let total: i64 = bank
                .world
                .lock()
                .await
                .ledger
                .accounts()
                .expect("accounts")
                .iter()
                .map(|a| a.balance_minor)
                .sum();
            assert_eq!(total, 0, "{id}'s books balance");
        }
        // Payees were credited the gross amounts, not the net.
        assert_eq!(balance(&banks[0], "customer:alex").await, -2_700);
        assert_eq!(balance(&banks[1], "customer:alex").await, -1_250);
        assert_eq!(balance(&banks[2], "customer:alex").await, -800);
        assert_eq!(
            op.ledger
                .lock()
                .await
                .count_entries_with_key_prefix("ach:window:")
                .expect("count"),
            1,
            "one settlement entry for six payments"
        );
    }

    #[tokio::test]
    async fn ach_cutoff_with_nothing_queued_leaves_the_window_open() {
        let (_banks, op) = network(3).await;
        let report = op.cutoff().await.expect("cutoff");
        assert!(!report.settled);
        assert_eq!(op.cutoff().await.expect("again").window, 1, "same window");
    }

    #[tokio::test]
    async fn ach_payment_after_cutoff_waits_for_the_next_window() {
        let (banks, op) = network(3).await;
        originate(
            &banks[0],
            pay("pet-b", "customer:alex", 400),
            Some("w1".into()),
        )
        .await
        .expect("queue");
        op.cutoff().await.expect("window 1");
        settled(&banks).await;
        originate(
            &banks[0],
            pay("pet-b", "customer:alex", 600),
            Some("w2".into()),
        )
        .await
        .expect("queue");
        assert_eq!(
            banks[0]
                .world
                .lock()
                .await
                .ledger
                .pending_ach()
                .expect("pending")
                .len(),
            1
        );
        let report = op.cutoff().await.expect("window 2");
        assert_eq!((report.window, report.payments), (2, 1));
        settled(&banks).await;
        assert_eq!(balance(&banks[1], "customer:alex").await, -1_000);
    }

    #[tokio::test]
    async fn ach_credit_for_an_unknown_account_is_held_in_suspense() {
        let (banks, op) = network(3).await;
        originate(
            &banks[0],
            pay("pet-b", "customer:ghost", 300),
            Some("g".into()),
        )
        .await
        .expect("queued: the sender can't know the account doesn't exist");
        op.cutoff().await.expect("cutoff");
        settled(&banks).await;
        assert_eq!(
            balance(&banks[1], ACH_SUSPENSE).await,
            -300,
            "held, not lost"
        );
        assert_eq!(
            balance(&banks[1], RESERVES_AT_OPERATOR).await,
            OPENING_RESERVES + 300
        );
    }

    #[tokio::test]
    async fn ach_originate_refuses_overdrafts_and_strangers() {
        let (banks, _op) = network(2).await;
        let overdraft = originate(&banks[0], pay("pet-b", "customer:alex", 9_999_999), None).await;
        assert!(matches!(overdraft, Err(Refusal::Invalid(e)) if e.contains("insufficient funds")));
        let stranger = originate(&banks[0], pay("pet-z", "customer:alex", 100), None).await;
        assert!(matches!(stranger, Err(Refusal::Invalid(e)) if e.contains("not a bank")));
        let to_operator = originate(&banks[0], pay("operator", "customer:alex", 100), None).await;
        assert!(matches!(to_operator, Err(Refusal::Invalid(_))));
    }
}
