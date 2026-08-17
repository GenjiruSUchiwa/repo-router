//! The text projection, exercised through the public API only.
//!
//! Every assertion here is about bytes a human or another tool will actually
//! see. The unit tests inside `rr_core::text` prove each piece in isolation;
//! this file proves that the pieces, assembled, produce a repository somebody
//! could commit and read.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rr_core::index::Snapshot;
use rr_core::text::{
    parse_map, parse_symbols, read_existing_purposes, ArtifactKind, Conflict, ConflictReason,
    ExistingPurposes, PageKind, RenderedArtifactSet, TextProjection, DEFAULT_MAP_BUDGET,
    MAP_FILE_NAME, PURPOSE_MAX_BYTES, SYMBOLS_PATH,
};

mod support;

/// A repository small enough to read in full and wide enough to have shape:
/// a root file, two directories, a nested directory, a test file, and one
/// private definition that must never appear.
fn sources() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "lib.rs",
            "pub fn entry() -> u32 { 1 }\nfn hidden() -> u32 { 2 }\n",
        ),
        (
            "src/auth/token.rs",
            "pub struct Claims { pub subject: String }\n\
             pub fn verify_token(token: &str) -> Claims {\n\
             \x20   let _ = token;\n\
             \x20   Claims { subject: String::new() }\n\
             }\n\
             pub(crate) fn refresh() {}\n",
        ),
        (
            "src/store.rs",
            "pub trait Store {\n    fn get(&self) -> u32;\n}\n",
        ),
        (
            "tests/token_test.rs",
            "#[test]\nfn verifies_expiry() {}\n#[test]\nfn rejects_forgery() {}\n",
        ),
    ]
}

fn snapshot() -> Snapshot {
    support::synthetic_snapshot(&sources())
}

fn render(snapshot: &Snapshot, budget: u32) -> RenderedArtifactSet {
    TextProjection::from_snapshot(snapshot, budget)
        .expect("project a snapshot")
        .render(&ExistingPurposes::none())
        .expect("render a projection")
}

/// The rendered set as a path-to-text map, for assertions that name one file.
fn texts(set: &RenderedArtifactSet) -> BTreeMap<String, String> {
    set.files()
        .iter()
        .map(|file| {
            (
                file.path().to_owned(),
                String::from_utf8(file.bytes().to_vec()).expect("an artifact is UTF-8"),
            )
        })
        .collect()
}

/// Writes a whole generation to disk, in publication order.
fn publish(root: &Path, set: &RenderedArtifactSet) {
    for file in set.files() {
        let path = root.join(file.path());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, file.bytes()).unwrap();
    }
}

#[test]
fn the_root_map_is_exactly_this() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    insta::assert_snapshot!("root_map", files.get(MAP_FILE_NAME).unwrap());
}

#[test]
fn a_directory_map_is_exactly_this() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    insta::assert_snapshot!("auth_map", files.get("src/auth/MAP.md").unwrap());
}

#[test]
fn the_symbol_index_is_exactly_this() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    insta::assert_snapshot!("symbols", files.get(SYMBOLS_PATH).unwrap());
}

#[test]
fn every_indexed_directory_and_only_those_get_a_map() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let maps: Vec<&str> = set.committed_paths().collect();
    assert_eq!(
        maps,
        vec![
            "src/auth/MAP.md",
            "src/MAP.md",
            "tests/MAP.md",
            MAP_FILE_NAME,
        ],
        "maps are published deepest first, root last"
    );
}

#[test]
fn rendering_the_same_snapshot_twice_produces_the_same_bytes() {
    let snapshot = snapshot();
    let first = render(&snapshot, DEFAULT_MAP_BUDGET);
    let second = render(&snapshot, DEFAULT_MAP_BUDGET);
    assert_eq!(first.index_hash(), second.index_hash());
    assert_eq!(texts(&first), texts(&second));
}

#[test]
fn every_artifact_of_one_generation_carries_the_same_index_hash() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let expected = set.index_hash().to_text();
    for file in set.files() {
        let text = String::from_utf8(file.bytes().to_vec()).unwrap();
        assert!(
            text.contains(&format!("index_hash: \"{expected}\"")),
            "{} does not carry the generation's index_hash",
            file.path()
        );
    }
}

