mod common;

use common::{git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::refresh::RefreshMode;
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
}
