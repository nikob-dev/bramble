//! The node's network face (decision 011): who its peers are, and how signed envelopes
//! move between them over plain HTTP on the compose network.
//!
//! - `GET /net/identity` tells a peer our id and public key.
//! - `POST /net/envelope` takes one signed envelope: 202 if new, 200 if it's a resend of
//!   one we already accepted (so retries are safe), 401 if we can't authenticate it,
//!   400 if it's authentic but unusable.
//!
//! Peer keys are pinned on first contact and persisted: a peer that later shows up with
//! a different key is refused, not silently trusted.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use bramble_core::net::{Envelope, NetMsg};
use bramble_net::{Rejection, Verifier};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::now_unix;
use super::store::PetStore;

const PINS_DOC: &str = "peer_keys";
#[allow(
    dead_code,
    reason = "first caller is the RTP flow (docs/tasks/m4.md T7)"
)]
const SEND_ATTEMPTS: u32 = 4;
const DISCOVERY_RETRY: Duration = Duration::from_secs(2);

/// What `GET /net/identity` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: String,
    pub public_key_hex: String,
}

/// An authenticated message from a peer, for whoever handles that kind (RTP, ACH, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub from: String,
    pub msg: NetMsg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    Accepted,
    /// A resend of an envelope we already accepted: nothing to do, and not an error.
    Duplicate,
    Rejected(Rejection),
}

#[allow(
    dead_code,
    reason = "first caller is the RTP flow (docs/tasks/m4.md T7)"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    AlreadyDelivered,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReceiptBody {
    status: String,
    #[serde(default)]
    error: Option<String>,
}

pub struct Net {
    pub id: String,
    key: SigningKey,
    verifier: Mutex<Verifier>,
    /// Peer id → base URL (`http://host:port`), filled in by discovery.
    peers: Mutex<HashMap<String, String>>,
    pins: Mutex<PetStore>,
    client: reqwest::Client,
    pub inbox: broadcast::Sender<Inbound>,
}

impl Net {
    pub fn new(id: &str, key: SigningKey, pins: PetStore) -> anyhow::Result<Self> {
        let pinned: HashMap<String, String> = pins.load(PINS_DOC)?.unwrap_or_default();
        let mut verifier = Verifier::new(HashMap::new());
        for (peer, hex_key) in &pinned {
            verifier.add_peer(peer, parse_key(hex_key)?);
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("building the peer HTTP client")?;
        let (inbox, _) = broadcast::channel(256);
        Ok(Self {
            id: id.to_owned(),
            key,
            verifier: Mutex::new(verifier),
            peers: Mutex::new(HashMap::new()),
            pins: Mutex::new(pins),
            client,
            inbox,
        })
    }

    pub fn identity(&self) -> Identity {
        Identity {
            id: self.id.clone(),
            public_key_hex: hex::encode(self.key.verifying_key().to_bytes()),
        }
    }

    /// Asks the node at `addr` who it is, pins its key, and remembers where it lives.
    pub async fn discover(&self, addr: &str) -> anyhow::Result<String> {
        let base = format!("http://{addr}");
        let identity: Identity = self
            .client
            .get(format!("{base}/net/identity"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if identity.id == self.id {
            bail!("peer at {addr} claims our own id {:?}", self.id);
        }
        let key = parse_key(&identity.public_key_hex)?;
        self.pin(&identity.id, &identity.public_key_hex)?;
        self.verifier
            .lock()
            .expect("verifier lock")
            .add_peer(&identity.id, key);
        self.peers
            .lock()
            .expect("peers lock")
            .insert(identity.id.clone(), base);
        Ok(identity.id)
    }

    fn pin(&self, peer: &str, hex_key: &str) -> anyhow::Result<()> {
        let pins = self.pins.lock().expect("pins lock");
        let mut pinned: HashMap<String, String> = pins.load(PINS_DOC)?.unwrap_or_default();
        match pinned.get(peer) {
            Some(known) if known == hex_key => Ok(()),
            Some(_) => bail!(
                "peer {peer:?} presented a different key than the one pinned on first contact; \
                 refusing it (reset this node's data to re-pin)"
            ),
            None => {
                pinned.insert(peer.to_owned(), hex_key.to_owned());
                pins.save(PINS_DOC, &pinned)?;
                tracing::info!(peer, "pinned peer key on first contact");
                Ok(())
            }
        }
    }

    /// Authenticates an envelope and, if it's new, hands its message to the inbox.
    pub fn receive(&self, envelope: &Envelope, now_unix: i64) -> Receipt {
        let verdict = self
            .verifier
            .lock()
            .expect("verifier lock")
            .verify(envelope, now_unix);
        match verdict {
            Ok(msg) => {
                tracing::info!(from = %envelope.from, kind = %envelope.kind, "envelope accepted");
                // No subscribers yet is fine: nothing handles this kind until later tasks.
                let _ = self.inbox.send(Inbound {
                    from: envelope.from.clone(),
                    msg,
                });
                Receipt::Accepted
            }
            Err(Rejection::Replay { .. }) => Receipt::Duplicate,
            Err(rejection) => {
                tracing::warn!(from = %envelope.from, %rejection, "envelope rejected");
                Receipt::Rejected(rejection)
            }
        }
    }

    /// Signs `msg` once and delivers it, retrying the *same* envelope on transient
    /// failures, so the receiver sees at most one new message however many tries it takes.
    #[allow(
        dead_code,
        reason = "first caller is the RTP flow (docs/tasks/m4.md T7)"
    )]
    pub async fn send(&self, to: &str, msg: &NetMsg) -> anyhow::Result<Delivery> {
        let envelope = bramble_net::seal(msg, &self.id, &self.key, now_unix());
        self.deliver(to, &envelope).await
    }

    #[allow(
        dead_code,
        reason = "first caller is the RTP flow (docs/tasks/m4.md T7)"
    )]
    pub async fn deliver(&self, to: &str, envelope: &Envelope) -> anyhow::Result<Delivery> {
        let base = self
            .peers
            .lock()
            .expect("peers lock")
            .get(to)
            .cloned()
            .with_context(|| format!("unknown peer {to:?}"))?;
        let url = format!("{base}/net/envelope");
        let mut last_error = None;
        for attempt in 0..SEND_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt - 1))).await;
            }
            match self.client.post(&url).json(envelope).send().await {
                Ok(response) => match response.status() {
                    StatusCode::ACCEPTED => return Ok(Delivery::Delivered),
                    StatusCode::OK => return Ok(Delivery::AlreadyDelivered),
                    status if status.is_client_error() => {
                        let body: Option<ReceiptBody> = response.json().await.ok();
                        let reason = body.and_then(|b| b.error).unwrap_or_default();
                        bail!("{to} refused the envelope ({status}): {reason}");
                    }
                    status => last_error = Some(anyhow::anyhow!("{to} answered {status}")),
                },
                Err(error) => last_error = Some(error.into()),
            }
            tracing::warn!(to, attempt, "envelope delivery failed; retrying");
        }
        Err(last_error
            .expect("at least one attempt")
            .context(format!("delivering to {to}")))
    }
}

