//! The published contract of the text artifacts `rr map` writes.
//!
//! These are the assertions a user can act on: where the files land, that a
//! second run touches nothing, that a hand-written file at a reserved path
//! survives, and that a conflict costs the whole run rather than one file.

mod common;

use std::fmt::Write as _;
use std::path::Path;

use tempfile::TempDir;

use common::{code, commit_all, empty_repo, read, run, stderr, stdout, write};

/// A repository with two directories, one of them nested.
fn repo() -> TempDir {
    let temp = empty_repo();
    write(temp.path(), "lib.rs", "pub fn entry() -> u32 { 1 }\n");
    write(
        temp.path(),
        "src/auth/token.rs",
        "pub struct Claims { pub subject: String }\n",
    );
    write(
        temp.path(),
        "extra/other.rs",
        "pub fn other() -> u32 { 2 }\n",
    );
    commit_all(temp.path(), "seed");
    temp
}

#[test]
fn a_map_for_a_tags_indexed_directory_has_a_populated_api_section() {
    let temp = empty_repo();
    write(
        temp.path(),
        "src/service.py",
        "class Service:\n    def run(value):\n        return helper(value)\n",
    );
    commit_all(temp.path(), "python source");

    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    let map = read(temp.path(), "src/MAP.md");
    assert!(map.contains("fidelity: \"syntax-tags\""), "{map}");
    let api = map
        .split("## API")
        .nth(1)
        .unwrap()
        .split("## Tests")
        .next()
        .unwrap();
    assert!(api.contains("Service"), "{map}");
    assert!(api.contains("def run(value):"), "{map}");
    assert!(!api.contains("_None._"), "{map}");
}

#[test]
fn a_first_map_writes_the_whole_generation() {
    let temp = repo();
    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    for path in [
        "MAP.md",
        "src/MAP.md",
        "src/auth/MAP.md",
        "extra/MAP.md",
        ".rr/SYMBOLS.md",
        ".rr/.gitignore",
    ] {
        assert!(
            temp.path().join(path).is_file(),
            "{path} was not written: {}",
            stdout(&output)
        );
    }
    assert!(stdout(&output).contains("SYMBOLS.md written"));
    assert!(read(temp.path(), ".rr/.gitignore").contains("/SYMBOLS.md"));
}

/// The property that makes committed maps reviewable: running twice is a no-op.
///
/// Asserted on bytes and on modification times, because a run that rewrites
/// identical bytes still shows up in every file watcher and build system.
#[test]
fn running_again_touches_nothing() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let before: Vec<_> = ["MAP.md", "src/MAP.md", ".rr/SYMBOLS.md"]
        .iter()
        .map(|path| {
            let full = temp.path().join(path);
            let meta = std::fs::metadata(&full).expect("artifact exists");
            (read(temp.path(), path), meta.modified().expect("mtime"))
        })
        .collect();

    let again = run(temp.path(), &["map"]);
    assert_eq!(code(&again), 0);
    assert!(
        stdout(&again).contains("0 written"),
        "second run wrote something: {}",
        stdout(&again)
    );

    for (index, path) in ["MAP.md", "src/MAP.md", ".rr/SYMBOLS.md"]
        .iter()
        .enumerate()
    {
        let full = temp.path().join(path);
        let meta = std::fs::metadata(&full).expect("artifact still exists");
        assert_eq!(read(temp.path(), path), before[index].0, "{path} changed");
        assert_eq!(
            meta.modified().expect("mtime"),
            before[index].1,
            "{path} was rewritten with identical bytes"
        );
    }
}

/// Committed maps do not make the next refresh think there is work to do.
///
/// Committing them first is what makes this worth asserting: the Git delta an
/// incremental refresh consults now names every one of them. What keeps them
/// out of the *index* is `walk::collected_lang`, tested there — the allowlist
/// comes from the registry, so this alone would pass either way.
#[test]
fn generated_maps_never_re_enter_the_index() {
    let temp = repo();
    run(temp.path(), &["map"]);
    commit_all(temp.path(), "maps");

    let refreshed = run(temp.path(), &["refresh", "--verbose"]);
    assert_eq!(code(&refreshed), 0);
    let text = stdout(&refreshed);
    assert!(text.contains("0 reparsed"), "{text}");
    assert!(text.contains("0 written"), "{text}");
    assert!(!text.contains("MAP.md"), "a map was indexed: {text}");
}

