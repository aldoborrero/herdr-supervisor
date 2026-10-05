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

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApiRequest {
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
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
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
    let mut reader = std::io::BufReader::new(stream.take(4 * 1024 * 1024));
    let mut response = String::new();
    reader.read_line(&mut response)?;
    let response: Value = serde_json::from_str(&response)?;
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
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
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
