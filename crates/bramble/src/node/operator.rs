//! `bramble node` with `BRAMBLE_ROLE=operator`: the clearing house that holds every
//! bank's reserves and settles between them (decision 012). It has no pet and no lessons.
//!
//! Work that moves reserves (an RTP settlement, an ACH cutoff) runs one item at a time
//! on a single queue, so two of them can never both pass the same reserve check.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use bramble_core::net::{NetMsg, Rail as RailKind};
use bramble_ledger::nacha::{self, AchFile, Batch};
use bramble_ledger::rails::{
    Rail, Rtp, Transfer, ach_positions, open_operator, operator_account, operator_ach_settlement,
};
use bramble_ledger::{AccountBalance, Ledger, NewEntry, PostOutcome};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc, oneshot};

use super::ach_bank::file_id;
use super::net::{Inbound, Net};
use super::rtp::{Status, Waiters, status_msg, transfer_msg, transfer_of};
use super::store::PetStore;
use super::{identity, net, now_unix};
use crate::config::Config;

const WINDOW_KEY_PREFIX: &str = "ach:window:";

enum Work {
    Rtp(Inbound),
    Cutoff(oneshot::Sender<Result<CutoffReport, String>>),
}

pub struct Operator {
    pub ledger: Mutex<Ledger>,
    pub net: Arc<Net>,
    banks: Vec<String>,
    waiters: Waiters,
    files: Waiters<String>,
    work: mpsc::UnboundedSender<Work>,
    queue: std::sync::Mutex<Option<mpsc::UnboundedReceiver<Work>>>,
}

impl Operator {
    pub fn new(ledger: Ledger, net: Arc<Net>, banks: Vec<String>) -> Arc<Self> {
        let (work, queue) = mpsc::unbounded_channel();
        Arc::new(Self {
            ledger: Mutex::new(ledger),
            net,
            banks,
            waiters: Waiters::default(),
            files: Waiters::default(),
            work,
            queue: std::sync::Mutex::new(Some(queue)),
        })
    }

    /// Closes the current ACH window: collects every bank's file, nets, and settles.
    pub async fn cutoff(&self) -> Result<CutoffReport, String> {
        let (tx, rx) = oneshot::channel();
        self.work
            .send(Work::Cutoff(tx))
            .map_err(|_| "the operator is shutting down".to_owned())?;
        rx.await
            .map_err(|_| "the cutoff was abandoned".to_owned())?
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BankPosition {
    pub bank: String,
    pub sent_minor: i64,
    pub received_minor: i64,
    pub net_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CutoffReport {
    pub window: u32,
    pub payments: usize,
    /// False when no bank had anything to send: the window stays open.
    pub settled: bool,
    pub positions: Vec<BankPosition>,
}

pub fn run(config: Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
    if config.banks.is_empty() {
        anyhow::bail!("the operator needs BRAMBLE_BANKS (e.g. pet-a,pet-b,pet-c)");
    }
    let mut ledger = Ledger::open(config.ledger_path())?;
    let banks: Vec<&str> = config.banks.iter().map(String::as_str).collect();
    open_operator(&mut ledger, &banks)?;

    let store = PetStore::open(config.pet_store_path())?;
    let key = identity::load_or_create(&store)?;
    let net = Arc::new(Net::new(&config.node_id, key, store)?);
    let operator = Operator::new(ledger, net.clone(), config.banks.clone());

    tokio::runtime::Runtime::new()?.block_on(async move {
        tokio::spawn(net::discover_peers(net.clone(), config.peers.clone()));
        tokio::spawn(dispatch(operator.clone()));
        if let Some(secs) = config.ach_cutoff_secs {
            tokio::spawn(cutoff_timer(operator.clone(), Duration::from_secs(secs)));
        }
        let listener = tokio::net::TcpListener::bind(&config.bind).await?;
        tracing::info!(bind = %config.bind, "operator listening");
        tokio::try_join!(
            async { Ok::<_, anyhow::Error>(axum::serve(listener, router(operator)).await?) },
            net::serve(&config.net_bind, net),
        )?;
        Ok(())
    })
}

/// Optional periodic cutoff (`BRAMBLE_ACH_CUTOFF_SECS`, decision 013).
async fn cutoff_timer(operator: Arc<Operator>, every: Duration) {
    let mut interval = tokio::time::interval(every);
    interval.tick().await; // The first tick fires immediately; skip it.
    loop {
        interval.tick().await;
        match operator.cutoff().await {
            Ok(report) => tracing::info!(
                window = report.window,
                settled = report.settled,
                "timed ACH cutoff"
            ),
            Err(error) => tracing::error!(%error, "timed ACH cutoff failed"),
        }
    }
}

fn router(operator: Arc<Operator>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/books", get(books))
        .route("/api/ach/cutoff", post(cutoff))
        .with_state(operator)
}

async fn books(State(operator): State<Arc<Operator>>) -> Json<Vec<AccountBalance>> {
    let accounts = operator
        .ledger
        .lock()
        .await
        .accounts()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "reading operator books");
            Vec::new()
        });
    Json(accounts)
}

