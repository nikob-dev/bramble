//! Shared fixtures: a world built from the shipped content and a seeded in-memory ledger.

use bramble_ledger::Ledger;
use ed25519_dalek::SigningKey;

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
