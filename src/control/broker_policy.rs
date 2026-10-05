//! Runtime reply inboxes and matching deny-by-default NATS permissions.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

use super::{
    identity::verifying_key,
    model::{digest, identifier},
};

#[derive(Clone, Copy)]
pub enum Endpoint<'a> {
    Provisioner,
    Controller,
    Node(&'a str),
}

pub fn node_id(node: &str) -> Result<()> {
    identifier(node)?;
    ensure!(
        node != "controller",
        "controller is a reserved broker route"
    );
    Ok(())
}

pub fn inbox_prefix(owner: &str, endpoint: Endpoint<'_>) -> Result<String> {
    verifying_key(owner)?;
    let suffix = match endpoint {
        Endpoint::Provisioner => "provisioner".into(),
        Endpoint::Controller => "controller".into(),
        Endpoint::Node(node) => {
            node_id(node)?;
            format!("node.{node}")
        }
    };
    Ok(format!("_HS_INBOX.{}.{suffix}", digest(owner.as_bytes())))
}

fn runtime_permissions(
    owner: &str,
    target: &str,
    endpoint: Endpoint<'_>,
    destinations: Vec<String>,
) -> Result<Value> {
    let stream = format!("HS_{}", digest(owner.as_bytes()));
    let prefix = inbox_prefix(owner, endpoint)?;
    let mut publish = destinations;
    publish.extend([
        format!("$JS.API.STREAM.INFO.{stream}"),
        format!("$JS.API.CONSUMER.INFO.{stream}.{target}"),
        format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.{target}"),
        format!("$JS.ACK.{stream}.{target}.>"),
    ]);
    Ok(json!({"inbox_prefix":prefix,"permissions":{
        "publish":{"allow":publish},
        "subscribe":{"allow":[format!("{prefix}.>")]}
    }}))
}

/// Public deployment material only. Does not grant identities, provision a
/// broker, generate credentials or change its configuration.
pub fn policy(owner: &str, nodes: &[String]) -> Result<Value> {
    verifying_key(owner)?;
    let prefix = format!("hs.{}", digest(owner.as_bytes()));
    let mut entries = serde_json::Map::new();
    let mut destinations = Vec::new();
    for node in nodes {
        node_id(node)?;
        ensure!(!entries.contains_key(node), "duplicate broker node");
        destinations.push(format!("{prefix}.to.{node}"));
        entries.insert(
            node.clone(),
            runtime_permissions(
                owner,
                node,
                Endpoint::Node(node),
                vec![format!("{prefix}.to.controller")],
            )?,
        );
    }
    destinations.sort();
    Ok(json!({
        "version":1,
        "owner":owner,
        "stream":format!("HS_{}",digest(owner.as_bytes())),
        "controller":runtime_permissions(owner,"controller",Endpoint::Controller,destinations)?,
        "nodes":entries
    }))
}
