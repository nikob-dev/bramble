//! The pet-bank's learner-facing HTTP API, modelled on real payment APIs.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bramble_core::lesson::PostingSpec;
use bramble_ledger::{Ledger, LedgerError, NewEntry, PostOutcome, Posting};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ach_bank::{Refusal, originate};
use super::rtp::{PaymentRequest, SendOutcome, send_payment};
use super::{Shared, now_unix};

pub const IDEMPOTENCY_HEADER: &str = "idempotency-key";

#[derive(Debug, Deserialize)]
pub struct EntryRequest {
    pub memo: String,
    pub postings: Vec<PostingSpec>,
}

/// `POST /api/entries`: 201 created, 200 idempotent replay, 409 key conflict, 422 invalid.
pub async fn post_entry(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(request): Json<EntryRequest>,
) -> (StatusCode, Json<Value>) {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut world = shared.world.lock().await;
    let (status, body) = submit_entry(&mut world.ledger, request, key);
    tracing::info!(status = status.as_u16(), body = %body, "api entry");
    if status == StatusCode::CREATED {
        // Every client's ledger view updates live. No subscribers is fine.
        let _ = shared.updates.send(world.snapshot(None, now_unix()));
    }
    (status, Json(body))
}

/// `POST /api/rtp/payments`: an instant payment to a customer at another bank.
/// 201 settled, 200 replay (same `Idempotency-Key`), 422 refused, 504 no answer in time.
pub async fn post_rtp_payment(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(request): Json<PaymentRequest>,
) -> (StatusCode, Json<Value>) {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let outcome = send_payment(&shared, request, key).await;
    let (status, body) = match outcome {
        SendOutcome::Settled { msg_id, entry_id } => (
            StatusCode::CREATED,
            json!({ "status": "settled", "msg_id": msg_id, "entry_id": entry_id }),
        ),
        SendOutcome::Replayed { msg_id, entry_id } => (
            StatusCode::OK,
            json!({ "status": "settled", "msg_id": msg_id, "entry_id": entry_id, "replayed": true }),
        ),
        SendOutcome::Invalid(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "status": "refused", "error": error }),
        ),
        SendOutcome::Rejected { msg_id, reason } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "status": "rejected", "msg_id": msg_id, "error": reason }),
        ),
        SendOutcome::TimedOut { msg_id } => (
            StatusCode::GATEWAY_TIMEOUT,
            json!({
                "status": "unknown",
                "msg_id": msg_id,
                "error": "no answer in time; retry with the same Idempotency-Key to find out"
            }),
        ),
    };
    tracing::info!(status = status.as_u16(), body = %body, "api rtp payment");
    (status, Json(body))
}

/// `POST /api/ach/payments`: queue a payment for the next ACH file. The customer is
/// debited now (into `clearing:ach-out`); money moves between banks at cutoff.
/// 201 queued, 200 replay (same `Idempotency-Key`), 409 key reused, 422 refused.
pub async fn post_ach_payment(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(request): Json<PaymentRequest>,
) -> (StatusCode, Json<Value>) {
    let key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let (status, body) = match originate(&shared, request, key).await {
        Ok(PostOutcome::Created(entry_id)) => (
            StatusCode::CREATED,
            json!({ "status": "queued", "entry_id": entry_id }),
        ),
        Ok(PostOutcome::Replayed(entry_id)) => (
            StatusCode::OK,
            json!({ "status": "queued", "entry_id": entry_id, "replayed": true }),
        ),
        Err(Refusal::Conflict(error)) => (StatusCode::CONFLICT, json!({ "error": error })),
        Err(Refusal::Invalid(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "status": "refused", "error": error }),
        ),
    };
    tracing::info!(status = status.as_u16(), body = %body, "api ach payment");
    (status, Json(body))
}

/// `GET /api/ach/pending`: the Nacha file the next cutoff would send (204 if empty).
pub async fn ach_pending(State(shared): State<Arc<Shared>>) -> Response {
    let world = shared.world.lock().await;
    let rows = match world.ledger.pending_ach() {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, "reading the ACH outbox");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if rows.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let file = crate::ach::pending_file(&shared.net.id, &shared.operator_id, &rows, now_unix());
    match file.to_nacha() {
        Ok(text) => (
            [(header::CONTENT_TYPE, "text/plain; charset=us-ascii")],
            text,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "writing the pending ACH file");
            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
    }
}

