//! In-memory key value store.

/// One stored entry.
pub struct Entry {
    /// Key the entry is filed under.
    pub key: String,
    /// Opaque payload the caller stored.
    pub value: Vec<u8>,
}

/// Flat key value store backed by a sorted vector.
pub struct KeyValueStore {
    entries: Vec<Entry>,
}

impl KeyValueStore {
    /// Inserts an entry, replacing any previous value filed under the key.
    pub fn insert(&mut self, key: String, value: Vec<u8>) {
        self.remove(&key);
        self.entries.push(Entry { key, value });
    }

    /// Looks up the value stored under a key.
    pub fn lookup(&self, key: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_slice())
    }

    /// Removes the entry stored under a key.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.key != key);
        self.entries.len() != before
    }
}
