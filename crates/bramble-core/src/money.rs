//! Money is stored as integer minor units (cents), never floats.

/// Formats minor units as dollars, e.g. `-7150` → `-$71.50`.
pub fn format_minor(amount_minor: i64) -> String {
    let sign = if amount_minor < 0 { "-" } else { "" };
    let abs = amount_minor.unsigned_abs();
    format!("{sign}${}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_format_minor_renders_sign_and_cents() {
        assert_eq!(format_minor(-7_150), "-$71.50");
        assert_eq!(format_minor(5), "$0.05");
        assert_eq!(format_minor(0), "$0.00");
    }
}