#[test]
fn an_edited_generated_section_costs_the_whole_run() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let original = read(temp.path(), "src/auth/MAP.md");
    write(
        temp.path(),
        "src/auth/MAP.md",
        &original.replace("Claims", "Claimz"),
    );
    // A real change elsewhere, so the run has something it would otherwise do.
    write(temp.path(), "src/added.rs", "pub fn added() -> u32 { 3 }\n");
    let before_root = read(temp.path(), "MAP.md");

    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 1, "{}", stdout(&output));
    assert!(stderr(&output).contains("a generated section was edited"));
    assert!(stderr(&output).contains("src/auth/MAP.md"));

    assert_eq!(
        read(temp.path(), "src/auth/MAP.md"),
        original.replace("Claims", "Claimz"),
        "the edited file was overwritten"
    );
    assert_eq!(
        read(temp.path(), "MAP.md"),
        before_root,
        "an unrelated map was written while another file conflicted"
    );
}

#[test]
fn a_hand_written_file_at_a_reserved_path_is_never_taken() {
    let temp = repo();
    let notes = "# Directory notes\n\nWritten by a person.\n";
    write(temp.path(), "src/MAP.md", notes);

    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 1, "{}", stdout(&output));
    assert!(stderr(&output).contains("path is not owned by rr"));
    assert_eq!(read(temp.path(), "src/MAP.md"), notes);
    assert!(
        !temp.path().join(".rr/SYMBOLS.md").is_file(),
        "the local index was written despite a conflict"
    );
}

#[test]
fn a_written_purpose_survives_every_later_run() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let purpose = "Authentication tokens and their claims.";
    let map = read(temp.path(), "src/auth/MAP.md");
    let seeded = map
        .lines()
        .find(|line| line.starts_with("TODO:"))
        .expect("a first generation seeds a pending purpose")
        .to_owned();
    write(
        temp.path(),
        "src/auth/MAP.md",
        &map.replace(&seeded, purpose),
    );

    write(
        temp.path(),
        "src/auth/session.rs",
        "pub fn open() -> u32 { 4 }\n",
    );
    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let after = read(temp.path(), "src/auth/MAP.md");
    assert!(
        after.contains(purpose),
        "the purpose was rewritten: {after}"
    );
    assert!(after.contains("session.rs"), "the map was not regenerated");
}