#[test]
fn every_artifact_is_lf_only_utf8_with_one_final_newline() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    for file in set.files() {
        let text = String::from_utf8(file.bytes().to_vec())
            .unwrap_or_else(|_| panic!("{} is not UTF-8", file.path()));
        assert!(!text.contains('\r'), "{} contains CR", file.path());
        assert!(text.ends_with('\n'), "{} has no final LF", file.path());
        assert!(
            !text.ends_with("\n\n"),
            "{} has more than one final LF",
            file.path()
        );
        assert!(
            !text.starts_with('\u{feff}'),
            "{} starts with a BOM",
            file.path()
        );
    }
}

#[test]
fn every_rendered_artifact_parses_back() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    for file in set.files() {
        match file.kind() {
            ArtifactKind::Symbols => {
                let parsed = parse_symbols(file.bytes())
                    .unwrap_or_else(|error| panic!("{}: {error}", file.path()));
                assert!(parsed.is_owned(), "{} is not owned", file.path());
                assert_eq!(parsed.index_hash(), set.index_hash());
                assert_eq!(
                    usize::try_from(parsed.declared_symbols()).unwrap(),
                    parsed.records().len()
                );
            }
            ArtifactKind::Router | ArtifactKind::Page => {
                let parsed = parse_map(file.bytes())
                    .unwrap_or_else(|error| panic!("{}: {error}", file.path()));
                assert!(parsed.is_owned(), "{} is not owned", file.path());
                assert_eq!(parsed.index_hash(), set.index_hash());
                assert_eq!(parsed.generated_hash(), file.generated_hash());
                let expected_page = if file.kind() == ArtifactKind::Router {
                    PageKind::Router
                } else {
                    parsed.page()
                };
                assert_eq!(parsed.page(), expected_page);
            }
        }
    }
}

#[test]
fn a_private_definition_is_in_no_artifact() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    for (path, text) in texts(&set) {
        assert!(
            !text.contains("hidden"),
            "{path} names a private definition"
        );
    }
}

#[test]
fn a_test_file_is_listed_under_tests_and_never_in_the_symbol_index() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    let tests_map = files.get("tests/MAP.md").unwrap();
    let parsed = parse_map(tests_map.as_bytes()).unwrap();
    assert!(
        parsed.api().is_empty(),
        "a test file contributed to ## API: {tests_map}"
    );
    let anchors: Vec<&str> = parsed.tests().iter().map(String::as_str).collect();
    assert_eq!(
        anchors,
        vec![
            "tests/token_test.rs#rejects_forgery",
            "tests/token_test.rs#verifies_expiry",
        ]
    );
    let symbols = files.get(SYMBOLS_PATH).unwrap();
    assert!(
        !symbols.contains("verifies_expiry"),
        "a test reached the symbol index"
    );
}

#[test]
fn a_symbol_record_names_the_map_that_owns_it() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    let parsed = parse_symbols(files.get(SYMBOLS_PATH).unwrap().as_bytes()).unwrap();
    let verify = parsed
        .records()
        .iter()
        .find(|record| record.symbol.ends_with("verify_token"))
        .expect("the symbol index lists verify_token");
    assert_eq!(verify.symbol, "auth::token::verify_token");
    assert_eq!(verify.map, "src/auth/MAP.md");
    assert_eq!(verify.anchor, "src/auth/token.rs#verify_token");
    assert_eq!(verify.visibility, "public");
    let owner = parse_map(files.get(&verify.map).unwrap().as_bytes()).unwrap();
    assert_eq!(owner.api_hash(), verify.api_hash);
}

#[test]
fn every_symbol_record_points_at_a_map_that_exists_and_lists_it() {
    let set = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    let parsed = parse_symbols(files.get(SYMBOLS_PATH).unwrap().as_bytes()).unwrap();
    assert!(!parsed.records().is_empty());
    for record in parsed.records() {
        let map = files
            .get(&record.map)
            .unwrap_or_else(|| panic!("{} names a map that was not rendered", record.symbol));
        let owner = parse_map(map.as_bytes()).unwrap();
        assert_eq!(owner.api_hash(), record.api_hash);
        assert!(
            owner.api().iter().any(|entry| entry.name == record.symbol),
            "{} claims {} owns it, but that map does not list it",
            record.symbol,
            record.map
        );
    }
}

