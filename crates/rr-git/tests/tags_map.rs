mod common;

use common::{git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::query::{finish_exact, parse_query, route_exact, QueryRequest};
use rr_core::refresh::RefreshMode;
use rr_core::result::{Pipeline, QueryResult, TargetId};
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
use rr_core::text::{ExistingPurposes, TextProjection, DEFAULT_MAP_BUDGET};

#[test]
fn a_map_for_a_tags_indexed_directory_has_a_populated_api_section() {
    let temp = init_git_repo();
    write(
        temp.path(),
        "src/service.py",
        "class Service:\n    def run(value):\n        return helper(value)\n",
    );
    git_add_and_commit(temp.path(), "python source");

    rr_git::refresh(temp.path(), 1, RefreshMode::Full, &CancelToken::new()).unwrap();
    let snapshot = match SnapshotStore::new(temp.path()).load().unwrap() {
        LoadOutcome::Ready(snapshot) => snapshot,
        outcome => panic!("unexpected snapshot outcome: {outcome:?}"),
    };
    let projection = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET).unwrap();
    let rendered = projection.render(&ExistingPurposes::none()).unwrap();
    let map = rendered
        .files()
        .iter()
        .find(|file| file.path() == "src/MAP.md")
        .unwrap();
    let map = std::str::from_utf8(map.bytes()).unwrap();
    let api = map
        .split("## API")
        .nth(1)
        .unwrap()
        .split("## Tests")
        .next()
        .unwrap();
    assert!(map.contains("fidelity: \"syntax-tags\""), "{map}");
    assert!(api.contains("Service"), "{map}");
    assert!(api.contains("def run(value):"), "{map}");
    assert!(!api.contains("_None._"), "{map}");
    // The qualified spelling is the language's, not Rust's: no extension kept
    // as a module segment, no `::`.
    assert!(!map.contains("service.py::"), "{map}");
}

/// The spelling the index stores is a spelling `rr query` accepts back.
#[test]
fn a_python_symbol_routes_by_its_qualified_name() {
    let temp = init_git_repo();
    write(
        temp.path(),
        "src/service.py",
        "class Service:\n    def run(value):\n        return value\n",
    );
    git_add_and_commit(temp.path(), "python source");

    rr_git::refresh(temp.path(), 1, RefreshMode::Full, &CancelToken::new()).unwrap();
    let snapshot = match SnapshotStore::new(temp.path()).load().unwrap() {
        LoadOutcome::Ready(snapshot) => snapshot,
        outcome => panic!("unexpected snapshot outcome: {outcome:?}"),
    };

    let parsed = parse_query(&snapshot, QueryRequest::new("service.Service.run", None)).unwrap();
    let result = finish_exact(route_exact(&snapshot, &parsed));
    match result {
        QueryResult::Direct {
            candidate,
            pipeline,
            ..
        } => {
            assert_eq!(pipeline, Pipeline::Exact);
            assert!(matches!(candidate.target, TargetId::Symbol(_)));
        }
        other => panic!("the stored qualified spelling did not route: {other:?}"),
    }
}

/// PEP 8 privacy reaches the map, and a tags-tier parse error reaches the
/// frontmatter instead of being counted like a clean read.
#[test]
fn python_privacy_and_parse_errors_reach_the_map() {
    let temp = init_git_repo();
    write(
        temp.path(),
        "src/service.py",
        "class Service:\n    def __mangled(value):\n        return value\n    def run(value):\n        return value\n",
    );
    write(temp.path(), "src/broken.py", "def broken(:\n    return 1\n");
    git_add_and_commit(temp.path(), "python source");

    rr_git::refresh(temp.path(), 1, RefreshMode::Full, &CancelToken::new()).unwrap();
    let snapshot = match SnapshotStore::new(temp.path()).load().unwrap() {
        LoadOutcome::Ready(snapshot) => snapshot,
        outcome => panic!("unexpected snapshot outcome: {outcome:?}"),
    };
    let projection = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET).unwrap();
    let rendered = projection.render(&ExistingPurposes::none()).unwrap();
    let map = rendered
        .files()
        .iter()
        .find(|file| file.path() == "src/MAP.md")
        .unwrap();
    let map = std::str::from_utf8(map.bytes()).unwrap();
    assert!(map.contains("fidelity: \"syntax-tags-recovered\""), "{map}");
    assert!(map.contains("def run(value):"), "{map}");
    assert!(!map.contains("__mangled"), "{map}");
}
