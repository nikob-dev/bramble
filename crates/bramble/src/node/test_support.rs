//! Shared fixtures: a world built from the shipped content and a seeded in-memory ledger.

use std::sync::Arc;

use bramble_ledger::Ledger;
use ed25519_dalek::SigningKey;
use tokio::sync::broadcast;

use super::Shared;
use super::net::{Net, serve_local};
use super::operator::{self, Operator};
use super::rtp::{RtpDesk, bank_dispatch};

use super::load_content;
use super::published::Published;
use super::seed::seed;
use super::store::PetStore;
use super::world::{World, WorldParts};

fn content_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content")
}

pub fn world_with(store: PetStore, ledger: Ledger) -> World {
    let (curriculum, lessons) = load_content(&content_dir()).expect("content");
    let key = SigningKey::from_bytes(&[1u8; 32]);
    let published = Published::build(&lessons, &key, String::new());
    World::load(
        WorldParts {
            pet_name: "pip".into(),
            curriculum,
            lessons,
            ledger,
            published,
            store,
        },
        0,
    )
    .expect("world")
}

pub fn world() -> World {
    let mut ledger = Ledger::open_in_memory().expect("ledger");
    seed(&mut ledger).expect("seed");
    world_with(PetStore::open_in_memory().expect("store"), ledger)
}

/// A bank node's shared state around `world`, with network identity `id`.
pub fn shared(id: &str, seed: u8, world: World) -> Arc<Shared> {
    let net = Net::new(
        id,
        SigningKey::from_bytes(&[seed; 32]),
        PetStore::open_in_memory().expect("store"),
    )
    .expect("net");
    let (updates, _) = broadcast::channel(64);
    Arc::new(Shared {
        world: tokio::sync::Mutex::new(world),
        updates,
        net: Arc::new(net),
        rtp: RtpDesk::default(),
        operator_id: "operator".into(),
    })
}

/// Banks (ids `pet-a`, `pet-b`, …, each with the shipped seed) and an operator, each
/// serving its peer endpoint on a local port, all discovered by each other, with their
/// message handlers running: a whole network in one process.
pub async fn network(banks: usize) -> (Vec<Arc<Shared>>, Arc<Operator>) {
    let ids: Vec<String> = (0..banks)
        .map(|i| format!("pet-{}", (b'a' + i as u8) as char))
        .collect();
    let shared: Vec<Arc<Shared>> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| self::shared(id, i as u8 + 1, world()))
        .collect();
    let mut ledger = Ledger::open_in_memory().expect("ledger");
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    bramble_ledger::rails::open_operator(&mut ledger, &refs).expect("open operator");
    let op_net = Arc::new(
        Net::new(
            "operator",
            SigningKey::from_bytes(&[99; 32]),
            PetStore::open_in_memory().expect("store"),
        )
        .expect("net"),
    );
    let op = Operator::new(ledger, op_net.clone(), ids.clone());

    let mut nets: Vec<Arc<Net>> = shared.iter().map(|s| s.net.clone()).collect();
    nets.push(op_net);
    let mut addrs = Vec::new();
    for net in &nets {
        addrs.push(serve_local(net.clone()).await.to_string());
    }
    for (i, net) in nets.iter().enumerate() {
        for (j, addr) in addrs.iter().enumerate() {
            if i != j {
                net.discover(addr).await.expect("discover");
            }
        }
    }
    for bank in &shared {
        tokio::spawn(bank_dispatch(bank.clone()));
    }
    tokio::spawn(operator::dispatch(op.clone()));
    (shared, op)
}
