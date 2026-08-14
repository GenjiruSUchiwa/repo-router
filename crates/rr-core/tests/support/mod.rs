//! Shared loading of the ranking fixture repositories.
//!
//! Every ranking test suite indexes the same on-disk repositories through the
//! same builder, so a fixture edit moves the goldens, the calibration report,
//! and the benchmark together instead of only one of them.

#![allow(dead_code)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use rr_core::index::{
    ContentRepresentation, FileInput, Snapshot, SnapshotBuilder, SnapshotMeta, SymbolId,
};
use rr_core::lang::Lang;
use rr_core::oid::Oid;
use rr_core::parser::RustExtractor;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, QueryRequest};
use rr_core::ranking::{route_lexical, RankingEvidence, RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::render::encode_anchor;
use rr_core::result::{resolve_anchor, QueryResult, TargetId};

/// Fixture repositories, in the order the calibrator folds over them.
pub const FIXTURE_REPOSITORIES: [&str; 5] = ["auth", "geometry", "http", "storage", "wide"];

/// Absolute path of one fixture repository.
#[must_use]
pub fn fixture_root(repository: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ranking")
        .join(repository)
}

/// Indexes one fixture repository.
///
/// Sources are visited in sorted relative-path order and identified by the
/// BLAKE3 hash of their bytes, so the snapshot is byte-identical on every
/// platform, on every run, and under any filesystem enumeration order.
#[must_use]
pub fn fixture_snapshot(repository: &str) -> Snapshot {
    let root = fixture_root(repository);
    let mut extractor = RustExtractor::new().expect("construct the Rust extractor");
    let mut inputs = Vec::new();
    for path in rust_sources(&root) {
        let bytes = fs::read(root.join(path.as_path())).expect("read a fixture source");
        let facts = extractor.extract(&bytes).expect("extract fixture facts");
        inputs.push(FileInput {
            path,
            oid: Oid::from_raw(blake3::hash(&bytes).as_bytes()).expect("hash a fixture source"),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: facts.status(),
            facts,
        });
    }
    assert!(
        !inputs.is_empty(),
        "fixture repository {repository} has no Rust sources"
    );
    let (snapshot, _counts) = SnapshotBuilder::new(SnapshotMeta::new(None, true))
        .build(inputs)
        .expect("index a fixture repository");
    snapshot
}

/// Indexes sources held in memory, in the order given.
///
/// The builder is free to be handed files in any order; passing them unsorted
/// is how a test proves the snapshot it produces does not depend on that order.
#[must_use]
pub fn synthetic_snapshot(sources: &[(&str, &str)]) -> Snapshot {
    let mut extractor = RustExtractor::new().expect("construct the Rust extractor");
    let inputs = sources
        .iter()
        .map(|(path, code)| FileInput {
            path: RelPath::new(*path).expect("a synthetic path is repository relative"),
            oid: Oid::from_raw(blake3::hash(code.as_bytes()).as_bytes()).expect("hash a source"),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: rr_core::ParseStatus::Complete,
            facts: extractor
                .extract(code.as_bytes())
                .expect("extract synthetic facts"),
        })
        .collect();
    let (snapshot, _counts) = SnapshotBuilder::new(SnapshotMeta::new(None, true))
        .build(inputs)
        .expect("index synthetic sources");
    snapshot
}

/// Ranks one natural-language query against an indexed fixture repository.
pub fn lexical(
    snapshot: &Snapshot,
    query: &str,
    scratch: &mut RankingScratch,
) -> (QueryResult, RankingEvidence) {
    let parsed =
        parse_query(snapshot, QueryRequest::new(query, None)).expect("parse a fixture query");
    route_lexical(snapshot, &parsed.terms, &DEFAULT_RANKING_PROFILE, scratch)
        .expect("rank a fixture query")
}

/// Renders the anchors a result names, in result order.
#[must_use]
pub fn result_anchors(snapshot: &Snapshot, result: &QueryResult) -> Vec<String> {
    let targets: Vec<TargetId> = match result {
        QueryResult::Direct { candidate, .. } => vec![candidate.target],
        QueryResult::Candidates { candidates, .. } => {
            candidates.iter().map(|entry| entry.target).collect()
        }
        QueryResult::None { .. } => Vec::new(),
    };
    targets
        .into_iter()
        .map(|target| {
            let anchor = resolve_anchor(snapshot, target).expect("resolve a result anchor");
            encode_anchor(anchor.path, anchor.symbol)
        })
        .collect()
}

/// Renders a symbol anchor exactly as the command line would print it.
#[must_use]
pub fn anchor_of(snapshot: &Snapshot, symbol: SymbolId) -> String {
    let anchor =
        resolve_anchor(snapshot, TargetId::Symbol(symbol)).expect("resolve a symbol anchor");
    encode_anchor(anchor.path, anchor.symbol)
}

/// Resolves the one symbol carrying `name`, panicking when it is ambiguous.
#[must_use]
pub fn symbol_named(snapshot: &Snapshot, name: &str) -> SymbolId {
    let mut found = None;
    for symbol in &snapshot.symbols {
        if snapshot.strings[symbol.name.index()] == name {
            assert!(found.is_none(), "fixture symbol name {name} is not unique");
            found = Some(symbol.id);
        }
    }
    found.unwrap_or_else(|| panic!("no fixture symbol is named {name}"))
}

fn rust_sources(root: &Path) -> Vec<RelPath> {
    let mut paths = Vec::new();
    collect_rust_sources(root, root, &mut paths);
    paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    paths
}

fn collect_rust_sources(root: &Path, directory: &Path, found: &mut Vec<RelPath>) {
    for entry in fs::read_dir(directory).expect("read a fixture directory") {
        let path = entry.expect("read a fixture directory entry").path();
        if path.is_dir() {
            collect_rust_sources(root, &path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(
                RelPath::from_path(root, &path).expect("fixture path is repository relative"),
            );
        }
    }
}
