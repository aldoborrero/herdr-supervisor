use super::*;
use tokio::net::UnixListener;

async fn accept(listener: &UnixListener) -> Frames {
    Frames::new(
        timeout(DEADLINE, listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0,
    )
}
async fn reply(reader: &mut Frames, value: Value) {
    request(reader.reader.get_mut(), value).await.unwrap();
}
async fn subscribed(listener: &UnixListener) -> Frames {
    let mut stream = accept(listener).await;
    let value = stream.next(FRAME_BYTES).await.unwrap();
    assert_eq!(value["method"], "events.subscribe");
    reply(
        &mut stream,
        json!({"id":value["id"],"result":{"type":"subscription_started"}}),
    )
    .await;
    stream
}
async fn snapshot_reply(listener: &UnixListener, protocol: u64) {
    let mut stream = accept(listener).await;
    let value = stream.next(FRAME_BYTES).await.unwrap();
    assert_eq!(value["method"], "session.snapshot");
    reply(&mut stream, json!({"id":value["id"],"result":{"type":"session_snapshot","snapshot":{"protocol":protocol,"version":"0.9.1","agents":[],"panes":[],"workspaces":[],"tabs":[],"layouts":[]}}})).await;
}
async fn wait_state(state: &AtomicU8, value: u8) {
    timeout(DEADLINE, async {
        while state.load(Ordering::Relaxed) != value {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn partial_frame_survives_cancel_and_keeps_its_original_deadline() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let mut reader = Frames::new(client);
    server.write_all(b"{\"event\":").await.unwrap();
    assert!(
        timeout(Duration::from_millis(20), reader.next(128))
            .await
            .is_err()
    );
    assert_eq!(reader.bytes, b"{\"event\":");
    let deadline = reader.deadline.unwrap();
    assert!(
        timeout(Duration::from_millis(20), reader.next(128))
            .await
            .is_err()
    );
    assert_eq!(reader.deadline, Some(deadline));
    server
        .write_all(b"\"pane_exited\",\"data\":{}}\n")
        .await
        .unwrap();
    assert!(event(&reader.next(128).await.unwrap()).unwrap());
    assert!(reader.bytes.is_empty());
    assert!(reader.deadline.is_none());
    server.write_all(b"{").await.unwrap();
    assert!(
        timeout(Duration::from_millis(20), reader.next(128))
            .await
            .is_err()
    );
    reader.deadline = Some(Instant::now() - Duration::from_millis(1));
    assert!(reader.next(128).await.is_err());
}

#[tokio::test]
async fn frames_bound_bytes_and_reject_eof_malformed_and_wrong_ack() {
    for bytes in [vec![b'x'; 33], b"{}".to_vec(), b"not-json\n".to_vec()] {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&bytes).await.unwrap();
        drop(server);
        assert!(Frames::new(client).next(32).await.is_err());
    }
    let (client, mut server) = UnixStream::pair().unwrap();
    server.write_all(b"{}\n").await.unwrap();
    assert_eq!(Frames::new(client).next(3).await.unwrap(), json!({}));
    assert!(
        acknowledge(
            &json!({"id":"other","result":{"type":"subscription_started"}}),
            "expected",
            "subscription_started"
        )
        .is_err()
    );
    assert!(event(&json!({"error":{"code":"events_lost"}})).is_err());
    assert!(event(&json!({"event":"pane_exited","data":null})).is_err());
    assert!(!event(&json!({"event":"future_event","data":{}})).unwrap());
    for name in ["pane_exited", "pane.exited", "pane.agent_status_changed"] {
        assert!(event(&json!({"event":name,"data":{}})).unwrap());
    }
}

#[tokio::test]
async fn subscription_precedes_snapshot_and_partial_event_survives_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let state = Arc::new(AtomicU8::new(0));
    let (tx, mut rx) = mpsc::channel(1);
    let observed = state.clone();
    let task =
        tokio::spawn(async move { session(&socket, &BTreeSet::new(), &tx, &observed).await });
    let mut events = subscribed(&listener).await;
    events
        .reader
        .get_mut()
        .write_all(b"{\"event\":\"pane_")
        .await
        .unwrap();
    // Leave a partial frame in flight when snapshot completion wins select!.
    tokio::time::sleep(Duration::from_millis(20)).await;
    snapshot_reply(&listener, 22).await;
    wait_state(&state, 2).await;
    assert_eq!(rx.recv().await, Some(()));
    events
        .reader
        .get_mut()
        .write_all(b"exited\",\"data\":{}}\n")
        .await
        .unwrap();
    assert_eq!(timeout(DEADLINE, rx.recv()).await.unwrap(), Some(()));
    reply(&mut events, json!({"error":{"code":"events_lost"}})).await;
    assert!(timeout(DEADLINE, task).await.unwrap().unwrap().is_err());
}

#[tokio::test]
async fn absent_named_pane_falls_back_to_lifecycle_and_unknown_protocol_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let state = Arc::new(AtomicU8::new(0));
    let (tx, mut rx) = mpsc::channel(1);
    let observed = state.clone();
    let path = socket.clone();
    let task = tokio::spawn(async move {
        session(&path, &BTreeSet::from(["old-pane".into()]), &tx, &observed).await
    });
    let mut first = accept(&listener).await;
    let req = first.next(FRAME_BYTES).await.unwrap();
    assert!(
        req["params"]["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["pane_id"] == "old-pane")
    );
    reply(
        &mut first,
        json!({"id":req["id"],"error":{"code":"pane_not_found"}}),
    )
    .await;
    let mut second = accept(&listener).await;
    let req = second.next(FRAME_BYTES).await.unwrap();
    assert!(
        req["params"]["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s.get("pane_id").is_none())
    );
    reply(
        &mut second,
        json!({"id":req["id"],"result":{"type":"subscription_started"}}),
    )
    .await;
    snapshot_reply(&listener, 22).await;
    wait_state(&state, 4).await;
    assert_eq!(rx.recv().await, Some(()));
    drop(second);
    assert!(timeout(DEADLINE, task).await.unwrap().unwrap().is_err());
    let responder = tokio::spawn(async move {
        snapshot_reply(&listener, 21).await;
    });
    assert!(snapshot(&socket).await.is_err());
    responder.await.unwrap();
}

#[tokio::test]
async fn watchers_reconnect_resnapshot_coalesce_and_close_owned_streams() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut watchers = Watchers::new();
    watchers.reconcile(Targets::from([(socket.clone(), BTreeSet::new())]));
    let mut stream = subscribed(&listener).await;
    snapshot_reply(&listener, 22).await;
    wait_state(&watchers.entries[&socket].state, 2).await;
    reply(&mut stream, json!({"error":{"code":"events_lost"}})).await;
    drop(stream);
    let mut stream = subscribed(&listener).await;
    snapshot_reply(&listener, 22).await;
    wait_state(&watchers.entries[&socket].state, 2).await;
    let wire = b"{\"event\":\"pane_updated\",\"data\":{}}\n".repeat(1000);
    stream.reader.get_mut().write_all(&wire).await.unwrap();
    watchers.wait().await;
    assert!(watchers.rx.len() <= 1);
    let started = Instant::now();
    watchers.tx.try_send(()).ok();
    watchers.wait().await;
    assert!(started.elapsed() >= Duration::from_millis(90));
    assert_eq!(watchers.health()["watching"], 1);
    watchers.shutdown().await;
    assert!(watchers.entries.is_empty());
    assert!(
        timeout(DEADLINE, stream.next(FRAME_BYTES))
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
async fn changed_targets_abort_old_stream_and_limits_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("events.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut watchers = Watchers::new();
    watchers.reconcile(Targets::from([(socket.clone(), BTreeSet::new())]));
    let mut stream = subscribed(&listener).await;
    snapshot_reply(&listener, 22).await;
    wait_state(&watchers.entries[&socket].state, 2).await;
    watchers.reconcile(Targets::new());
    assert!(
        timeout(DEADLINE, stream.next(FRAME_BYTES))
            .await
            .unwrap()
            .is_err()
    );
    let panes: BTreeSet<String> = (0..MAX_PANES + 1).map(|n| format!("pane-{n}")).collect();
    let targets = (0..MAX_SOCKETS + 1)
        .map(|n| {
            (
                dir.path().join(format!("absent-{n:03}")),
                if n == 0 {
                    panes.clone()
                } else {
                    BTreeSet::new()
                },
            )
        })
        .collect();
    watchers.reconcile(targets);
    assert_eq!(watchers.health()["polling_only_sockets"], 1);
    assert_eq!(watchers.health()["polling_only_panes"], 1);
    assert_eq!(watchers.entries.len(), MAX_SOCKETS);
    watchers.shutdown().await;
}

#[test]
fn target_discovery_uses_only_active_saved_plans_and_explicit_configuration() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE jobs(state TEXT,checkpoint TEXT);CREATE TABLE coordinator_launches(state TEXT,doc TEXT);").unwrap();
    for (state, doc) in [
        (
            "prepared",
            json!({"socket":"/saved.sock","binding":{"pane":"before-launch"}}),
        ),
        (
            "ready",
            json!({"checkout":{"socket":"/saved.sock","binding":{"pane":"launched"}}}),
        ),
        (
            "stopped",
            json!({"socket":"/retired.sock","binding":{"pane":"retired"}}),
        ),
        ("reserved", json!({})),
    ] {
        db.execute(
            "INSERT INTO jobs VALUES (?1,?2)",
            rusqlite::params![state, doc.to_string()],
        )
        .unwrap();
    }
    let targets = node_targets(&db, Path::new("/configured.sock")).unwrap();
    assert_eq!(targets.len(), 2);
    assert!(targets[Path::new("/configured.sock")].is_empty());
    assert_eq!(
        targets[Path::new("/saved.sock")],
        BTreeSet::from(["before-launch".into(), "launched".into()])
    );
    for state in ["launching", "exited"] {
        let doc = json!({"config":{"herdr_socket":format!("/{state}.sock")},"pane":{"pane":"coordinator"}});
        db.execute(
            "INSERT INTO coordinator_launches VALUES (?1,?2)",
            rusqlite::params![state, doc.to_string()],
        )
        .unwrap();
    }
    assert_eq!(
        controller_targets(&db).unwrap(),
        Targets::from([(
            PathBuf::from("/launching.sock"),
            BTreeSet::from(["coordinator".into()])
        )])
    );
}
