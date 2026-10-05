//! Private JSON-line API and MCP facade. Both use the same authenticated handlers.
use std::io::{BufRead, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use super::model::{Mutation, Request};
use super::store::{Store, private_dir};

const MAX_FRAME: usize = 300000;
pub(super) const MAX_RESPONSE: usize = 4 * 1024 * 1024;

fn response_bytes(value: &Value) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RESPONSE - 1 - self.0.len() {
                return Err(std::io::Error::other(
                    "API response too large; use control context/page or supervisor_page",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value)?;
    output.0.push(b'\n');
    Ok(output.0)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApiRequest {
    Record {
        token: String,
        run: String,
        cursor: String,
    },
    ProjectApplyReceipt {
        token: String,
        run: String,
        operation_id: String,
        command: Box<Mutation>,
    },
    CoordinatorOpenContext {
        token: String,
        run: String,
        profile: Option<String>,
    },
    MatchSessions {
        token: String,
        run: String,
        observations: Vec<super::pagination::SessionObservation>,
    },
    ProjectPage {
        token: String,
        run: String,
        collection: super::pagination::Collection,
        #[serde(default)]
        cursor: Option<String>,
    },
    CoordinatorOverview {
        token: String,
        run: String,
    },
    RunPage {
        token: String,
        run: String,
        collection: super::pagination::Collection,
        #[serde(default)]
        cursor: Option<String>,
        limit: u32,
    },
    ProjectApply {
        token: String,
        run: String,
        operation_id: String,
        command: Box<Mutation>,
    },
    CoordinatorContext {
        token: String,
        run: String,
    },
    Apply {
        token: String,
        operation_id: String,
        command: Box<Mutation>,
    },
    Snapshot {
        token: String,
        run: String,
    },
    Memory {
        token: String,
        run: String,
        topics: Vec<String>,
        budget: usize,
    },
}

pub fn handle(store: &mut Store, request: ApiRequest) -> Result<Value> {
    match request {
        ApiRequest::Record { token, run, cursor } => {
            super::record::read(store, &token, &run, &cursor)
        }
        ApiRequest::ProjectApplyReceipt {
            token,
            run,
            operation_id,
            command,
        } => {
            let response = store.apply_in_run(
                &run,
                &Request {
                    token,
                    operation_id: operation_id.clone(),
                    command: *command,
                },
            )?;
            super::receipt::committed(&operation_id, &response)
        }
        ApiRequest::CoordinatorOpenContext {
            token,
            run,
            profile,
        } => super::pagination::open_context(store, &token, &run, profile.as_deref()),
        ApiRequest::MatchSessions {
            token,
            run,
            observations,
        } => super::pagination::matches_sessions(store, &token, &run, &observations),
        ApiRequest::ProjectPage {
            token,
            run,
            collection,
            cursor,
        } => super::pagination::project_page(store, &token, &run, collection, cursor.as_deref()),
        ApiRequest::CoordinatorOverview { token, run } => {
            super::pagination::overview(store, &token, &run)
        }
        ApiRequest::RunPage {
            token,
            run,
            collection,
            cursor,
            limit,
        } => super::pagination::page(store, &token, &run, collection, cursor.as_deref(), limit),
        ApiRequest::ProjectApply {
            token,
            run,
            operation_id,
            command,
        } => store.apply_in_run(
            &run,
            &Request {
                token,
                operation_id,
                command: *command,
            },
        ),
        ApiRequest::CoordinatorContext { token, run } => {
            super::coordinator::context(store, &token, &run)
        }
        ApiRequest::Apply {
            token,
            operation_id,
            command,
        } => store.apply(&Request {
            token,
            operation_id,
            command: *command,
        }),
        ApiRequest::Snapshot { token, run } => store.snapshot(&token, &run),
        ApiRequest::Memory {
            token,
            run,
            topics,
            budget,
        } => store.memory(&token, &run, &topics, budget),
    }
}

pub struct Lease(std::fs::File);

impl Drop for Lease {
    fn drop(&mut self) {
        // Explicit unlock is essential in a process that spawns children: a
        // fork may briefly inherit the same open-file description until exec.
        // Closing only the parent's fd would leave that lease held meanwhile.
        let _ = self.0.unlock();
    }
}

pub fn lock(dir: &Path, name: &str) -> Result<Lease> {
    acquire_lock(dir, name, false)
}

pub fn lock_wait(dir: &Path, name: &str) -> Result<Lease> {
    acquire_lock(dir, name, true)
}

fn acquire_lock(dir: &Path, name: &str, wait: bool) -> Result<Lease> {
    private_dir(dir)?;
    let path = dir.join(name);
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "lock must be regular"
        );
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    if wait {
        file.lock()?;
    } else {
        file.try_lock()
            .map_err(|_| anyhow::anyhow!("another process owns this state"))?;
    }
    Ok(Lease(file))
}

/// Probe only an existing private lease; missing/invalid files are not evidence.
pub(super) fn existing_lock(dir: &Path, name: &str) -> Result<Option<Lease>> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(name))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "private regular ownership lease required"
    );
    // SAFETY: a valid owned fd and a nonblocking flock; no process is signalled.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(Lease(file)));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error.into())
    }
}