pub fn submit_entry(
    ledger: &mut Ledger,
    request: EntryRequest,
    key: Option<String>,
) -> (StatusCode, Value) {
    let entry = NewEntry {
        memo: request.memo,
        idempotency_key: key,
        postings: request
            .postings
            .into_iter()
            .map(|p| Posting::new(p.account, p.amount_minor))
            .collect(),
    };
    match ledger.post(&entry) {
        Ok(PostOutcome::Created(id)) => (
            StatusCode::CREATED,
            json!({ "entry_id": id, "replayed": false }),
        ),
        Ok(PostOutcome::Replayed(id)) => {
            (StatusCode::OK, json!({ "entry_id": id, "replayed": true }))
        }
        Err(error @ LedgerError::IdempotencyConflict(_)) => {
            (StatusCode::CONFLICT, json!({ "error": error.to_string() }))
        }
        Err(
            error @ (LedgerError::TooFewPostings(_)
            | LedgerError::Unbalanced(_)
            | LedgerError::ZeroAmount
            | LedgerError::UnknownAccount(_)),
        ) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "error": error.to_string() }),
        ),
        Err(error) => {
            tracing::error!(%error, "ledger failure while posting");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal ledger error" }),
            )
        }
    }
}

/// `GET /api/statements/card-network`: the network's settlement report as CSV.
pub async fn statement(State(shared): State<Arc<Shared>>) -> Response {
    let csv = shared.world.lock().await.published.statement_csv();
    ([(header::CONTENT_TYPE, "text/csv")], csv).into_response()
}

/// `GET /api/files/{name}`, plus `{name}.sha256` and `{name}.sig` companions.
pub async fn file(State(shared): State<Arc<Shared>>, Path(name): Path<String>) -> Response {
    let world = shared.world.lock().await;
    let published = &world.published;
    if let Some(base) = name.strip_suffix(".sha256")
        && let Some(file) = published.file(base)
    {
        return (
            [(header::CONTENT_TYPE, "text/plain")],
            format!("{}  {base}\n", file.sha256_hex),
        )
            .into_response();
    }
    if let Some(base) = name.strip_suffix(".sig")
        && let Some(signature) = published.file(base).and_then(|f| f.signature)
    {
        return (
            [(header::CONTENT_TYPE, "application/octet-stream")],
            signature.to_vec(),
        )
            .into_response();
    }
    match published.file(&name) {
        Some(file) => ([(header::CONTENT_TYPE, "text/csv")], file.content.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("no file named {name:?}") })),
        )
            .into_response(),
    }
}

/// `GET /api/keys/node.pem`: the node's Ed25519 public key.
pub async fn public_key(State(shared): State<Arc<Shared>>) -> Response {
    let pem = shared.world.lock().await.published.public_key_pem.clone();
    ([(header::CONTENT_TYPE, "application/x-pem-file")], pem).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bramble_ledger::AccountKind;

    fn ledger() -> Ledger {
        let ledger = Ledger::open_in_memory().expect("ledger");
        ledger
            .ensure_account("customer:you", AccountKind::Liability)
            .expect("acct");
        ledger
            .ensure_account("customer:alex", AccountKind::Liability)
            .expect("acct");
        ledger
    }

    fn request(amount: i64) -> EntryRequest {
        EntryRequest {
            memo: "Pay Alex".into(),
            postings: vec![
                PostingSpec {
                    account: "customer:you".into(),
                    amount_minor: amount,
                },
                PostingSpec {
                    account: "customer:alex".into(),
                    amount_minor: -amount,
                },
            ],
        }
    }

    #[test]
    fn api_submit_entry_returns_payment_api_status_codes() {
        let mut ledger = ledger();
        let key = || Some("pay-1".to_owned());
        let (status, body) = submit_entry(&mut ledger, request(1200), key());
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["replayed"], false);

        let (status, body) = submit_entry(&mut ledger, request(1200), key());
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.to_string(), r#"{"entry_id":1,"replayed":true}"#);

        let (status, body) = submit_entry(&mut ledger, request(900), key());
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["error"]
                .as_str()
                .expect("msg")
                .contains("already used for a different request")
        );

        let mut unbalanced = request(100);
        unbalanced.postings.pop();
        let (status, body) = submit_entry(&mut ledger, unbalanced, None);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"]
                .as_str()
                .expect("msg")
                .contains("at least two postings")
        );

        let mut ghost = request(100);
        ghost.postings[1].account = "customer:ghost".into();
        let (status, _) = submit_entry(&mut ledger, ghost, None);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
