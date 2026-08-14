//! Wire representation of stored entries.

use crate::store::kv::Entry;

/// Serializes an entry into its wire representation.
pub fn serialize_entry(entry: &Entry) -> Vec<u8> {
    let mut bytes = entry.key.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(&entry.value);
    bytes
}

/// Deserializes an entry from its wire representation.
pub fn deserialize_entry(bytes: &[u8]) -> Option<Entry> {
    let split = bytes.iter().position(|byte| *byte == 0)?;
    Some(Entry {
        key: String::from_utf8(bytes[..split].to_vec()).ok()?,
        value: bytes[split + 1..].to_vec(),
    })
}