/// A directory that loses its last source file leaves a committed map behind.
///
/// Nothing links to it and no later run would revisit it, so the generation
/// that replaces it also unlinks it — including a whole vanished subtree.
#[test]
fn a_map_whose_directory_is_gone_is_removed() {
    let temp = repo();
    write(
        temp.path(),
        "deep/nested/leaf.rs",
        "pub fn leaf() -> u32 { 5 }\n",
    );
    run(temp.path(), &["map"]);
    assert!(temp.path().join("deep/nested/MAP.md").is_file());

    std::fs::remove_file(temp.path().join("deep/nested/leaf.rs")).expect("remove source");
    let output = run(temp.path(), &["map", "--verbose"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    assert!(!temp.path().join("deep/nested/MAP.md").exists());
    assert!(!temp.path().join("deep/MAP.md").exists());
    assert!(
        temp.path().join("MAP.md").is_file(),
        "the root map was removed with the subtree"
    );
    assert!(stdout(&output).contains("text remove  deep/MAP.md"));
}

/// `rr refresh` finds nothing to rebuild but still owes the missing file.
#[test]
fn refresh_restores_a_deleted_map_without_rebuilding() {
    let temp = repo();
    run(temp.path(), &["map"]);
    std::fs::remove_file(temp.path().join("src/MAP.md")).expect("remove map");

    let output = run(temp.path(), &["refresh", "--verbose"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(temp.path().join("src/MAP.md").is_file());

    let text = stdout(&output);
    assert!(text.contains("0 reparsed"), "{text}");
    assert!(text.contains("1 written"), "{text}");
}

/// `.rr/SYMBOLS.md` is fully replaceable, so rr repairs its own damaged copy —
/// and says so, rather than reporting an ordinary rewrite as a repair.
#[test]
fn a_damaged_symbol_index_is_repaired_and_named_as_such() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let ordinary = run(temp.path(), &["map"]);
    assert!(stdout(&ordinary).contains("SYMBOLS.md unchanged"));

    let damaged = read(temp.path(), ".rr/SYMBOLS.md").replace("symbols: ", "symbols_: ");
    write(temp.path(), ".rr/SYMBOLS.md", &damaged);

    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("SYMBOLS.md repaired"),
        "{}",
        stdout(&output)
    );
}

/// Writes one file holding `count` public functions with names long enough to
/// force paging at the default budget.
fn wide_module(root: &Path, count: usize) {
    let mut source = String::new();
    for index in 0..count {
        let _ = writeln!(
            source,
            "pub fn function_with_a_reasonably_long_name_{index:03}(argument: u32) -> u32 {{ {index} }}"
        );
    }
    write(root, "big/many.rs", &source);
}

/// A scope too wide for one page becomes a tree, and the tree shrinks again.
///
/// Growth is the easy direction. Shrinking is where a router can be left
/// pointing at pages that no longer exist, so both the count and the links are
/// asserted, and a third run has to find nothing left to do.
#[test]
fn an_overflow_tree_grows_and_shrinks_without_dangling_links() {
    let temp = repo();
    wide_module(temp.path(), 120);
    run(temp.path(), &["map"]);

    let grown = overflow_pages(temp.path());
    assert!(grown > 30, "120 symbols did not page: {grown} pages");
    assert_links_resolve(temp.path());

    wide_module(temp.path(), 4);
    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 0, "{}", stderr(&output));

    let shrunk = overflow_pages(temp.path());
    assert!(shrunk < grown, "{grown} pages did not shrink");
    assert_links_resolve(temp.path());
    assert!(stdout(&output).contains(&format!("{} removed", grown - shrunk)));

    let settled = run(temp.path(), &["map"]);
    assert!(
        stdout(&settled).contains("0 written") && stdout(&settled).contains("0 removed"),
        "the shrunk tree did not settle: {}",
        stdout(&settled)
    );
}

fn overflow_pages(root: &Path) -> usize {
    std::fs::read_dir(root.join("big"))
        .expect("the wide scope exists")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("MAP.rr-"))
        .count()
}

/// Every page this scope's router names has to be a page that is there.
fn assert_links_resolve(root: &Path) {
    let router = read(root, "big/MAP.md");
    for line in router.lines() {
        let Some(rest) = line.strip_prefix("- [MAP.rr-") else {
            continue;
        };
        let name = format!("MAP.rr-{}", rest.split(']').next().unwrap_or_default());
        assert!(
            root.join("big").join(&name).is_file(),
            "the router points at {name}, which is not on disk"
        );
    }
}

/// Where the command was invoked from must not change what it does.
///
/// Artifact paths are repository-relative, so staging them against the working
/// directory instead of the work root finds every map missing and turns a
/// nothing-to-do run into one that takes the publication guard. Invisible in
/// the report — both spellings say "unchanged" — so it is asserted where it
/// shows: against a held lock, which only a run that reaches for it can fail.
#[test]
fn a_no_op_refresh_takes_no_lock_from_any_directory() {
    let temp = repo();
    run(temp.path(), &["map"]);
    // Held by nobody, but `gix_lock` refuses a claim whose file already exists.
    write(temp.path(), ".rr/local/publication.lock", "");

    for from in [".", "src", "src/auth"] {
        let output = run(&temp.path().join(from), &["refresh"]);
        assert_eq!(
            code(&output),
            0,
            "refresh from {from} reached for the guard: {}",
            stderr(&output)
        );
        assert!(stdout(&output).contains("0 written"), "{from}");
    }
}

