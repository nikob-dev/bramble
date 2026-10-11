//! ACH files in a subset of the Nacha format (decision 014).
//!
//! Every record is one 94-character line:
//!
//! | type | record        | carries                                              |
//! |------|---------------|------------------------------------------------------|
//! | `1`  | file header   | who sent the file, to whom, and when                 |
//! | `5`  | batch header  | the originating company and the entries' purpose     |
//! | `6`  | entry detail  | one payment: receiving bank, account, amount, trace  |
//! | `8`  | batch control | counts, entry hash and totals for the batch          |
//! | `9`  | file control  | counts, entry hash and totals for the whole file     |
//!
//! Not supported (yet): addenda records (`7`), and padding the file to a multiple of
//! ten records with `9999…` lines, though the parser tolerates that padding.
//! Names are uppercased and truncated to fit, as banks do. Identifiers and amounts
//! that don't fit are errors, never truncated: that would misroute money.

use std::fmt::Write as _;

pub const RECORD_LEN: usize = 94;
const BLOCKING_FACTOR: usize = 10;
const SEC_CODE: &str = "PPD";

/// Synthetic routing numbers. The leading `99` is not assigned to any Federal
/// Reserve district, so these can't collide with a real bank, but each still passes
/// the ABA check-digit rule (see [`routing_check_digit`]).
const ROUTING: [(&str, &str); 4] = [
    ("operator", "990000000"),
    ("pet-a", "990000013"),
    ("pet-b", "990000026"),
    ("pet-c", "990000039"),
];

pub fn routing_number(bank: &str) -> Option<&'static str> {
    ROUTING.iter().find(|(b, _)| *b == bank).map(|(_, r)| *r)
}

pub fn bank_for_routing(routing: &str) -> Option<&'static str> {
    ROUTING.iter().find(|(_, r)| *r == routing).map(|(b, _)| *b)
}

/// ABA check digit for the first eight digits: weights 3, 7, 1 repeating, and the
/// ninth digit brings the weighted sum to a multiple of ten.
pub fn routing_check_digit(first8: &str) -> Option<u32> {
    if first8.len() != 8 {
        return None;
    }
    let mut sum = 0;
    for (digit, weight) in first8.chars().zip([3, 7, 1].iter().cycle()) {
        sum += digit.to_digit(10)? * weight;
    }
    Some((10 - sum % 10) % 10)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Transaction code 22: deposit to a checking account (push payment).
    Credit,
    /// Transaction code 27: withdrawal from a checking account (pull payment).
    Debit,
}

impl EntryKind {
    fn code(self) -> &'static str {
        match self {
            Self::Credit => "22",
            Self::Debit => "27",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub receiving_bank: String,
    /// The account at the receiving bank, as it appears in the file (17 chars max).
    pub account: String,
    pub amount_minor: i64,
    pub individual_id: String,
    pub individual_name: String,
    /// Unique within the originating bank; the trace number is ODFI + this.
    pub trace_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub company_name: String,
    pub company_id: String,
    pub entry_description: String,
    pub effective_yymmdd: String,
    pub batch_number: u32,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AchFile {
    pub origin_bank: String,
    pub destination_bank: String,
    pub created_yymmdd: String,
    pub created_hhmm: String,
    pub file_id_modifier: char,
    pub batches: Vec<Batch>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NachaError {
    #[error("line {line}: {message}")]
    Format { line: usize, message: String },
    #[error("{field} {value:?} does not fit in {width} characters")]
    TooLong {
        field: &'static str,
        value: String,
        width: usize,
    },
    #[error("{0:?} has no routing number")]
    UnknownBank(String),
    #[error("{field} must be {expected}, got {value:?}")]
    BadValue {
        field: &'static str,
        expected: &'static str,
        value: String,
    },
}

fn format_err(line: usize, message: impl Into<String>) -> NachaError {
    NachaError::Format {
        line,
        message: message.into(),
    }
}

// ---- writing ----

/// Left-justified identifier; too long is an error.
fn ident(field: &'static str, value: &str, width: usize) -> Result<String, NachaError> {
    if !value.is_ascii() || value.len() > width {
        return Err(NachaError::TooLong {
            field,
            value: value.to_owned(),
            width,
        });
    }
    Ok(format!("{value:<width$}"))
}

/// Left-justified, uppercased name; truncated to fit, as banks do.
fn name(value: &str, width: usize) -> String {
    let upper: String = value
        .chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_ascii_uppercase()
            } else {
                '?'
            }
        })
        .take(width)
        .collect();
    format!("{upper:<width$}")
}

