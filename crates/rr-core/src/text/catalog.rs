//! Which committed map owns each symbol, answered two ways.
//!
//! The pair exists because the two callers ask for different guarantees at
//! very different prices. A writer is about to touch every artifact anyway and
//! wants to know the files it names are really there; a reader answering one
//! query wants the ownership map and nothing else, because reading the whole
//! generation to shortcut one lookup costs more than the lookup it saves.
//!
//! Both doors open onto [`catalog_of`], so they can differ in what they
//! checked and never in what they concluded.

use std::collections::BTreeMap;
use std::path::Path;

use crate::index::{Snapshot, SymbolId};
use crate::path::RelPath;

use super::digest::{ApiHash, Digest, HashStream};
use super::model::TextProjection;
use super::validate::validate_text_artifacts;

/// The memo `.rr/local/memo/` files [`MapCatalog::api_identity`] under.
pub const API_IDENTITY_MEMO: &str = "route-corpus";

/// Which committed map owns each symbol, and at what API identity.
///
/// `.rr/ROUTES.md` stores exactly this pair against a learned route. It
/// deliberately does not expose `generated_hash`: a route invalidated by
/// somebody rewording a purpose would be a route that never survives a day.
#[derive(Debug, Clone)]
pub struct MapCatalog {
    owners: BTreeMap<SymbolId, MapIdentity>,
    index_hash: Digest,
    api_identity: Digest,
}

/// One committed map and the API identity of its scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapIdentity {
    path: RelPath,
    api_hash: ApiHash,
}

impl MapIdentity {
    /// The canonical repository-relative path of the owning map.
    #[must_use]
    pub const fn path(&self) -> &RelPath {
        &self.path
    }

    /// The scope's API identity, which is the invalidation key.
    #[must_use]
    pub const fn api_hash(&self) -> ApiHash {
        self.api_hash
    }
}

impl MapCatalog {
    /// The map that lists this symbol, if any does.
    #[must_use]
    pub fn owner(&self, symbol: SymbolId) -> Option<&MapIdentity> {
        self.owners.get(&symbol)
    }

    /// The projection this catalog was built from.
    #[must_use]
    pub const fn index_hash(&self) -> Digest {
        self.index_hash
    }

    /// The public API of the *whole* corpus, as one digest.
    ///
    /// Deliberately not [`Self::index_hash`], which is the wrong shape twice
    /// over: it carries `start_line`, so moving a definition down its file would
    /// retire every route in the repository, and it carries the budget and the
    /// plans, so re-paginating a map would too. This covers each scope's path
    /// and its `api_hash` and nothing else, which keeps both of the survival
    /// promises `.rr/ROUTES.md` makes — a reworded purpose and a moved
    /// definition leave it alone — while changing whenever any public name,
    /// kind, visibility or signature *anywhere* moves.
    ///
    /// Corpus-wide rather than per-scope because a route is an answer the ranker
    /// gave about the whole index, not about one directory. A new
    /// `verify_token_request` under `src/api/` changes what "verify token"
    /// should resolve to while leaving `src/auth`'s own `api_hash` untouched, so
    /// a per-scope key keeps serving `direct` where the ranker would now be
    /// ambiguous. Over-invalidating costs one ranked query; under-invalidating
    /// costs a wrong answer.
    #[must_use]
    pub const fn api_identity(&self) -> Digest {
        self.api_identity
    }

    /// Files [`Self::api_identity`] in `.rr/local/`, so the next query can check
    /// a route without projecting the snapshot again.
    ///
    /// Called from the paths that had to build a catalog anyway — learning a
    /// route, and reconciling them after a publication — so the memo costs the
    /// run that fills it nothing it had not already spent.
    ///
    /// `stamp` is [`crate::workspace::snapshot_stamp`] for the snapshot this
    /// catalog was projected from, and it is a parameter rather than something
    /// read here because the file on disk may already be a later one: see
    /// [`crate::workspace::write_memo`].
    pub fn remember(&self, root: &Path, stamp: &str) {
        crate::workspace::write_memo(root, API_IDENTITY_MEMO, stamp, &self.api_identity.to_text());
    }

    /// How many symbols have an owning map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.owners.len()
    }

    /// Whether no symbol has an owning map.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }
}