fn parse_key(hex_key: &str) -> anyhow::Result<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(hex_key)
        .context("peer key is not hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer key is not 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes).context("peer key is not a valid Ed25519 point")
}

/// Keeps trying each peer until it answers: containers start in any order.
pub async fn discover_peers(net: Arc<Net>, addrs: Vec<String>) {
    for addr in addrs {
        let net = net.clone();
        tokio::spawn(async move {
            loop {
                match net.discover(&addr).await {
                    Ok(id) => {
                        tracing::info!(peer = %id, %addr, "peer discovered");
                        return;
                    }
                    Err(error) if error.to_string().contains("pinned") => {
                        tracing::error!(%addr, %error, "peer refused");
                        return;
                    }
                    Err(error) => {
                        tracing::debug!(%addr, %error, "peer not reachable yet");
                        tokio::time::sleep(DISCOVERY_RETRY).await;
                    }
                }
            }
        });
    }
}

pub fn router(net: Arc<Net>) -> Router {
    Router::new()
        .route("/net/identity", get(identity))
        .route("/net/envelope", post(envelope))
        .with_state(net)
}

pub async fn serve(bind: &str, net: Arc<Net>) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, id = %net.id, "peer listener up");
    axum::serve(listener, router(net)).await?;
    Ok(())
}

/// For tests: serve on an ephemeral local port.
#[cfg(test)]
pub async fn serve_local(net: Arc<Net>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, router(net)).await });
    addr
}

async fn identity(State(net): State<Arc<Net>>) -> Json<Identity> {
    Json(net.identity())
}

async fn envelope(
    State(net): State<Arc<Net>>,
    Json(envelope): Json<Envelope>,
) -> impl IntoResponse {
    let (status, label, error) = match net.receive(&envelope, now_unix()) {
        Receipt::Accepted => (StatusCode::ACCEPTED, "accepted", None),
        Receipt::Duplicate => (StatusCode::OK, "duplicate", None),
        Receipt::Rejected(rejection) => {
            let status = match rejection {
                Rejection::UnknownSender(_) | Rejection::BadSignature => StatusCode::UNAUTHORIZED,
                _ => StatusCode::BAD_REQUEST,
            };
            (status, "rejected", Some(rejection.to_string()))
        }
    };
    (
        status,
        Json(ReceiptBody {
            status: label.to_owned(),
            error,
        }),
    )
}

#[cfg(test)]
mod tests {
    use bramble_core::net::Rail;

    use super::*;

    fn node(id: &str, seed: u8) -> Arc<Net> {
        node_with(id, seed, PetStore::open_in_memory().expect("store"))
    }

    fn node_with(id: &str, seed: u8, pins: PetStore) -> Arc<Net> {
        Arc::new(Net::new(id, SigningKey::from_bytes(&[seed; 32]), pins).expect("net"))
    }

