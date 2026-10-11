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
    /// Ring the bell: the operator closes the window, nets every bank, and settles.
    Cutoff,
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
        AchCommand::Cutoff => cutoff(config)?,
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct CutoffReport {
    window: u32,
    payments: usize,
    settled: bool,
    positions: Vec<BankPosition>,
}

#[derive(serde::Deserialize)]
struct BankPosition {
    bank: String,
    sent_minor: i64,
    received_minor: i64,
    net_minor: i64,
}

#[derive(serde::Deserialize)]
struct Statement {
    balance_minor: i64,
    movements: Vec<Movement>,
}

#[derive(serde::Deserialize)]
struct Movement {
    entry_id: i64,
    memo: String,
    amount_minor: i64,
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> anyhow::Result<T> {
    tokio::runtime::Runtime::new()?.block_on(async {
        let response = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("{url} answered {status}");
        }
        Ok(response.json().await?)
    })
}

/// Compares our books with the operator's statement of our account. Exit status 1 on
/// a break, so scripts and lessons can check it.
pub fn reconcile(config: &Config) -> anyhow::Result<()> {
    let path = config.ledger_path();
    let ledger = Ledger::open_read_only(&path).with_context(|| {
        format!(
            "opening ledger at {} (is the node running?)",
            path.display()
        )
    })?;
    let ours = ledger.balance(bramble_ledger::rails::RESERVES_AT_OPERATOR)?;
    let statement: Statement = get_json(&format!(
        "{}/api/statements/{}",
        config.operator_url, config.node_id
    ))?;
    let theirs = statement.balance_minor;

    println!("Reserves at the operator, {}:\n", config.node_id);
    println!(
        "  our books (reserves:operator)   {:>14}",
        format_minor(ours)
    );
    println!(
        "  operator's statement            {:>14}",
        format_minor(theirs)
    );
    println!(
        "  difference                      {:>14}\n",
        format_minor(ours - theirs)
    );
    if ours == theirs {
        println!("✓ Reconciled: both sides agree.");
        return Ok(());
    }
    println!("✗ Break. The operator's last movements on our account:\n");
    for movement in statement.movements.iter().take(8) {
        println!(
            "  #{:<5} {:>12}  {}",
            movement.entry_id,
            format_minor(movement.amount_minor),
            movement.memo
        );
    }
    println!("\nCompare them with `bramble ledger postings reserves:operator`.");
    std::process::exit(1);
}

fn cutoff(config: &Config) -> anyhow::Result<()> {
    let url = format!("{}/api/ach/cutoff", config.operator_url);
    let response = tokio::runtime::Runtime::new()?.block_on(async {
        let response = reqwest::Client::new()
            .post(&url)
            .send()
            .await
            .with_context(|| format!("reaching the operator at {url}"))?;
        let status = response.status();
        let body = response.text().await?;
        anyhow::Ok((status, body))
    })?;
    let (status, body) = response;
    if !status.is_success() {
        anyhow::bail!("cutoff failed ({status}): {body}");
    }
    let report: CutoffReport = serde_json::from_str(&body).context("reading the cutoff report")?;
    if !report.settled {
        println!(
            "Window {}: no ACH payments anywhere, nothing to settle.",
            report.window
        );
        return Ok(());
    }
    println!(
        "Window {} settled: {} payments became {} net movements.\n",
        report.window,
        report.payments,
        report.positions.iter().filter(|p| p.net_minor != 0).count()
    );
    println!(
        "{:<8} {:>12} {:>12} {:>12}",
        "BANK", "SENT", "RECEIVED", "NET"
    );
    for p in &report.positions {
        println!(
            "{:<8} {:>12} {:>12} {:>12}",
            p.bank,
            format_minor(p.sent_minor),
            format_minor(p.received_minor),
            format_minor(p.net_minor)
        );
    }
    Ok(())
}

/// Lesson control commands are interpreted by the node from the lab's command log;
/// the CLI only needs to succeed and tell the learner it was heard.
pub fn acknowledge(message: &str) -> anyhow::Result<()> {
    println!("→ {message}");
    Ok(())
}