/// The guard is still taken from a subdirectory when there is work to do.
#[test]
fn a_refresh_from_a_subdirectory_still_repairs_the_whole_repository() {
    let temp = repo();
    run(temp.path(), &["map"]);
    std::fs::remove_file(temp.path().join("extra/MAP.md")).expect("remove map");

    let output = run(&temp.path().join("src/auth"), &["refresh"]);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(temp.path().join("extra/MAP.md").is_file());
    assert!(stdout(&output).contains("1 written"), "{}", stdout(&output));
}

/// `.rr/.gitignore` is written last, so validating it last means diagnosing it
/// after the snapshot and every map are already replaced. Staged with the rest
/// instead, because "nothing was written" has to be true when it is printed.
#[test]
fn a_malformed_managed_ignore_is_refused_before_anything_is_written() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let doubled = read(temp.path(), ".rr/.gitignore").repeat(2);
    write(temp.path(), ".rr/.gitignore", &doubled);
    let root_map = read(temp.path(), "MAP.md");
    write(
        temp.path(),
        "src/auth/extra.rs",
        "pub fn extra() -> u32 { 7 }\n",
    );

    let output = run(temp.path(), &["map"]);

    assert_eq!(code(&output), 1, "{}", stdout(&output));
    assert!(
        stderr(&output).contains(".rr/.gitignore: managed ignore markers"),
        "{}",
        stderr(&output)
    );
    assert_eq!(read(temp.path(), "MAP.md"), root_map, "a map was written");
    assert!(
        stdout(&run(temp.path(), &["status"])).contains("snapshot: stale"),
        "the snapshot was republished despite the conflict"
    );
}

/// Every read here follows links, so an unchecked symlink would be judged on
/// its target's bytes and then written through — rr's output landing somewhere
/// rr never chose. The `.rr/SYMBOLS.md` case is the sharp one: that path is
/// repaired in place, so a link there is an offer to overwrite its target.
#[cfg(unix)]
#[test]
fn a_reserved_path_that_is_a_symlink_is_never_written_through() {
    let temp = repo();
    run(temp.path(), &["map"]);

    std::fs::write(temp.path().join("outside.txt"), "not rr's to take\n").expect("write target");
    let unchanged = read(temp.path(), "src/auth/MAP.md");

    std::fs::remove_file(temp.path().join("extra/MAP.md")).expect("remove map");
    std::os::unix::fs::symlink("../src/auth/MAP.md", temp.path().join("extra/MAP.md"))
        .expect("link map");
    std::fs::remove_file(temp.path().join(".rr/SYMBOLS.md")).expect("remove symbols");
    std::os::unix::fs::symlink("../outside.txt", temp.path().join(".rr/SYMBOLS.md"))
        .expect("link symbols");

    // Something to do, so the run is not declining work it never had.
    write(temp.path(), "extra/more.rs", "pub fn more() -> u32 { 6 }\n");
    let output = run(temp.path(), &["map"]);

    assert_eq!(code(&output), 1, "{}", stdout(&output));
    let message = stderr(&output);
    assert!(
        message.contains("extra/MAP.md: path is a symbolic link"),
        "{message}"
    );
    assert!(
        message.contains(".rr/SYMBOLS.md: path is a symbolic link"),
        "{message}"
    );
    assert_eq!(read(temp.path(), "outside.txt"), "not rr's to take\n");
    assert_eq!(read(temp.path(), "src/auth/MAP.md"), unchanged);
}

#[test]
fn a_merge_conflict_is_reported_as_one() {
    let temp = repo();
    run(temp.path(), &["map"]);

    let map = read(temp.path(), "MAP.md");
    write(
        temp.path(),
        "MAP.md",
        &format!("<<<<<<< HEAD\n{map}=======\n>>>>>>> other\n"),
    );

    let output = run(temp.path(), &["map"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("Git conflict markers"));
}