#[test]
fn a_written_purpose_survives_regeneration_byte_for_byte() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let map = root.path().join(MAP_FILE_NAME);
    let before = fs::read_to_string(&map).unwrap();
    let written = "Routes callers to the auth and store subsystems.";
    let after = replace_purpose(&before, written);
    assert_ne!(before, after, "the test did not actually write a purpose");
    fs::write(&map, &after).unwrap();

    let projection = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET).unwrap();
    let purposes = read_existing_purposes(root.path(), &projection).unwrap();
    assert_eq!(purposes.for_map(MAP_FILE_NAME), Some(written));
    let regenerated = projection.render(&purposes).unwrap();
    let rendered_root = texts(&regenerated).remove(MAP_FILE_NAME).unwrap();
    assert_eq!(
        parse_map(rendered_root.as_bytes()).unwrap().purpose(),
        Some(written)
    );
    assert_eq!(
        strip_tokens(&rendered_root),
        strip_tokens(&after),
        "regeneration changed something other than the token count"
    );
    publish(root.path(), &regenerated);
    let again = read_existing_purposes(root.path(), &projection).unwrap();
    let twice = texts(&projection.render(&again).unwrap());
    assert_eq!(twice.get(MAP_FILE_NAME), Some(&rendered_root));
}

/// The text with its `tokens` line removed.
fn strip_tokens(text: &str) -> String {
    text.lines()
        .filter(|line| !line.starts_with("tokens: "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn editing_only_the_purpose_leaves_every_hash_alone() {
    let snapshot = snapshot();
    let seeded = render(&snapshot, DEFAULT_MAP_BUDGET);
    let seeded_root = texts(&seeded).remove(MAP_FILE_NAME).unwrap();

    let mut purposes = ExistingPurposes::none();
    let edited_text = replace_purpose(&seeded_root, "A wholly different sentence.");
    let projection = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET).unwrap();
    let _ = &mut purposes;

    let before = parse_map(seeded_root.as_bytes()).unwrap();
    let after = parse_map(edited_text.as_bytes()).unwrap();
    assert_ne!(before.purpose(), after.purpose());
    assert_eq!(before.generated_hash(), after.generated_hash());
    assert_eq!(before.index_hash(), after.index_hash());
    assert_eq!(before.api_hash(), after.api_hash());
    assert_eq!(projection.index_hash(), before.index_hash());
}

#[test]
fn changing_a_signature_changes_that_scope_and_the_generation() {
    let before = render(&snapshot(), DEFAULT_MAP_BUDGET);
    let mut changed = sources();
    changed[1].1 = "pub struct Claims { pub subject: String }\n\
                    pub fn verify_token(token: &str, now: u64) -> Claims {\n\
                    \x20   let _ = (token, now);\n\
                    \x20   Claims { subject: String::new() }\n\
                    }\n\
                    pub(crate) fn refresh() {}\n";
    let after = render(&support::synthetic_snapshot(&changed), DEFAULT_MAP_BUDGET);

    assert_ne!(before.index_hash(), after.index_hash());

    let (before, after) = (texts(&before), texts(&after));
    let before_auth = parse_map(before.get("src/auth/MAP.md").unwrap().as_bytes()).unwrap();
    let after_auth = parse_map(after.get("src/auth/MAP.md").unwrap().as_bytes()).unwrap();
    assert_ne!(
        before_auth.api_hash(),
        after_auth.api_hash(),
        "the changed scope kept its api_hash"
    );

    let before_store = parse_map(before.get("src/MAP.md").unwrap().as_bytes()).unwrap();
    let after_store = parse_map(after.get("src/MAP.md").unwrap().as_bytes()).unwrap();
    assert_eq!(
        before_store.api_hash(),
        after_store.api_hash(),
        "an untouched scope's api_hash moved"
    );
}

#[test]
fn a_small_budget_produces_one_page_that_states_the_rest() {
    let snapshot = snapshot();
    let generous = render(&snapshot, DEFAULT_MAP_BUDGET);
    let tight = render(&snapshot, 12);

    let tight_paths: Vec<&str> = tight.committed_paths().collect();
    assert!(
        tight_paths.iter().all(|path| !path.contains("MAP.rr-")),
        "a 12-token budget still wrote overflow pages: {tight_paths:?}"
    );
    assert_eq!(
        tight.committed_paths().count(),
        generous.committed_paths().count(),
        "truncation changed how many committed files exist"
    );
    assert!(
        tight.files().iter().any(|file| {
            std::str::from_utf8(file.bytes())
                .unwrap()
                .contains("omitted by the map budget")
        }),
        "a 12-token budget omitted nothing"
    );
}

#[test]
fn every_page_stays_within_its_budget_unless_one_record_cannot() {
    let snapshot = snapshot();
    for budget in [4_u32, 8, 16, 32, 64, 250] {
        let projection = TextProjection::from_snapshot(&snapshot, budget).unwrap();
        let over: Vec<String> = projection.over_budget_scopes().map(str::to_owned).collect();
        let set = projection.render(&ExistingPurposes::none()).unwrap();
        for file in set.files() {
            if file.kind() == ArtifactKind::Symbols {
                continue;
            }
            let parsed = parse_map(file.bytes()).unwrap();
            assert_eq!(parsed.budget(), u64::from(budget));
            if parsed.tokens() > parsed.budget() {
                assert!(
                    over.contains(&parsed.scope().to_owned()),
                    "{} is over budget at {budget} but was not reported: {} > {}",
                    file.path(),
                    parsed.tokens(),
                    parsed.budget()
                );
            }
        }
    }
}

#[test]
fn the_declared_token_count_is_the_body_it_describes() {
    let set = render(&snapshot(), 16);
    for file in set.files() {
        if file.kind() == ArtifactKind::Symbols {
            continue;
        }
        let text = String::from_utf8(file.bytes().to_vec()).unwrap();
        let body = text
            .split_once("---\n")
            .and_then(|(_, rest)| rest.split_once("---\n"))
            .map(|(_, body)| body)
            .expect("a map has frontmatter");
        let parsed = parse_map(file.bytes()).unwrap();
        let expected = (body.len() as u64).div_ceil(4);
        assert_eq!(
            parsed.tokens(),
            expected,
            "{} declares {} tokens for a {}-byte body",
            file.path(),
            parsed.tokens(),
            body.len()
        );
    }
}

#[test]
fn a_freshly_published_repository_needs_no_work() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();

    let before =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    assert!(before.conflicts().is_empty());
    assert!(!before.missing().is_empty(), "nothing was reported missing");
    assert!(!before.is_up_to_date());

    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let after =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    assert!(
        after.is_up_to_date(),
        "a freshly published repository reports work: {:?} {:?} {:?} {:?}",
        after.conflicts(),
        after.stale(),
        after.missing(),
        after.removable()
    );
    assert_eq!(after.fresh().len(), before.missing().len());
}

#[test]
fn an_edited_generated_section_is_a_conflict_not_an_overwrite() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let map = root.path().join("src/auth/MAP.md");
    let text = fs::read_to_string(&map).unwrap();
    let tampered = text.replace("verify_token", "verify_tokens");
    assert_ne!(text, tampered);
    fs::write(&map, tampered).unwrap();

    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    let conflict = validation
        .conflicts()
        .iter()
        .find(|conflict| conflict.path() == "src/auth/MAP.md")
        .expect("an edited generated section is a conflict");
    assert_eq!(conflict.reason(), ConflictReason::GeneratedEdited);
    assert!(!validation.is_publishable());
}