/// `POST /api/ach/cutoff`: 200 with the window's report, 409 if it could not settle.
async fn cutoff(
    State(operator): State<Arc<Operator>>,
) -> Result<Json<CutoffReport>, (StatusCode, Json<serde_json::Value>)> {
    operator.cutoff().await.map(Json).map_err(|error| {
        tracing::error!(%error, "ACH cutoff failed");
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": error })),
        )
    })
}

/// Routes replies to whoever awaits them; everything that moves reserves goes on the
/// one-at-a-time work queue.
pub async fn dispatch(operator: Arc<Operator>) {
    let Some(mut queue) = operator.queue.lock().expect("queue lock").take() else {
        tracing::error!("operator dispatch started twice");
        return;
    };
    let worker = {
        let operator = operator.clone();
        tokio::spawn(async move {
            while let Some(work) = queue.recv().await {
                match work {
                    Work::Rtp(inbound) => settle_rtp(&operator, inbound).await,
                    Work::Cutoff(reply) => {
                        let _ = reply.send(run_cutoff(&operator).await);
                    }
                }
            }
        })
    };
    let mut inbox = operator.net.inbox.subscribe();
    loop {
        match inbox.recv().await {
            Ok(inbound) => match &inbound.msg {
                NetMsg::RtpStatus {
                    msg_id,
                    accepted,
                    reason,
                } => {
                    let status = Status {
                        accepted: *accepted,
                        reason: reason.clone(),
                    };
                    if !operator.waiters.resolve(msg_id, status) {
                        tracing::warn!(%msg_id, "status for a transfer nobody is waiting on");
                    }
                }
                NetMsg::AchFile { file_id, contents } => {
                    // A bank may only answer for its own file.
                    if !file_id.starts_with(&format!("{}-w", inbound.from)) {
                        tracing::warn!(from = %inbound.from, %file_id, "file id doesn't belong to its sender");
                    } else if !operator.files.resolve(file_id, contents.clone()) {
                        tracing::warn!(%file_id, "ACH file nobody is waiting on");
                    }
                }
                NetMsg::RtpCreditTransfer { .. } => {
                    if operator.work.send(Work::Rtp(inbound)).is_err() {
                        break;
                    }
                }
                other => tracing::debug!(kind = other.kind(), "ignored"),
            },
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::error!(skipped, "operator inbox lagged; messages were dropped");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    worker.abort();
}

async fn run_cutoff(operator: &Operator) -> Result<CutoffReport, String> {
    let window = {
        let ledger = operator.ledger.lock().await;
        let settled = ledger
            .count_entries_with_key_prefix(WINDOW_KEY_PREFIX)
            .map_err(|e| e.to_string())?;
        u32::try_from(settled + 1).map_err(|e| e.to_string())?
    };

    // 1. Ask every bank for its file, then collect them all.
    let mut pending = Vec::new();
    for bank in &operator.banks {
        let id = file_id(bank, window);
        let rx = operator.files.register(&id);
        operator
            .net
            .send(bank, &NetMsg::AchCutoff { window })
            .await
            .map_err(|e| format!("{bank} is unreachable: {e}"))?;
        pending.push((bank, id, rx));
    }
    let mut files = Vec::new();
    for (bank, id, rx) in pending {
        let contents = operator
            .files
            .wait(&id, rx)
            .await
            .ok_or_else(|| format!("{bank} sent no file for window {window}"))?;
        if contents.is_empty() {
            continue;
        }
        let file = nacha::parse(&contents).map_err(|e| format!("{bank}'s file: {e}"))?;
        check_file(&file, bank, &operator.banks)?;
        files.push(file);
    }

    // 2. Net, check reserves, and settle in one entry.
    let positions = ach_positions(&files);
    let payments: usize = files.iter().map(AchFile::entry_count).sum();
    let report = CutoffReport {
        window,
        payments,
        settled: payments > 0,
        positions: positions
            .iter()
            .map(|(bank, p)| BankPosition {
                bank: bank.clone(),
                sent_minor: p.sent_minor,
                received_minor: p.received_minor,
                net_minor: p.net_minor(),
            })
            .collect(),
    };
    if payments == 0 {
        return Ok(report);
    }
    {
        let mut ledger = operator.ledger.lock().await;
        for (bank, position) in &positions {
            let available = -ledger
                .balance(&operator_account(bank))
                .map_err(|e| e.to_string())?;
            if available < -position.net_minor() {
                return Err(format!(
                    "{bank} cannot cover its net debit of {} (reserves {})",
                    bramble_ledger::format_minor(-position.net_minor()),
                    bramble_ledger::format_minor(available)
                ));
            }
        }
        ledger
            .post(&NewEntry {
                memo: format!("ACH settlement, window {window}: {payments} payments, netted"),
                idempotency_key: Some(format!("{WINDOW_KEY_PREFIX}{window}")),
                postings: operator_ach_settlement(&positions),
            })
            .map_err(|e| format!("settlement could not be booked: {e}"))?;
    }
    tracing::info!(window, payments, "ACH window settled");

    // 3. Tell every bank: its incoming entries, then its settlement.
    for bank in positions.keys() {
        if let Some(file) = forwarded_file(&files, bank, now_unix()) {
            match file.to_nacha() {
                Ok(contents) => {
                    let msg = NetMsg::AchFile {
                        file_id: format!("operator-w{window}-{bank}"),
                        contents,
                    };
                    send_or_log(operator, bank, &msg).await;
                }
                Err(error) => tracing::error!(%error, bank, "could not write a forwarded file"),
            }
        }
    }
    for (bank, position) in &positions {
        let notice = NetMsg::SettlementNotice {
            settlement_id: format!("ach-w{window}"),
            rail: RailKind::Ach,
            bank: bank.clone(),
            amount_minor: position.net_minor(),
            reference: file_id(bank, window),
        };
        send_or_log(operator, bank, &notice).await;
    }
    Ok(report)
}

/// A bank may only send its own payments to network members.
fn check_file(file: &AchFile, sender: &str, members: &[String]) -> Result<(), String> {
    if file.origin_bank != sender {
        return Err(format!("{sender} sent a file from {}", file.origin_bank));
    }
    for batch in &file.batches {
        if batch.originating_bank != sender {
            return Err(format!(
                "{sender} sent a batch originated by {}",
                batch.originating_bank
            ));
        }
        if let Some(entry) = batch
            .entries
            .iter()
            .find(|e| !members.contains(&e.receiving_bank))
        {
            return Err(format!(
                "{sender} sent a payment to non-member {}",
                entry.receiving_bank
            ));
        }
    }
    Ok(())
}

/// The entries addressed to `bank`, kept in their original batches so the receiver can
/// see which bank originated each one.
fn forwarded_file(files: &[AchFile], bank: &str, now: i64) -> Option<AchFile> {
    let batches: Vec<Batch> = files
        .iter()
        .flat_map(|f| &f.batches)
        .filter_map(|batch| {
            let entries: Vec<_> = batch
                .entries
                .iter()
                .filter(|e| e.receiving_bank == bank)
                .cloned()
                .collect();
            (!entries.is_empty()).then(|| Batch {
                entries,
                ..batch.clone()
            })
        })
        .collect();
    (!batches.is_empty()).then(|| AchFile {
        origin_bank: "operator".into(),
        destination_bank: bank.to_owned(),
        created_yymmdd: crate::ach::yymmdd(now),
        created_hhmm: crate::ach::hhmm(now),
        file_id_modifier: 'A',
        batches,
    })
}

async fn settle_rtp(operator: &Operator, Inbound { from, msg }: Inbound) {
    let Some((msg_id, transfer, memo)) = transfer_of(&msg) else {
        return;
    };
    let status = decide(operator, &from, &msg_id, &transfer, &memo).await;
    tracing::info!(%msg_id, accepted = status.accepted, reason = ?status.reason, "rtp decided");
    let reply = status_msg(&msg_id, &status);
    if status.accepted {
        // The receiving bank books its side on this confirmation.
        send_or_log(operator, &transfer.creditor_bank, &reply).await;
    }
    send_or_log(operator, &from, &reply).await;
}

async fn send_or_log(operator: &Operator, to: &str, msg: &NetMsg) {
    if let Err(error) = operator.net.send(to, msg).await {
        tracing::error!(%error, to, kind = msg.kind(), "could not deliver to a bank");
    }
}

fn settlement_entry(msg_id: &str, transfer: &Transfer, memo: &str) -> NewEntry {
    NewEntry {
        memo: format!(
            "RTP {} → {}: {memo}",
            transfer.debtor_bank, transfer.creditor_bank
        ),
        idempotency_key: Some(format!("rtp:{msg_id}")),
        postings: Rtp.operator_postings(transfer),
    }
}

async fn decide(
    operator: &Operator,
    from: &str,
    msg_id: &str,
    transfer: &Transfer,
    memo: &str,
) -> Status {
    if from != transfer.debtor_bank {
        return Status::rejected(format!(
            "{from} cannot send a payment on behalf of {}",
            transfer.debtor_bank
        ));
    }
    let entry = settlement_entry(msg_id, transfer, memo);
    {
        let mut ledger = operator.ledger.lock().await;
        match ledger.entry_id_by_idempotency_key(entry.idempotency_key.as_deref().unwrap_or("")) {
            // Already settled: a retry. Posting again is a no-op if identical, a conflict if not.
            Ok(Some(_)) => {
                return match ledger.post(&entry) {
                    Ok(PostOutcome::Replayed(_) | PostOutcome::Created(_)) => Status {
                        accepted: true,
                        reason: None,
                    },
                    Err(error) => Status::rejected(format!("msg_id {msg_id} reused: {error}")),
                };
            }
            Ok(None) => {}
            Err(error) => return Status::rejected(format!("operator ledger error: {error}")),
        }
        if let Err(error) = transfer.validate() {
            return Status::rejected(error.to_string());
        }
        for bank in [&transfer.debtor_bank, &transfer.creditor_bank] {
            if ledger.balance(&operator_account(bank)).is_err() {
                return Status::rejected(format!("{bank} is not a member of this network"));
            }
        }
        let available = -ledger
            .balance(&operator_account(&transfer.debtor_bank))
            .unwrap_or(0);
        if available < transfer.amount_minor {
            return Status::rejected(format!(
                "insufficient reserves at the operator: {} has {}, needs {}",
                transfer.debtor_bank,
                bramble_ledger::format_minor(available),
                bramble_ledger::format_minor(transfer.amount_minor)
            ));
        }
    }

    let rx = operator.waiters.register(msg_id);
    if let Err(error) = operator
        .net
        .send(
            &transfer.creditor_bank,
            &transfer_msg(msg_id, transfer, memo),
        )
        .await
    {
        return Status::rejected(format!(
            "{} is unreachable: {error}",
            transfer.creditor_bank
        ));
    }
    match operator.waiters.wait(msg_id, rx).await {
        None => Status::rejected(format!("{} did not answer in time", transfer.creditor_bank)),
        Some(status) if !status.accepted => status,
        Some(_) => match operator.ledger.lock().await.post(&entry) {
            Ok(_) => Status {
                accepted: true,
                reason: None,
            },
            Err(error) => {
                tracing::error!(%error, %msg_id, "settlement could not be booked");
                Status::rejected(format!("settlement failed: {error}"))
            }
        },
    }
}
