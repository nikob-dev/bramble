//! Real-time payments between banks, through the operator (decisions 011, 012).
//!
//! ```text
//! paying bank ──transfer──► operator ──transfer──► receiving bank
//!                              ◄──────accept───────
//!                           settles (final)
//! paying bank ◄──settled───   operator   ───settled──► receiving bank
//! books: customer↓ reserves↓                       books: reserves↑ customer↑
//! ```
//!
//! Every leg is booked under an idempotency key derived from the payment's `msg_id`,
//! so retrying a payment (same `Idempotency-Key`) never moves money twice, even when
//! the first attempt timed out after the operator had already settled it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bramble_core::net::NetMsg;
use bramble_ledger::rails::{Rail, Rtp, Transfer};
use bramble_ledger::{Ledger, NewEntry, PostOutcome};
use serde::Deserialize;
use tokio::sync::oneshot;

use super::net::Inbound;
use super::{Shared, now_unix};

/// How long a bank or the operator waits for the next hop to answer.
pub const HOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub accepted: bool,
    pub reason: Option<String>,
}

impl Status {
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self {
            accepted: false,
            reason: Some(reason.into()),
        }
    }
}

/// Requests awaiting an `RtpStatus` from the next hop, by `msg_id`.
#[derive(Default)]
pub struct Waiters(Mutex<HashMap<String, oneshot::Sender<Status>>>);

impl Waiters {
    pub fn register(&self, msg_id: &str) -> oneshot::Receiver<Status> {
        let (tx, rx) = oneshot::channel();
        self.0
            .lock()
            .expect("waiters lock")
            .insert(msg_id.to_owned(), tx);
        rx
    }

    /// Hands a status to whoever is waiting for it. False if nobody was.
    pub fn resolve(&self, msg_id: &str, status: Status) -> bool {
        let waiter = self.0.lock().expect("waiters lock").remove(msg_id);
        waiter.is_some_and(|tx| tx.send(status).is_ok())
    }

    pub async fn wait(&self, msg_id: &str, rx: oneshot::Receiver<Status>) -> Option<Status> {
        let status = tokio::time::timeout(HOP_TIMEOUT, rx).await.ok()?.ok();
        if status.is_none() {
            self.0.lock().expect("waiters lock").remove(msg_id);
        }
        status
    }
}

/// A bank's RTP state that lives outside the ledger.
#[derive(Default)]
pub struct RtpDesk {
    pub waiters: Waiters,
    /// Transfers we accepted as the receiving bank, booked once the operator settles.
    incoming: Mutex<HashMap<String, PendingIncoming>>,
}

pub fn transfer_of(msg: &NetMsg) -> Option<(String, Transfer, String)> {
    match msg {
        NetMsg::RtpCreditTransfer {
            msg_id,
            debtor_bank,
            debtor_account,
            creditor_bank,
            creditor_account,
            amount_minor,
            memo,
        } => Some((
            msg_id.clone(),
            Transfer {
                debtor_bank: debtor_bank.clone(),
                debtor_account: debtor_account.clone(),
                creditor_bank: creditor_bank.clone(),
                creditor_account: creditor_account.clone(),
                amount_minor: *amount_minor,
            },
            memo.clone(),
        )),
        _ => None,
    }
}

pub fn transfer_msg(msg_id: &str, t: &Transfer, memo: &str) -> NetMsg {
    NetMsg::RtpCreditTransfer {
        msg_id: msg_id.to_owned(),
        debtor_bank: t.debtor_bank.clone(),
        debtor_account: t.debtor_account.clone(),
        creditor_bank: t.creditor_bank.clone(),
        creditor_account: t.creditor_account.clone(),
        amount_minor: t.amount_minor,
        memo: memo.to_owned(),
    }
}

pub fn status_msg(msg_id: &str, status: &Status) -> NetMsg {
    NetMsg::RtpStatus {
        msg_id: msg_id.to_owned(),
        accepted: status.accepted,
        reason: status.reason.clone(),
    }
}

fn is_customer_account(ledger: &Ledger, account: &str) -> bool {
    account.starts_with("customer:") && ledger.balance(account).is_ok()
}

// ---- paying bank ----

