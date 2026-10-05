//! Recover a missing wrapper's native-process exit receipt from kernel evidence.
//! No process is signalled, no resources are deleted, and exit status stays unknown.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn recover(c: &Connection, state: &Path, id: &str, coordinator: bool) -> Result<bool> {
    let (table, key, version) = if coordinator {
        ("coordinator_processes", "launch", "generation")
    } else {
        ("runtime_processes", "attempt", "epoch")
    };
    let row: Option<(u64, String)> = c
        .query_row(
            &format!("SELECT {version},doc FROM {table} WHERE {key}=?1 AND state='running'"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((epoch, original)) = row else {
        return Ok(false);
    };
    let Ok(mut receipt) = serde_json::from_str::<Value>(&original) else {
        return Ok(false);
    };
    if receipt[key] != id
        || receipt[version] != epoch
        || receipt["scope"] != "native_process"
        || receipt["kernel_identity"]["pid"] != receipt["child_pid"]
    {
        return Ok(false);
    }
    // Only a successfully acquired, existing private ownership lease permits
    // recovery. Missing/corrupt/legacy evidence and live wrappers stay unknown.
    let Ok(Some(_ownership)) = super::process::recovery_owner(state, &receipt) else {
        return Ok(false);
    };
    let Ok(Some(evidence)) = super::kernel_process::exited(&receipt["kernel_identity"]) else {
        return Ok(false);
    };
    receipt["reason"] = json!("recovered_native_exit");
    receipt["exit_evidence"] = evidence;
    receipt["observed_at"] = json!(super::store::now());
    receipt["code"] = Value::Null;
    receipt["signal"] = Value::Null;
    // Compare the whole prior receipt so a late normal exit receipt wins. This
    // atomic local receipt precedes signed stop delivery or coordinator retirement.
    let n = c.execute(&format!("UPDATE {table} SET state='exited',doc=?4 WHERE {key}=?1 AND {version}=?2 AND state='running' AND doc=?3"),params![id,epoch,original,serde_json::to_string(&receipt)?])?;
    Ok(n == 1)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::control::{api, kernel_process, model::id};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn process_recovery_requires_exact_identity_and_abandoned_private_owner() {
        for coordinator in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let c = Connection::open_in_memory().unwrap();
            let (table, key, version) = if coordinator {
                ("coordinator_processes", "launch", "generation")
            } else {
                ("runtime_processes", "attempt", "epoch")
            };
            c.execute_batch(&format!("CREATE TABLE {table} ({key} TEXT PRIMARY KEY, {version} INTEGER, state TEXT, doc TEXT);")).unwrap();
            let target = id();
            let nonce = id();
            let filename = format!("native-owner-{nonce}.lock");
            let lease = api::lock(dir.path(), &filename).unwrap();
            let mut child = std::process::Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .unwrap();
            struct Cleanup<'a>(&'a mut std::process::Child);
            impl Drop for Cleanup<'_> {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let child = Cleanup(&mut child);
            let identity = kernel_process::capture(child.0.id()).unwrap();
            let receipt = json!({key:target, version:7, "scope":"native_process", "owner_nonce":nonce, "child_pid":child.0.id(), "kernel_identity":identity});
            let write = |doc: &Value| {
                c.execute(
                    &format!("INSERT OR REPLACE INTO {table} VALUES (?1,7,'running',?2)"),
                    params![target, doc.to_string()],
                )
                .unwrap();
            };
            write(&receipt);
            assert!(!recover(&c, dir.path(), &target, coordinator).unwrap());
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            assert!(
                !recover(&c, dir.path(), &target, coordinator).unwrap(),
                "live owner retains its normal exit receipt even after child exit"
            );
            drop(lease);
            for (field, value) in [
                (key, json!("other")),
                (version, json!(8)),
                ("scope", json!("other")),
                ("child_pid", json!(0)),
                ("owner_nonce", Value::Null),
                ("kernel_identity", Value::Null),
            ] {
                let mut bad = receipt.clone();
                bad[field] = value;
                write(&bad);
                assert!(
                    !recover(&c, dir.path(), &target, coordinator).unwrap(),
                    "invalid field: {field}"
                );
            }
            let mut foreign = receipt.clone();
            foreign["kernel_identity"]["scope"]["boot_id"] = json!(id());
            write(&foreign);
            assert!(!recover(&c, dir.path(), &target, coordinator).unwrap());
            write(&receipt);
            std::fs::remove_file(dir.path().join(&filename)).unwrap();
            assert!(
                !recover(&c, dir.path(), &target, coordinator).unwrap(),
                "missing lease must not be recreated as evidence"
            );
            drop(api::lock(dir.path(), &filename).unwrap());
            assert!(recover(&c, dir.path(), &target, coordinator).unwrap());
            let stored: String = c
                .query_row(
                    &format!("SELECT doc FROM {table} WHERE state='exited'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&stored).unwrap()["reason"],
                "recovered_native_exit"
            );
            assert!(!recover(&c, dir.path(), &target, coordinator).unwrap());
            // A completed normal wrapper receipt is never overwritten.
            c.execute(
                &format!("UPDATE {table} SET doc='{{\"reason\":\"exit\",\"code\":7}}'"),
                [],
            )
            .unwrap();
            assert!(!recover(&c, dir.path(), &target, coordinator).unwrap());
            let normal: String = c
                .query_row(&format!("SELECT doc FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&normal).unwrap()["code"], 7);
        }
    }
}
