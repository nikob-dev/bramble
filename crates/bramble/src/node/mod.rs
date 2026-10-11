//! `bramble node`: the pet-bank daemon. Owns the ledger and the pet; serves clients.

mod seed;
mod server;
mod state;
mod store;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bramble_core::lesson::Lesson;
use bramble_ledger::Ledger;
use tokio::sync::{Mutex, broadcast};

use crate::config::Config;
use state::World;
use store::PetStore;

const TICK_INTERVAL: Duration = Duration::from_secs(10);

pub struct Shared {
    pub world: Mutex<World>,
    pub updates: broadcast::Sender<bramble_core::protocol::ServerMsg>,
}

pub fn run(config: Config) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
    let mut ledger = Ledger::open(config.ledger_path())?;
    seed::seed(&mut ledger)?;
    let lessons = load_lessons(&config.content_dir.join("lessons"))?;
    let store = PetStore::open(config.pet_store_path())?;
    let world = World::load(&config.pet_name, lessons, ledger, store, now_unix())?;

    let (updates, _) = broadcast::channel(64);
    let shared = Arc::new(Shared {
        world: Mutex::new(world),
        updates,
    });

    tokio::runtime::Runtime::new()?.block_on(async move {
        tokio::spawn(tick_loop(shared.clone()));
        server::serve(&config.bind, shared).await
    })
}

async fn tick_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    loop {
        interval.tick().await;
        let mut world = shared.world.lock().await;
        match world.tick(now_unix()) {
            Ok(true) => {
                // No subscribers is fine: nobody is watching right now.
                let _ = shared.updates.send(world.snapshot(None));
            }
            Ok(false) => {}
            Err(error) => tracing::error!(%error, "pet tick failed"),
        }
    }
}

/// Loads every `<track>/*.toml` lesson, ordered by path so file names set curriculum order.
fn load_lessons(dir: &Path) -> anyhow::Result<Vec<Lesson>> {
    let mut paths = Vec::new();
    for track in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        for file in std::fs::read_dir(track?.path())? {
            let path = file?.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)?;
            Lesson::from_toml(&text).with_context(|| format!("loading lesson {}", path.display()))
        })
        .collect()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}
