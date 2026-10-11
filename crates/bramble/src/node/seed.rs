//! Demo data for a brand-new pet-bank: one customer (you) with a month of activity.

use bramble_ledger::{AccountKind, Ledger, NewEntry, Posting};

const ACCOUNTS: &[(&str, AccountKind)] = &[
    ("reserves", AccountKind::Asset),
    ("customer:you", AccountKind::Liability),
    ("settlement:card-network", AccountKind::Liability),
    ("revenue:fees", AccountKind::Revenue),
];

/// (idempotency key, memo, debit account, credit account, amount in minor units)
const ENTRIES: &[(&str, &str, &str, &str, i64)] = &[
    (
        "seed-1",
        "Paycheck deposit",
        "reserves",
        "customer:you",
        10_000,
    ),
    (
        "seed-2",
        "Coffee (card)",
        "customer:you",
        "settlement:card-network",
        450,
    ),
    (
        "seed-3",
        "Rent transfer out",
        "customer:you",
        "reserves",
        2_200,
    ),
    (
        "seed-4",
        "Monthly account fee",
        "customer:you",
        "revenue:fees",
        200,
    ),
];

/// Idempotent: safe to run on every start.
pub fn seed(ledger: &mut Ledger) -> anyhow::Result<()> {
    for (id, kind) in ACCOUNTS {
        ledger.ensure_account(id, *kind)?;
    }
    for (key, memo, debit, credit, amount) in ENTRIES {
        ledger.post(&NewEntry {
            memo: (*memo).to_owned(),
            idempotency_key: Some((*key).to_owned()),
            postings: vec![
                Posting::new(*debit, *amount),
                Posting::new(*credit, -amount),
            ],
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_is_idempotent_and_gives_customer_a_7150_credit() {
        let mut ledger = Ledger::open_in_memory().expect("ledger");
        seed(&mut ledger).expect("seed");
        seed(&mut ledger).expect("reseed");
        assert_eq!(ledger.entry_count().expect("count"), 4);
        assert_eq!(ledger.balance("customer:you").expect("balance"), -7_150);
        let total: i64 = ledger
            .accounts()
            .expect("accounts")
            .iter()
            .map(|a| a.balance_minor)
            .sum();
        assert_eq!(total, 0, "the books always balance");
    }
}
