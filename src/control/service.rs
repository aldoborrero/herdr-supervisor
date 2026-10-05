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
    Ok(json!({"attempts":results}))
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
