//! Messages exchanged between nodes (banks and the operator) over the network.
//!
//! An [`Envelope`] carries one message. Its payload travels as a JSON *string*, not a
//! nested object, so the signature covers exact bytes and a receiver can verify the
//! signature before it parses anything the sender wrote (decision 011). Signing and
//! verification live in `bramble-net`; this module only defines the shapes.

use serde::{Deserialize, Serialize};

/// Major version of the node-to-node protocol. Receivers reject other majors.
pub const NET_VERSION: u16 = 1;

/// Domain-separation prefix, so an envelope signature can never be replayed as a
/// signature over some other kind of document the node signs (e.g. payment files).
const SIGNING_DOMAIN: &str = "bramble-envelope";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u16,
    pub kind: String,
    /// Sender's node id (e.g. `pet-a`, `operator`).
    pub from: String,
    /// Unix seconds when sent; receivers reject stale envelopes.
    pub ts: i64,
    /// Random, hex; receivers reject a nonce they've already seen.
    pub nonce: String,
    /// The message, JSON-encoded (see the module docs).
    pub payload: String,
    /// Base64 Ed25519 signature over [`Envelope::signing_bytes`]; empty until signed.
    #[serde(default)]
    pub sig: String,
}

impl Envelope {
    /// Wraps a message, unsigned.
    pub fn new(msg: &NetMsg, from: &str, ts: i64, nonce: &str) -> Self {
        Self {
            v: NET_VERSION,
            kind: msg.kind().to_owned(),
            from: from.to_owned(),
            ts,
            nonce: nonce.to_owned(),
            payload: serde_json::to_string(msg).expect("NetMsg always serializes"),
            sig: String::new(),
        }
    }