#[test]
fn a_foreign_file_at_a_reserved_path_is_never_replaced() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("src/auth")).unwrap();
    fs::write(
        root.path().join("src/auth").join(MAP_FILE_NAME),
        "# Hand-written notes\n\nSomebody's actual documentation.\n",
    )
    .unwrap();

    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    let conflict = validation
        .conflicts()
        .iter()
        .find(|conflict| conflict.path() == "src/auth/MAP.md")
        .expect("a foreign file at a reserved path is a conflict");
    assert_ne!(conflict.reason(), ConflictReason::Unreadable);
    assert!(!validation.is_publishable());
}

/// The replaceability of `.rr/SYMBOLS.md` is a licence to repair rr's own file,
/// not a licence to take the path. Both halves are asserted together because
/// they are one rule read from two sides, and an implementation that widens the
/// licence passes each half alone.
#[test]
fn the_symbol_index_is_repaired_but_never_seized() {
    let snapshot = snapshot();

    let damaged = tempfile::tempdir().unwrap();
    publish(damaged.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));
    let path = damaged.path().join(SYMBOLS_PATH);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("symbols: ", "symbols_: ")).unwrap();

    let repair =
        rr_core::text::validate_text_artifacts(&snapshot, damaged.path(), DEFAULT_MAP_BUDGET)
            .unwrap();
    assert!(
        repair.stale().iter().any(|path| path == SYMBOLS_PATH),
        "a damaged copy of rr's own index was not offered for repair: {:?}",
        repair.conflicts()
    );

    for (label, bytes) in [
        (
            "foreign",
            "# My notes\n\nNothing to do with rr.\n".to_owned(),
        ),
        (
            "conflicted",
            format!(
                "<<<<<<< HEAD\n{}=======\n>>>>>>> other\n",
                fs::read_to_string(&path).unwrap()
            ),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".rr")).unwrap();
        fs::write(root.path().join(SYMBOLS_PATH), bytes).unwrap();

        let validation =
            rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET)
                .unwrap();
        assert!(
            !validation.stale().iter().any(|path| path == SYMBOLS_PATH),
            "a {label} file at {SYMBOLS_PATH} was queued for overwrite"
        );
        assert!(
            validation
                .conflicts()
                .iter()
                .any(|conflict| conflict.path() == SYMBOLS_PATH),
            "a {label} file at {SYMBOLS_PATH} was not reported"
        );
        assert!(!validation.is_publishable());
    }
}

