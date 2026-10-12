//! How money moves between banks, and where each leg is booked (decision 012).
//!
//! Banks never pay each other directly. Each holds a balance at the operator (the
//! central bank / clearing house): `reserves:operator` in the bank's books, mirrored
//! by `bank:<id>` in the operator's. An interbank payment touches up to three sets of
//! books: the paying bank, the operator, and the receiving bank.
//!
//! - **RTP** settles gross and at once: all three books change per payment.
//! - **ACH** settles net at cutoff: each bank books the payment against a clearing
//!   account now, and the operator only moves reserves when the batch settles.
//!
//! Invariant that cross-bank reconciliation checks: for every bank,
//! `reserves:operator` at the bank == −`bank:<id>` at the operator.

use std::collections::BTreeMap;

use bramble_core::net::Rail as RailKind;

use crate::nacha::{AchFile, EntryKind};
use crate::{AccountKind, Ledger, NewEntry, Posting, Result};

/// The bank's balance at the operator (an asset: the operator owes it to the bank).
pub const RESERVES_AT_OPERATOR: &str = "reserves:operator";
/// Owners' capital that funded the bank's opening reserves.
pub const OWNERS_CAPITAL: &str = "equity:capital";
/// ACH payments sent but not yet settled (a liability: we owe them to the network).
pub const ACH_OUTGOING: &str = "clearing:ach-out";
/// ACH payments received but not yet settled (an asset: the network owes them to us).
pub const ACH_INCOMING: &str = "clearing:ach-in";
/// Money that arrived for an account we don't have: held, not lost, until someone
/// sorts it out (real banks would return it to the sender).
pub const ACH_SUSPENSE: &str = "suspense:ach-unapplied";
/// What the operator holds against the balances it owes banks.
pub const OPERATOR_ASSETS: &str = "assets:securities";
/// Each bank starts with $10,000.00 at the operator.
pub const OPENING_RESERVES: i64 = 1_000_000;

