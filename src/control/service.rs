//! Continuous node execution uses the same transport and effect handlers as CLI.
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

use super::executor::ExecutorConfig;
use super::runtime::RuntimeConfig;
use super::worker::Access;

fn advance_jobs(
    access: &Access,
    executor: &ExecutorConfig,
    runtime: &RuntimeConfig,
) -> Result<Value> {
    let (mut store, key) = access.open()?;
    let lifecycle = match super::lifecycle::reconcile(&mut store, runtime, &key) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("node lifecycle: {error}");
            json!({"unresolved":error.to_string()})
        }
    };
    let jobs: Vec<(String, String)> = {
        let mut q=store.connection.prepare("SELECT id,state FROM jobs WHERE state IN ('reserved','preparing','prepared','launching','awaiting_binding') ORDER BY rowid LIMIT 128")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut results = Vec::new();
    for (id, state) in jobs {
        let result = if ["reserved", "preparing"].contains(&state.as_str()) {
            super::executor::prepare(&mut store, executor, &id, &crate::runner::RealRunner)
                .map(|plan| super::executor::summary(&plan))
        } else {
            super::runtime::advance(&mut store, runtime, &id, &key, &crate::runner::RealRunner)
        };
        match result {
            Ok(value) => results.push(json!({"attempt":id,"result":value})),
            Err(error) => {
                // Errors preserve the durable state and occupied slot. A later
                // pass may reconcile evidence; it does not reset launch markers.
                eprintln!("attempt {id}: {error}");
                results.push(json!({"attempt":id,"unresolved":error.to_string()}));
            }
        }
    }
    Ok(json!({"attempts":results,"lifecycle":lifecycle}))
}

pub async fn run(
    access: Access,
    broker_config: PathBuf,
    executor: ExecutorConfig,
    runtime: RuntimeConfig,
    once: bool,
) -> Result<Value> {
    let _relay = super::api::lock(&access.state, "relay.lock")?;
    let (mut store, _) = access.open()?;
    let broker = super::transport::Broker::connect(
        &super::transport::read_config(&broker_config)?,
        &access.owner,
    )
    .await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let api = tokio::spawn(super::worker::serve(access.clone(), async {
        let _ = stopped.await;
    }));
    let result = async {
        loop {
            anyhow::ensure!(
                !api.is_finished(),
                "worker API stopped; node execution halted"
            );
            let relayed = super::transport::node_once(&mut store, &broker).await;
            if let Err(error) = &relayed {
                eprintln!("node relay: {error}");
            }
            anyhow::ensure!(!api.is_finished(), "worker API failed before execution");
            let a = access.clone();
            let e = executor.clone();
            let r = runtime.clone();
            // Runtime/Herdr commands may take seconds. Keep worker IPC responsive.
            let executed = tokio::task::spawn_blocking(move || advance_jobs(&a, &e, &r)).await??;
            if once {
                return Ok(json!({"relayed":relayed?,"execution":executed}));
            }
            tokio::select! {
                _=tokio::signal::ctrl_c()=>return Ok(json!({"stopped":true})),
                _=tokio::time::sleep(Duration::from_millis(500))=>(),
            }
        }
    }
    .await;
    let _ = stop.send(());
    api.await??;
    result
}