#[test]
fn a_merge_marker_is_reported_as_one() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let map = root.path().join(MAP_FILE_NAME);
    let text = fs::read_to_string(&map).unwrap();
    fs::write(
        &map,
        format!("<<<<<<< HEAD\n{text}=======\n>>>>>>> other\n"),
    )
    .unwrap();

    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    let conflict = validation
        .conflicts()
        .iter()
        .find(|conflict| conflict.path() == MAP_FILE_NAME)
        .expect("a merge marker is a conflict");
    assert_eq!(conflict.reason(), ConflictReason::MergeConflict);
}

#[test]
fn an_oversize_purpose_is_a_conflict_and_is_never_trimmed() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let map = root.path().join(MAP_FILE_NAME);
    let text = fs::read_to_string(&map).unwrap();
    let oversize = "x".repeat(PURPOSE_MAX_BYTES + 1);
    fs::write(&map, replace_purpose(&text, &oversize)).unwrap();
    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    let conflict = validation
        .conflicts()
        .iter()
        .find(|conflict| conflict.path() == MAP_FILE_NAME)
        .expect("an oversize purpose is a conflict");
    assert_eq!(conflict.reason(), ConflictReason::Purpose);
    assert!(
        !validation.stale().contains(&MAP_FILE_NAME.to_owned()),
        "an oversize purpose was queued for replacement"
    );
    assert!(!validation.is_publishable());
    assert!(
        fs::read_to_string(&map).unwrap().contains(&oversize),
        "the oversize purpose was modified"
    );
}

#[test]
fn a_stale_overflow_page_is_removable_and_a_modified_one_is_not() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let stale_rel = "src/auth/MAP.rr-00000000-00000000.md";
    let stale_path = root.path().join(stale_rel);
    fs::create_dir_all(stale_path.parent().unwrap()).unwrap();
    fs::write(&stale_path, V1_OVERFLOW_PAGE.as_bytes()).unwrap();

    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    assert_eq!(validation.removable(), &[stale_rel.to_owned()]);
    assert!(validation.conflicts().is_empty());

    fs::write(&stale_path, "hand written\n").unwrap();
    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    assert!(validation.removable().is_empty());
    assert_eq!(
        validation.conflicts().first().map(Conflict::reason),
        Some(ConflictReason::NotOwned)
    );
}