pub async fn serve(dir: PathBuf, shutdown: impl std::future::Future<Output = ()>) -> Result<()> {
    Store::open(&dir)?.owner()?;
    let state = dir.clone();
    serve_json(
        dir,
        "control.sock",
        "api.lock",
        move |value| handle(&mut Store::open(&state)?, serde_json::from_value(value)?),
        shutdown,
    )
    .await
}

pub async fn serve_json(
    dir: PathBuf,
    socket_name: &str,
    lock_name: &str,
    handler: impl Fn(Value) -> Result<Value> + Send + Sync + 'static,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let _lock = lock(&dir, lock_name)?;
    let handler = Arc::new(handler);
    let socket = dir.join(socket_name);
    if socket.exists() {
        use std::os::unix::fs::FileTypeExt;
        ensure!(
            std::fs::symlink_metadata(&socket)?.file_type().is_socket(),
            "API path is not a socket"
        );
        std::fs::remove_file(&socket)?;
    }
    let listener = tokio::net::UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let slots = Arc::new(tokio::sync::Semaphore::new(32));
    let mut handlers = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _=&mut shutdown=>break,
            Some(_)=handlers.join_next(),if !handlers.is_empty()=>(),
            accepted=listener.accept()=>{
                let(stream,_)=accepted?;
                let Ok(permit)=slots.clone().try_acquire_owned() else {drop(stream);continue;};
                let handler=handler.clone();
                handlers.spawn(async move {
                    let _permit=permit;
                    let _=tokio::time::timeout(Duration::from_secs(5),connection(stream,handler.as_ref())).await;
                });
            }
        }
    }
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
    std::fs::remove_file(socket)?;
    Ok(())
}

async fn connection(
    stream: tokio::net::UnixStream,
    handler: &(impl Fn(Value) -> Result<Value> + Sync),
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader.take((MAX_FRAME + 1) as u64));
    let mut bytes = Vec::new();
    reader.read_until(b'\n', &mut bytes).await?;
    ensure!(
        bytes.len() <= MAX_FRAME && bytes.last() == Some(&b'\n'),
        "incomplete or oversized request"
    );
    let result = (|| handler(serde_json::from_slice(&bytes)?))();
    let response = match result {
        Ok(value) => json!({"data":value}),
        Err(error) => json!({"error":error.to_string()}),
    };
    let bytes = response_bytes(&response).unwrap_or_else(|_| {
        b"{\"error\":\"API response too large; use control context/page or supervisor_page\"}\n"
            .to_vec()
    });
    writer.write_all(&bytes).await?;
    Ok(())
}

pub fn call(dir: &Path, request: &ApiRequest) -> Result<Value> {
    call_json(&dir.join("control.sock"), &serde_json::to_value(request)?)
}

pub fn call_json(socket: &Path, request: &Value) -> Result<Value> {
    use std::io::Read;
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    ensure!(bytes.len() <= MAX_FRAME, "request too large");
    stream.write_all(&bytes)?;
    let mut reader = std::io::BufReader::new(stream.take((MAX_RESPONSE + 1) as u64));
    let mut response = Vec::new();
    reader.read_until(b'\n', &mut response)?;
    ensure!(
        response.len() <= MAX_RESPONSE && response.last() == Some(&b'\n'),
        "incomplete or oversized API response"
    );
    let response: Value = serde_json::from_slice(&response)?;
    if let Some(error) = response.get("error") {
        anyhow::bail!("{}", error.as_str().unwrap_or("API error"));
    }
    response
        .get("data")
        .cloned()
        .context("missing API response")
}