/// Right-justified, zero-padded number; too large is an error.
fn num(field: &'static str, value: u64, width: usize) -> Result<String, NachaError> {
    let text = format!("{value:0width$}");
    if text.len() > width {
        return Err(NachaError::TooLong {
            field,
            value: value.to_string(),
            width,
        });
    }
    Ok(text)
}

fn digits(field: &'static str, value: &str, width: usize) -> Result<String, NachaError> {
    if value.len() != width || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(NachaError::BadValue {
            field,
            expected: "all digits",
            value: value.to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn routing(bank: &str) -> Result<&'static str, NachaError> {
    routing_number(bank).ok_or_else(|| NachaError::UnknownBank(bank.to_owned()))
}

fn amount(value: i64, width: usize) -> Result<String, NachaError> {
    let unsigned = u64::try_from(value).map_err(|_| NachaError::BadValue {
        field: "amount",
        expected: "non-negative",
        value: value.to_string(),
    })?;
    num("amount", unsigned, width)
}

/// Sum of each entry's 8-digit receiving routing prefix, keeping the last 10 digits.
fn entry_hash<'a>(entries: impl Iterator<Item = &'a Entry>) -> Result<u64, NachaError> {
    let mut hash: u64 = 0;
    for entry in entries {
        let prefix: u64 = routing(&entry.receiving_bank)?[..8]
            .parse()
            .expect("routing table holds digits");
        hash += prefix;
    }
    Ok(hash % 10_000_000_000)
}

fn totals<'a>(entries: impl Iterator<Item = &'a Entry>) -> (i64, i64) {
    entries.fold((0, 0), |(debits, credits), e| match e.kind {
        EntryKind::Debit => (debits + e.amount_minor, credits),
        EntryKind::Credit => (debits, credits + e.amount_minor),
    })
}

fn service_class(batch: &Batch) -> &'static str {
    let credits = batch.entries.iter().any(|e| e.kind == EntryKind::Credit);
    let debits = batch.entries.iter().any(|e| e.kind == EntryKind::Debit);
    match (credits, debits) {
        (true, false) => "220",
        (false, true) => "225",
        _ => "200",
    }
}

impl AchFile {
    pub fn entry_count(&self) -> usize {
        self.batches.iter().map(|b| b.entries.len()).sum()
    }