/// The operator's account for one bank (a liability: it owes the bank this balance).
pub fn operator_account(bank: &str) -> String {
    format!("bank:{bank}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub debtor_bank: String,
    pub debtor_account: String,
    pub creditor_bank: String,
    pub creditor_account: String,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    #[error("amount must be positive, got {0}")]
    NonPositiveAmount(i64),
    #[error("debtor and creditor are both {0:?}: that's a book transfer, not an interbank payment")]
    SameBank(String),
}

impl Transfer {
    /// A negative amount would silently reverse the payment's direction, so refuse it.
    pub fn validate(&self) -> std::result::Result<(), TransferError> {
        if self.amount_minor <= 0 {
            return Err(TransferError::NonPositiveAmount(self.amount_minor));
        }
        if self.debtor_bank == self.creditor_bank {
            return Err(TransferError::SameBank(self.debtor_bank.clone()));
        }
        Ok(())
    }
}

/// Where a payment on this rail is booked, in each of the three sets of books.
pub trait Rail {
    fn kind(&self) -> RailKind;
    /// The paying bank, when it sends the payment.
    fn debtor_postings(&self, transfer: &Transfer) -> Vec<Posting>;
    /// The operator, per payment. Empty for rails that settle in batches.
    fn operator_postings(&self, transfer: &Transfer) -> Vec<Posting>;
    /// The receiving bank, when the payment arrives.
    fn creditor_postings(&self, transfer: &Transfer) -> Vec<Posting>;
}

/// Real-time payments: gross settlement, final once the operator moves reserves.
pub struct Rtp;

impl Rail for Rtp {
    fn kind(&self) -> RailKind {
        RailKind::Rtp
    }

    fn debtor_postings(&self, t: &Transfer) -> Vec<Posting> {
        vec![
            // We owe the customer less (debit a liability)...
            Posting::new(&t.debtor_account, t.amount_minor),
            // ...because our money at the operator went out (credit an asset).
            Posting::new(RESERVES_AT_OPERATOR, -t.amount_minor),
        ]
    }

    fn operator_postings(&self, t: &Transfer) -> Vec<Posting> {
        vec![
            Posting::new(operator_account(&t.debtor_bank), t.amount_minor),
            Posting::new(operator_account(&t.creditor_bank), -t.amount_minor),
        ]
    }

    fn creditor_postings(&self, t: &Transfer) -> Vec<Posting> {
        vec![
            Posting::new(RESERVES_AT_OPERATOR, t.amount_minor),
            Posting::new(&t.creditor_account, -t.amount_minor),
        ]
    }
}

/// Automated Clearing House: payments are booked to clearing accounts now and settled
/// net, per bank, at cutoff (see the ACH tasks for netting).
pub struct Ach;

impl Rail for Ach {
    fn kind(&self) -> RailKind {
        RailKind::Ach
    }

    fn debtor_postings(&self, t: &Transfer) -> Vec<Posting> {
        vec![
            Posting::new(&t.debtor_account, t.amount_minor),
            Posting::new(ACH_OUTGOING, -t.amount_minor),
        ]
    }

    fn operator_postings(&self, _: &Transfer) -> Vec<Posting> {
        Vec::new()
    }

    fn creditor_postings(&self, t: &Transfer) -> Vec<Posting> {
        vec![
            Posting::new(ACH_INCOMING, t.amount_minor),
            Posting::new(&t.creditor_account, -t.amount_minor),
        ]
    }
}

/// Gives a bank its interbank accounts and opening reserves. Idempotent.
pub fn open_bank(ledger: &mut Ledger) -> Result<()> {
    ledger.ensure_account(RESERVES_AT_OPERATOR, AccountKind::Asset)?;
    ledger.ensure_account(OWNERS_CAPITAL, AccountKind::Equity)?;
    ledger.ensure_account(ACH_OUTGOING, AccountKind::Liability)?;
    ledger.ensure_account(ACH_INCOMING, AccountKind::Asset)?;
    ledger.ensure_account(ACH_SUSPENSE, AccountKind::Liability)?;
    ledger.post(&NewEntry {
        memo: "Opening reserves at the operator, funded by owners' capital".into(),
        idempotency_key: Some("opening:reserves".into()),
        postings: vec![
            Posting::new(RESERVES_AT_OPERATOR, OPENING_RESERVES),
            Posting::new(OWNERS_CAPITAL, -OPENING_RESERVES),
        ],
    })?;
    Ok(())
}

/// Gives the operator an account per bank, each holding that bank's opening reserves.
/// Idempotent, and safe to call again when a bank joins.
pub fn open_operator(ledger: &mut Ledger, banks: &[&str]) -> Result<()> {
    ledger.ensure_account(OPERATOR_ASSETS, AccountKind::Asset)?;
    for bank in banks {
        let account = operator_account(bank);
        ledger.ensure_account(&account, AccountKind::Liability)?;
        ledger.post(&NewEntry {
            memo: format!("Opening reserves held for {bank}"),
            idempotency_key: Some(format!("opening:{account}")),
            postings: vec![
                Posting::new(OPERATOR_ASSETS, OPENING_RESERVES),
                Posting::new(&account, -OPENING_RESERVES),
            ],
        })?;
    }
    Ok(())
}

/// One bank's gross ACH flows in a settlement window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub sent_minor: i64,
    pub received_minor: i64,
}

impl Position {
    /// Positive: the bank is owed money at settlement. Negative: it pays.
    pub fn net_minor(&self) -> i64 {
        self.received_minor - self.sent_minor
    }
}