/// Builds the catalog, but only from artifacts that are actually valid on disk.
///
/// A catalog built from a projection alone would name maps that may not exist,
/// leaving a reader with routes to files nobody can open. That check costs a
/// full `stage_text_artifacts` — every artifact rendered and every `MAP.md`
/// read — which measured 137 ms against a 3900-symbol snapshot where the
/// projection alone took 17 ms. So this is the constructor for a caller that is
/// about to write those files anyway. A caller that only needs to *read* an
/// identity wants [`projected_map_catalog`] and its stated weaker guarantee.
///
/// # Errors
/// When the snapshot cannot be projected, and [`super::TextError::IndexHashMismatch`]
/// when the artifacts on disk disagree with it — repair before trusting a route.
pub fn validated_map_catalog(
    snapshot: &Snapshot,
    root: &Path,
    budget: u32,
) -> crate::Result<MapCatalog> {
    let projection = TextProjection::from_snapshot(snapshot, budget)?;
    let validation = validate_text_artifacts(snapshot, root, budget)?;
    if !validation.is_up_to_date() {
        return Err(crate::Error::Text(super::TextError::IndexHashMismatch));
    }
    Ok(catalog_of(&projection))
}

/// Which committed map *would* own each symbol, without reading one of them.
///
/// The weaker guarantee, stated so a caller cannot mistake it for the other
/// one: this names the map a symbol belongs in according to the snapshot. It
/// does not promise that map exists on disk, that it is current, or that it is
/// not conflicted. A caller that acts on the answer must be able to survive the
/// file being absent.
///
/// That is exactly the trade `rr query` wants. Reading every artifact to answer
/// one route lookup measured 137 ms against a 3900-symbol snapshot, where the
/// projection alone took 17 ms and the ranking this is supposed to *save* takes
/// single-digit milliseconds — a validated catalog on the query path is a cache
/// that costs more than the miss.
///
/// # Errors
/// Returns [`super::TextError::Budget`] or [`super::TextError::DuplicateRecord`]
/// when the snapshot cannot be projected. It reads no files, so it has no I/O
/// failures to report.
pub fn projected_map_catalog(snapshot: &Snapshot, budget: u32) -> crate::Result<MapCatalog> {
    let projection = TextProjection::from_snapshot(snapshot, budget)?;
    Ok(catalog_of(&projection))
}

/// The owner map, spelled once.
///
/// Both constructors call this rather than each carrying its own loop: two
/// copies could disagree about which map owns a symbol, and the whole point of
/// the pair is that they differ only in what they checked, never in what they
/// concluded.
fn catalog_of(projection: &TextProjection) -> MapCatalog {
    let mut owners = BTreeMap::new();
    let mut stream = HashStream::new("route-corpus");
    stream.count(projection.scopes().len());
    for scope in projection.scopes() {
        stream.text(scope.path.as_str());
        stream.digest(scope.api_hash.digest());

        let Ok(path) = RelPath::new(scope.path.map_path()) else {
            continue;
        };
        for record in &scope.api {
            owners.insert(
                record.symbol,
                MapIdentity {
                    path: path.clone(),
                    api_hash: scope.api_hash,
                },
            );
        }
    }
    MapCatalog {
        owners,
        index_hash: projection.index_hash(),
        api_identity: stream.finish(),
    }
}

