//! Text helpers shared by every client.

/// Joins hard-wrapped prose into paragraphs so the widget can wrap to the panel width.
/// Blank lines separate paragraphs; indented or bulleted lines (commands, lists) stay as-is.
pub fn reflow(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut joinable = false;
    for line in text.trim().lines() {
        let verbatim = line.starts_with(' ') || line.starts_with('\t') || line.starts_with('•');
        if line.trim().is_empty() {
            out.push(String::new());
            joinable = false;
        } else if joinable && !verbatim {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            out.push(line.to_owned());
            joinable = !verbatim;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_reflow_joins_prose_and_keeps_indented_lines() {
        let text =
            "Amounts are integers,\nnever floats.\n\nRun:\n  bramble next\n  • a bullet\nAfter.";
        assert_eq!(
            reflow(text),
            vec![
                "Amounts are integers, never floats.",
                "",
                "Run:",
                "  bramble next",
                "  • a bullet",
                "After."
            ]
        );
    }
}
