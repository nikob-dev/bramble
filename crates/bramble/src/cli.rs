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

/// Lesson control commands are interpreted by the node from the lab's command log;
/// the CLI only needs to succeed and tell the learner it was heard.
pub fn acknowledge(message: &str) -> anyhow::Result<()> {
    println!("→ {message}");
    Ok(())
}
