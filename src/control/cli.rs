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
    /// Local operator retirement with explicit process/retention attestations; keeps commits
    CleanupRetire {
        #[arg(long)]
        executor: PathBuf,
        /// JSON decision with exact attempt, epoch, HEAD and operation UUID
        #[arg(long)]
        request: PathBuf,
    },
    /// Recover completed verifier receipts without running checks or changing Git
    ReconcileChecks {
        #[arg(long)]
        credential: PathBuf,
    },
    /// Inspect local node delivery/recovery metadata without the request or response body
    EventStatus { event: String },
    /// Queue a renewed delivery of an expired signed node event; retains the original
    RecoverEvent {
        #[arg(long)]
        node: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        grant: PathBuf,
        #[arg(long)]
        operation: String,
        /// UUID of the original event, not a previous recovery envelope
        event: String,
    },
    /// Read owner-scoped node authority, drain state and occupied capacity
    NodeStatus {
        #[arg(long)]
        credential: PathBuf,
        node: String,
    },
    /// Inspect retention reasons for one node checkout; never authorize or perform removal
    CleanupInspect {
        #[arg(long)]
        executor: PathBuf,
        #[arg(long)]
        expected_epoch: u64,
        attempt: String,
    },
    /// Read the next UTF-8 JSON fragment of a referenced large record
    Record {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        cursor: String,
        run: String,
    },
    /// Read one bounded collection page from the authorized run
    Page {
        #[arg(long)]
        credential: PathBuf,
        #[arg(long, value_enum)]
        collection: super::pagination::Collection,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        run: String,
    },
    /// Local owner maintenance: authorize one registration retry before receiver binding
    RecoverRegistration {
        /// Recover a worker/reviewer in node.sqlite; otherwise recover a coordinator
        #[arg(long)]
        node: bool,
        #[arg(long)]
        operation: String,
        /// Exact worker epoch or coordinator generation from the saved launch
        #[arg(long)]
        expected_epoch: u64,
        id: String,
    },
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
    /// Prepare a resumable local controller/node bundle; starts no services or models
    SetupLocal {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        bundle: PathBuf,
        /// Contact the explicitly configured broker with its separate admin credential
        #[arg(long)]
        provision: bool,
    },
    /// Diagnose a saved local bundle and its dependencies without provisioning or starting models
    CheckLocal {
        #[arg(long)]
        bundle: PathBuf,
    },
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
    /// Read a bounded coordinator overview and cursors for complete record pages
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
    /// Queue a bounded local file as immutable, resumable artifact chunks
    UploadArtifact {
        name: String,
        file: PathBuf,
    },
    /// Inspect controller verification of the exact uploaded content
    ArtifactStatus {
        name: String,
        sha256: String,
    },
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
        ControlCommand::ReconcileChecks { credential: path } => {
            drop(super::reception::local_db(&args.state, false, false)?);
            let mut store = Store::open(&args.state)?;
            super::store::own(&super::store::authenticate(
                &store.connection,
                &credential(&path)?,
            )?)?;
            super::check_receipts::reconcile(&mut store)?
        }
        ControlCommand::Record {
            credential: path,
            cursor,
            run,
        } => super::api::call(
            &args.state,
            &super::api::ApiRequest::Record {
                token: credential(&path)?,
                run,
                cursor,
            },
        )?,
        ControlCommand::Page {
            credential: path,
            collection,
            cursor,
            limit,
            run,
        } => super::api::call(
            &args.state,
            &super::api::ApiRequest::RunPage {
                token: credential(&path)?,
                run,
                collection,
                cursor,
                limit,
            },
        )?,
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
            &super::api::ApiRequest::CoordinatorOverview {
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
                WorkerAction::UploadArtifact { name, file } => {
                    let result = super::artifacts::upload(
                        &args.state,
                        &token,
                        &attempt,
                        epoch,
                        &name,
                        &file,
                    )?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    return Ok(());
                }
                WorkerAction::ArtifactStatus { name, sha256 } => {
                    super::worker::WorkerRequest::ArtifactStatus {
                        token,
                        attempt,
                        epoch,
                        name,
                        sha256,
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
        ControlCommand::SetupLocal {
            spec,
            bundle,
            provision,
        } => {
            let spec = super::service_setup::read_spec(&spec)?;
            let mut result = super::service_setup::prepare(&args.state, &bundle, &spec)?;
            if provision {
                runtime()?.block_on(super::service_setup::provision(
                    &bundle,
                    &spec,
                    &mut result,
                ))?;
            }
            result
        }
        ControlCommand::CheckLocal { bundle } => {
            let report = runtime()?.block_on(super::service_setup::check(&args.state, &bundle))?;
            println!("{}", serde_json::to_string(&report)?);
            ensure!(
                report["checks_passed"] == true,
                "local dependency checks failed; see the structured report"
            );
            return Ok(());
        }
        ControlCommand::EventStatus { event } => {
            super::event_recovery::status(&args.state, &event)?
        }
        ControlCommand::RecoverEvent {
            node,
            owner,
            grant,
            operation,
            event,
        } => {
            // Maintenance cannot silently initialize a missing node database.
            drop(super::reception::local_db(&args.state, true, false)?);
            let identity = Identity::load(&args.state.join("node.key"))?;
            let mut store = super::node::NodeStore::open(
                &args.state,
                &node,
                &owner,
                read_grant(&grant)?,
                &identity,
            )?;
            store.recover_event(&event, &operation, &identity)?
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
            std::fs::symlink_metadata(args.state.join("control.sqlite"))?;
            let grant = Store::open(&args.state)?.node_grant(&node, &key, epoch, expires_at)?;
            serde_json::to_value(grant)?
        }
        ControlCommand::NodeStatus {
            credential: path,
            node,
        } => Store::open(&args.state)?.node_status(&credential(&path)?, &node)?,
        ControlCommand::CleanupRetire { executor, request } => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(request)?
                .take(16385)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 16384, "retirement request exceeds 16 KiB");
            super::retirement::retire(
                &args.state,
                &super::executor::read_config(&executor)?,
                &serde_json::from_slice(&bytes)?,
            )?
        }
        ControlCommand::CleanupInspect {
            executor,
            expected_epoch,
            attempt,
        } => super::cleanup::inspect(
            &args.state,
            &super::executor::read_config(&executor)?,
            &attempt,
            expected_epoch,
        )?,
        ControlCommand::RecoverRegistration {
            node,
            operation,
            expected_epoch,
            id,
        } => super::registration_recovery::recover(
            &args.state,
            node,
            &id,
            expected_epoch,
            &operation,
        )?,
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
