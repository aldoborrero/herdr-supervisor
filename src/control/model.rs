use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn token() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("OS randomness: {e}"))?;
    Ok(bs58::encode(bytes).into_string())
}

pub fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 100,
        "identifier length must be 1..100"
    );
    ensure!(
        value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "invalid identifier"
    );
    Ok(())
}

pub fn oid(value: &str) -> Result<()> {
    ensure!(
        [40, 64].contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "expected a full Git OID"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub runtime: String,
    pub argv: Vec<String>,
    pub limit: u32,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["codex", "claude", "pi"].contains(&self.runtime.as_str()),
            "unsupported runtime"
        );
        ensure!(self.limit > 0, "profile limit is zero");
        super::runtime::validate_args(&self.runtime, &self.argv)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Active execution slots, including uncertain processes and reviewers.
    pub max_active: u32,
    pub reserved_reviewers: u32,
    pub max_attempts: u32,
    pub duration_secs: u64,
    pub profiles: BTreeMap<String, Profile>,
    /// Named argv arrays, chosen by the owner, not executable strings from workers.
    pub checks: BTreeMap<String, Vec<String>>,
    pub require_review: bool,
}

impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_active > 0 && self.max_active <= 128,
            "invalid active limit"
        );
        ensure!(
            self.reserved_reviewers < self.max_active,
            "review reserve consumes every slot"
        );
        ensure!(
            !self.require_review || self.reserved_reviewers > 0,
            "independent review requires a reserved slot"
        );
        ensure!(
            self.max_attempts > 0 && self.max_attempts <= 100,
            "invalid attempt limit"
        );
        ensure!(
            self.duration_secs > 0 && self.duration_secs <= 30 * 86400,
            "invalid run duration"
        );
        ensure!(
            !self.profiles.is_empty(),
            "at least one profile is required"
        );
        for (name, p) in &self.profiles {
            identifier(name)?;
            p.validate()?;
        }
        for (name, argv) in &self.checks {
            identifier(name)?;
            ensure!(
                !argv.is_empty() && !argv[0].is_empty(),
                "empty check command"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub title: String,
    pub instructions: String,
    pub criteria: Vec<String>,
    pub kind: String,
    pub checks: Vec<String>,
    pub memory_topics: Vec<String>,
}

impl Contract {
    pub fn validate(&self, policy: &Policy) -> Result<()> {
        self.validate_allowed_checks(|name| policy.checks.contains_key(name))
    }

    pub fn validate_allowed_checks(&self, allowed: impl Fn(&str) -> bool) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 512,
            "invalid title"
        );
        ensure!(
            !self.instructions.trim().is_empty() && self.instructions.len() <= 65536,
            "invalid instructions"
        );
        ensure!(
            !self.criteria.is_empty() && self.criteria.iter().all(|s| !s.trim().is_empty()),
            "acceptance criteria required"
        );
        ensure!(
            ["code", "artifact"].contains(&self.kind.as_str()),
            "invalid contract kind"
        );
        for check in &self.checks {
            ensure!(allowed(check), "check is not authorized by run policy");
        }
        for topic in &self.memory_topics {
            identifier(topic)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub project: String,
    pub repo: String,
    pub base_oid: String,
    pub integration_oid: String,
    pub objective: String,
    pub policy: Policy,
    pub state: String,
    pub revision: u64,
    pub deadline: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub run: String,
    pub contract: Contract,
    pub contract_hash: String,
    pub state: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub task: String,
    pub run: String,
    pub number: u64,
    pub epoch: u64,
    pub node: String,
    pub profile: String,
    pub role: String,
    pub base_oid: String,
    pub contract_hash: String,
    pub state: String,
    pub native_session: Option<String>,
    pub resource: Option<Resource>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub repo_common_dir: String,
    pub worktree: String,
    pub branch: String,
    pub workspace: String,
    pub pane: String,
    pub terminal: String,
    pub session: String,
    pub runtime: String,
    pub mailer_agent: String,
    pub mailer_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub role: String,
    pub run: Option<String>,
    pub attempt: Option<String>,
    pub node: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub id: String,
    pub run: String,
    pub task: String,
    pub attempt: String,
    pub base_oid: String,
    pub result_oid: String,
    pub candidate_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<super::artifacts::Candidate>,
    pub tree_oid: Option<String>,
    pub contract_hash: String,
    pub policy_hash: String,
    pub checks_digest: Option<String>,
    pub state: String,
    pub revision: u64,
    pub created_at: i64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<super::artifacts::Candidate>,
    pub integration: String,
    pub candidate_oid: String,
    pub base_oid: String,
    pub result_oid: String,
    pub checks_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionReview {
    pub criterion: usize,
    pub passed: bool,
    pub evidence: String,
}

/// Every mutation is journaled under an operation ID; explicit revisions fence decisions.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Mutation {
    FetchArtifactChunk {
        attempt: String,
        epoch: u64,
        integration: String,
        name: String,
        index: u32,
    },
    SubmitArtifactReview {
        attempt: String,
        epoch: u64,
        integration: String,
        manifest_hash: String,
        contract_hash: String,
        approve: bool,
        criteria: Vec<CriterionReview>,
        findings: Vec<String>,
    },
    PutArtifact {
        attempt: String,
        epoch: u64,
        chunk: super::artifacts::Chunk,
    },
    BindCoordinator {
        run: String,
        expected_revision: u64,
        endpoint: super::coordinator::Endpoint,
    },
    AcknowledgeNotices {
        run: String,
        ids: Vec<String>,
    },
    RequestVerification {
        attempt: String,
        expected_revision: u64,
    },
    RequestReview {
        integration: String,
        expected_revision: u64,
        node: String,
        profile: String,
    },
    SubmitReview {
        attempt: String,
        epoch: u64,
        integration: String,
        candidate_oid: String,
        contract_hash: String,
        approve: bool,
        criteria: Vec<CriterionReview>,
        findings: Vec<String>,
    },
    ApplyIntegration {
        integration: String,
        expected_revision: u64,
    },
    EnrollNode {
        id: String,
        key: String,
        max_active: u32,
    },
    CreateRun {
        project: String,
        repo: String,
        base_oid: String,
        objective: String,
        policy: Policy,
    },
    CreateProfile {
        run: String,
        expected_revision: u64,
        name: String,
        profile: Profile,
    },
    SetProfile {
        run: String,
        expected_revision: u64,
        name: String,
        #[serde(deserialize_with = "Option::deserialize")]
        profile: Option<Profile>,
    },
    BindSupervisor {
        run: String,
        expected_revision: u64,
    },
    StartCoordinator {
        run: String,
        expected_revision: u64,
        profile: String,
        config: super::coordinator_launch::Config,
    },
    CreateTask {
        run: String,
        expected_revision: u64,
        contract: Contract,
        dependencies: Vec<String>,
    },
    AmendTask {
        task: String,
        expected_revision: u64,
        contract: Contract,
        dependencies: Vec<String>,
    },
    AddDependency {
        task: String,
        expected_revision: u64,
        predecessor: String,
    },
    Dispatch {
        task: String,
        expected_revision: u64,
        node: String,
        profile: String,
    },
    BindResource {
        attempt: String,
        epoch: u64,
        resource: Resource,
    },
    WorkerReady {
        attempt: String,
        epoch: u64,
        session: String,
    },
    SubmitResult {
        attempt: String,
        epoch: u64,
        contract_hash: String,
        commit: Option<String>,
        artifacts: BTreeMap<String, String>,
        summary: String,
    },
    Pause {
        run: String,
        expected_revision: u64,
    },
    Resume {
        run: String,
        expected_revision: u64,
    },
    Cancel {
        run: String,
        expected_revision: u64,
    },
    ObserveStopped {
        attempt: String,
        epoch: u64,
    },
    StopAttempt {
        attempt: String,
        expected_epoch: u64,
    },
    CompleteRun {
        run: String,
        expected_revision: u64,
    },
    RetryTask {
        task: String,
        expected_revision: u64,
        contract: Option<Contract>,
    },
    ProposeMemory {
        run: String,
        topic: String,
        text: String,
        source_attempt: Option<String>,
        evidence: Vec<String>,
    },
    ApproveMemory {
        proposal: String,
        expected_revision: u64,
    },
    WithdrawMemory {
        run: String,
        topic: String,
        expected_revision: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub token: String,
    pub operation_id: String,
    pub command: Mutation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub target: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub state: String,
}