    fn notice() -> NetMsg {
        NetMsg::SettlementNotice {
            settlement_id: "rtp-0001".into(),
            rail: Rail::Rtp,
            bank: "pet-b".into(),
            amount_minor: 2500,
            reference: "pet-a-0001".into(),
        }
    }

    /// Two nodes that have discovered each other.
    async fn pair() -> (Arc<Net>, Arc<Net>) {
        let a = node("pet-a", 1);
        let b = node("pet-b", 2);
        let a_addr = serve_local(a.clone()).await;
        let b_addr = serve_local(b.clone()).await;
        assert_eq!(a.discover(&b_addr.to_string()).await.expect("a→b"), "pet-b");
        assert_eq!(b.discover(&a_addr.to_string()).await.expect("b→a"), "pet-a");
        (a, b)
    }

    #[tokio::test]
    async fn net_two_nodes_deliver_one_envelope_and_a_retry_is_a_no_op() {
        let (a, b) = pair().await;
        let mut inbox = b.inbox.subscribe();

        let envelope = bramble_net::seal(&notice(), "pet-a", &a.key, now_unix());
        assert_eq!(
            a.deliver("pet-b", &envelope).await.expect("send"),
            Delivery::Delivered
        );
        assert_eq!(
            inbox.try_recv().expect("delivered"),
            Inbound {
                from: "pet-a".into(),
                msg: notice()
            }
        );

        assert_eq!(
            a.deliver("pet-b", &envelope).await.expect("resend"),
            Delivery::AlreadyDelivered
        );
        assert!(inbox.try_recv().is_err(), "the retry delivered nothing new");
    }

    #[tokio::test]
    async fn net_send_seals_a_fresh_envelope_each_call() {
        let (a, b) = pair().await;
        let mut inbox = b.inbox.subscribe();
        for _ in 0..2 {
            assert_eq!(
                a.send("pet-b", &notice()).await.expect("send"),
                Delivery::Delivered
            );
        }
        assert!(
            inbox.try_recv().is_ok() && inbox.try_recv().is_ok(),
            "two distinct messages"
        );
    }

    #[tokio::test]
    async fn net_rejects_envelopes_from_undiscovered_nodes() {
        let (_, b) = pair().await;
        let stranger = node("pet-z", 9);
        let b_addr = serve_local(b.clone()).await;
        stranger
            .discover(&b_addr.to_string())
            .await
            .expect("stranger finds b");
        let error = stranger
            .send("pet-b", &notice())
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[tokio::test]
    async fn net_refuses_a_peer_whose_key_changed_since_it_was_pinned() {
        let a = node("pet-a", 1);
        let original = node("pet-b", 2);
        let addr = serve_local(original).await;
        a.discover(&addr.to_string())
            .await
            .expect("first contact pins");

        let impostor = node("pet-b", 3);
        let addr = serve_local(impostor).await;
        let error = a.discover(&addr.to_string()).await.expect_err("refused");
        assert!(error.to_string().contains("pinned"), "{error}");
    }

    #[tokio::test]
    async fn net_pins_survive_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pet.db");
        let b = node("pet-b", 2);
        let b_addr = serve_local(b).await;
        {
            let a = node_with("pet-a", 1, PetStore::open(&path).expect("store"));
            a.discover(&b_addr.to_string()).await.expect("pin");
        }
        let a = node_with("pet-a", 1, PetStore::open(&path).expect("store"));
        let impostor = node("pet-b", 3);
        let addr = serve_local(impostor).await;
        assert!(
            a.discover(&addr.to_string()).await.is_err(),
            "pin persisted"
        );
    }

    #[tokio::test]
    async fn net_receive_rejects_a_tampered_envelope() {
        let (a, b) = pair().await;
        let mut envelope = bramble_net::seal(&notice(), "pet-a", &a.key, now_unix());
        envelope.payload = envelope.payload.replace("2500", "250000");
        assert_eq!(
            b.receive(&envelope, now_unix()),
            Receipt::Rejected(Rejection::BadSignature)
        );
    }

    #[tokio::test]
    async fn net_delivery_retries_until_the_peer_comes_up() {
        let a = node("pet-a", 1);
        let b = node("pet-b", 2);
        // Learn b's identity, then take b down and bring it back on the same port.
        // Bind before spawning so the server is listening when `discover` connects.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = {
            let b = b.clone();
            tokio::spawn(async move { axum::serve(listener, router(b)).await })
        };
        a.discover(&addr.to_string()).await.expect("discover");
        b.verifier
            .lock()
            .expect("lock")
            .add_peer("pet-a", a.key.verifying_key());
        server.abort();
        let _ = server.await;

        let restart = {
            let b = b.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let listener = tokio::net::TcpListener::bind(addr).await.expect("rebind");
                axum::serve(listener, router(b)).await
            })
        };
        assert_eq!(
            a.send("pet-b", &notice()).await.expect("retried"),
            Delivery::Delivered
        );
        restart.abort();
    }
}
