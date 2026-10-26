use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Default, Deserialize, Serialize)]
pub struct Index {
    pub format_version: u32,
    pub builds: BTreeMap<String, Record>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Record {
    pub archive: String,
    pub fingerprint: String,
    pub files: BTreeMap<String, String>,
}

impl Record {
    pub fn new(archive: String, files: BTreeMap<String, String>) -> Result<Self> {
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&files)?));
        Ok(Self {
            archive,
            fingerprint,
            files,
        })
    }
}
