//! A bounded reply for an already committed controller operation. The hash is
//! over its original serialized result, never a newer query of current state.
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;

const INLINE_BYTES: usize = 65536;
struct Output {
    hash: Sha256,
    bytes: u64,
    inline: Option<Vec<u8>>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.hash.update(bytes);
        self.bytes += bytes.len() as u64;
        if let Some(inline) = &mut self.inline {
            if inline.len() + bytes.len() <= INLINE_BYTES {
                inline.extend_from_slice(bytes);
            } else {
                self.inline = None;
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn committed(operation: &str, response: &Value) -> Result<Value> {
    let mut output = Output {
        hash: Sha256::new(),
        bytes: 0,
        inline: Some(Vec::new()),
    };
    serde_json::to_writer(&mut output, response)?;
    let inline = output
        .inline
        .map(|bytes| serde_json::from_slice::<Value>(&bytes))
        .transpose()?;
    let id = response
        .get("id")
        .or_else(|| response.get("attempt"))
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 256);
    let state = response
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 256);
    Ok(
        json!({"kind":"operation_receipt","operation_id":operation,"committed":true,"response_sha256":format!("{:x}",output.hash.finalize()),"response_bytes":output.bytes,"response_inline":inline.is_some(),"response":inline,"summary":{"id":id,"revision":response.get("revision").and_then(Value::as_u64),"state":state}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_bounds_encoded_json_and_hashes_the_entire_original_result() {
        for (value, inline) in [
            (json!("x".repeat(INLINE_BYTES - 2)), true),
            (json!("x".repeat(INLINE_BYTES - 1)), false),
            (json!("\u{0001}".repeat(INLINE_BYTES / 6 + 1)), false),
            (
                json!({"id":"record","revision":7,"state":"paused","text":"é".repeat(INLINE_BYTES)}),
                false,
            ),
        ] {
            let encoded = serde_json::to_vec(&value).unwrap();
            let receipt = committed("operation", &value).unwrap();
            assert_eq!(receipt["committed"], true);
            assert_eq!(receipt["response_bytes"], encoded.len());
            assert_eq!(
                receipt["response_sha256"],
                format!("{:x}", Sha256::digest(&encoded))
            );
            assert_eq!(receipt["response_inline"], inline);
            assert_eq!(
                receipt["response"],
                if inline { value.clone() } else { Value::Null }
            );
            assert!(serde_json::to_vec(&receipt).unwrap().len() < INLINE_BYTES + 1024);
            assert_eq!(committed("operation", &value).unwrap(), receipt);
        }
    }
}
