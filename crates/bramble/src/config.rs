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
        }
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.data_dir.join("ledger.db")
    }

    pub fn pet_store_path(&self) -> PathBuf {
        self.data_dir.join("pet.db")
    }
}
