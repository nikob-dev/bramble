//! Turning a bank's queued ACH payments (its `ach_outbox`) into the Nacha file it
//! will send at the next cutoff. Shared by the node's API and the `bramble ach` CLI.

use bramble_ledger::OutboxRow;
use bramble_ledger::nacha::{AchFile, Batch, Entry, EntryKind};

const SECONDS_PER_DAY: i64 = 86_400;

/// Customer accounts appear in files without the ledger's `customer:` prefix.
pub fn file_account(ledger_account: &str) -> &str {
    ledger_account
        .strip_prefix("customer:")
        .unwrap_or(ledger_account)
}

/// A 10-character company id derived from the bank id, e.g. `pet-a` → `PETA000001`.
fn company_id(bank: &str) -> String {
    let letters: String = bank
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .take(4)
        .collect();
    format!("{letters:<4}000001")
}

pub fn pending_file(bank: &str, operator: &str, rows: &[OutboxRow], now_unix: i64) -> AchFile {
    let entries = rows
        .iter()
        .map(|row| Entry {
            kind: EntryKind::Credit,
            receiving_bank: row.item.receiving_bank.clone(),
            account: file_account(&row.item.receiving_account).to_owned(),
            amount_minor: row.item.amount_minor,
            individual_id: file_account(&row.item.originator).to_owned(),
            individual_name: row.item.receiver_name.clone(),
            trace_seq: u32::try_from(row.trace_seq).unwrap_or(u32::MAX),
        })
        .collect();
    AchFile {
        origin_bank: bank.to_owned(),
        destination_bank: operator.to_owned(),
        created_yymmdd: yymmdd(now_unix),
        created_hhmm: hhmm(now_unix),
        file_id_modifier: 'A',
        batches: vec![Batch {
            originating_bank: bank.to_owned(),
            company_name: format!("{bank} customers"),
            company_id: company_id(bank),
            entry_description: "PAYMENT".into(),
            // ACH credits settle on a later banking day; we use "tomorrow".
            effective_yymmdd: yymmdd(now_unix + SECONDS_PER_DAY),
            batch_number: 1,
            entries,
        }],
    }
}

/// UTC calendar date for a Unix timestamp (Howard Hinnant's days-to-civil algorithm).
fn civil(unix: i64) -> (i64, u32, u32) {
    let z = unix.div_euclid(SECONDS_PER_DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn yymmdd(unix: i64) -> String {
    let (year, month, day) = civil(unix);
    format!("{:02}{month:02}{day:02}", year.rem_euclid(100))
}

pub fn hhmm(unix: i64) -> String {
    let seconds = unix.rem_euclid(SECONDS_PER_DAY);
    format!("{:02}{:02}", seconds / 3_600, seconds % 3_600 / 60)
}

#[cfg(test)]
mod tests {
    use bramble_ledger::{OutboxItem, nacha};

    use super::*;

    #[test]
    fn ach_dates_are_utc_calendar_dates() {
        assert_eq!(yymmdd(0), "700101");
        assert_eq!(hhmm(0), "0000");
        // 2026-10-12 09:30:00 UTC
        assert_eq!(yymmdd(1_791_797_400), "261012");
        assert_eq!(hhmm(1_791_797_400), "0930");
        // Leap day: 2024-02-29 12:00:00 UTC
        assert_eq!(yymmdd(1_709_208_000), "240229");
    }

    #[test]
    fn ach_pending_file_round_trips_through_nacha() {
        let rows = vec![OutboxRow {
            trace_seq: 7,
            entry_id: 12,
            item: OutboxItem {
                originator: "customer:you".into(),
                receiving_bank: "pet-b".into(),
                receiving_account: "customer:alex".into(),
                receiver_name: "Alex".into(),
                amount_minor: 1_300,
            },
        }];
        let file = pending_file("pet-a", "operator", &rows, 1_791_797_400);
        let text = file.to_nacha().expect("write");
        let back = nacha::parse(&text).expect("parse");
        let entry = &back.batches[0].entries[0];
        assert_eq!(entry.account, "alex");
        assert_eq!(entry.individual_id, "you");
        assert_eq!(entry.trace_seq, 7);
        assert_eq!(back.batches[0].company_id, "PETA000001");
        assert_eq!(back.batches[0].effective_yymmdd, "261013");
    }
}