/// MCP uses one tool per authority operation; credentials stay in the server environment.
pub fn mcp(
    dir: &Path,
    credential: &str,
    input: impl std::io::Read,
    output: impl Write,
) -> Result<()> {
    mcp_loop(input, output, |request| {
        mcp_request(dir, credential, request)
    })
}

pub fn mcp_loop(
    input: impl std::io::Read,
    mut output: impl Write,
    handler: impl Fn(&Value) -> Result<Value>,
) -> Result<()> {
    let mut input = std::io::BufReader::new(input);
    loop {
        let mut bytes = Vec::new();
        // Fill one bounded line without allocating an attacker-controlled frame.
        loop {
            let buffer = input.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let n = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buffer.len(), |i| i + 1);
            ensure!(bytes.len() + n <= MAX_FRAME, "MCP request too large");
            let complete = buffer[n - 1] == b'\n';
            bytes.extend_from_slice(&buffer[..n]);
            input.consume(n);
            if complete {
                break;
            }
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let request: Value = serde_json::from_slice(&bytes)?;
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = handler(&request);
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":error.to_string()}})
            }
        };
        let bytes = match response_bytes(&response) {
            Ok(bytes) => bytes,
            Err(_) => response_bytes(
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"MCP response too large; use supervisor_context and supervisor_page"}}),
            )?,
        };
        output.write_all(&bytes)?;
        output.flush()?;
    }
}

fn mcp_request(dir: &Path, token: &str, request: &Value) -> Result<Value> {
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
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"herdr-supervisor","version":env!("CARGO_PKG_VERSION")}}),
            )
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":[
            {"name":"supervisor_context","description":"Read a bounded run overview and collection cursors. Read supervisor_page for tasks, profiles, checks, memory and notices before deciding.","inputSchema":{"type":"object","properties":{"run":{"type":"string"}},"required":["run"],"additionalProperties":false}},
            {"name":"supervisor_page","description":"Read complete records from one authorized collection. Follow next_cursor until null. Pages are current read snapshots, not one frozen scan; refresh context for new rows and recheck revisions before decisions.","inputSchema":{"type":"object","properties":{"run":{"type":"string"},"collection":{"type":"string","enum":super::pagination::Collection::ALL.iter().map(|c|c.name()).collect::<Vec<_>>()},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50}},"required":["run","collection"],"additionalProperties":false}},
            {"name":"supervisor_record","description":"Read one UTF-8 JSON fragment from a page's record reference. Concatenate text fields in offset order through next_cursor=null and verify bytes/sha256 before interpreting. A changed record requires a fresh page. Cursors do not grant authority.","inputSchema":{"type":"object","properties":{"run":{"type":"string"},"cursor":{"type":"string"}},"required":["run","cursor"],"additionalProperties":false}},
            {"name":"supervisor_apply","description":"Submit a scoped decision with a stable operation ID and explicit aggregate revision. Never interprets delivery as approval.","inputSchema":{"type":"object","properties":{"operation_id":{"type":"string"},"command":{"type":"object"}},"required":["operation_id","command"],"additionalProperties":false}},
            {"name":"supervisor_snapshot","description":"Read authoritative tasks and attempts for the authorized run.","inputSchema":{"type":"object","properties":{"run":{"type":"string"}},"required":["run"],"additionalProperties":false}},
            {"name":"supervisor_memory","description":"Read approved, versioned memory relevant to these topics.","inputSchema":{"type":"object","properties":{"run":{"type":"string"},"topics":{"type":"array","items":{"type":"string"}}},"required":["run","topics"],"additionalProperties":false}}
        ]})),
        "tools/call" => {
            let args = &request["params"]["arguments"];
            let get = |name: &str| -> Result<String> {
                Ok(args[name]
                    .as_str()
                    .with_context(|| format!("missing {name}"))?
                    .into())
            };
            let api = match request["params"]["name"].as_str().context("tool name")? {
                "supervisor_context" => ApiRequest::CoordinatorOverview {
                    token: token.into(),
                    run: get("run")?,
                },
                "supervisor_page" => ApiRequest::RunPage {
                    token: token.into(),
                    run: get("run")?,
                    collection: serde_json::from_value(args["collection"].clone())?,
                    cursor: args
                        .get("cursor")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()?,
                    limit: args
                        .get("limit")
                        .map(|v| serde_json::from_value(v.clone()))
                        .transpose()?
                        .unwrap_or(50),
                },
                "supervisor_record" => ApiRequest::Record {
                    token: token.into(),
                    run: get("run")?,
                    cursor: get("cursor")?,
                },
                "supervisor_apply" => ApiRequest::Apply {
                    token: token.into(),
                    operation_id: get("operation_id")?,
                    command: serde_json::from_value(args["command"].clone())?,
                },
                "supervisor_snapshot" => ApiRequest::Snapshot {
                    token: token.into(),
                    run: get("run")?,
                },
                "supervisor_memory" => ApiRequest::Memory {
                    token: token.into(),
                    run: get("run")?,
                    topics: serde_json::from_value(args["topics"].clone())?,
                    budget: 32000,
                },
                _ => anyhow::bail!("unknown tool"),
            };
            match call(dir, &api) {
                Ok(data) => Ok(
                    json!({"content":[{"type":"text","text":serde_json::to_string(&data)?}],"isError":false}),
                ),
                Err(error) => {
                    Ok(json!({"content":[{"type":"text","text":error.to_string()}],"isError":true}))
                }
            }
        }
        _ => anyhow::bail!("unsupported MCP method"),
    }
}

