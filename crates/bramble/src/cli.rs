use anyhow::Context;
use bramble_ledger::{Ledger, format_minor};
use clap::Subcommand;

use crate::config::Config;

#[derive(Subcommand)]
pub enum LedgerCommand {
    /// List accounts and their balances.
    Accounts,
    /// List postings, newest first, optionally for one account.
    Postings { account: Option<String> },
}

pub fn ledger(config: &Config, command: LedgerCommand) -> anyhow::Result<()> {
    let path = config.ledger_path();
    let ledger = Ledger::open_read_only(&path).with_context(|| {
        format!(
            "opening ledger at {} (is the node running?)",
            path.display()
        )
    })?;
    match command {
        LedgerCommand::Accounts => {
            println!(
                "{:<26} {:<10} {:>14} {:>12}",
                "ACCOUNT", "KIND", "BALANCE_MINOR", "BALANCE"
            );
            for row in ledger.accounts()? {
                println!(
                    "{:<26} {:<10} {:>14} {:>12}",
                    row.account,
                    row.kind.as_str(),
                    row.balance_minor,
                    format_minor(row.balance_minor)
                );
            }
        }
        LedgerCommand::Postings { account } => {
            println!(
                "{:>5}  {:<26} {:>12}  MEMO",
                "ENTRY", "ACCOUNT", "AMOUNT_MINOR"
            );
            for row in ledger.postings(account.as_deref())? {
                println!(
                    "{:>5}  {:<26} {:>12}  {}",
                    row.entry_id, row.account, row.amount_minor, row.memo
                );
            }
        }
    }
    Ok(())
}

#[derive(Subcommand)]
pub enum AchCommand {
    /// Print the Nacha file the next cutoff will send to the operator.
    Pending,
}

pub fn ach(config: &Config, command: AchCommand) -> anyhow::Result<()> {
    let path = config.ledger_path();
    let ledger = Ledger::open_read_only(&path).with_context(|| {
        format!(
            "opening ledger at {} (is the node running?)",
            path.display()
        )
    })?;
    match command {
        AchCommand::Pending => {
            let rows = ledger.pending_ach()?;
            if rows.is_empty() {
                eprintln!("No ACH payments are waiting for the next cutoff.");
                return Ok(());
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default();
            let file = crate::ach::pending_file(&config.node_id, &config.operator_id, &rows, now);
            print!("{}", file.to_nacha()?);
        }
    }
    Ok(())
}

/// Lesson control commands are interpreted by the node from the lab's command log;
/// the CLI only needs to succeed and tell the learner it was heard.
pub fn acknowledge(message: &str) -> anyhow::Result<()> {
    println!("→ {message}");
    Ok(())
}
