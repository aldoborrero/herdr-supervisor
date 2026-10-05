//! Chunked reads of the same public projections exposed by collection pages.
//! Each request reauthorizes and hashes the complete current projection before
//! returning a UTF-8 fragment. A changed record must be fetched from a new page.
use super::{
    model,
    pagination::{self, Collection},
    store::Store,
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CHUNK_BYTES: usize = 32768;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    version: u8,
    owner: String,
    principal: String,
    run: String,
    collection: Collection,
    key: String,
    bytes: u64,
    sha256: String,
    offset: u64,
}
impl Position {
    fn encode(&self) -> Result<String> {
        Ok(bs58::encode(serde_json::to_vec(self)?).into_string())
    }
    fn decode(cursor: &str) -> Result<Self> {
        ensure!(cursor.len() <= 2048, "record cursor too large");
        let p: Self = serde_json::from_slice(&bs58::decode(cursor).into_vec()?)?;
        ensure!(
            p.version == 1 && p.offset < p.bytes,
            "invalid record position"
        );
        ensure!(
            p.sha256.len() == 64
                && p.sha256
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "invalid record digest"
        );
        if p.collection.policy() {
            model::identifier(&p.key)?;
        } else {
            ensure!(
                p.key.len() == 20
                    && p.key.bytes().all(|c| c.is_ascii_digit())
                    && p.key.parse::<u64>()? <= i64::MAX as u64,
                "invalid record key"
            );
        }
        Ok(p)
    }
}

fn query(collection: Collection) -> String {
    let key = if collection.policy() {
        "?2"
    } else {
        "CAST(?2 AS INTEGER)"
    };
    format!("SELECT doc FROM ({}) WHERE key={key}", collection.source())
}

pub(super) fn reference(
    c: &Connection,
    owner: &str,
    principal: &str,
    run: &str,
    collection: Collection,
    key: &str,
) -> Result<Value> {
    let (bytes, sha256) = c.query_row(&query(collection), params![run, key], |row| {
        // Borrow SQLite's projection; do not copy/parse the whole JSON into Rust.
        let doc = row.get_ref(0)?.as_str()?;
        Ok((doc.len() as u64, model::digest(doc.as_bytes())))
    })?;
    let p = Position {
        version: 1,
        owner: owner.into(),
        principal: principal.into(),
        run: run.into(),
        collection,
        key: key.into(),
        bytes,
        sha256,
        offset: 0,
    };
    Ok(
        json!({"kind":"record_reference","bytes":p.bytes,"sha256":p.sha256,"encoding":"utf8_json","cursor":p.encode()?}),
    )
}

pub fn read(store: &Store, token: &str, run: &str, cursor: &str) -> Result<Value> {
    let mut p = Position::decode(cursor)?;
    let tx = store.connection.unchecked_transaction()?;
    let (principal, owner, _) = pagination::authorize(&tx, token, run)?;
    ensure!(
        p.owner == owner && p.principal == principal.id && p.run == run,
        "record cursor belongs to a different scope"
    );
    let (bytes, hash, text, end) =
        tx.query_row(&query(p.collection), params![run, p.key], |row| {
            let doc = row.get_ref(0)?.as_str()?;
            let start = usize::try_from(p.offset).unwrap_or(usize::MAX);
            let mut end = start.saturating_add(CHUNK_BYTES).min(doc.len());
            while end > start && !doc.is_char_boundary(end) {
                end -= 1;
            }
            let text = doc.get(start..end).map(str::to_owned);
            Ok((
                doc.len() as u64,
                model::digest(doc.as_bytes()),
                text,
                end as u64,
            ))
        })?;
    ensure!(
        bytes == p.bytes && hash == p.sha256,
        "record changed; restart from a fresh collection page"
    );
    let text = text.ok_or_else(|| anyhow::anyhow!("record offset is not a UTF-8 boundary"))?;
    ensure!(end > p.offset, "record read made no progress");
    let offset = p.offset;
    p.offset = end;
    let result = json!({"kind":"record_chunk","run":run,"collection":p.collection,"key":p.key,"encoding":"utf8_json","offset":offset,"end":end,"bytes":bytes,"sha256":hash,"text":text,"next_cursor":if end < bytes {Some(p.encode()?)} else {None}});
    ensure!(
        serde_json::to_vec(&result)?.len() <= 256 * 1024,
        "record chunk envelope too large"
    );
    tx.commit()?;
    Ok(result)
}
