//! `bramble node`: the pet-bank daemon. Owns the ledger and the pet; serves clients.

mod api;
mod identity;
mod net;
mod operator;
mod published;
mod rtp;
mod seed;
mod server;
mod store;
#[cfg(test)]
mod test_support;
mod world;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use bramble_core::curriculum::Curriculum;
use bramble_core::lesson::Lesson;
use bramble_ledger::Ledger;
use tokio::sync::{Mutex, broadcast};

use crate::config::{Config, Role};
use published::Published;
use store::PetStore;
use world::{World, WorldParts};

const TICK_INTERVAL: Duration = Duration::from_secs(10);

pub struct Shared {
    pub world: Mutex<World>,
    pub updates: broadcast::Sender<bramble_core::protocol::ServerMsg>,
    pub net: Arc<net::Net>,
    pub rtp: rtp::RtpDesk,
    pub operator_id: String,
}

pub fn run(config: Config) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    if config.role == Role::Operator {
        return operator::run(config);
    }

    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
    let mut ledger = Ledger::open(config.ledger_path())?;
    seed::seed(&mut ledger)?;
    let (curriculum, lessons) = load_content(&config.content_dir)?;
    check_setup_accounts(&lessons, &ledger)?;

    let store = PetStore::open(config.pet_store_path())?;
    let key = identity::load_or_create(&store)?;
    // Its own connection to pet.db: peer pins are written from network tasks.
    let net = Arc::new(net::Net::new(
        &config.node_id,
        key.clone(),
        PetStore::open(config.pet_store_path())?,
    )?);
    let published = Published::build(&lessons, &key, identity::public_key_pem(&key)?);
    let world = World::load(
        WorldParts {
            pet_name: config.pet_name.clone(),
            curriculum,
            lessons,
            ledger,
            published,
            store,
        },
        now_unix(),
    )?;

    let (updates, _) = broadcast::channel(64);
    let shared = Arc::new(Shared {
        world: Mutex::new(world),
        updates,
        net: net.clone(),
        rtp: rtp::RtpDesk::default(),
        operator_id: config.operator_id.clone(),
    });

    tokio::runtime::Runtime::new()?.block_on(async move {
        tokio::spawn(tick_loop(shared.clone()));
        tokio::spawn(net::discover_peers(net.clone(), config.peers.clone()));
        tokio::spawn(rtp::bank_dispatch(shared.clone()));
        tokio::try_join!(
            server::serve(&config.bind, shared, &config.web_dir),
            net::serve(&config.net_bind, net),
        )?;
        Ok(())
    })
}

async fn tick_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    loop {
        interval.tick().await;
        let now = now_unix();
        let mut world = shared.world.lock().await;
        match world.tick(now) {
            Ok(Some(say)) => {
                // No subscribers is fine: nobody is watching right now.
                let _ = shared.updates.send(world.snapshot(say, now));
            }
            Ok(None) => {}
            Err(error) => tracing::error!(%error, "pet tick failed"),
        }
    }
}

/// Loads `concepts.toml` and every `lessons/<track>/*.toml`, and checks they agree.
/// Lessons are ordered by path so file names set authoring order.
pub fn load_content(dir: &Path) -> anyhow::Result<(Curriculum, Vec<Lesson>)> {
    let concepts_path = dir.join("concepts.toml");
    let curriculum = Curriculum::from_toml(
        &std::fs::read_to_string(&concepts_path)
            .with_context(|| format!("reading {}", concepts_path.display()))?,
    )
    .with_context(|| format!("loading {}", concepts_path.display()))?;

    let lessons_dir = dir.join("lessons");
    let mut paths = Vec::new();
    for track in std::fs::read_dir(&lessons_dir)
        .with_context(|| format!("reading {}", lessons_dir.display()))?
    {
        for file in std::fs::read_dir(track?.path())? {
            let path = file?.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    let lessons = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)?;
            Lesson::from_toml(&text).with_context(|| format!("loading lesson {}", path.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    curriculum.validate_lessons(&lessons)?;
    Ok((curriculum, lessons))
}

/// Setup entries post to accounts by name; a typo should stop the node at startup,
/// not surface as a confusing error halfway through a lesson.
fn check_setup_accounts(lessons: &[Lesson], ledger: &Ledger) -> anyhow::Result<()> {
    for lesson in lessons {
        for entry in &lesson.setup.entries {
            for account in [&entry.debit, &entry.credit] {
                ledger.balance(account).with_context(|| {
                    format!(
                        "lesson {:?} setup entry {:?} uses account {account:?}",
                        lesson.id, entry.key
                    )
                })?;
            }
        }
    }
    Ok(())
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_shipped_setup_entries_use_seeded_accounts() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let (_, lessons) = load_content(&dir).expect("content");
        let mut ledger = Ledger::open_in_memory().expect("ledger");
        seed::seed(&mut ledger).expect("seed");
        check_setup_accounts(&lessons, &ledger).expect("all setup accounts exist");
    }
}
