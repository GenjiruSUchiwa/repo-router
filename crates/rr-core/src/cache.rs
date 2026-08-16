//! Local cache storage for parsed file facts keyed by Git OID.
//!
//! Stores serialized fact blobs atomically under `.rr/local/facts/<shard>/<key>.bin`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::facts::FACT_SCHEMA_VERSION;
use crate::lang::Lang;
use crate::oid::Oid;
use crate::parser::EXTRACTOR_VERSION;
use crate::{Error, Result};

/// Composite cache key identifying facts extracted from a specific file content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// Git object identifier of the file's content.
    pub oid: Oid,
    /// Language classifier of the file.
    pub lang: Lang,
    /// Extractor version used when parsing facts.
    pub extractor: u32,
    /// Schema version of the fact data model.
    pub schema: u32,
}

impl CacheKey {
    /// Creates a new [`CacheKey`] stamped with current extractor and schema versions.
    ///
    /// This is the only public constructor for [`CacheKey`].
    #[must_use]
    pub const fn new(oid: Oid, lang: Lang) -> Self {
        Self {
            oid,
            lang,
            extractor: EXTRACTOR_VERSION,
            schema: FACT_SCHEMA_VERSION,
        }
    }

    /// Derives the unique cache file name for this key.
    fn file_name(&self) -> String {
        format!(
            "{}-{}-{}-{}.bin",
            self.oid.to_hex(),
            self.lang.as_str(),
            self.extractor,
            self.schema
        )
    }
}

/// Statistics tracking fact cache efficiency.
#[derive(Debug, Default)]
pub struct CacheStats {
    /// Total number of successful cache hits.
    pub hits: AtomicU64,
    /// Total number of cache misses.
    pub misses: AtomicU64,
    /// Total number of corrupted entries encountered.
    pub corrupt: AtomicU64,
}

impl CacheStats {
    /// Returns the number of cache hits.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Returns the number of cache misses.
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Returns the number of corrupted cache entries.
    #[must_use]
    pub fn corrupt(&self) -> u64 {
        self.corrupt.load(Ordering::Relaxed)
    }

    /// Returns the cache hit rate as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let h = self.hits();
        let m = self.misses();
        let c = self.corrupt();
        let total = h + m + c;
        if total == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let rate = (h as f64 / total as f64) * 100.0;
            rate
        }
    }
}

/// Outcome of attempting to read a key from the fact cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheOutcome<T> {
    /// Entry exists and was successfully deserialized.
    Hit(T),
    /// Entry does not exist in the cache.
    Miss,
    /// Entry exists on disk but failed to deserialize (treated as miss for regeneration).
    Corrupt,
}

/// On-disk key-value cache storing serialized facts keyed by [`CacheKey`].
pub struct FactCache {
    root: PathBuf,
    stats: CacheStats,
}

impl FactCache {
    /// Opens or creates the facts cache directory within `repo_root`.
    ///
    /// Creates the directory hierarchy `.rr/local/facts` and validates writability with a probe file.
    ///
    /// # Errors
    /// Returns [`Error::CacheIo`] if the directory cannot be created or is not writable.
    pub fn open(repo_root: &Path) -> Result<Self> {
        // Marking the state directory ignored before creating the cache inside
        // it means Git never observes a moment where thousands of cache files
        // exist without the rule that hides them.
        crate::workspace::ensure_private(repo_root).map_err(|source| Error::CacheIo {
            path: crate::workspace::state_dir(repo_root),
            source,
        })?;

        let root = crate::workspace::facts_dir(repo_root);
        fs::create_dir_all(&root).map_err(|source| Error::CacheIo {
            path: root.clone(),
            source,
        })?;

        tempfile::NamedTempFile::new_in(&root).map_err(|source| Error::CacheIo {
            path: root.clone(),
            source,
        })?;

        Ok(Self {
            root,
            stats: CacheStats::default(),
        })
    }

    /// Derives the canonical path for a cache entry.
    ///
    /// This is the only place in the codebase where cache file paths are derived.
    fn path_for(&self, key: &CacheKey) -> PathBuf {
        self.root.join(key.oid.shard_prefix()).join(key.file_name())
    }