#[test]
fn a_case_collision_is_reported_before_the_file_is_read() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    let rendered = render(&snapshot, DEFAULT_MAP_BUDGET);
    for file in rendered.files() {
        if file.path() == "src/auth/MAP.md" {
            continue;
        }
        let path = root.path().join(file.path());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, file.bytes()).unwrap();
    }
    let auth = rendered
        .files()
        .iter()
        .find(|file| file.path() == "src/auth/MAP.md")
        .expect("the fixture has an auth map");
    fs::create_dir_all(root.path().join("src/auth")).unwrap();
    fs::write(root.path().join("src/auth/map.md"), auth.bytes()).unwrap();
    if !case_insensitive(root.path()) {
        return;
    }

    let validation =
        rr_core::text::validate_text_artifacts(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    let conflict = validation
        .conflicts()
        .iter()
        .find(|conflict| conflict.path() == "src/auth/MAP.md")
        .expect("a folded name is a conflict");
    assert_eq!(conflict.reason(), ConflictReason::CaseCollision);
    assert_eq!(conflict.found(), Some("src/auth/map.md"));
    assert!(
        !validation
            .fresh()
            .iter()
            .any(|path| path == "src/auth/MAP.md"),
        "a folded valid page was treated as fresh"
    );
    assert!(
        !validation
            .stale()
            .iter()
            .any(|path| path == "src/auth/MAP.md"),
        "a folded valid page was treated as stale"
    );
}

fn case_insensitive(root: &Path) -> bool {
    let lower = root.join("rr-case-probe");
    let upper = root.join("RR-CASE-PROBE");
    fs::write(&lower, b"probe").unwrap();
    let folds = upper.is_file();
    let _ = fs::remove_file(&lower);
    let _ = fs::remove_file(&upper);
    folds
}

const V1_OVERFLOW_PAGE: &str = "\
---
type: \"rr-map\"
format: 1
scope: \"src/auth\"
page: \"part-00000000-00000000\"
fidelity: \"syntax\"
index_hash: \"blake3:5c9145d4af52610c1d19f495e9db4a82c4b2f57e0760168493bbbca072642c91\"
api_hash: \"blake3:2833cedef8c5ef0b17b18f4846ff4dd36ffbd23de83e663e74ce6256a375fae5\"
generated_hash: \"blake3:7e51ee413f2c767fef64d81d6dbe434901b30f98c491a5a7fe71df9a58685fe5\"
tokens: 87
budget: 12
---
<!-- generated by rr format 1; edit only the purpose slot -->
# Repository map part: src/auth

## Children
_None._

## API
### src/auth/token.rs
- `auth::token::Claims` — `pub struct Claims` — [source](<token.rs#Claims>)

## Tests
_None._

<!-- rr:merge-conflict: resolve one side, then run `rr map`; never hand-merge generated sections -->
";

#[test]
fn the_catalog_names_the_map_that_lists_each_symbol() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &render(&snapshot, DEFAULT_MAP_BUDGET));

    let catalog =
        rr_core::text::validated_map_catalog(&snapshot, root.path(), DEFAULT_MAP_BUDGET).unwrap();
    assert!(!catalog.is_empty());
    let verify = support::symbol_named(&snapshot, "verify_token");
    let owner = catalog
        .owner(verify)
        .expect("verify_token has an owning map");
    assert_eq!(owner.path().as_str(), "src/auth/MAP.md");

    let map = fs::read(root.path().join("src/auth/MAP.md")).unwrap();
    assert_eq!(
        parse_map(&map).unwrap().api_hash(),
        owner.api_hash().digest()
    );
}

#[test]
fn the_catalog_refuses_to_describe_a_repository_it_does_not_match() {
    let snapshot = snapshot();
    let root = tempfile::tempdir().unwrap();
    assert!(
        rr_core::text::validated_map_catalog(&snapshot, root.path(), DEFAULT_MAP_BUDGET).is_err(),
        "a catalog was built for a repository with no maps"
    );
}

#[test]
fn an_empty_repository_still_produces_a_readable_root() {
    let snapshot = support::synthetic_snapshot(&[]);
    let set = render(&snapshot, DEFAULT_MAP_BUDGET);
    let files = texts(&set);
    let root = files
        .get(MAP_FILE_NAME)
        .expect("an empty repository has a root map");
    assert!(root.contains("_None._"));
    let parsed = parse_map(root.as_bytes()).unwrap();
    assert!(parsed.api().is_empty() && parsed.children().is_empty() && parsed.tests().is_empty());
}

/// Replaces the logical purpose content, leaving every other byte alone.
fn replace_purpose(text: &str, purpose: &str) -> String {
    let open = "<!-- rr:slot purpose max=160 -->\n";
    let close = "<!-- /rr:slot -->";
    let start = text.find(open).expect("a router has a purpose slot") + open.len();
    let end = text[start..].find(close).expect("a slot closes") + start;
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(purpose);
    if !purpose.is_empty() {
        out.push('\n');
    }
    out.push_str(&text[end..]);
    out
}
