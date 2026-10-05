use super::*;
use crate::control::{
    broker_policy::{self, Endpoint},
    transport::Broker,
};
use futures_util::StreamExt;
use std::time::Duration;

#[test]
fn broker_routes_reject_reserved_names_and_wildcards_before_enrollment_or_grant() {
    let mut f = Fixture::new();
    let owner = f.store.owner().unwrap();
    for node in ["controller", "*", ">", "node.>", "", "has space"] {
        assert!(broker_policy::policy(&owner, &[node.into()]).is_err());
        assert!(broker_policy::inbox_prefix(&owner, Endpoint::Node(node)).is_err());
        let key = Identity::generate().unwrap();
        assert!(
            f.store
                .apply(&request(
                    &f.owner,
                    Mutation::EnrollNode {
                        id: node.into(),
                        key: key.did(),
                        max_active: 1
                    }
                ))
                .is_err()
        );
        assert!(
            key.delegation(node, &f.node_key.did(), 1, now() + 60)
                .is_err()
        );
    }
    assert_eq!(f.store.nodes().unwrap(), vec!["local"]);
    assert!(broker_policy::policy(&owner, &["local".into(), "local".into()]).is_err());
    assert!(broker_policy::policy("not-a-did", &["local".into()]).is_err());
    let policy = broker_policy::policy(&owner, &["provisioner".into(), "local".into()]).unwrap();
    assert_ne!(
        policy["nodes"]["provisioner"]["inbox_prefix"],
        broker_policy::inbox_prefix(&owner, Endpoint::Provisioner).unwrap()
    );
}

#[tokio::test]
async fn real_broker_generated_acl_isolates_reply_inboxes_and_runtime_management() {
    let f = Fixture::new();
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = Broker::connect(&server.config("admin"), &owner, Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into(), "other".into()])
        .await
        .unwrap();
    let controller = Broker::connect(&server.config("controller"), &owner, Endpoint::Controller)
        .await
        .unwrap();
    let local = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    let other_config = server.config("other");
    let other = Broker::connect(&other_config, &owner, Endpoint::Node("other"))
        .await
        .unwrap();

    let (events, mut errors) = tokio::sync::mpsc::unbounded_channel();
    let snooper = async_nats::ConnectOptions::new()
        .user_and_password("node".into(), "node-test-password".into())
        .custom_inbox_prefix(broker_policy::inbox_prefix(&owner, Endpoint::Node("local")).unwrap())
        .event_callback(move |event| {
            let events = events.clone();
            async move {
                if let async_nats::Event::ServerError(error) = event {
                    let _ = events.send(error.to_string());
                }
            }
        })
        .connect(&server.url)
        .await
        .unwrap();

    let foreign_owner = Identity::generate().unwrap().did();
    let mut denied = Vec::new();
    for prefix in [
        "_INBOX".into(),
        "_HS_INBOX".into(),
        format!("_HS_INBOX.{}", digest(owner.as_bytes())),
        broker_policy::inbox_prefix(&owner, Endpoint::Controller).unwrap(),
        broker_policy::inbox_prefix(&owner, Endpoint::Provisioner).unwrap(),
        broker_policy::inbox_prefix(&owner, Endpoint::Node("other")).unwrap(),
        broker_policy::inbox_prefix(&foreign_owner, Endpoint::Node("local")).unwrap(),
    ] {
        let subject = format!("{prefix}.>");
        denied.push(snooper.subscribe(subject.clone()).await.unwrap());
        snooper.flush().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(3), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            error.to_lowercase().contains("permissions violation") && error.contains(&subject),
            "{error}"
        );
    }

    // A legitimate pull publishes private data to another runtime's inbox.
    // The rejected wildcard subscriptions must see none of that response.
    for (target, receiver, payload) in [
        ("other", &other, "other-private-assignment"),
        ("local", &local, "own-assignment"),
    ] {
        controller
            .context
            .publish(controller.subject(target).unwrap(), payload.into())
            .await
            .unwrap()
            .await
            .unwrap();
        let consumer = receiver.consumer(target).await.unwrap();
        let mut messages = consumer
            .fetch()
            .max_messages(1)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .unwrap();
        let message = messages.next().await.unwrap().unwrap();
        assert_eq!(message.payload.as_ref(), payload.as_bytes());
        message.double_ack().await.unwrap();
    }
    other
        .context
        .publish(
            other.subject("controller").unwrap(),
            "private-node-event".into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    let consumer = controller.consumer("controller").await.unwrap();
    let mut messages = consumer
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(2))
        .messages()
        .await
        .unwrap();
    let message = messages.next().await.unwrap().unwrap();
    assert_eq!(message.payload.as_ref(), b"private-node-event");
    message.double_ack().await.unwrap();
    snooper.flush().await.unwrap();
    for mut subscription in denied {
        assert!(!matches!(
            tokio::time::timeout(Duration::from_millis(30), subscription.next()).await,
            Ok(Some(_))
        ));
    }

    // Neither reads nor management APIs of another consumer can be invoked.
    // Record a broker permission error, not merely a client request timeout.
    for subject in [
        format!("$JS.API.CONSUMER.MSG.NEXT.{}.other", local.stream),
        format!("$JS.API.CONSUMER.INFO.{}.controller", local.stream),
        format!("$JS.API.STREAM.MSG.GET.{}", local.stream),
        format!("$JS.API.STREAM.PURGE.{}", local.stream),
        format!("$JS.API.STREAM.DELETE.{}", local.stream),
        format!("$JS.API.CONSUMER.DELETE.{}.local", local.stream),
        format!("$JS.API.CONSUMER.CREATE.{}", local.stream),
        format!("$JS.ACK.{}.other.1.1.1.1.0", local.stream),
        controller.subject("other").unwrap(),
        format!(
            "{}.fake",
            broker_policy::inbox_prefix(&owner, Endpoint::Node("other")).unwrap()
        ),
    ] {
        snooper.publish(subject.clone(), "{}".into()).await.unwrap();
        snooper.flush().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(3), errors.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            error.to_lowercase().contains("permissions violation") && error.contains(&subject),
            "{error}"
        );
    }
    drop(other);
    let reconnected = Broker::connect(&other_config, &owner, Endpoint::Node("other"))
        .await
        .unwrap();
    let mut consumer = reconnected.consumer("other").await.unwrap();
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    let mut stream = admin.context.get_stream(&admin.stream).await.unwrap();
    assert_eq!(stream.info().await.unwrap().state.messages, 3);
}