    pub fn to_nacha(&self) -> Result<String, NachaError> {
        let origin = routing(&self.origin_bank)?;
        let odfi = &origin[..8];
        let destination = routing(&self.destination_bank)?;
        let mut lines = Vec::new();

        if !self.file_id_modifier.is_ascii_alphanumeric() {
            return Err(NachaError::BadValue {
                field: "file id modifier",
                expected: "A-Z or 0-9",
                value: self.file_id_modifier.to_string(),
            });
        }
        lines.push(format!(
            "101 {destination} {origin}{}{}{}094{BLOCKING_FACTOR}1{}{}{}",
            digits("file creation date", &self.created_yymmdd, 6)?,
            digits("file creation time", &self.created_hhmm, 4)?,
            self.file_id_modifier.to_ascii_uppercase(),
            name(&self.destination_bank, 23),
            name(&self.origin_bank, 23),
            " ".repeat(8),
        ));

        for batch in &self.batches {
            let class = service_class(batch);
            let company_id = ident("company id", &batch.company_id, 10)?;
            let batch_number = num("batch number", batch.batch_number.into(), 7)?;
            lines.push(format!(
                "5{class}{}{}{company_id}{SEC_CODE}{}{}{}{}1{odfi}{batch_number}",
                name(&batch.company_name, 16),
                " ".repeat(20),
                name(&batch.entry_description, 10),
                " ".repeat(6),
                digits("effective date", &batch.effective_yymmdd, 6)?,
                // Settlement date: filled in by the operator, never the originator.
                " ".repeat(3),
            ));
            for entry in &batch.entries {
                let receiving = routing(&entry.receiving_bank)?;
                lines.push(format!(
                    "6{}{}{}{}{}{}{}{}0{odfi}{}",
                    entry.kind.code(),
                    &receiving[..8],
                    &receiving[8..],
                    ident("account", &entry.account, 17)?,
                    amount(entry.amount_minor, 10)?,
                    ident("individual id", &entry.individual_id, 15)?,
                    name(&entry.individual_name, 22),
                    // Discretionary data: two blanks, before the addenda indicator.
                    "  ",
                    num("trace sequence", entry.trace_seq.into(), 7)?,
                ));
            }
            let (debits, credits) = totals(batch.entries.iter());
            lines.push(format!(
                "8{class}{}{}{}{}{company_id}{}{odfi}{batch_number}",
                num("entry count", batch.entries.len() as u64, 6)?,
                num("entry hash", entry_hash(batch.entries.iter())?, 10)?,
                amount(debits, 12)?,
                amount(credits, 12)?,
                // Message authentication code (19) and reserved (6).
                " ".repeat(25),
            ));
        }

        let entries = || self.batches.iter().flat_map(|b| b.entries.iter());
        let records = 2 + self
            .batches
            .iter()
            .map(|b| 2 + b.entries.len())
            .sum::<usize>();
        let (debits, credits) = totals(entries());
        lines.push(format!(
            "9{}{}{}{}{}{}{}",
            num("batch count", self.batches.len() as u64, 6)?,
            num("block count", records.div_ceil(BLOCKING_FACTOR) as u64, 6)?,
            num("entry count", self.entry_count() as u64, 8)?,
            num("entry hash", entry_hash(entries())?, 10)?,
            amount(debits, 12)?,
            amount(credits, 12)?,
            " ".repeat(39),
        ));

        debug_assert!(lines.iter().all(|l| l.len() == RECORD_LEN), "{lines:#?}");
        let mut out = String::new();
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
        Ok(out)
    }
}

// ---- reading ----

struct Line<'a> {
    number: usize,
    text: &'a str,
}

impl Line<'_> {
    /// 1-based, inclusive positions, as the Nacha spec writes them.
    fn field(&self, from: usize, to: usize) -> &str {
        &self.text[from - 1..to]
    }

    fn text(&self, from: usize, to: usize) -> String {
        self.field(from, to).trim_end().to_owned()
    }

    fn number(&self, from: usize, to: usize, what: &str) -> Result<u64, NachaError> {
        let field = self.field(from, to);
        field
            .trim()
            .parse()
            .map_err(|_| format_err(self.number, format!("{what} {field:?} is not a number")))
    }

    fn routing(&self, from: usize) -> Result<&'static str, NachaError> {
        let value = self.field(from, from + 8);
        bank_for_routing(value)
            .ok_or_else(|| format_err(self.number, format!("unknown routing number {value}")))
    }

    fn expect(&self, from: usize, to: usize, what: &str, expected: u64) -> Result<(), NachaError> {
        let found = self.number(from, to, what)?;
        if found == expected {
            Ok(())
        } else {
            Err(format_err(
                self.number,
                format!("{what} says {found}, but the entries add up to {expected}"),
            ))
        }
    }
}