/// What a bank checks before any payment leaves it, on any rail.
pub fn check_outgoing(ledger: &Ledger, transfer: &Transfer) -> Result<(), String> {
    transfer.validate().map_err(|error| error.to_string())?;
    if !is_customer_account(ledger, &transfer.debtor_account) {
        return Err(format!(
            "{:?} is not a customer account at this bank",
            transfer.debtor_account
        ));
    }
    // A customer's balance is a liability (negative = we owe them), so funds are -balance.
    // Checked before sending; concurrent sends from one account could still overdraw it.
    let available = -ledger.balance(&transfer.debtor_account).unwrap_or(0);
    if available < transfer.amount_minor {
        return Err(format!(
            "insufficient funds: {} available, {} requested",
            bramble_ledger::format_minor(available),
            bramble_ledger::format_minor(transfer.amount_minor)
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaymentRequest {
    pub from_account: String,
    pub to_bank: String,
    pub to_account: String,
    pub amount_minor: i64,
    #[serde(default)]
    pub memo: String,
    /// ACH files carry the receiver's name; defaults to the account name.
    #[serde(default)]
    pub receiver_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Settled {
        msg_id: String,
        entry_id: i64,
    },
    /// Same idempotency key as a payment that already settled: nothing new moved.
    Replayed {
        msg_id: String,
        entry_id: i64,
    },
    /// We refused it before it left the bank.
    Invalid(String),
    /// The operator or the receiving bank refused it.
    Rejected {
        msg_id: String,
        reason: String,
    },
    /// No answer in time. The payment may or may not have settled: retry with the
    /// same idempotency key to find out without paying twice.
    TimedOut {
        msg_id: String,
    },
}

/// Sends a payment from one of our customers to a customer at another bank.
pub async fn send_payment(
    shared: &Shared,
    request: PaymentRequest,
    idempotency_key: Option<String>,
) -> SendOutcome {
    let net = &shared.net;
    let key = idempotency_key.unwrap_or_else(bramble_net::new_nonce);
    let msg_id = format!("{}-{key}", net.id);
    let booked_key = format!("rtp:{msg_id}:sent");
    let transfer = Transfer {
        debtor_bank: net.id.clone(),
        debtor_account: request.from_account.clone(),
        creditor_bank: request.to_bank.clone(),
        creditor_account: request.to_account.clone(),
        amount_minor: request.amount_minor,
    };

    {
        let world = shared.world.lock().await;
        match world.ledger.entry_id_by_idempotency_key(&booked_key) {
            Ok(Some(entry_id)) => return SendOutcome::Replayed { msg_id, entry_id },
            Ok(None) => {}
            Err(error) => return SendOutcome::Invalid(format!("ledger error: {error}")),
        }
        if let Err(error) = check_outgoing(&world.ledger, &transfer) {
            return SendOutcome::Invalid(error);
        }
    }

    let rx = shared.rtp.waiters.register(&msg_id);
    let msg = transfer_msg(&msg_id, &transfer, &request.memo);
    if let Err(error) = net.send(&shared.operator_id, &msg).await {
        tracing::warn!(%error, %msg_id, "could not reach the operator");
        return SendOutcome::TimedOut { msg_id };
    }
    let Some(status) = shared.rtp.waiters.wait(&msg_id, rx).await else {
        return SendOutcome::TimedOut { msg_id };
    };
    if !status.accepted {
        return SendOutcome::Rejected {
            msg_id,
            reason: status.reason.unwrap_or_else(|| "no reason given".into()),
        };
    }

    // Settled at the operator: final. Book our side.
    let mut world = shared.world.lock().await;
    let entry = NewEntry {
        memo: format!(
            "RTP to {}/{}: {}",
            transfer.creditor_bank, transfer.creditor_account, request.memo
        ),
        idempotency_key: Some(booked_key),
        postings: Rtp.debtor_postings(&transfer),
    };
    match world.ledger.post(&entry) {
        Ok(PostOutcome::Created(entry_id)) => {
            let _ = shared.updates.send(world.snapshot(None, now_unix()));
            SendOutcome::Settled { msg_id, entry_id }
        }
        Ok(PostOutcome::Replayed(entry_id)) => SendOutcome::Replayed { msg_id, entry_id },
        Err(error) => {
            // The money already moved at the operator; our books are now out of line
            // until this is fixed. Reconciliation will show the break.
            tracing::error!(%error, %msg_id, "settled RTP could not be booked");
            SendOutcome::Invalid(format!("settled but not booked: {error}"))
        }
    }
}

// ---- receiving bank (and status routing for the paying bank) ----

/// Handles RTP messages arriving at a bank. Runs for the node's lifetime.
pub async fn bank_dispatch(shared: Arc<Shared>) {
    let mut inbox = shared.net.inbox.subscribe();
    loop {
        match inbox.recv().await {
            Ok(inbound) => handle_bank_message(&shared, inbound).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::error!(skipped, "bank inbox lagged; messages were dropped");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn handle_bank_message(shared: &Shared, Inbound { from, msg }: Inbound) {
    if from != shared.operator_id {
        tracing::warn!(%from, kind = msg.kind(), "RTP messages only come from the operator");
        return;
    }
    match msg {
        NetMsg::RtpCreditTransfer { .. } => {
            let Some((msg_id, transfer, memo)) = transfer_of(&msg) else {
                return;
            };
            let status = accept_incoming(shared, &msg_id, transfer, memo).await;
            if let Err(error) = shared
                .net
                .send(&shared.operator_id, &status_msg(&msg_id, &status))
                .await
            {
                tracing::error!(%error, %msg_id, "could not answer the operator");
            }
        }
        NetMsg::RtpStatus {
            msg_id,
            accepted,
            reason,
        } => {
            let status = Status { accepted, reason };
            if shared.rtp.waiters.resolve(&msg_id, status.clone()) {
                return; // We were the paying bank.
            }
            let pending = shared
                .rtp
                .incoming
                .lock()
                .expect("incoming lock")
                .remove(&msg_id);
            if let (Some((transfer, memo)), true) = (pending.map(|p| p.into_parts()), accepted) {
                book_incoming(shared, &msg_id, &transfer, &memo).await;
            }
        }
        _ => {}
    }
}

async fn accept_incoming(
    shared: &Shared,
    msg_id: &str,
    transfer: Transfer,
    memo: String,
) -> Status {
    let world = shared.world.lock().await;
    if transfer.creditor_bank != shared.net.id {
        return Status::rejected(format!(
            "this is {}, not {}",
            shared.net.id, transfer.creditor_bank
        ));
    }
    if let Err(error) = transfer.validate() {
        return Status::rejected(error.to_string());
    }
    if !is_customer_account(&world.ledger, &transfer.creditor_account) {
        return Status::rejected(format!(
            "no customer account {:?} at {}",
            transfer.creditor_account, shared.net.id
        ));
    }
    shared
        .rtp
        .incoming
        .lock()
        .expect("incoming lock")
        .insert(msg_id.to_owned(), PendingIncoming { transfer, memo });
    Status {
        accepted: true,
        reason: None,
    }
}

async fn book_incoming(shared: &Shared, msg_id: &str, transfer: &Transfer, memo: &str) {
    let mut world = shared.world.lock().await;
    let entry = NewEntry {
        memo: format!(
            "RTP from {}/{}: {memo}",
            transfer.debtor_bank, transfer.debtor_account
        ),
        idempotency_key: Some(format!("rtp:{msg_id}:received")),
        postings: Rtp.creditor_postings(transfer),
    };
    match world.ledger.post(&entry) {
        Ok(PostOutcome::Created(_)) => {
            let _ = shared.updates.send(world.snapshot(None, now_unix()));
        }
        Ok(PostOutcome::Replayed(_)) => {}
        Err(error) => {
            tracing::error!(%error, %msg_id, "settled incoming RTP could not be booked");
        }
    }
}

struct PendingIncoming {
    transfer: Transfer,
    memo: String,
}

impl PendingIncoming {
    fn into_parts(self) -> (Transfer, String) {
        (self.transfer, self.memo)
    }
}

#[cfg(test)]
mod tests {
    use bramble_ledger::rails::{
        OPENING_RESERVES, OPERATOR_ASSETS, RESERVES_AT_OPERATOR, open_operator, operator_account,
    };
    use bramble_ledger::{Ledger, Posting};
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::node::net::{Net, serve_local};
    use crate::node::operator::{self, Operator};
    use crate::node::store::PetStore;
    use crate::node::test_support::{shared, world};

    struct Network {
        a: Arc<Shared>,
        b: Arc<Shared>,
        operator: Arc<Operator>,
    }

    /// pet-a, pet-b and the operator, each serving its peer endpoint on a local port,
    /// all discovered by each other, with their message handlers running.
    async fn network() -> Network {
        let a = shared("pet-a", 1, world());
        let b = shared("pet-b", 2, world());
        let mut ledger = Ledger::open_in_memory().expect("ledger");
        open_operator(&mut ledger, &["pet-a", "pet-b"]).expect("open");
        let op_net = Arc::new(
            Net::new(
                "operator",
                SigningKey::from_bytes(&[3; 32]),
                PetStore::open_in_memory().expect("store"),
            )
            .expect("net"),
        );
        let operator = Operator::new(ledger, op_net.clone());

        let nets = [a.net.clone(), b.net.clone(), op_net];
        let mut addrs = Vec::new();
        for net in &nets {
            addrs.push(serve_local(net.clone()).await.to_string());
        }
        for (i, net) in nets.iter().enumerate() {
            for (j, addr) in addrs.iter().enumerate() {
                if i != j {
                    net.discover(addr).await.expect("discover");
                }
            }
        }
        tokio::spawn(bank_dispatch(a.clone()));
        tokio::spawn(bank_dispatch(b.clone()));
        tokio::spawn(operator::dispatch(operator.clone()));
        Network { a, b, operator }
    }

    fn pay(amount_minor: i64, to_account: &str) -> PaymentRequest {
        PaymentRequest {
            from_account: "customer:you".into(),
            to_bank: "pet-b".into(),
            to_account: to_account.into(),
            amount_minor,
            memo: "Dinner".into(),
            receiver_name: None,
        }
    }

    async fn balance(shared: &Shared, account: &str) -> i64 {
        shared
            .world
            .lock()
            .await
            .ledger
            .balance(account)
            .expect("balance")
    }

    async fn operator_balance(operator: &Operator, account: &str) -> i64 {
        operator
            .ledger
            .lock()
            .await
            .balance(account)
            .expect("balance")
    }

    /// The receiving bank books on the operator's confirmation, which can land just
    /// after the paying bank's; give it a moment.
    async fn eventually_balance(shared: &Shared, account: &str, expected: i64) {
        for _ in 0..40 {
            if balance(shared, account).await == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(balance(shared, account).await, expected, "{account}");
    }

    async fn assert_reserves_mirror(net: &Network) {
        for (bank, shared) in [("pet-a", &net.a), ("pet-b", &net.b)] {
            assert_eq!(
                balance(shared, RESERVES_AT_OPERATOR).await,
                -operator_balance(&net.operator, &operator_account(bank)).await,
                "{bank} reconciles with the operator"
            );
        }
    }

    #[tokio::test]
    async fn rtp_payment_settles_across_three_ledgers() {
        let net = network().await;
        let outcome =
            send_payment(&net.a, pay(1_200, "customer:alex"), Some("dinner-1".into())).await;
        assert!(
            matches!(outcome, SendOutcome::Settled { .. }),
            "{outcome:?}"
        );

        assert_eq!(balance(&net.a, "customer:you").await, -7_150 + 1_200);
        assert_eq!(
            balance(&net.a, RESERVES_AT_OPERATOR).await,
            OPENING_RESERVES - 1_200
        );
        eventually_balance(&net.b, "customer:alex", -1_200).await;
        assert_eq!(
            balance(&net.b, RESERVES_AT_OPERATOR).await,
            OPENING_RESERVES + 1_200
        );
        assert_reserves_mirror(&net).await;
    }

    #[tokio::test]
    async fn rtp_retry_with_the_same_key_moves_money_once() {
        let net = network().await;
        let first = send_payment(&net.a, pay(1_200, "customer:alex"), Some("rent".into())).await;
        let SendOutcome::Settled { entry_id, .. } = first else {
            panic!("{first:?}");
        };
        let again = send_payment(&net.a, pay(1_200, "customer:alex"), Some("rent".into())).await;
        assert!(
            matches!(again, SendOutcome::Replayed { entry_id: id, .. } if id == entry_id),
            "{again:?}"
        );
        eventually_balance(&net.b, "customer:alex", -1_200).await;
        let settlements = net
            .operator
            .ledger
            .lock()
            .await
            .entry_count()
            .expect("count");
        assert_eq!(settlements, 2 + 1, "two openings and one settlement");
        assert_reserves_mirror(&net).await;
    }

    #[tokio::test]
    async fn rtp_operator_replays_a_settlement_the_paying_bank_never_heard_about() {
        // The operator settled, but the paying bank's books don't show it (as if its
        // wait had timed out). A retry with the same key must book it, not pay again.
        let net = network().await;
        let first = send_payment(&net.a, pay(500, "customer:alex"), Some("lost".into())).await;
        assert!(matches!(first, SendOutcome::Settled { .. }));
        let mut a = net.a.world.lock().await;
        let mut fresh = Ledger::open_in_memory().expect("ledger");
        crate::node::seed::seed(&mut fresh).expect("seed");
        a.ledger = fresh;
        drop(a);

        let retry = send_payment(&net.a, pay(500, "customer:alex"), Some("lost".into())).await;
        assert!(matches!(retry, SendOutcome::Settled { .. }), "{retry:?}");
        assert_eq!(
            balance(&net.a, RESERVES_AT_OPERATOR).await,
            OPENING_RESERVES - 500
        );
        assert_eq!(
            net.operator
                .ledger
                .lock()
                .await
                .entry_count()
                .expect("count"),
            3,
            "settled once"
        );
    }

    #[tokio::test]
    async fn rtp_refuses_overdrafts_before_anything_leaves_the_bank() {
        let net = network().await;
        let outcome = send_payment(&net.a, pay(10_000_000, "customer:alex"), None).await;
        assert!(
            matches!(&outcome, SendOutcome::Invalid(e) if e.contains("insufficient funds")),
            "{outcome:?}"
        );
        assert_eq!(
            net.operator
                .ledger
                .lock()
                .await
                .entry_count()
                .expect("count"),
            2
        );
    }

    #[tokio::test]
    async fn rtp_rejects_an_unknown_payee_and_moves_nothing() {
        let net = network().await;
        let outcome = send_payment(&net.a, pay(1_200, "customer:nobody"), None).await;
        assert!(
            matches!(&outcome, SendOutcome::Rejected { reason, .. } if reason.contains("no customer account")),
            "{outcome:?}"
        );
        assert_eq!(balance(&net.a, "customer:you").await, -7_150);
        assert_reserves_mirror(&net).await;
    }

    #[tokio::test]
    async fn rtp_rejects_payments_beyond_the_banks_reserves() {
        let net = network().await;
        // Leave pet-a with $10.00 at the operator.
        net.operator
            .ledger
            .lock()
            .await
            .post(&NewEntry {
                memo: "drain for test".into(),
                idempotency_key: None,
                postings: vec![
                    Posting::new(operator_account("pet-a"), OPENING_RESERVES - 1_000),
                    Posting::new(OPERATOR_ASSETS, -(OPENING_RESERVES - 1_000)),
                ],
            })
            .expect("drain");
        let outcome = send_payment(&net.a, pay(1_200, "customer:alex"), None).await;
        assert!(
            matches!(&outcome, SendOutcome::Rejected { reason, .. } if reason.contains("insufficient reserves")),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn rtp_operator_refuses_a_bank_paying_from_another_banks_customer() {
        let net = network().await;
        let forged = Transfer {
            debtor_bank: "pet-a".into(),
            debtor_account: "customer:you".into(),
            creditor_bank: "pet-b".into(),
            creditor_account: "customer:alex".into(),
            amount_minor: 100,
        };
        let rx = net.b.rtp.waiters.register("forged-1");
        net.b
            .net
            .send("operator", &transfer_msg("forged-1", &forged, "not mine"))
            .await
            .expect("delivered");
        let status = net
            .b
            .rtp
            .waiters
            .wait("forged-1", rx)
            .await
            .expect("answer");
        assert!(!status.accepted);
        assert!(
            status
                .reason
                .expect("reason")
                .contains("on behalf of pet-a")
        );
        assert_eq!(
            net.operator
                .ledger
                .lock()
                .await
                .entry_count()
                .expect("count"),
            2
        );
    }
}