    /// Retrieves an entry from the cache.
    ///
    /// Distinguishes explicitly between:
    /// - Normal cache miss (`Ok(CacheOutcome::Miss)`).
    /// - Corrupted file (`Ok(CacheOutcome::Corrupt)`).
    /// - Filesystem I/O errors (propagated as [`Error::CacheIo`]).
    ///
    /// # Errors
    /// Returns [`Error::CacheIo`] on unexpected I/O errors (e.g. permission denied).
    pub fn get<T: DeserializeOwned>(&self, key: &CacheKey) -> Result<CacheOutcome<T>> {
        let path = self.path_for(key);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                return Ok(CacheOutcome::Miss);
            }
            Err(source) => return Err(Error::CacheIo { path, source }),
        };

        if let Ok((value, [])) = postcard::take_from_bytes::<T>(&bytes) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            Ok(CacheOutcome::Hit(value))
        } else {
            self.stats.corrupt.fetch_add(1, Ordering::Relaxed);
            Ok(CacheOutcome::Corrupt)
        }
    }

    /// Writes a value to the cache atomically.
    ///
    /// Serializes `value` with `postcard`, writes to a temporary file in the same shard
    /// directory, and persists over the target path via rename. No fsync is issued: the
    /// cache is fully rebuildable and a lost entry is regenerated on the next miss.
    ///
    /// # Errors
    /// Returns [`Error::CacheSerialization`] if serialization fails.
    /// Returns [`Error::CacheIo`] if temporary file creation, writing, or renaming fails.
    pub fn put<T: Serialize>(&self, key: &CacheKey, value: &T) -> Result<()> {
        let path = self.path_for(key);
        let parent = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent).map_err(|source| Error::CacheIo {
            path: parent.to_path_buf(),
            source,
        })?;

        let bytes = postcard::to_allocvec(value).map_err(Error::from)?;

        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| Error::CacheIo {
                path: parent.to_path_buf(),
                source,
            })?;

        temp.write_all(&bytes).map_err(|source| Error::CacheIo {
            path: temp.path().to_path_buf(),
            source,
        })?;

        temp.persist(&path).map_err(|persist_err| Error::CacheIo {
            path,
            source: persist_err.error,
        })?;

        Ok(())
    }

    /// Returns a reference to the cache performance statistics counters.
    #[must_use]
    pub const fn stats(&self) -> &CacheStats {
        &self.stats
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    const SHA1_HEX: &str = "95d09f2b10159347eece71399a7e2e907ea3df4f";

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct DummyFacts {
        symbols: Vec<String>,
        imports: Vec<String>,
    }

    #[test]
    fn cache_key_file_name_format() {
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);
        assert_eq!(
            key.file_name(),
            format!("{SHA1_HEX}-rust-{EXTRACTOR_VERSION}-{FACT_SCHEMA_VERSION}.bin")
        );
    }

    /// An entry written under the previous fact schema is not served, and is
    /// not reported corrupt either — it is simply not found, so the caller
    /// reparses and the whole cache rebuilds itself one miss at a time.
    ///
    /// This is why #31's format break costs nothing: the schema version is in
    /// the file name, so no migration code exists, and none is needed.
    #[test]
    fn a_stale_cache_from_the_previous_schema_triggers_a_full_rebuild() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let stale = cache.root.join(oid.shard_prefix()).join(format!(
            "{SHA1_HEX}-rust-{EXTRACTOR_VERSION}-{}.bin",
            FACT_SCHEMA_VERSION - 1
        ));
        assert_ne!(
            stale.file_name(),
            cache.path_for(&key).file_name(),
            "the schema version has to be part of the name for any of this to work"
        );
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        let previous = DummyFacts {
            symbols: vec!["written_by_schema_2".to_string()],
            imports: Vec::new(),
        };
        std::fs::write(&stale, postcard::to_allocvec(&previous).unwrap()).unwrap();

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Miss);
        assert_eq!(cache.stats().hits(), 0, "a stale entry was served");
        assert_eq!(cache.stats().corrupt(), 0, "a stale entry was even opened");
        assert_eq!(cache.stats().misses(), 1);

        // Reparsing writes the entry this schema asks for beside — not over —
        // the one the previous schema left, so no reader ever has to decide
        // between them. Nothing reclaims the old file: there is no pruner in
        // this workspace, and a bump strands every entry written before it
        // until the cache directory is deleted. That is a disk cost, paid once
        // per bump, and it is the price of the miss being unmistakable.
        let rebuilt = DummyFacts {
            symbols: vec!["written_by_schema_3".to_string()],
            imports: Vec::new(),
        };
        cache.put(&key, &rebuilt).unwrap();
        assert_eq!(
            cache.get::<DummyFacts>(&key).unwrap(),
            CacheOutcome::Hit(rebuilt)
        );
        assert!(stale.exists(), "the previous schema's entry was disturbed");
    }

    #[test]
    fn open_creates_directory_structure_and_verifies_probe() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        assert!(cache.root.exists());
        assert_eq!(
            cache.root,
            temp.path().join(".rr").join("local").join("facts")
        );
    }

    #[test]
    fn cache_miss_outcome_and_stats() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Miss);
        assert_eq!(cache.stats().misses(), 1);
        assert_eq!(cache.stats().hits(), 0);
        assert_eq!(cache.stats().corrupt(), 0);
    }

    #[test]
    fn cache_hit_roundtrip_and_stats() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let facts = DummyFacts {
            symbols: vec!["foo".to_string(), "bar".to_string()],
            imports: vec!["std::fmt".to_string()],
        };

        cache.put(&key, &facts).unwrap();

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Hit(facts.clone()));
        assert_eq!(cache.stats().hits(), 1);
        assert_eq!(cache.stats().misses(), 0);
        assert_eq!(cache.stats().corrupt(), 0);
        assert!((cache.stats().hit_rate() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cache_corrupt_outcome_and_overwriting() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let path = cache.path_for(&key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not valid postcard data\xff\xff").unwrap();

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Corrupt);
        assert_eq!(cache.stats().corrupt(), 1);

        let valid_facts = DummyFacts {
            symbols: vec!["recovered".to_string()],
            imports: vec![],
        };
        cache.put(&key, &valid_facts).unwrap();

        let outcome2: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome2, CacheOutcome::Hit(valid_facts));
        assert_eq!(cache.stats().hits(), 1);
    }

    #[test]
    fn trailing_garbage_after_valid_record_is_corrupt() {
        let temp = TempDir::new().unwrap();
        let cache = FactCache::open(temp.path()).unwrap();
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let facts = DummyFacts {
            symbols: vec!["foo".to_string()],
            imports: vec![],
        };
        cache.put(&key, &facts).unwrap();

        let path = cache.path_for(&key);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"trailing garbage");
        fs::write(&path, &bytes).unwrap();

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert_eq!(outcome, CacheOutcome::Corrupt);
        assert_eq!(cache.stats().corrupt(), 1);
    }

    #[test]
    fn concurrent_puts_same_key_produces_valid_entry() {
        let temp = TempDir::new().unwrap();
        let cache = Arc::new(FactCache::open(temp.path()).unwrap());
        let oid = Oid::from_hex(SHA1_HEX).unwrap();
        let key = CacheKey::new(oid, Lang::Rust);

        let mut handles = Vec::new();
        for i in 0..10 {
            let c = Arc::clone(&cache);
            let k = key.clone();
            let handle = std::thread::spawn(move || {
                let facts = DummyFacts {
                    symbols: vec![format!("sym_{i}")],
                    imports: vec![],
                };
                c.put(&k, &facts)
            });
            handles.push(handle);
        }

        for h in handles {
            h.join().unwrap().unwrap();
        }

        let outcome: CacheOutcome<DummyFacts> = cache.get(&key).unwrap();
        assert!(matches!(outcome, CacheOutcome::Hit(_)));
    }
}
