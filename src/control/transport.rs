//! NATS carries signed records. All acknowledgements follow local durable application.
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use async_nats::jetstream::{self, consumer::PullConsumer};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::broker_policy::{Endpoint, inbox_prefix};
use super::identity::{Identity, SignedRecord};
use super::model::{digest, identifier};
use super::node::NodeStore;
use super::store::Store;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub url: String,
    /// Standard NATS .creds (JWT/nkey); alternatively a private JSON user/password file.
    pub credentials: PathBuf,
    #[serde(default)]
    pub private_network_plaintext: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password {
    user: String,
    password: String,
}

pub struct Broker {
    pub context: jetstream::Context,
    pub stream: String,
    pub prefix: String,
}

impl Broker {
    pub async fn connect(
        config: &BrokerConfig,
        owner: &str,
        endpoint: Endpoint<'_>,
    ) -> Result<Self> {
        let inbox = inbox_prefix(owner, endpoint)?;
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(&config.credentials)?;
        ensure!(
            meta.is_file() && meta.permissions().mode() & 0o077 == 0 && meta.len() <= 65536,
            "NATS credentials must be private, regular and bounded"
        );
        let contents = std::fs::read_to_string(&config.credentials)?;
        let mut options = if contents.starts_with("-----BEGIN NATS USER JWT-----") {
            async_nats::ConnectOptions::new().credentials(&contents)?
        } else {
            let pass: Password = serde_json::from_str(&contents)?;
            ensure!(
                !pass.user.is_empty() && !pass.password.is_empty(),
                "empty broker credentials"
            );
            async_nats::ConnectOptions::new().user_and_password(pass.user, pass.password)
        };
        // TLS remains the default even over NetBird. Plaintext is an explicit private-network choice.
        options = options
            .custom_inbox_prefix(inbox)
            .require_tls(!config.private_network_plaintext)
            .connection_timeout(Duration::from_secs(5));
        let client = options.connect(&config.url).await?;
        let prefix = format!("hs.{}", digest(owner.as_bytes()));
        let stream = format!("HS_{}", digest(owner.as_bytes()));
        Ok(Self {
            context: jetstream::new(client),
            stream,
            prefix,
        })
    }

    pub fn subject(&self, target: &str) -> Result<String> {
        identifier(target)?;
        Ok(format!("{}.to.{target}", self.prefix))
    }

    /// Explicit administrator operation; normal controller/node loops only bind existing resources.
    pub async fn provision(&self, targets: &[String]) -> Result<()> {
        let mut stream = self
            .context
            .get_or_create_stream(jetstream::stream::Config {
                name: self.stream.clone(),
                subjects: vec![format!("{}.to.*", self.prefix)],
                retention: jetstream::stream::RetentionPolicy::Limits,
                storage: jetstream::stream::StorageType::File,
                discard: jetstream::stream::DiscardPolicy::New,
                max_bytes: 1024 * 1024 * 1024,
                max_messages: 1_000_000,
                max_message_size: 300_000,
                max_age: Duration::ZERO,
                duplicate_window: Duration::from_secs(120),
                ..Default::default()
            })
            .await?;
        let config = &stream.info().await?.config;
        ensure!(
            config.retention == jetstream::stream::RetentionPolicy::Limits
                && config.storage == jetstream::stream::StorageType::File
                && config.discard == jetstream::stream::DiscardPolicy::New
                && config.max_age == Duration::ZERO
                && config.subjects == vec![format!("{}.to.*", self.prefix)],
            "existing stream has incompatible durability or routing"
        );
        for target in targets {
            identifier(target)?;
            let consumer: PullConsumer = stream
                .get_or_create_consumer(
                    target,
                    jetstream::consumer::pull::Config {
                        durable_name: Some(target.clone()),
                        filter_subject: self.subject(target)?,
                        ack_policy: jetstream::consumer::AckPolicy::Explicit,
                        ack_wait: Duration::from_secs(10),
                        max_ack_pending: 64,
                        ..Default::default()
                    },
                )
                .await?;
            ensure!(
                consumer.cached_info().config.filter_subject == self.subject(target)?,
                "consumer route mismatch"
            );
        }
        Ok(())
    }

    pub async fn consumer(&self, target: &str) -> Result<PullConsumer> {
        identifier(target)?;
        let stream = self.context.get_stream(&self.stream).await?;
        let c: PullConsumer = stream
            .get_consumer(target)
            .await
            .map_err(anyhow::Error::from_boxed)?;
        ensure!(
            c.cached_info().config.filter_subject == self.subject(target)?
                && c.cached_info().config.ack_policy == jetstream::consumer::AckPolicy::Explicit,
            "invalid consumer configuration"
        );
        Ok(c)
    }

    pub async fn publish(&self, target: &str, record: &SignedRecord) -> Result<()> {
        record.verify(jiff::Timestamp::now().as_second())?;
        let bytes = serde_json::to_vec(record)?;
        ensure!(bytes.len() <= 300000, "wire record too large");
        let mut headers = async_nats::HeaderMap::new();
        let dedup = digest(
            format!(
                "{}\0{}\0{}",
                record.record.owner, record.record.author, record.record.id
            )
            .as_bytes(),
        );
        headers.insert("Nats-Msg-Id", dedup.as_str());
        // Second await proves durable broker storage, not receipt or execution by the recipient.
        self.context
            .publish_with_headers(self.subject(target)?, headers, bytes.into())
            .await?
            .await?;
        Ok(())
    }
}

pub fn decode(bytes: &[u8]) -> Result<SignedRecord> {
    ensure!(bytes.len() <= 300000, "incoming record too large");
    Ok(serde_json::from_slice(bytes)?)
}