/// Gross flows per bank across a window's files. A credit entry moves money from the
/// originating bank to the receiving bank; a debit entry pulls it the other way.
pub fn ach_positions(files: &[AchFile]) -> BTreeMap<String, Position> {
    let mut positions: BTreeMap<String, Position> = BTreeMap::new();
    for batch in files.iter().flat_map(|f| &f.batches) {
        for entry in &batch.entries {
            let (payer, payee) = match entry.kind {
                EntryKind::Credit => (&batch.originating_bank, &entry.receiving_bank),
                EntryKind::Debit => (&entry.receiving_bank, &batch.originating_bank),
            };
            positions.entry(payer.clone()).or_default().sent_minor += entry.amount_minor;
            positions.entry(payee.clone()).or_default().received_minor += entry.amount_minor;
        }
    }
    positions
}

/// The operator's single settlement entry for a window: each bank's account moves by
/// its *net* position, however many payments made it up. Sums to zero, because every
/// cent one bank sent, another received.
pub fn operator_ach_settlement(positions: &BTreeMap<String, Position>) -> Vec<Posting> {
    positions
        .iter()
        .filter(|(_, p)| p.net_minor() != 0)
        // Owing a bank more is a credit to that liability (negative).
        .map(|(bank, p)| Posting::new(operator_account(bank), -p.net_minor()))
        .collect()
}