    /// The exact bytes that get signed: every field except `sig`, newline-separated.
    /// Fields can't contain a raw newline (`payload` is JSON, which escapes them), so
    /// the encoding is unambiguous.
    pub fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "{SIGNING_DOMAIN}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.v, self.kind, self.from, self.ts, self.nonce, self.payload
        )
        .into_bytes()
    }

    /// Parses the payload. Call only after the signature has been verified.
    pub fn message(&self) -> Result<NetMsg, NetError> {
        if !KNOWN_KINDS.contains(&self.kind.as_str()) {
            return Err(NetError::UnknownKind(self.kind.clone()));
        }
        let msg: NetMsg = serde_json::from_str(&self.payload)
            .map_err(|error| NetError::BadPayload(error.to_string()))?;
        if msg.kind() != self.kind {
            return Err(NetError::KindMismatch {
                envelope: self.kind.clone(),
                payload: msg.kind().to_owned(),
            });
        }
        Ok(msg)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("unknown message kind {0:?}")]
    UnknownKind(String),
    #[error("payload doesn't parse: {0}")]
    BadPayload(String),
    #[error("envelope says {envelope:?} but payload is {payload:?}")]
    KindMismatch { envelope: String, payload: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rail {
    Rtp,
    Ach,
}

const KNOWN_KINDS: [&str; 4] = [
    "rtp_credit_transfer",
    "rtp_status",
    "ach_file",
    "settlement_notice",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetMsg {
    /// Push payment on the instant rail. The real-world message is ISO 20022 `pacs.008`.
    RtpCreditTransfer {
        /// Sender-chosen id; doubles as the idempotency key for retries.
        msg_id: String,
        debtor_bank: String,
        debtor_account: String,
        creditor_bank: String,
        creditor_account: String,
        amount_minor: i64,
        memo: String,
    },
    /// Accept or reject a credit transfer (ISO 20022 `pacs.002`). Acceptance is final.
    RtpStatus {
        msg_id: String,
        accepted: bool,
        #[serde(default)]
        reason: Option<String>,
    },
    /// A Nacha-format file of batched ACH entries, as text.
    AchFile { file_id: String, contents: String },
    /// The operator moved money in a bank's settlement account. Positive amounts are
    /// credits to the bank (it received funds); negative amounts are debits.
    SettlementNotice {
        settlement_id: String,
        rail: Rail,
        bank: String,
        amount_minor: i64,
        reference: String,
    },
}

impl NetMsg {
    pub fn kind(&self) -> &'static str {
        match self {
            NetMsg::RtpCreditTransfer { .. } => "rtp_credit_transfer",
            NetMsg::RtpStatus { .. } => "rtp_status",
            NetMsg::AchFile { .. } => "ach_file",
            NetMsg::SettlementNotice { .. } => "settlement_notice",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer() -> NetMsg {
        NetMsg::RtpCreditTransfer {
            msg_id: "pet-a-0001".into(),
            debtor_bank: "pet-a".into(),
            debtor_account: "customer:you".into(),
            creditor_bank: "pet-b".into(),
            creditor_account: "customer:sam".into(),
            amount_minor: 2500,
            memo: "Rent share\nsplit".into(),
        }
    }

    fn all_messages() -> Vec<NetMsg> {
        vec![
            transfer(),
            NetMsg::RtpStatus {
                msg_id: "pet-a-0001".into(),
                accepted: false,
                reason: Some("insufficient reserves".into()),
            },
            NetMsg::AchFile {
                file_id: "pet-a-20261011-A".into(),
                contents: "101 ...\n5200...".into(),
            },
            NetMsg::SettlementNotice {
                settlement_id: "ach-0001".into(),
                rail: Rail::Ach,
                bank: "pet-b".into(),
                amount_minor: -1300,
                reference: "cutoff 1".into(),
            },
        ]
    }

    #[test]
    fn net_envelope_round_trips_every_message_kind() {
        for msg in all_messages() {
            let envelope = Envelope::new(&msg, "pet-a", 1_760_000_000, "ab12");
            let json = serde_json::to_string(&envelope).expect("serialize");
            let back: Envelope = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, envelope);
            assert_eq!(back.message().expect("message"), msg);
        }
    }

    #[test]
    fn net_known_kinds_match_every_variant() {
        let kinds: Vec<_> = all_messages().iter().map(NetMsg::kind).collect();
        assert_eq!(kinds, KNOWN_KINDS);
    }

    #[test]
    fn net_message_rejects_unknown_kind() {
        let mut envelope = Envelope::new(&transfer(), "pet-a", 0, "n");
        envelope.kind = "card_authorization".into();
        assert_eq!(
            envelope.message(),
            Err(NetError::UnknownKind("card_authorization".into()))
        );
    }

    #[test]
    fn net_message_rejects_kind_that_disagrees_with_payload() {
        let mut envelope = Envelope::new(&transfer(), "pet-a", 0, "n");
        envelope.kind = "ach_file".into();
        assert!(matches!(
            envelope.message(),
            Err(NetError::KindMismatch { .. })
        ));
    }

    #[test]
    fn net_message_rejects_malformed_payload() {
        let mut envelope = Envelope::new(&transfer(), "pet-a", 0, "n");
        envelope.payload = r#"{"kind":"rtp_status"}"#.into();
        envelope.kind = "rtp_status".into();
        assert!(matches!(envelope.message(), Err(NetError::BadPayload(_))));
    }

    #[test]
    fn net_signing_bytes_cover_every_field_but_sig() {
        let envelope = Envelope::new(&transfer(), "pet-a", 42, "n1");
        let base = envelope.signing_bytes();
        let mut signed = envelope.clone();
        signed.sig = "c2ln".into();
        assert_eq!(signed.signing_bytes(), base, "sig is not signed");

        let tampered: Vec<Envelope> = vec![
            Envelope {
                v: 2,
                ..envelope.clone()
            },
            Envelope {
                kind: "x".into(),
                ..envelope.clone()
            },
            Envelope {
                from: "pet-b".into(),
                ..envelope.clone()
            },
            Envelope {
                ts: 43,
                ..envelope.clone()
            },
            Envelope {
                nonce: "n2".into(),
                ..envelope.clone()
            },
            Envelope {
                payload: "{}".into(),
                ..envelope.clone()
            },
        ];
        for changed in tampered {
            assert_ne!(changed.signing_bytes(), base, "{changed:?}");
        }
    }

    #[test]
    fn net_signing_bytes_are_unambiguous_with_newlines_in_memo() {
        let envelope = Envelope::new(&transfer(), "pet-a", 42, "n1");
        let text = String::from_utf8(envelope.signing_bytes()).expect("utf8");
        assert_eq!(
            text.lines().count(),
            7,
            "payload JSON escapes the memo newline"
        );
        assert!(text.starts_with("bramble-envelope\n1\nrtp_credit_transfer\npet-a\n42\nn1\n"));
    }
}
