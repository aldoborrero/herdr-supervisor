//! Scoped worker API. Workers see their assignment and submit events; node keys
//! and credentials for other attempts never cross this boundary.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::identity::{Identity, SignedRecord};
use super::model::{Mutation, Request, digest, identifier};
use super::node::{ExecuteOrder, NodeStore};

#[derive(Clone)]
pub struct Access {
    pub state: PathBuf,
    pub node: String,
    pub owner: String,
    pub grant: SignedRecord,
}

impl Access {
    pub fn open(&self) -> Result<(NodeStore, Identity)> {
        let identity = Identity::load(&self.state.join("node.key"))?;
        let store = NodeStore::open(
            &self.state,
            &self.node,
            &self.owner,
            self.grant.clone(),
            &identity,
        )?;
        Ok((store, identity))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerRequest {
    ArtifactStatus {
        token: String,
        attempt: String,
        epoch: u64,
        name: String,
        sha256: String,
    },
    AcknowledgeMemory {
        token: String,
        attempt: String,
        epoch: u64,
        version: u64,
    },
    Context {
        token: String,
        attempt: String,
        epoch: u64,
    },
    Apply {
        token: String,
        operation_id: String,
        command: Box<Mutation>,
    },
    Receipt {
        token: String,
        attempt: String,
        epoch: u64,
        event: String,
    },
}

fn authorized(
    store: &NodeStore,
    token: &str,
    attempt: &str,
    epoch: Option<u64>,
) -> Result<(ExecuteOrder, String)> {
    identifier(attempt)?;
    let row: Option<(u64, String, String)> = store
        .connection
        .query_row(
            "SELECT epoch,state,doc FROM jobs WHERE id=?1",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (current, state, doc) = row.context("unknown or unauthorized attempt")?;
    ensure!(
        !["cancelling", "stopped", "cancelled"].contains(&state.as_str()),
        "attempt credential is retired"
    );
    let order: ExecuteOrder = serde_json::from_str(&doc)?;
    ensure!(
        current == order.attempt.epoch && epoch.is_none_or(|e| e == current),
        "attempt epoch is stale"
    );
    ensure!(
        digest(token.as_bytes()) == digest(order.credential.as_bytes()),
        "unknown or unauthorized attempt"
    );
    Ok((order, state))
}

pub fn handle(store: &mut NodeStore, identity: &Identity, request: WorkerRequest) -> Result<Value> {
    let _lease = if matches!(&request, WorkerRequest::ArtifactStatus { .. }) {
        None
    } else {
        Some(super::api::lock(&store.dir, "execution.lock")?)
    };
    store.grant.verify(jiff::Timestamp::now().as_second())?;
    match request {
        WorkerRequest::ArtifactStatus {
            token,
            attempt,
            epoch,
            name,
            sha256,
        } => {
            // Authorization and receipt projection share one read snapshot.
            let tx = store.connection.unchecked_transaction()?;
            let (order, _) = authorized(store, &token, &attempt, Some(epoch))?;
            ensure!(
                order.attempt.role == "worker",
                "artifact upload status belongs to the author"
            );
            let result = super::artifacts::status(&tx, &token, &attempt, &name, &sha256)?;
            tx.commit()?;
            Ok(result)
        }
        WorkerRequest::AcknowledgeMemory {
            token,
            attempt,
            epoch,
            version,
        } => {
            let (order, _) = authorized(store, &token, &attempt, Some(epoch))?;
            super::live_memory::acknowledge(&store.connection, &order, version)
        }
        WorkerRequest::Context {
            token,
            attempt,
            epoch,
        } => {
            let (order, state) = authorized(store, &token, &attempt, Some(epoch))?;
            Ok(
                json!({"attempt":order.attempt,"state":state,"contract":order.contract,"memory":order.memory,"review":order.review,"current_memory":super::live_memory::context(&store.connection,&order)?}),
            )
        }
        WorkerRequest::Apply {
            token,
            operation_id,
            command,
        } => {
            identifier(&operation_id)?;
            let (attempt, epoch) = match command.as_ref() {
                Mutation::PutArtifact { attempt, epoch, .. }
                | Mutation::WorkerReady { attempt, epoch, .. }
                | Mutation::SubmitResult { attempt, epoch, .. }
                | Mutation::SubmitReview { attempt, epoch, .. } => (attempt, Some(*epoch)),
                Mutation::ProposeMemory {
                    source_attempt: Some(attempt),
                    ..
                } => (attempt, None),
                _ => {
                    bail!(
                        "workers may only upload artifacts or report readiness, results, reviews or memory proposals"
                    )
                }
            };
            let (order, _) = authorized(store, &token, attempt, epoch)?;
            if let Mutation::PutArtifact { chunk, .. } = command.as_ref() {
                ensure!(
                    order.attempt.role == "worker",
                    "reviewers cannot upload author artifacts"
                );
                chunk.validate()?;
            }
            if let Mutation::ProposeMemory { run, .. } = command.as_ref() {
                ensure!(run == &order.attempt.run, "foreign memory scope");
            }
            let event = store.enqueue(
                &Request {
                    token,
                    operation_id,
                    command: *command,
                },
                identity,
            )?;
            Ok(json!({"event":event,"state":"queued","controller_applied":false}))
        }
        WorkerRequest::Receipt {
            token,
            attempt,
            epoch,
            event,
        } => {
            authorized(store, &token, &attempt, Some(epoch))?;
            let row: Option<(String,Option<String>,u64,i64,String)> = store.connection.query_row(
                "SELECT state,response,tries,available_at,last_error FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.id')=?1 AND substr(id,1,64)=?2",
                params![event,digest(token.as_bytes())], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
            ).optional()?;
            let (state, response, tries, next_attempt_at, last_error) =
                row.context("unknown or unauthorized event")?;
            let result: Option<Value> = response.map(|s| serde_json::from_str(&s)).transpose()?;
            Ok(
                json!({"event":event,"state":state,"result":result,"delivery":{"tries":tries,"next_attempt_at":next_attempt_at,"last_error":last_error}}),
            )
        }
    }
}

pub async fn serve(access: Access, shutdown: impl std::future::Future<Output = ()>) -> Result<()> {
    access.open()?;
    super::api::serve_json(
        access.state.clone(),
        "worker.sock",
        "worker-api.lock",
        move |value| {
            let (mut store, key) = access.open()?;
            handle(&mut store, &key, serde_json::from_value(value)?)
        },
        shutdown,
    )
    .await
}

pub fn call(state: &Path, request: &WorkerRequest) -> Result<Value> {
    super::api::call_json(&state.join("worker.sock"), &serde_json::to_value(request)?)
}

pub fn mcp(
    state: &Path,
    token: &str,
    attempt: &str,
    epoch: u64,
    input: impl std::io::Read,
    output: impl std::io::Write,
) -> Result<()> {
    super::api::mcp_loop(input, output, |request| {
        match request["method"].as_str().context("MCP method")? {
            "initialize" => {
                let version = request["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2025-03-26");
                ensure!(
                    ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&version),
                    "unsupported MCP version"
                );
                Ok(
                    json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"herdr-supervisor-worker","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":[
                {"name":"worker_artifact_status","description":"Inspect your upload's controller content-verification receipts. Content verification is not task acceptance.","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"sha256":{"type":"string"}},"required":["name","sha256"],"additionalProperties":false}},
                {"name":"worker_context","description":"Read your immutable assignment memory and current_memory (latest replicated approved snapshot and version).","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
                {"name":"worker_acknowledge_memory","description":"Declare that you read the exact current memory version. Delivery alone does not acknowledge it.","inputSchema":{"type":"object","properties":{"version":{"type":"integer","minimum":0}},"required":["version"],"additionalProperties":false}},
                {"name":"worker_apply","description":"Queue an artifact chunk, readiness, an author result, an assigned review or a memory proposal with a stable operation_id. Queueing is not controller acceptance; inspect the receipt.","inputSchema":{"type":"object","properties":{"operation_id":{"type":"string"},"command":{"type":"object"}},"required":["operation_id","command"],"additionalProperties":false}},
                {"name":"worker_receipt","description":"Read controller application receipt for your event.","inputSchema":{"type":"object","properties":{"event":{"type":"string"}},"required":["event"],"additionalProperties":false}}
            ]})),
            "tools/call" => {
                let args = &request["params"]["arguments"];
                let op = match request["params"]["name"].as_str().context("tool name")? {
                    "worker_acknowledge_memory" => WorkerRequest::AcknowledgeMemory {
                        token: token.into(),
                        attempt: attempt.into(),
                        epoch,
                        version: args["version"].as_u64().context("memory version")?,
                    },
                    "worker_artifact_status" => WorkerRequest::ArtifactStatus {
                        token: token.into(),
                        attempt: attempt.into(),
                        epoch,
                        name: args["name"].as_str().context("artifact name")?.into(),
                        sha256: args["sha256"].as_str().context("artifact hash")?.into(),
                    },
                    "worker_context" => WorkerRequest::Context {
                        token: token.into(),
                        attempt: attempt.into(),
                        epoch,
                    },
                    "worker_apply" => WorkerRequest::Apply {
                        token: token.into(),
                        operation_id: args["operation_id"]
                            .as_str()
                            .context("operation ID")?
                            .into(),
                        command: serde_json::from_value(args["command"].clone())?,
                    },
                    "worker_receipt" => WorkerRequest::Receipt {
                        token: token.into(),
                        attempt: attempt.into(),
                        epoch,
                        event: args["event"].as_str().context("event ID")?.into(),
                    },
                    _ => bail!("unknown worker tool"),
                };
                match call(state, &op) {
                    Ok(data) => Ok(
                        json!({"content":[{"type":"text","text":serde_json::to_string(&data)?}],"isError":false}),
                    ),
                    Err(error) => Ok(
                        json!({"content":[{"type":"text","text":error.to_string()}],"isError":true}),
                    ),
                }
            }
            _ => bail!("unsupported MCP method"),
        }
    })
}