pub fn parse(text: &str) -> Result<AchFile, NachaError> {
    let mut lines = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let raw = raw.trim_end_matches('\r');
        if raw.is_empty() {
            continue;
        }
        if raw.len() != RECORD_LEN || !raw.is_ascii() {
            return Err(format_err(
                i + 1,
                format!(
                    "records are {RECORD_LEN} ASCII characters; this one is {}",
                    raw.len()
                ),
            ));
        }
        lines.push(Line {
            number: i + 1,
            text: raw,
        });
    }
    // Blocking-factor padding: whole lines of 9s after the file control record.
    while lines.len() > 1
        && lines
            .last()
            .is_some_and(|l| l.text.bytes().all(|b| b == b'9'))
    {
        lines.pop();
    }

    let mut iter = lines.iter().peekable();
    let header = iter.next().ok_or_else(|| format_err(0, "empty file"))?;
    if header.field(1, 1) != "1" {
        return Err(format_err(
            header.number,
            "the first record must be a file header (1)",
        ));
    }
    let mut file = AchFile {
        destination_bank: header.routing(5)?.to_owned(),
        origin_bank: header.routing(15)?.to_owned(),
        created_yymmdd: header.field(24, 29).to_owned(),
        created_hhmm: header.field(30, 33).to_owned(),
        file_id_modifier: header.text[33..34].chars().next().unwrap_or(' '),
        batches: Vec::new(),
    };

    loop {
        let line = iter
            .next()
            .ok_or_else(|| format_err(header.number, "missing file control (9) record"))?;
        match line.field(1, 1) {
            "5" => file.batches.push(parse_batch(line, &mut iter)?),
            "9" => {
                let entries = || file.batches.iter().flat_map(|b| b.entries.iter());
                let (debits, credits) = totals(entries());
                line.expect(2, 7, "batch count", file.batches.len() as u64)?;
                line.expect(14, 21, "entry count", file.entry_count() as u64)?;
                line.expect(22, 31, "entry hash", entry_hash(entries())?)?;
                line.expect(32, 43, "total debits", debits as u64)?;
                line.expect(44, 55, "total credits", credits as u64)?;
                if let Some(extra) = iter.next() {
                    return Err(format_err(extra.number, "records after the file control"));
                }
                return Ok(file);
            }
            other => {
                return Err(format_err(
                    line.number,
                    format!("expected a batch header (5) or file control (9), got {other:?}"),
                ));
            }
        }
    }
}

fn parse_batch<'a, I>(
    header: &Line<'_>,
    iter: &mut std::iter::Peekable<I>,
) -> Result<Batch, NachaError>
where
    I: Iterator<Item = &'a Line<'a>>,
{
    let mut batch = Batch {
        company_name: header.text(5, 20),
        company_id: header.text(41, 50),
        entry_description: header.text(54, 63),
        effective_yymmdd: header.field(70, 75).to_owned(),
        batch_number: header.number(88, 94, "batch number")? as u32,
        entries: Vec::new(),
    };
    loop {
        let line = iter
            .next()
            .ok_or_else(|| format_err(header.number, "batch has no control (8) record"))?;
        match line.field(1, 1) {
            "6" => batch.entries.push(parse_entry(line)?),
            "8" => {
                line.expect(5, 10, "entry count", batch.entries.len() as u64)?;
                line.expect(11, 20, "entry hash", entry_hash(batch.entries.iter())?)?;
                let (debits, credits) = totals(batch.entries.iter());
                line.expect(21, 32, "total debits", debits as u64)?;
                line.expect(33, 44, "total credits", credits as u64)?;
                line.expect(88, 94, "batch number", batch.batch_number.into())?;
                return Ok(batch);
            }
            other => {
                return Err(format_err(
                    line.number,
                    format!("expected an entry (6) or batch control (8), got {other:?}"),
                ));
            }
        }
    }
}

