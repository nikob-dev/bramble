//! Shared fixtures: a world built from the shipped content and a seeded in-memory ledger.

use std::sync::Arc;

use bramble_ledger::Ledger;
use ed25519_dalek::SigningKey;
use tokio::sync::broadcast;

use super::Shared;
use super::net::Net;
use super::rtp::RtpDesk;

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
