use std::path::PathBuf;

/// Runtime configuration, read from environment variables (set in the Docker image).
#[derive(Debug, Clone)]
pub struct Config {
    pub pet_name: String,
    pub data_dir: PathBuf,
    pub content_dir: PathBuf,
    pub bind: String,
    pub node_url: String,
    pub lab_dir: PathBuf,
    pub web_dir: PathBuf,
    /// This node's id on the network; peers address it by this name.
    pub node_id: String,
    /// Listener for node-to-node envelopes. Never published to the host.
    pub net_bind: String,
    /// Peer addresses (`host:port`), e.g. `pet-b:9090,pet-c:9090`.
    pub peers: Vec<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let var =
            |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.to_owned());
        let home = var("HOME", "/tmp");
        Self {
            pet_name: var("BRAMBLE_PET_NAME", "pip"),
            data_dir: var("BRAMBLE_DATA_DIR", "./data").into(),
            content_dir: var("BRAMBLE_CONTENT_DIR", "./content").into(),
            bind: var("BRAMBLE_BIND", "0.0.0.0:8080"),
            node_url: var("BRAMBLE_NODE_URL", "ws://127.0.0.1:8080/ws"),
            lab_dir: var("BRAMBLE_LAB_DIR", &format!("{home}/lab")).into(),
            web_dir: var("BRAMBLE_WEB_DIR", "./crates/bramble-web/dist").into(),
            node_id: var("BRAMBLE_NODE_ID", "pet-a"),
            net_bind: var("BRAMBLE_NET_BIND", "0.0.0.0:9090"),
            peers: parse_peers(&var("BRAMBLE_PEERS", "")),
        }
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.data_dir.join("ledger.db")
    }

    pub fn pet_store_path(&self) -> PathBuf {
        self.data_dir.join("pet.db")
    }
}

fn parse_peers(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|peer| !peer.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parse_peers_ignores_blanks_and_spaces() {
        assert_eq!(
            parse_peers(" pet-b:9090, ,pet-c:9090,"),
            ["pet-b:9090", "pet-c:9090"]
        );
        assert!(parse_peers("").is_empty());
    }
}