fn parse_entry(line: &Line<'_>) -> Result<Entry, NachaError> {
    let kind = match line.field(2, 3) {
        "22" => EntryKind::Credit,
        "27" => EntryKind::Debit,
        code => {
            return Err(format_err(
                line.number,
                format!("unsupported transaction code {code}"),
            ));
        }
    };
    let check = routing_check_digit(line.field(4, 11));
    if check.map(|d| d.to_string()).as_deref() != Some(line.field(12, 12)) {
        return Err(format_err(
            line.number,
            format!(
                "routing {}{} fails its check digit",
                line.field(4, 11),
                line.field(12, 12)
            ),
        ));
    }
    Ok(Entry {
        kind,
        receiving_bank: line.routing(4)?.to_owned(),
        account: line.text(13, 29),
        amount_minor: line.number(30, 39, "amount")? as i64,
        individual_id: line.text(40, 54),
        individual_name: line.text(55, 76),
        trace_seq: line.number(88, 94, "trace sequence")? as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../tests/fixtures/pet-a-sample.ach");

    pub fn sample() -> AchFile {
        let entry = |bank: &str, account: &str, amount, name: &str, seq| Entry {
            kind: EntryKind::Credit,
            receiving_bank: bank.into(),
            account: account.into(),
            amount_minor: amount,
            individual_id: "you".into(),
            individual_name: name.into(),
            trace_seq: seq,
        };
        AchFile {
            origin_bank: "pet-a".into(),
            destination_bank: "operator".into(),
            created_yymmdd: "261012".into(),
            created_hhmm: "0930".into(),
            file_id_modifier: 'A',
            batches: vec![Batch {
                company_name: "Pip Bank customers".into(),
                company_id: "PETA000001".into(),
                entry_description: "PAYMENT".into(),
                effective_yymmdd: "261013".into(),
                batch_number: 1,
                entries: vec![
                    entry("pet-b", "alex", 1_300, "Alex", 1),
                    entry("pet-c", "sam", 25_000, "Sam Rivera", 2),
                    entry("pet-b", "alex", 799, "Alex", 3),
                ],
            }],
        }
    }

    #[test]
    fn nacha_routing_numbers_pass_the_aba_check_digit() {
        for (bank, routing) in ROUTING {
            let check = routing_check_digit(&routing[..8]).expect("digits");
            assert_eq!(check.to_string(), &routing[8..], "{bank}");
        }
        assert_eq!(
            routing_check_digit("01100001"),
            Some(5),
            "a published example: 011000015"
        );
    }

    #[test]
    fn nacha_writer_matches_the_golden_file() {
        assert_eq!(sample().to_nacha().expect("write"), GOLDEN);
    }

    #[test]
    fn nacha_every_record_is_94_characters() {
        for line in GOLDEN.lines() {
            assert_eq!(line.len(), RECORD_LEN, "{line:?}");
        }
    }

    #[test]
    fn nacha_golden_file_parses_back_to_the_same_file() {
        let mut expected = sample();
        // Names come back as written: uppercased, and the company name cut to 16.
        expected.batches[0].company_name = "PIP BANK CUSTOME".into();
        for entry in &mut expected.batches[0].entries {
            entry.individual_name = entry.individual_name.to_uppercase();
        }
        assert_eq!(parse(GOLDEN).expect("parse"), expected);
    }

    #[test]
    fn nacha_controls_carry_counts_hash_and_totals() {
        let control = GOLDEN
            .lines()
            .find(|l| l.starts_with('8'))
            .expect("batch control");
        assert_eq!(&control[4..10], "000003", "entry count");
        // 99000002 + 99000003 + 99000002
        assert_eq!(&control[10..20], "0297000007", "entry hash");
        assert_eq!(&control[32..44], "000000027099", "total credits");
    }

    #[test]
    fn nacha_parse_rejects_a_tampered_amount() {
        let tampered = GOLDEN.replacen("0000001300", "0000009300", 1);
        let error = parse(&tampered).expect_err("totals no longer match");
        assert!(error.to_string().contains("total credits"), "{error}");
    }

    #[test]
    fn nacha_parse_rejects_a_bad_routing_check_digit() {
        let tampered = GOLDEN.replacen("622990000026alex", "622990000027alex", 1);
        assert_ne!(tampered, GOLDEN, "the tamper must change something");
        let error = parse(&tampered).expect_err("check digit");
        assert!(error.to_string().contains("check digit"), "{error}");
    }

    #[test]
    fn nacha_parse_rejects_short_records_and_missing_controls() {
        assert!(parse("101 short").is_err());
        let without_control: String = GOLDEN
            .lines()
            .filter(|l| !l.starts_with('9'))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(parse(&without_control).is_err());
    }

    #[test]
    fn nacha_parse_tolerates_blocking_padding() {
        let padded = format!("{GOLDEN}{}\n", "9".repeat(RECORD_LEN));
        assert_eq!(
            parse(&padded).expect("parse"),
            parse(GOLDEN).expect("parse")
        );
    }

    #[test]
    fn nacha_writer_refuses_identifiers_that_do_not_fit() {
        let mut file = sample();
        file.batches[0].entries[0].account = "x".repeat(18);
        assert!(matches!(
            file.to_nacha(),
            Err(NachaError::TooLong {
                field: "account",
                ..
            })
        ));
        let mut file = sample();
        file.batches[0].entries[0].receiving_bank = "pet-z".into();
        assert_eq!(
            file.to_nacha(),
            Err(NachaError::UnknownBank("pet-z".into()))
        );
    }

    #[test]
    fn nacha_writer_truncates_long_names() {
        let mut file = sample();
        file.batches[0].entries[0].individual_name = "Alexandria Ocasio Montgomery".into();
        let text = file.to_nacha().expect("write");
        assert!(text.contains("ALEXANDRIA OCASIO MONT"), "22 chars kept");
    }
}
