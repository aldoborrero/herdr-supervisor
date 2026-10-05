//! Reconcile durable local process evidence into signed controller events.
use super::{
    identity::Identity,
    model::{Mutation, Request},
    node::NodeStore,
    runtime::{LaunchPlan, RuntimeConfig},
};

pub fn expire_runs(store: &mut super::store::Store) -> Result<usize> {
    let expired: Vec<super::model::Run> = {
        let mut q=store.connection.prepare("SELECT doc FROM runs WHERE state IN ('active','paused') AND json_extract(doc,'$.deadline')<=?1")?;
        q.query_map([super::store::now()], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_>>()?
    };
    let credential = super::runtime::credential(&store.dir.join("admin.token"))?;
    for run in &expired {
        store.apply(&Request {
            token: credential.clone(),
            operation_id: format!("deadline-{}", run.id),
            command: Mutation::Cancel {
                run: run.id.clone(),
                expected_revision: run.revision,
            },
        })?;
    }
    Ok(expired.len())
}
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

pub fn reconcile(store: &mut NodeStore, config: &RuntimeConfig, key: &Identity) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "execution.lock")?;
    let jobs: Vec<(String, u64, String, String)> = {
        let mut q = store.connection.prepare(
            "SELECT id,epoch,state,checkpoint FROM jobs WHERE state<>'stopped' ORDER BY rowid",
        )?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut observed = Vec::new();
    for (attempt, epoch, state, checkpoint) in jobs {
        let process: Option<(u64, String, Value)> = store
            .connection
            .query_row(
                "SELECT epoch,state,doc FROM runtime_processes WHERE attempt=?1",
                [&attempt],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?
            .map(|(epoch, state, doc)| {
                Ok::<_, anyhow::Error>((epoch, state, serde_json::from_str(&doc)?))
            })
            .transpose()?;
        let proof = match process {
            Some((generation, status, doc)) if generation <= epoch && status == "exited" => {
                Some(doc)
            }
            Some(_) => None,
            None if state == "cancelling" => {
                let wrapper_started = serde_json::from_str::<LaunchPlan>(&checkpoint)
                    .ok()
                    .map(|p| p.wrapper_started);
                if wrapper_started == Some(false)
                    || checkpoint == "{}"
                    || serde_json::from_str::<super::executor::CheckoutPlan>(&checkpoint).is_ok()
                {
                    Some(json!({"reason":"fenced_before_spawn","epoch":epoch}))
                } else {
                    None
                }
            }
            None => None,
        };
        let Some(proof) = proof else {
            continue;
        };
        let event = store.enqueue(
            &Request {
                token: super::runtime::credential(&config.node_credential)?,
                operation_id: format!("stopped-{attempt}-{epoch}"),
                command: Mutation::ObserveStopped {
                    attempt: attempt.clone(),
                    epoch,
                },
            },
            key,
        )?;
        // enqueue is idempotent: a crash between it and this update reuses the event.
        store.connection.execute(
            "UPDATE jobs SET state='stopped' WHERE id=?1 AND epoch=?2",
            params![attempt, epoch],
        )?;
        observed.push(json!({"attempt":attempt,"epoch":epoch,"event":event,"proof":proof,"controller_applied":false}));
    }
    Ok(json!({"observed":observed}))
}