/// The corpus API identity, projecting the snapshot only when it has to.
///
/// This is the whole of what a route lookup needs from the text layer, and it is
/// the reason the lookup is a shortcut rather than a detour. Projecting a
/// 3900-symbol snapshot measured 17 ms against a ranking pass in single-digit
/// milliseconds, so a hit that projected would cost more than the miss it
/// replaced — and an exact-name query, which was a binary search, would pay it
/// on every run. The memo turns that into one projection per published
/// snapshot, paid by the run that learns a route and by nobody after it.
///
/// A memo filed against some other snapshot is not read at all — see
/// [`crate::workspace::read_memo`] — so the fallback is a fresh projection and
/// never a stale identity. `stamp` is
/// [`crate::workspace::snapshot_stamp`] for `snapshot` itself, taken by whoever
/// loaded it; `None` says the loader could not stamp what it read, and then
/// there is nothing a memo could safely be filed under or matched against.
///
/// # Errors
/// Returns what [`projected_map_catalog`] returns, and only when the memo missed.
pub fn api_identity(
    root: &Path,
    stamp: Option<&str>,
    snapshot: &Snapshot,
    budget: u32,
) -> crate::Result<Digest> {
    if let Some(digest) = stamp
        .and_then(|stamp| crate::workspace::read_memo(root, API_IDENTITY_MEMO, stamp))
        .and_then(|text| Digest::parse(&text).ok())
    {
        return Ok(digest);
    }
    let catalog = projected_map_catalog(snapshot, budget)?;
    if let Some(stamp) = stamp {
        catalog.remember(root, stamp);
    }
    Ok(catalog.api_identity())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
    use crate::lang::Lang;
    use crate::oid::Oid;
    use crate::parser::RustExtractor;
    use crate::text::{ExistingPurposes, DEFAULT_MAP_BUDGET, MAP_FILE_NAME};

    fn snapshot() -> Snapshot {
        let sources: [(&str, &str); 3] = [
            ("lib.rs", "pub fn entry() -> u32 { 1 }\n"),
            (
                "src/auth/token.rs",
                "pub fn verify_token(token: &str) -> bool { token.is_empty() }\n",
            ),
            ("src/auth/keys.rs", "pub fn rotate_signing_key() {}\n"),
        ];
        let mut extractor = RustExtractor::new().unwrap();
        let inputs = sources
            .iter()
            .map(|(path, code)| FileInput {
                path: RelPath::new(*path).unwrap(),
                oid: Oid::from_raw(blake3::hash(code.as_bytes()).as_bytes()).unwrap(),
                representation: ContentRepresentation::RawNoGit,
                generated: false,
                language: Lang::Rust,
                parse_status: crate::facts::ParseStatus::Complete,
                facts: extractor.extract(code.as_bytes()).unwrap(),
            })
            .collect();
        let (snapshot, _) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
            .build(inputs)
            .unwrap();
        snapshot
    }

    /// Renders the generation and writes every file, so the validated
    /// constructor has a repository to agree with.
    fn publish(snapshot: &Snapshot, root: &Path) {
        let rendered = TextProjection::from_snapshot(snapshot, DEFAULT_MAP_BUDGET)
            .unwrap()
            .render(&ExistingPurposes::none())
            .unwrap();
        for file in rendered.files() {
            let absolute = root.join(file.path());
            std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            std::fs::write(&absolute, file.bytes()).unwrap();
        }
    }

    /// The one test that would catch `catalog_of` being bypassed by a second
    /// loop: the two constructors differ in what they checked, never in what
    /// they concluded.
    #[test]
    fn the_two_catalogs_agree_on_every_owner() {
        let snapshot = snapshot();
        let temp = tempfile::tempdir().unwrap();
        publish(&snapshot, temp.path());

        let validated =
            validated_map_catalog(&snapshot, temp.path(), DEFAULT_MAP_BUDGET).expect("validated");
        let projected = projected_map_catalog(&snapshot, DEFAULT_MAP_BUDGET).expect("projected");

        assert_eq!(validated.index_hash(), projected.index_hash());
        assert_eq!(validated.len(), projected.len());
        assert!(!validated.is_empty(), "the fixture has public symbols");
        for symbol in &snapshot.symbols {
            assert_eq!(
                validated.owner(symbol.id),
                projected.owner(symbol.id),
                "the two catalogs disagree about who owns a symbol"
            );
        }
    }

    /// The whole reason the projected constructor exists: a query path that
    /// reads no artifact cannot be made slower by how many there are.
    #[test]
    fn a_projected_catalog_reads_no_files() {
        let snapshot = snapshot();
        let catalog = projected_map_catalog(&snapshot, DEFAULT_MAP_BUDGET)
            .expect("a projection needs no repository");
        assert!(!catalog.is_empty());
    }

    #[test]
    fn a_validated_catalog_refuses_a_half_published_repository() {
        let snapshot = snapshot();
        let temp = tempfile::tempdir().unwrap();
        publish(&snapshot, temp.path());
        std::fs::remove_file(temp.path().join("src/auth").join(MAP_FILE_NAME)).unwrap();

        let error = validated_map_catalog(&snapshot, temp.path(), DEFAULT_MAP_BUDGET)
            .expect_err("a missing map is not a catalog");
        assert!(
            matches!(
                error,
                crate::Error::Text(super::super::TextError::IndexHashMismatch)
            ),
            "unexpected error: {error}"
        );
    }
}