#[cfg(test)]
mod lease_tests {
    #[test]
    fn lease_release_is_not_delayed_by_an_inherited_file_description() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state");
        let lease = super::lock(&dir, "effect.lock").unwrap();
        let inherited = lease.0.try_clone().unwrap();
        assert!(super::lock(&dir, "effect.lock").is_err());
        drop(lease);
        let next = super::lock(&dir, "effect.lock").unwrap();
        drop(inherited);
        assert!(super::lock(&dir, "effect.lock").is_err());
        drop(next);
        assert!(super::lock(&dir, "effect.lock").is_ok());
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn api_response_encoder_accepts_exact_frame_and_rejects_one_more_byte() {
        let overhead = response_bytes(&json!({"data":""})).unwrap().len();
        let exact = json!({"data":"x".repeat(MAX_RESPONSE-overhead)});
        assert_eq!(response_bytes(&exact).unwrap().len(), MAX_RESPONSE);
        assert!(response_bytes(&json!({"data":"x".repeat(MAX_RESPONSE-overhead+1)})).is_err());
        assert!(response_bytes(&json!({"data":"\u{1}".repeat(MAX_RESPONSE/6)})).is_err());
    }
    #[test]
    fn api_response_client_requires_complete_bounded_json_line() {
        for mode in ["exact", "overflow", "truncated"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("api.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            let sender = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(socket.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let mut response = b"{\"data\":true}".to_vec();
                if mode != "truncated" {
                    response.resize(MAX_RESPONSE - 1 + usize::from(mode == "overflow"), b' ');
                    response.push(b'\n');
                }
                let _ = socket.write_all(&response);
            });
            let response = call_json(&path, &json!({}));
            if mode == "exact" {
                assert_eq!(response.unwrap(), true);
            } else {
                assert!(
                    response
                        .unwrap_err()
                        .to_string()
                        .contains("incomplete or oversized")
                );
            }
            sender.join().unwrap();
        }
    }
    #[test]
    fn mcp_response_overflow_returns_one_complete_error_and_keeps_next_request() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1}\n{\"jsonrpc\":\"2.0\",\"id\":2}\n";
        let mut output = Vec::new();
        mcp_loop(&input[..], &mut output, |req| {
            Ok(if req["id"] == 1 {
                json!({"huge":"x".repeat(MAX_RESPONSE)})
            } else {
                json!({"ok":true})
            })
        })
        .unwrap();
        let values: Vec<Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["error"]["code"], -32001);
        assert_eq!(values[1]["result"]["ok"], true);
    }
}