pub async fn controller_once(store: &mut Store, broker: &Broker) -> Result<usize> {
    let identity = Identity::load(&store.dir.join("owner.key"))?;
    let mut count = 0;
    // Receive before publishing: a failed/expired outgoing record must not
    // prevent new results, cancellation receipts or resource bindings.
    let consumer = broker.consumer("controller").await?;
    let mut messages = consumer
        .fetch()
        .max_messages(64)
        .expires(Duration::from_millis(200))
        .messages()
        .await?;
    while let Some(message) = messages.next().await {
        let message = message.map_err(anyhow::Error::from_boxed)?;
        let accepted = decode(&message.payload).and_then(|signed| store.receive(&signed));
        if let Err(error) = accepted {
            eprintln!("controller rejected record: {error}");
            continue;
        }
        message
            .double_ack()
            .await
            .map_err(|e| anyhow::anyhow!("broker ACK: {e}"))?;
        count += 1;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    for (id, target) in super::delivery::controller_due(&store.connection)? {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        if !super::delivery::begin(&store.connection, super::delivery::Queue::Controller, &id)? {
            continue;
        }
        let wire = match store.prepare_wire(&id, &identity) {
            Ok(wire) => wire,
            Err(_) => {
                super::delivery::failed(
                    &store.connection,
                    super::delivery::Queue::Controller,
                    &id,
                    "envelope preparation failed; retry scheduled",
                )?;
                continue;
            }
        };
        if !valid_outgoing(
            &store.connection,
            super::delivery::Queue::Controller,
            &id,
            &wire,
        )? {
            continue;
        }
        if publish_bounded(broker, &target, &wire, deadline).await {
            if store.published(&id).is_ok() {
                count += 1;
            } else {
                super::delivery::failed(
                    &store.connection,
                    super::delivery::Queue::Controller,
                    &id,
                    "broker accepted; local receipt not recorded",
                )?;
            }
        } else {
            super::delivery::failed(
                &store.connection,
                super::delivery::Queue::Controller,
                &id,
                "broker acceptance unconfirmed; retry scheduled",
            )?;
        }
    }
    Ok(count)
}

pub async fn node_once(store: &mut NodeStore, broker: &Broker) -> Result<usize> {
    let mut count = 0;
    let consumer = broker.consumer(&store.node_id).await?;
    let mut messages = consumer
        .fetch()
        .max_messages(64)
        .expires(Duration::from_millis(200))
        .messages()
        .await?;
    while let Some(message) = messages.next().await {
        let message = message.map_err(anyhow::Error::from_boxed)?;
        let accepted = decode(&message.payload).and_then(|signed| store.receive(&signed));
        if let Err(error) = accepted {
            eprintln!("node rejected record: {error}");
            continue;
        }
        message
            .double_ack()
            .await
            .map_err(|e| anyhow::anyhow!("broker ACK: {e}"))?;
        count += 1;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    for (id, wire) in super::delivery::node_due(&store.connection)? {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let record: SignedRecord = match serde_json::from_str(&wire) {
            Ok(record) => record,
            Err(_) => {
                super::delivery::terminal(
                    &store.connection,
                    super::delivery::Queue::Node,
                    &id,
                    false,
                )?;
                continue;
            }
        };
        if !valid_outgoing(
            &store.connection,
            super::delivery::Queue::Node,
            &id,
            &record,
        )? {
            continue;
        }
        if !super::delivery::begin(&store.connection, super::delivery::Queue::Node, &id)? {
            continue;
        }
        if publish_bounded(broker, "controller", &record, deadline).await {
            if store.published(&id).is_ok() {
                count += 1;
            } else {
                super::delivery::failed(
                    &store.connection,
                    super::delivery::Queue::Node,
                    &id,
                    "broker accepted; local receipt not recorded",
                )?;
            }
        } else {
            super::delivery::failed(
                &store.connection,
                super::delivery::Queue::Node,
                &id,
                "broker acceptance unconfirmed; retry scheduled",
            )?;
        }
    }
    Ok(count)
}

fn valid_outgoing(
    connection: &rusqlite::Connection,
    queue: super::delivery::Queue,
    id: &str,
    record: &SignedRecord,
) -> Result<bool> {
    let now = super::store::now();
    if record.record.expires_at <= now {
        super::delivery::terminal(connection, queue, id, true)?;
        return Ok(false);
    }
    if record.record.created_at > now + 30 {
        // A local clock correction can make a valid frozen record premature.
        // Defer without changing its signed timestamps or permanently rejecting it.
        super::delivery::begin(connection, queue, id)?;
        super::delivery::failed(
            connection,
            queue,
            id,
            "outgoing timestamp is ahead of local clock; retry scheduled",
        )?;
        return Ok(false);
    }
    if record.verify(now).is_err() {
        super::delivery::terminal(connection, queue, id, false)?;
        return Ok(false);
    }
    Ok(true)
}

async fn publish_bounded(
    broker: &Broker,
    target: &str,
    record: &SignedRecord,
    deadline: tokio::time::Instant,
) -> bool {
    let timeout = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .min(Duration::from_secs(2));
    matches!(
        tokio::time::timeout(timeout, broker.publish(target, record)).await,
        Ok(Ok(()))
    )
}

pub fn read_config(path: &Path) -> Result<BrokerConfig> {
    ensure!(
        std::fs::metadata(path)?.len() <= 65536,
        "broker configuration too large"
    );
    serde_json::from_slice(&std::fs::read(path)?).context("broker configuration")
}