/// Controller API and relay remain responsive while a verifier command runs.
/// The check itself runs off the async runtime and owns its durable receipt.
pub async fn controller(
    state: PathBuf,
    broker_config: PathBuf,
    config: super::git_pipeline::Config,
    once: bool,
    coordinator: Option<super::coordinator::Config>,
) -> Result<Value> {
    let _relay = super::api::lock(&state, "relay.lock")?;
    let mut store = super::store::Store::open(&state)?;
    let broker = super::transport::Broker::connect(
        &super::transport::read_config(&broker_config)?,
        &store.owner()?,
    )
    .await?;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let api = tokio::spawn(super::api::serve(state.clone(), async {
        let _ = stopped.await;
    }));
    let (launch_stop, launch_stopped) = tokio::sync::oneshot::channel();
    let launch_state = state.clone();
    let launch_config = coordinator.clone();
    let launcher = tokio::spawn(async move {
        if let Some(config) = launch_config {
            let mut stopped = launch_stopped;
            loop {
                let path = launch_state.clone();
                let config = config.clone();
                let result = tokio::task::spawn_blocking(move || {
                    super::coordinator_launch::advance_all(
                        &mut super::store::Store::open(&path)?,
                        &config,
                        &crate::runner::RealRunner,
                    )
                })
                .await;
                match result {
                    Ok(Ok(_)) => (),
                    Ok(Err(e)) => eprintln!("coordinator launch: {e}"),
                    Err(e) => eprintln!("coordinator launcher task: {e}"),
                }
                tokio::select! {_=&mut stopped=>break,_=tokio::time::sleep(Duration::from_millis(500))=>()}
            }
        }
    });
    let (notify_stop, notify_stopped) = tokio::sync::oneshot::channel();
    let notify_state = state.clone();
    let notifier = tokio::spawn(async move {
        if let Some(config) = coordinator {
            let mut stopped = notify_stopped;
            loop {
                let path = notify_state.clone();
                let config = config.clone();
                let result = tokio::task::spawn_blocking(move || {
                    super::coordinator::deliver(&mut super::store::Store::open(&path)?, &config)
                })
                .await;
                match result {
                    Ok(Ok(_)) => (),
                    Ok(Err(e)) => eprintln!("coordinator notices: {e}"),
                    Err(e) => eprintln!("coordinator notifier task: {e}"),
                };
                tokio::select! {_=&mut stopped=>break,_=tokio::time::sleep(Duration::from_millis(500))=>()}
            }
        }
    });
    let outcome=async{
        let mut last_rowid=0i64;
        loop {
            if let Err(error)=super::lifecycle::expire_runs(&mut store){eprintln!("deadline reconciliation: {error}");}
            let relayed=super::transport::controller_once(&mut store,&broker).await;
            if let Err(error)=&relayed{eprintln!("controller relay: {error}");}
            anyhow::ensure!(!api.is_finished(),"controller API stopped before verification");
            let next:Option<(i64,String)>={
                use rusqlite::OptionalExtension;
                store.connection.query_row("SELECT i.rowid,i.id FROM integrations i JOIN runs r ON r.id=i.run WHERE (i.state IN ('preparing','checking') AND r.state='active' AND json_extract(r.doc,'$.deadline')>?2) OR i.state='applying' ORDER BY (i.rowid<=?1),i.rowid LIMIT 1",rusqlite::params![last_rowid,super::store::now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?
            };
            let mut executed=serde_json::Value::Null;
            if let Some((rowid,id))=next {
                // An unavailable node's Git remote must not starve other repositories.
                last_rowid=rowid;
                let path=state.clone();let config=config.clone();
                let mut work=tokio::task::spawn_blocking(move|| {
                    let mut store=super::store::Store::open(&path)?;
                    super::git_pipeline::advance(&mut store,&config,&id,&crate::runner::RealRunner)
                });
                let mut interval=tokio::time::interval(Duration::from_millis(500));
                loop {tokio::select!{
                    result=&mut work=>{
                        executed=match result? {
                            Ok(i)=>serde_json::to_value(i)?,
                            Err(error)=>{eprintln!("controller verifier: {error}");json!({"unresolved":error.to_string()})}
                        };break;
                    }
                    _=interval.tick()=>{
                        if let Err(error)=super::lifecycle::expire_runs(&mut store){eprintln!("deadline reconciliation: {error}");}
                        if let Err(error)=super::transport::controller_once(&mut store,&broker).await{eprintln!("controller relay during verification: {error}");}
                        if api.is_finished(){
                            let _=work.await?;
                            anyhow::bail!("controller API stopped during verification");
                        }
                    }
                    _=tokio::signal::ctrl_c()=>{
                        // Drain the bounded in-flight check instead of silently detaching it.
                        let _=work.await?;
                        return Ok(json!({"stopped":true}));
                    }
                }}
            }
            if once{return Ok(json!({"relayed":relayed?,"verification":executed}));}
            tokio::select!{
                _=tokio::signal::ctrl_c()=>return Ok(json!({"stopped":true})),
                _=tokio::time::sleep(Duration::from_millis(500))=>(),
            }
        }
    }.await;
    let _ = stop.send(());
    let _ = notify_stop.send(());
    let _ = launch_stop.send(());
    let api_result = api.await;
    notifier.await?;
    launcher.await?;
    api_result??;
    outcome
}
