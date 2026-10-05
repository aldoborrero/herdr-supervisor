//! Ed25519 did:key identities and signed application records, not ATProto federation.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::model::{digest, id};

pub struct Identity(SigningKey);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(rename = "$type")]
    pub record_type: String,
    pub id: String,
    pub owner: String,
    pub author: String,
    pub recipient: String,
    pub epoch: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRecord {
    pub record: Record,
    pub content_hash: String,
    pub signature: String,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let mut seed = [0; 32];
        getrandom::getrandom(&mut seed).map_err(|e| anyhow::anyhow!("OS randomness: {e}"))?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    pub fn load(path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::symlink_metadata(path)?;
        ensure!(
            m.is_file() && m.permissions().mode() & 0o077 == 0,
            "identity file must be private and regular"
        );
        let bytes: [u8; 32] = std::fs::read(path)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid private key length"))?;
        Ok(Self(SigningKey::from_bytes(&bytes)))
    }

    pub fn save_new(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&self.0.to_bytes())?;
        file.sync_all()?;
        std::fs::File::open(path.parent().context("key parent")?)?.sync_all()?;
        Ok(())
    }

    pub fn did(&self) -> String {
        let mut bytes = vec![0xed, 0x01]; // unsigned varint multicodec ed25519-pub
        bytes.extend(self.0.verifying_key().as_bytes());
        format!("did:key:z{}", bs58::encode(bytes).into_string())
    }

    pub fn sign(&self, record: Record) -> Result<SignedRecord> {
        ensure!(record.author == self.did(), "author/key mismatch");
        let bytes = serde_json::to_vec(&record)?;
        let signature = bs58::encode(self.0.sign(&bytes).to_bytes()).into_string();
        Ok(SignedRecord {
            record,
            content_hash: digest(&bytes),
            signature,
        })
    }

    pub fn delegation(
        &self,
        node: &str,
        node_did: &str,
        epoch: u64,
        expires: i64,
    ) -> Result<SignedRecord> {
        super::broker_policy::node_id(node)?;
        verifying_key(node_did)?;
        let now = jiff::Timestamp::now().as_second();
        self.sign(Record {
            record_type: "local.herdr.supervisor.nodeGrant".into(),
            id: id(),
            owner: self.did(),
            author: self.did(),
            recipient: node_did.into(),
            epoch,
            created_at: now,
            expires_at: expires,
            payload: serde_json::json!({"node": node, "key": node_did}),
        })
    }
}

pub fn verifying_key(did: &str) -> Result<VerifyingKey> {
    let encoded = did
        .strip_prefix("did:key:z")
        .context("expected Ed25519 did:key")?;
    ensure!(encoded.len() <= 64, "DID too long");
    let bytes = bs58::decode(encoded).into_vec()?;
    ensure!(
        bytes.len() == 34 && bytes[..2] == [0xed, 1],
        "invalid Ed25519 multicodec"
    );
    let key: [u8; 32] = bytes[2..].try_into()?;
    Ok(VerifyingKey::from_bytes(&key)?)
}

impl SignedRecord {
    /// Authentication only. The receiver must also authorize owner, role, route and epoch.
    pub fn verify(&self, now: i64) -> Result<()> {
        ensure!(
            self.record.created_at <= now.saturating_add(30)
                && self.record.expires_at > now
                && self.record.expires_at > self.record.created_at,
            "record expired or invalid time"
        );
        ensure!(
            self.record
                .record_type
                .starts_with("local.herdr.supervisor."),
            "unsupported record namespace"
        );
        uuid::Uuid::parse_str(&self.record.id)?;
        let bytes = serde_json::to_vec(&self.record)?;
        ensure!(
            bytes.len() <= 262144 && digest(&bytes) == self.content_hash,
            "record content mismatch or too large"
        );
        let sig = bs58::decode(&self.signature).into_vec()?;
        verifying_key(&self.record.author)?.verify_strict(&bytes, &Signature::from_slice(&sig)?)?;
        Ok(())
    }
}