/// A bank's side of the same settlement: reserves move by the net, and both clearing
/// accounts are emptied of this window's gross amounts.
pub fn bank_ach_settlement(position: &Position) -> Vec<Posting> {
    [
        (RESERVES_AT_OPERATOR, position.net_minor()),
        (ACH_OUTGOING, position.sent_minor),
        (ACH_INCOMING, -position.received_minor),
    ]
    .into_iter()
    .filter(|(_, amount)| *amount != 0)
    .map(|(account, amount)| Posting::new(account, amount))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Network {
        a: Ledger,
        b: Ledger,
        operator: Ledger,
    }

    fn network() -> Network {
        let bank = || {
            let mut ledger = Ledger::open_in_memory().expect("ledger");
            ledger
                .ensure_account("customer:you", AccountKind::Liability)
                .expect("customer");
            ledger
                .ensure_account("customer:sam", AccountKind::Liability)
                .expect("customer");
            open_bank(&mut ledger).expect("open bank");
            ledger
        };
        let (a, b) = (bank(), bank());
        let mut operator = Ledger::open_in_memory().expect("ledger");
        open_operator(&mut operator, &["pet-a", "pet-b"]).expect("open operator");
        Network { a, b, operator }
    }

    fn transfer(amount_minor: i64) -> Transfer {
        Transfer {
            debtor_bank: "pet-a".into(),
            debtor_account: "customer:you".into(),
            creditor_bank: "pet-b".into(),
            creditor_account: "customer:sam".into(),
            amount_minor,
        }
    }

    fn book(ledger: &mut Ledger, memo: &str, postings: Vec<Posting>) {
        if postings.is_empty() {
            return;
        }
        ledger
            .post(&NewEntry {
                memo: memo.into(),
                idempotency_key: None,
                postings,
            })
            .expect("post");
    }

    fn pay(net: &mut Network, rail: &dyn Rail, t: &Transfer) {
        book(&mut net.a, "sent", rail.debtor_postings(t));
        book(&mut net.operator, "settled", rail.operator_postings(t));
        book(&mut net.b, "received", rail.creditor_postings(t));
    }

    fn total(ledger: &Ledger) -> i64 {
        ledger
            .accounts()
            .expect("accounts")
            .iter()
            .map(|a| a.balance_minor)
            .sum()
    }

    fn balance(ledger: &Ledger, account: &str) -> i64 {
        ledger.balance(account).expect("balance")
    }

    fn assert_mirrored(net: &Network) {
        for (bank, ledger) in [("pet-a", &net.a), ("pet-b", &net.b)] {
            assert_eq!(
                balance(ledger, RESERVES_AT_OPERATOR),
                -balance(&net.operator, &operator_account(bank)),
                "{bank}'s reserves match the operator's books"
            );
        }
    }

    #[test]
    fn rails_opening_is_idempotent_and_mirrored() {
        let mut net = network();
        open_bank(&mut net.a).expect("again");
        open_operator(&mut net.operator, &["pet-a", "pet-b"]).expect("again");
        assert_eq!(balance(&net.a, RESERVES_AT_OPERATOR), OPENING_RESERVES);
        assert_eq!(net.operator.entry_count().expect("count"), 2);
        assert_mirrored(&net);
    }

    #[test]
    fn rails_rtp_moves_reserves_at_once_and_every_ledger_stays_zero_sum() {
        let mut net = network();
        pay(&mut net, &Rtp, &transfer(2_500));

        assert_eq!(
            balance(&net.a, "customer:you"),
            2_500,
            "we owe the payer less"
        );
        assert_eq!(
            balance(&net.b, "customer:sam"),
            -2_500,
            "they owe the payee more"
        );
        assert_eq!(
            balance(&net.a, RESERVES_AT_OPERATOR),
            OPENING_RESERVES - 2_500
        );
        assert_eq!(
            balance(&net.b, RESERVES_AT_OPERATOR),
            OPENING_RESERVES + 2_500
        );
        assert_mirrored(&net);
        for ledger in [&net.a, &net.b, &net.operator] {
            assert_eq!(total(ledger), 0);
        }
    }

    #[test]
    fn rails_ach_books_to_clearing_and_leaves_reserves_until_cutoff() {
        let mut net = network();
        pay(&mut net, &Ach, &transfer(1_300));

        assert_eq!(balance(&net.a, ACH_OUTGOING), -1_300, "owed to the network");
        assert_eq!(balance(&net.b, ACH_INCOMING), 1_300, "owed by the network");
        assert_eq!(balance(&net.a, RESERVES_AT_OPERATOR), OPENING_RESERVES);
        assert_eq!(
            net.operator.entry_count().expect("count"),
            2,
            "operator untouched"
        );
        assert_mirrored(&net);
        for ledger in [&net.a, &net.b, &net.operator] {
            assert_eq!(total(ledger), 0);
        }
    }

    #[test]
    fn rails_every_rail_books_zero_sum_legs() {
        let t = transfer(999);
        for rail in [&Rtp as &dyn Rail, &Ach] {
            for legs in [
                rail.debtor_postings(&t),
                rail.operator_postings(&t),
                rail.creditor_postings(&t),
            ] {
                let sum: i64 = legs.iter().map(|p| p.amount_minor).sum();
                assert_eq!(sum, 0, "{:?}: {legs:?}", rail.kind());
            }
        }
    }

    #[test]
    fn rails_transfer_validate_rejects_nonsense() {
        assert_eq!(
            transfer(0).validate(),
            Err(TransferError::NonPositiveAmount(0))
        );
        assert_eq!(
            transfer(-5).validate(),
            Err(TransferError::NonPositiveAmount(-5))
        );
        let same = Transfer {
            creditor_bank: "pet-a".into(),
            ..transfer(10)
        };
        assert_eq!(
            same.validate(),
            Err(TransferError::SameBank("pet-a".into()))
        );
        assert_eq!(transfer(10).validate(), Ok(()));
    }

    fn ach_file(origin: &str, payments: &[(&str, i64)]) -> AchFile {
        use crate::nacha::{Batch, Entry};
        AchFile {
            origin_bank: origin.into(),
            destination_bank: "operator".into(),
            created_yymmdd: "261012".into(),
            created_hhmm: "1200".into(),
            file_id_modifier: 'A',
            batches: vec![Batch {
                originating_bank: origin.into(),
                company_name: "test".into(),
                company_id: "TEST000001".into(),
                entry_description: "PAYMENT".into(),
                effective_yymmdd: "261013".into(),
                batch_number: 1,
                entries: payments
                    .iter()
                    .enumerate()
                    .map(|(i, (to, amount))| Entry {
                        kind: EntryKind::Credit,
                        receiving_bank: (*to).into(),
                        account: "alex".into(),
                        amount_minor: *amount,
                        individual_id: "you".into(),
                        individual_name: "Alex".into(),
                        trace_seq: i as u32 + 1,
                    })
                    .collect(),
            }],
        }
    }

    /// Six payments in three files.
    fn window() -> Vec<AchFile> {
        vec![
            ach_file("pet-a", &[("pet-b", 1_000), ("pet-c", 500), ("pet-b", 250)]),
            ach_file("pet-b", &[("pet-a", 700), ("pet-c", 300)]),
            ach_file("pet-c", &[("pet-a", 2_000)]),
        ]
    }

    #[test]
    fn rails_ach_net_position_equals_the_sum_of_gross_flows_for_each_bank() {
        let files = window();
        let positions = ach_positions(&files);
        for bank in ["pet-a", "pet-b", "pet-c"] {
            let sent: i64 = files
                .iter()
                .filter(|f| f.origin_bank == bank)
                .flat_map(|f| f.batches.iter().flat_map(|b| &b.entries))
                .map(|e| e.amount_minor)
                .sum();
            let received: i64 = files
                .iter()
                .flat_map(|f| f.batches.iter().flat_map(|b| &b.entries))
                .filter(|e| e.receiving_bank == bank)
                .map(|e| e.amount_minor)
                .sum();
            let p = positions[bank];
            assert_eq!((p.sent_minor, p.received_minor), (sent, received), "{bank}");
            assert_eq!(p.net_minor(), received - sent, "{bank}");
        }
        // pet-a: sent 1750, received 2700 → +950; pet-b: sent 1000, received 1250 → +250;
        // pet-c: sent 2000, received 800 → −1200.
        let nets: Vec<i64> = positions.values().map(Position::net_minor).collect();
        assert_eq!(nets, [950, 250, -1_200]);
    }

    #[test]
    fn rails_ach_settlement_is_one_zero_sum_entry_and_clears_every_bank() {
        let mut net = network();
        let mut c = Ledger::open_in_memory().expect("ledger");
        open_bank(&mut c).expect("open");
        open_operator(&mut net.operator, &["pet-c"]).expect("join");
        let positions = ach_positions(&window());

        // Origination and receipt, as the rail books them.
        let mut banks = [
            ("pet-a", &mut net.a),
            ("pet-b", &mut net.b),
            ("pet-c", &mut c),
        ];
        for (bank, ledger) in banks.iter_mut() {
            for customer in ["customer:you", "customer:alex"] {
                ledger
                    .ensure_account(customer, AccountKind::Liability)
                    .expect("customer");
            }
            let p = positions[*bank];
            book(
                ledger,
                "sent",
                vec![
                    Posting::new("customer:you", p.sent_minor),
                    Posting::new(ACH_OUTGOING, -p.sent_minor),
                ],
            );
            book(
                ledger,
                "received",
                vec![
                    Posting::new(ACH_INCOMING, p.received_minor),
                    Posting::new("customer:alex", -p.received_minor),
                ],
            );
        }

        let operator_legs = operator_ach_settlement(&positions);
        assert_eq!(operator_legs.iter().map(|p| p.amount_minor).sum::<i64>(), 0);
        book(&mut net.operator, "ACH window 1", operator_legs);
        for (bank, ledger) in banks.iter_mut() {
            book(
                ledger,
                "ACH settlement",
                bank_ach_settlement(&positions[*bank]),
            );
            assert_eq!(balance(ledger, ACH_OUTGOING), 0, "{bank}: outgoing cleared");
            assert_eq!(balance(ledger, ACH_INCOMING), 0, "{bank}: incoming cleared");
            assert_eq!(
                balance(ledger, RESERVES_AT_OPERATOR),
                -balance(&net.operator, &operator_account(bank)),
                "{bank} reconciles with the operator"
            );
            assert_eq!(total(ledger), 0);
        }
        assert_eq!(
            balance(&net.operator, &operator_account("pet-c")),
            -(OPENING_RESERVES - 1_200)
        );
    }
}
