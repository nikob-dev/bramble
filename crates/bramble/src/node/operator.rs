//! `bramble node` with `BRAMBLE_ROLE=operator`: the clearing house that holds every
//! bank's reserves and settles between them (decision 012). It has no pet and no lessons.

use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use axum::extract::State;
use axum::response::Json;
use axum::routing::get;
use bramble_core::net::NetMsg;
use bramble_ledger::rails::{Rail, Rtp, Transfer, open_operator, operator_account};
use bramble_ledger::{AccountBalance, Ledger, NewEntry, PostOutcome};
use tokio::sync::{Mutex, mpsc};

use super::net::{Inbound, Net};
use super::rtp::{Status, Waiters, status_msg, transfer_msg, transfer_of};
use super::store::PetStore;
use super::{identity, net};
use crate::config::Config;

pub struct Operator {
    pub ledger: Mutex<Ledger>,
    pub net: Arc<Net>,
    waiters: Waiters,
}

impl Operator {
    pub fn new(ledger: Ledger, net: Arc<Net>) -> Arc<Self> {
        Arc::new(Self {
            ledger: Mutex::new(ledger),
            net,
            waiters: Waiters::default(),
        })
    }
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
    let operator = Operator::new(ledger, net.clone());

    tokio::runtime::Runtime::new()?.block_on(async move {
        tokio::spawn(net::discover_peers(net.clone(), config.peers.clone()));
        tokio::spawn(dispatch(operator.clone()));
        let listener = tokio::net::TcpListener::bind(&config.bind).await?;
        tracing::info!(bind = %config.bind, "operator listening");
        tokio::try_join!(
            async { Ok::<_, anyhow::Error>(axum::serve(listener, router(operator)).await?) },
            net::serve(&config.net_bind, net),
        )?;
        Ok(())
    })
}

fn router(operator: Arc<Operator>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/books", get(books))
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

/// Routes statuses to whoever awaits them, and queues transfers for one-at-a-time
/// settlement, so two payments can't both pass the same reserve check.
pub async fn dispatch(operator: Arc<Operator>) {
    let (queue, mut work) = mpsc::unbounded_channel::<Inbound>();
    let worker = {
        let operator = operator.clone();
        tokio::spawn(async move {
            while let Some(inbound) = work.recv().await {
                settle_rtp(&operator, inbound).await;
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
                NetMsg::RtpCreditTransfer { .. } => {
                    if queue.send(inbound).is_err() {
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
        tracing::error!(%error, to, "could not deliver RTP status");
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
