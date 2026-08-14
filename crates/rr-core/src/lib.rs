#![deny(unsafe_code)]

pub mod cache;
pub mod lang;
pub mod oid;
pub mod path;
pub mod walk;

pub use cache::{
    CacheKey, CacheOutcome, CacheStats, FactCache, EXTRACTOR_VERSION, FACT_SCHEMA_VERSION,
};
pub use lang::Lang;
pub use oid::{HashAlgo, Oid, OidError};
pub use path::{RelPath, RelPathError};
pub use walk::{discover, is_generated, SourceFile, WalkCfg, DEFAULT_EXCLUDES};

/// Core error types for `rr-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ignore error: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("Invalid relative path: {0}")]
    InvalidRelPath(#[from] RelPathError),
    #[error("OID error: {0}")]
    Oid(#[from] OidError),
    #[error("Cache I/O error at {path}: {source}")]
    CacheIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Cache serialization error: {0}")]
    CacheSerialization(#[from] postcard::Error),
}

/// A specialized [`Result`](std::result::Result) type for `rr-core` operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_result_type() {
        let ok_result: Result<&str> = Ok("success");
        assert!(ok_result.is_ok());
    }
}
