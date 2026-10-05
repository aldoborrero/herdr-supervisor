use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use clap::{Args, Subcommand};
use serde_json::json;

use super::identity::Identity;
use super::model::{Mutation, Request};
use super::store::{Store, private_dir};

#[derive(Args)]
pub struct ControlArgs {
    /// Private controller state, independent from mailer and upstream Projects
    #[arg(long)]
    state: PathBuf,
    #[command(subcommand)]
    command: ControlCommand,
}

#[derive(Subcommand)]
enum ControlCommand {
    /// Delete only terminal local rejection records older than an explicit Unix timestamp
    ReceptionPrune {
        #[arg(long)]
        node: bool,
        #[arg(long)]
        before: i64,
    },
    /// Inspect local rejected-delivery metadata, without message bodies or credentials
    ReceptionStatus {
        /// Inspect node.sqlite instead of control.sqlite
        #[arg(long)]
        node: bool,
    },
    /// Inspect bounded outgoing delivery metadata without exposing command bodies
    DeliveryStatus {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        node: String,
    },
    /// Print runtime NATS permissions and reply prefixes; does not configure a broker
    BrokerPolicy {
        /// Node routes to include; defaults to the controller's enrolled active nodes
        #[arg(long)]
        node: Vec<String>,
    },
    /// Run controller API, NATS relay and durable Git/check executor together
    RunController {
        #[arg(long)]
        broker: PathBuf,
        #[arg(long)]
        repositories: PathBuf,
        /// Local mailer sender and native coordinator notification configuration
        #[arg(long)]
        coordinator_config: Option<PathBuf>,
        #[arg(long)]
        once: bool,
    },
    /// Execute one durable verification/integration intent using an owner-approved repository map
    Verify {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        repositories: PathBuf,
        integration: String,
    },
    /// Run the node relay, worker API and durable executor continuously
    RunNode {
        #[arg(long)]
        broker: PathBuf,
        #[arg(long)]
        executor: PathBuf,
        #[arg(long)]
        runtime_config: PathBuf,
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        #[arg(long)]
        once: bool,
    },
    /// Advance a prepared worker through native runtime launch and mailer bootstrap
    Launch {
        #[arg(long)]
        runtime_config: PathBuf,
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        attempt: String,
    },
    /// Internal one-shot pane wrapper; refuses replay or cancelled attempts
    #[command(hide = true)]
    RunRuntime { attempt: String },
    /// Internal one-shot coordinator process owner
    #[command(hide = true)]
    RunCoordinatorRuntime { launch: String },
    /// Node-local authenticated worker API; no controller/admin credential is exposed
    ServeWorkers {
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
    },
    /// Scoped worker operations through the node API (same handlers as MCP)
    Worker {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        attempt: String,
        #[arg(long)]
        epoch: u64,
        #[command(subcommand)]
        action: WorkerAction,
    },
    /// Prepare an isolated worktree and exact Herdr pane; does not start a model
    Prepare {
        #[arg(long)]
        executor: PathBuf,
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        attempt: String,
    },
    /// Serve authenticated controller operations on a private Unix socket
    Serve,
    /// MCP stdio facade, using a scoped credential file and the private API
    Mcp {
        #[arg(long)]
        credential: PathBuf,
    },
    /// Create owner key, private database and admin credential (never print the token)
    Init,
    /// Bind an already registered native coordinator and publish its scoped credential file
    ConnectSupervisor {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        runtime_credential: PathBuf,
        #[arg(long)]
        mailer_state: PathBuf,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        expected_revision: u64,
        run: String,
    },
    /// Read authoritative coordinator context, instructions and native notice receipts
    Context {
        #[arg(long)]
        credential: PathBuf,
        run: String,
    },
    /// Create a node key locally. Enroll its public DID at the controller separately.
    NodeKey,
    /// Apply one versioned JSON mutation with a stable operation ID
    Apply {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        request: PathBuf,
    },
    /// Authoritative run summary; no model or external command is executed
    Snapshot {
        #[arg(long)]
        credential: PathBuf,
        run: String,
    },
    /// Inspect pending delivery intents for an authorized node
    Pending {
        #[arg(long)]
        credential: PathBuf,
        node: String,
    },
    /// Select approved memory by topic, bounded to 32000 UTF-8 bytes
    Memory {
        #[arg(long)]
        credential: PathBuf,
        run: String,
        #[arg(long)]
        topic: Vec<String>,
    },
    /// Issue an owner-signed node delegation, retaining the owner key centrally
    Grant {
        node: String,
        key: String,
        #[arg(long, default_value_t = 1)]
        epoch: u64,
        #[arg(long)]
        expires_at: i64,
    },
    /// Provision the private stream/consumers with a broker administrator credential
    Provision {
        #[arg(long)]
        broker: PathBuf,
        #[arg(long)]
        target: Vec<String>,
    },
    /// Relay controller intents and authenticated node events through JetStream
    RelayController {
        #[arg(long)]
        broker: PathBuf,
        #[arg(long)]
        once: bool,
    },
    /// Persist incoming node commands and relay its outgoing events (executor runs separately)
    RelayNode {
        #[arg(long)]
        broker: PathBuf,
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        #[arg(long)]
        once: bool,
    },
    /// Durably queue a scoped node/worker event; returns queued, never model acceptance
    NodeEvent {
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        request: PathBuf,
    },
}

