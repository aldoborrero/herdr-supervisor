use super::*;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

const SENDER: &str = "sender-capability-never-print-this-secret";
const ADMIN: &str = "admin-capability-never-send-this-secret";

struct Probe {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    seen: Arc<Mutex<Vec<Value>>>,
}
impl Probe {
    fn new(path: &Path, response: impl Fn(Value) -> Value + Send + 'static) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        let (stop, mut stopped) = oneshot::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = seen.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted =
                    tokio::select! {_= &mut stopped=>break,accepted=listener.accept()=>accepted};
                let (stream, _) = accepted.unwrap();
                let mut reader = BufReader::new(stream);
                let mut bytes = Vec::new();
                timeout(Duration::from_secs(3), reader.read_until(b'\n', &mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                observed.lock().unwrap().push(request.clone());
                let mut bytes = serde_json::to_vec(&response(request)).unwrap();
                bytes.push(b'\n');
                reader.get_mut().write_all(&bytes).await.unwrap();
            }
        });
        Self { stop, task, seen }
    }
    async fn close(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}
fn herdr(request: &Value, protocol: u64) -> Value {
    assert_eq!(request["method"], "session.snapshot");
    json!({"id":request["id"],"result":{"type":"session_snapshot","snapshot":{"version":"0.9.1","protocol":protocol,"workspaces":[],"tabs":[],"panes":[],"layouts":[],"agents":[]}}})
}
fn mailer(request: &Value, owner: &str) -> Value {
    assert_eq!(request["token"], SENDER);
    match request["op"].as_str().unwrap() {
        "capabilities" => {
            json!({"data":{"protocol":2,"idempotentSend":true,"recipientPrecondition":true}})
        }
        "doctor" => {
            json!({"data":{"sqlite":"ok","owner":owner,"agent":"notifier","session":id(),"epoch":1}})
        }
        _ => panic!("diagnostic issued a non-read-only mailer operation"),
    }
}
async fn online() -> (Setup, NatsServer) {
    let mut f = Setup::new();
    let owner = Store::initialize(&f.controller).unwrap().owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = server.config("admin");
    let controller = server.config("controller");
    let node = server.config("node");
    f.spec.broker = BrokerSpec {
        url: admin.url,
        admin_credentials: admin.credentials,
        controller_credentials: controller.credentials,
        node_credentials: node.credentials,
        private_network_plaintext: true,
    };
    let mut prepared = f.prepare().unwrap();
    service_setup::provision(&f.bundle, &f.spec, &mut prepared)
        .await
        .unwrap();
    private_dir(&f.spec.native.mailer_state).unwrap();
    secret_file(&f.spec.notifier_credential, SENDER.as_bytes()).unwrap();
    secret_file(&f.spec.native.mailer_admin_credential, ADMIN.as_bytes()).unwrap();
    let result = std::process::Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "init", "--quiet"])
        .arg(&f.spec.repositories["/demo"])
        .output()
        .unwrap();
    assert!(result.status.success());
    (f, server)
}
fn passed(report: &Value, check: &str) -> bool {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["check"] == check && r["status"] == "passed")
}

#[tokio::test]
async fn check_local_missing_or_changed_bundle_is_not_repaired_and_sends_no_credentials() {
    let f = Setup::new();
    let report = service_setup::check(&f.controller, &f.bundle)
        .await
        .unwrap();
    assert_eq!(report["checks_passed"], false);
    assert!(!f.controller.exists());
    assert!(!f.bundle.exists());
    f.prepare().unwrap();
    let bytes = f.file("executor.json");
    let listener = UnixListener::bind(&f.spec.native.herdr_socket).unwrap();
    std::fs::remove_file(f.bundle.join("executor.json")).unwrap();
    let report = service_setup::check(&f.controller, &f.bundle)
        .await
        .unwrap();
    assert!(!passed(&report, "bundle_integrity"));
    assert!(!f.bundle.join("executor.json").exists());
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    secret_file(&f.bundle.join("executor.json"), &bytes).unwrap();
    let foreign = f.dir.path().join("foreign-controller");
    assert!(!passed(
        &service_setup::check(&foreign, &f.bundle).await.unwrap(),
        "bundle_integrity"
    ));
    assert!(!foreign.exists());
    assert_eq!(f.count("nodes"), 1);
    assert_eq!(f.count("operations"), 1);
}

