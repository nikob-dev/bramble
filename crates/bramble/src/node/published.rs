//! Files and reports the node publishes for lessons: payment batches, their hashes and
//! signatures, and the card network's settlement report.

use std::collections::BTreeMap;

use bramble_core::lesson::{Lesson, StatementLine};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

pub struct PublishedFile {
    pub content: Vec<u8>,
    pub sha256_hex: String,
    pub signature: Option<[u8; 64]>,
}

pub struct Published {
    files: BTreeMap<String, PublishedFile>,
    statement: Vec<StatementLine>,
    pub public_key_pem: String,
}

impl Published {
    pub fn build(lessons: &[Lesson], key: &SigningKey, public_key_pem: String) -> Self {
        let mut files = BTreeMap::new();
        let mut statement = Vec::new();
        for lesson in lessons {
            for file in &lesson.setup.files {
                let content = file.content.as_bytes().to_vec();
                files.insert(
                    file.name.clone(),
                    PublishedFile {
                        sha256_hex: hex::encode(Sha256::digest(&content)),
                        signature: file.sign.then(|| key.sign(&content).to_bytes()),
                        content,
                    },
                );
            }
            statement.extend(lesson.setup.statement.iter().cloned());
        }
        Self {
            files,
            statement,
            public_key_pem,
        }
    }

    pub fn file(&self, name: &str) -> Option<&PublishedFile> {
        self.files.get(name)
    }

    /// Facts lessons can check output against, e.g. `sha256:batch-001.csv`.
    pub fn fact(&self, name: &str) -> Option<String> {
        let file = name.strip_prefix("sha256:")?;
        self.files.get(file).map(|f| f.sha256_hex.clone())
    }

    pub fn statement_csv(&self) -> String {
        let mut csv = String::from("network_ref,description,amount_minor\n");
        for line in &self.statement {
            csv.push_str(&format!(
                "{},{},{}\n",
                line.network_ref, line.description, line.amount_minor
            ));
        }
        csv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    fn lesson() -> Lesson {
        Lesson::from_toml(
            r#"
            id = "t"
            title = "T"
            concept = "c"
            [[steps]]
            kind = "explain"
            text = "x"
            [[setup.files]]
            name = "batch.csv"
            content = "a,b\n1,2\n"
            sign = true
            [[setup.statement]]
            network_ref = "NR-1"
            description = "Shop"
            amount_minor = 100
            "#,
        )
        .expect("lesson")
    }

    #[test]
    fn published_build_hashes_and_signs_files() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let published = Published::build(&[lesson()], &key, String::new());
        let file = published.file("batch.csv").expect("file");
        // Computed independently: printf 'a,b\n1,2\n' | shasum -a 256
        let expected = "492d5ea496056f1a6a6592241032fab764c321596317930b4fa0e1e8bc3b7470";
        assert_eq!(file.sha256_hex, expected);
        assert_eq!(
            published.fact("sha256:batch.csv").as_deref(),
            Some(expected)
        );
        let signature = Signature::from_bytes(&file.signature.expect("signed"));
        assert!(
            key.verifying_key()
                .verify(&file.content, &signature)
                .is_ok()
        );
        assert!(key.verifying_key().verify(b"tampered", &signature).is_err());
        assert_eq!(published.fact("sha256:missing.csv"), None);
    }

    #[test]
    fn published_statement_csv_has_header_and_lines() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let csv = Published::build(&[lesson()], &key, String::new()).statement_csv();
        assert_eq!(csv, "network_ref,description,amount_minor\nNR-1,Shop,100\n");
    }
}