#[derive(Subcommand)]
enum WorkerAction {
    /// Declare that you read this exact current memory version
    AcknowledgeMemory {
        version: u64,
    },
    Context,
    Apply {
        #[arg(long)]
        operation: String,
        #[arg(long)]
        request: PathBuf,
    },
    Receipt {
        event: String,
    },
    Mcp,
}

fn credential(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "credential must be a private regular file"
    );
    Ok(std::fs::read_to_string(path)?.trim().into())
}

pub fn run(args: ControlArgs) -> Result<()> {
    let result = match args.command {
        ControlCommand::RunController {
            broker,
            repositories,
            coordinator_config,
            once,
        } => runtime()?.block_on(super::service::controller(
            args.state,
            broker,
            super::git_pipeline::read_config(&repositories)?,
            once,
            coordinator_config
                .as_deref()
                .map(super::coordinator::read_config)
                .transpose()?,
        ))?,
        ControlCommand::Context {
            credential: path,
            run,
        } => super::api::call(
            &args.state,
            &super::api::ApiRequest::CoordinatorContext {
                token: credential(&path)?,
                run,
            },
        )?,
        ControlCommand::ConnectSupervisor {
            credential: path,
            runtime_credential,
            mailer_state,
            operation,
            expected_revision,
            run,
        } => {
            let doctor = super::api::call_json(
                &mailer_state.join("daemon.sock"),
                &json!({"op":"doctor","token":credential(&runtime_credential)?}),
            )?;
            let endpoint: super::coordinator::Endpoint = serde_json::from_value(
                json!({"agent":doctor["agent"],"mailer_session":doctor["session"],"mailer_epoch":doctor["epoch"],"runtime":doctor["runtime"],"native_session":doctor["location"]}),
            )?;
            endpoint.validate()?;
            let response = super::api::call(
                &args.state,
                &super::api::ApiRequest::Apply {
                    token: credential(&path)?,
                    operation_id: operation,
                    command: Box::new(Mutation::BindCoordinator {
                        run: run.clone(),
                        expected_revision,
                        endpoint,
                    }),
                },
            )?;
            let generation = response["generation"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("coordinator generation missing"))?;
            let mut store = Store::open(&args.state)?;
            super::coordinator::collect(&mut store)?;
            json!({"run":run,"generation":generation,"revision":response["revision"],"credential_file":super::coordinator::credential_path(&args.state,&run,generation),"native_delivery":"pending"})
        }
        ControlCommand::Verify {
            credential: path,
            repositories,
            integration,
        } => {
            let mut store = Store::open(&args.state)?;
            let principal = super::store::authenticate(&store.connection, &credential(&path)?)?;
            ensure!(
                principal.role == "owner",
                "local verifier execution requires the owner capability"
            );
            serde_json::to_value(super::git_pipeline::advance(
                &mut store,
                &super::git_pipeline::read_config(&repositories)?,
                &integration,
                &crate::runner::RealRunner,
            )?)?
        }
        ControlCommand::RunNode {
            broker,
            executor,
            runtime_config,
            node,
            owner,
            grant,
            once,
        } => {
            let access = super::worker::Access {
                state: args.state,
                node,
                owner,
                grant: read_grant(&grant)?,
            };
            runtime()?.block_on(super::service::run(
                access,
                broker,
                super::executor::read_config(&executor)?,
                super::runtime::read_config(&runtime_config)?,
                once,
            ))?
        }
        ControlCommand::RunCoordinatorRuntime { launch } => {
            super::coordinator_launch::run_owned(&args.state, &launch)?;
            json!({"launch":launch,"wrapper_finished":true})
        }
        ControlCommand::RunRuntime { attempt } => {
            super::runtime::run_owned(&args.state, &attempt)?;
            json!({"attempt":attempt,"wrapper_finished":true})
        }
        ControlCommand::Launch {
            runtime_config,
            node,
            owner,
            grant,
            attempt,
        } => {
            let identity = Identity::load(&args.state.join("node.key"))?;
            let mut store = super::node::NodeStore::open(
                &args.state,
                &node,
                &owner,
                read_grant(&grant)?,
                &identity,
            )?;
            super::runtime::advance(
                &mut store,
                &super::runtime::read_config(&runtime_config)?,
                &attempt,
                &identity,
                &crate::runner::RealRunner,
            )?
        }
        ControlCommand::ServeWorkers { node, owner, grant } => {
            let access = super::worker::Access {
                state: args.state,
                node,
                owner,
                grant: read_grant(&grant)?,
            };
            runtime()?.block_on(super::worker::serve(access, async {
                let _ = tokio::signal::ctrl_c().await;
            }))?;
            json!({"stopped":true})
        }
        ControlCommand::Worker {
            credential: path,
            attempt,
            epoch,
            action,
        } => {
            let token = credential(&path)?;
            let request = match action {
                WorkerAction::AcknowledgeMemory { version } => {
                    super::worker::WorkerRequest::AcknowledgeMemory {
                        token,
                        attempt,
                        epoch,
                        version,
                    }
                }
                WorkerAction::Context => super::worker::WorkerRequest::Context {
                    token,
                    attempt,
                    epoch,
                },
                WorkerAction::Apply { operation, request } => {
                    ensure!(
                        std::fs::metadata(&request)?.len() <= 262144,
                        "request too large"
                    );
                    super::worker::WorkerRequest::Apply {
                        token,
                        operation_id: operation,
                        command: serde_json::from_slice(&std::fs::read(request)?)?,
                    }
                }
                WorkerAction::Receipt { event } => super::worker::WorkerRequest::Receipt {
                    token,
                    attempt,
                    epoch,
                    event,
                },
                WorkerAction::Mcp => {
                    super::worker::mcp(
                        &args.state,
                        &token,
                        &attempt,
                        epoch,
                        std::io::stdin().lock(),
                        std::io::stdout().lock(),
                    )?;
                    return Ok(());
                }
            };
            super::worker::call(&args.state, &request)?
        }
        ControlCommand::Prepare {
            executor,
            node,
            owner,
            grant,
            attempt,
        } => {
            let identity = Identity::load(&args.state.join("node.key"))?;
            let mut store = super::node::NodeStore::open(
                &args.state,
                &node,
                &owner,
                read_grant(&grant)?,
                &identity,
            )?;
            let plan = super::executor::prepare(
                &mut store,
                &super::executor::read_config(&executor)?,
                &attempt,
                &crate::runner::RealRunner,
            )?;
            super::executor::summary(&plan)
        }
        ControlCommand::Serve => {
            runtime()?.block_on(super::api::serve(args.state.clone(), async {
                let _ = tokio::signal::ctrl_c().await;
            }))?;
            json!({"stopped":true})
        }
        ControlCommand::Mcp { credential: path } => {
            super::api::mcp(
                &args.state,
                &credential(&path)?,
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            )?;
            return Ok(());
        }
        ControlCommand::Init => {
            let store = Store::initialize(&args.state)?;
            json!({"owner":store.owner()?,"credential_file":args.state.join("admin.token")})
        }
        ControlCommand::NodeKey => {
            private_dir(&args.state)?;
            let key = Identity::generate()?;
            key.save_new(&args.state.join("node.key"))?;
            json!({"key":key.did(),"private_key_file":args.state.join("node.key")})
        }
        ControlCommand::Apply {
            credential: path,
            operation,
            request,
        } => {
            ensure!(
                std::fs::metadata(&request)?.len() <= 262144,
                "request too large"
            );
            let command: Mutation = serde_json::from_slice(&std::fs::read(request)?)?;
            Store::open(&args.state)?.apply(&Request {
                token: credential(&path)?,
                operation_id: operation,
                command,
            })?
        }
        ControlCommand::Snapshot {
            credential: path,
            run,
        } => Store::open(&args.state)?.snapshot(&credential(&path)?, &run)?,
        ControlCommand::Pending {
            credential: path,
            node,
        } => serde_json::to_value(Store::open(&args.state)?.pending(&credential(&path)?, &node)?)?,
        ControlCommand::Memory {
            credential: path,
            run,
            topic,
        } => Store::open(&args.state)?.memory(&credential(&path)?, &run, &topic, 32000)?,
        ControlCommand::Grant {
            node,
            key,
            epoch,
            expires_at,
        } => {
            super::model::identifier(&node)?;
            let identity = Identity::load(&args.state.join("owner.key"))?;
            let grant = identity.delegation(&node, &key, epoch, expires_at)?;
            grant.verify(jiff::Timestamp::now().as_second())?;
            serde_json::to_value(grant)?
        }
        ControlCommand::ReceptionPrune { node, before } => {
            super::reception::prune(&args.state, node, before)?
        }
        ControlCommand::ReceptionStatus { node } => {
            super::reception::local_status(&args.state, node)?
        }
        ControlCommand::DeliveryStatus {
            credential: path,
            node,
        } => Store::open(&args.state)?.delivery_status(&credential(&path)?, &node)?,
        ControlCommand::BrokerPolicy { node } => {
            let store = Store::open(&args.state)?;
            let nodes = if node.is_empty() {
                store.nodes()?
            } else {
                node
            };
            super::broker_policy::policy(&store.owner()?, &nodes)?
        }
        ControlCommand::Provision { broker, target } => {
            let store = Store::open(&args.state)?;
            runtime()?.block_on(async {
                let broker = super::transport::Broker::connect(
                    &super::transport::read_config(&broker)?,
                    &store.owner()?,
                    super::broker_policy::Endpoint::Provisioner,
                )
                .await?;
                let mut targets = target;
                targets.push("controller".into());
                targets.sort();
                targets.dedup();
                broker.provision(&targets).await?;
                Ok::<_, anyhow::Error>(json!({"stream":broker.stream,"targets":targets}))
            })?
        }
        ControlCommand::RelayController { broker, once } => {
            let _lock = relay_lock(&args.state)?;
            let mut store = Store::open(&args.state)?;
            runtime()?.block_on(async {
                let broker = super::transport::Broker::connect(
                    &super::transport::read_config(&broker)?,
                    &store.owner()?,
                    super::broker_policy::Endpoint::Controller,
                )
                .await?;
                loop {
                    let result = super::transport::controller_once(&mut store, &broker).await;
                    if once {
                        return result.map(|count| json!({"processed":count}));
                    }
                    if let Err(error) = result {
                        eprintln!("controller relay: {error}");
                    }
                    tokio::select! {
                        _=tokio::signal::ctrl_c()=>return Ok(json!({"stopped":true})),
                        _=tokio::time::sleep(std::time::Duration::from_millis(500))=>(),
                    }
                }
            })?
        }
        ControlCommand::RelayNode {
            broker,
            node,
            owner,
            grant,
            once,
        } => {
            let _lock = relay_lock(&args.state)?;
            let identity = Identity::load(&args.state.join("node.key"))?;
            let grant = read_grant(&grant)?;
            let mut store =
                super::node::NodeStore::open(&args.state, &node, &owner, grant, &identity)?;
            runtime()?.block_on(async {
                let broker = super::transport::Broker::connect(
                    &super::transport::read_config(&broker)?,
                    &owner,
                    super::broker_policy::Endpoint::Node(&node),
                )
                .await?;
                loop {
                    let result = super::transport::node_once(&mut store, &broker).await;
                    if once {
                        return result.map(|count| json!({"processed":count}));
                    }
                    if let Err(error) = result {
                        eprintln!("node relay: {error}");
                    }
                    tokio::select! {
                        _=tokio::signal::ctrl_c()=>return Ok(json!({"stopped":true})),
                        _=tokio::time::sleep(std::time::Duration::from_millis(500))=>(),
                    }
                }
            })?
        }
        ControlCommand::NodeEvent {
            node,
            owner,
            grant,
            credential: path,
            operation,
            request,
        } => {
            ensure!(
                std::fs::metadata(&request)?.len() <= 262144,
                "request too large"
            );
            let command: Mutation = serde_json::from_slice(&std::fs::read(request)?)?;
            let identity = Identity::load(&args.state.join("node.key"))?;
            let mut store = super::node::NodeStore::open(
                &args.state,
                &node,
                &owner,
                read_grant(&grant)?,
                &identity,
            )?;
            let id = store.enqueue(
                &Request {
                    token: credential(&path)?,
                    operation_id: operation,
                    command,
                },
                &identity,
            )?;
            json!({"id":id,"state":"queued","state_dir":store.dir})
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}
fn read_grant(path: &Path) -> Result<super::identity::SignedRecord> {
    ensure!(std::fs::metadata(path)?.len() <= 300000, "grant too large");
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn relay_lock(dir: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    private_dir(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("relay.lock"))?;
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("another relay owns this state directory"))?;
    Ok(file)
}