#[tokio::test]
async fn check_local_observes_real_broker_git_and_private_protocols_without_decisions_or_admin_broker_use()
 {
    let (f, _server) = online().await;
    let owner = Identity::generate().unwrap().did();
    let h = Probe::new(&f.spec.native.herdr_socket, |r| herdr(&r, 22));
    let m = Probe::new(&f.spec.native.mailer_state.join("daemon.sock"), move |r| {
        mailer(&r, &owner)
    });
    // Runtime checks must work without the provisioner's credentials.
    std::fs::remove_file(&f.spec.broker.admin_credentials).unwrap();
    let before: BTreeMap<_, _> = std::fs::read_dir(&f.bundle)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    let report = service_setup::check(&f.controller, &f.bundle)
        .await
        .unwrap();
    assert_eq!(report["checks_passed"], true, "{report}");
    assert_eq!(report["model_readiness"], "not_checked");
    assert_eq!(report["services_started"], false);
    assert_eq!(report["models_started"], false);
    assert!(
        report["not_checked"]
            .as_array()
            .unwrap()
            .contains(&json!("model_authentication"))
    );
    assert!(!report.to_string().contains(SENDER));
    assert!(!report.to_string().contains(ADMIN));
    for (name, bytes) in before {
        assert_eq!(std::fs::read(f.bundle.join(name)).unwrap(), bytes);
    }
    assert_eq!(f.count("operations"), 1);
    assert_eq!(f.count("journal"), 1);
    assert_eq!(f.count("outbox"), 0);
    assert_eq!(f.count("runs"), 0);
    assert_eq!(h.seen.lock().unwrap().len(), 1);
    assert_eq!(m.seen.lock().unwrap().len(), 2);
    h.close().await;
    m.close().await;
}

#[tokio::test]
async fn check_local_reports_protocol_auth_repository_failures_without_external_error_text() {
    let (f, server) = online().await;
    let secret = format!("leaked {SENDER} {ADMIN}");
    let h = Probe::new(&f.spec.native.herdr_socket, |r| herdr(&r, 21));
    let m = Probe::new(
        &f.spec.native.mailer_state.join("daemon.sock"),
        move |_| json!({"error":secret}),
    );
    // Same configured credential file, now an account lacking this node's route.
    let other = server.config("other");
    std::fs::write(
        &f.spec.broker.node_credentials,
        std::fs::read(other.credentials).unwrap(),
    )
    .unwrap();
    std::fs::remove_dir_all(f.spec.repositories["/demo"].join(".git")).unwrap();
    let report = service_setup::check(&f.controller, &f.bundle)
        .await
        .unwrap();
    assert!(passed(&report, "bundle_integrity"));
    assert!(passed(&report, "controller_broker"));
    assert!(passed(&report, "authority_rechecked"));
    for name in [
        "node_broker",
        "herdr_snapshot",
        "mailer_sender",
        "repository",
    ] {
        assert!(!passed(&report, name));
    }
    assert_eq!(report["checks_passed"], false);
    assert!(!report.to_string().contains("leaked"));
    assert!(!report.to_string().contains(SENDER));
    assert!(!report.to_string().contains(ADMIN));
    h.close().await;
    m.close().await;
}

#[tokio::test]
async fn check_local_rechecks_retirement_after_successful_external_observations() {
    let (f, _server) = online().await;
    let controller = f.controller.clone();
    let owner = Identity::generate().unwrap().did();
    let h = Probe::new(&f.spec.native.herdr_socket, move |r| {
        Store::open(&controller)
            .unwrap()
            .connection
            .execute("UPDATE nodes SET active=0", [])
            .unwrap();
        herdr(&r, 22)
    });
    let m = Probe::new(&f.spec.native.mailer_state.join("daemon.sock"), move |r| {
        mailer(&r, &owner)
    });
    let report = service_setup::check(&f.controller, &f.bundle)
        .await
        .unwrap();
    assert!(passed(&report, "bundle_integrity"));
    assert!(passed(&report, "herdr_snapshot"));
    assert!(passed(&report, "mailer_sender"));
    assert!(!passed(&report, "authority_rechecked"));
    assert_eq!(report["checks_passed"], false);
    assert_eq!(f.count("operations"), 1);
    h.close().await;
    m.close().await;
}

#[tokio::test]
async fn check_local_rejects_oversized_mailer_frames_and_unhealthy_or_incomplete_doctor() {
    let (f, _server) = online().await;
    let owner = Identity::generate().unwrap().did();
    let mode = Arc::new(AtomicU8::new(0));
    let selected = mode.clone();
    let h = Probe::new(&f.spec.native.herdr_socket, |r| herdr(&r, 22));
    let m = Probe::new(&f.spec.native.mailer_state.join("daemon.sock"), move |r| {
        if selected.load(Ordering::Relaxed) == 0 {
            return json!({"data":"x".repeat(65537)});
        }
        let mut response = mailer(&r, &owner);
        if r["op"] == "doctor" {
            if selected.load(Ordering::Relaxed) == 1 {
                response["data"]["sqlite"] = json!("corrupt");
            } else {
                response["data"]["session"] = json!("not-a-uuid");
            }
        }
        response
    });
    for value in 0..3 {
        mode.store(value, Ordering::Relaxed);
        let report = service_setup::check(&f.controller, &f.bundle)
            .await
            .unwrap();
        assert!(!passed(&report, "mailer_sender"));
        assert!(passed(&report, "herdr_snapshot"));
    }
    h.close().await;
    m.close().await;
}
