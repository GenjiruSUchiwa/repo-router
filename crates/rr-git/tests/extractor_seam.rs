mod common;

use common::write;
use rr_core::cache::FactCache;
use rr_core::facts::{DegradedReason, ParseStatus};
use rr_core::lang::Lang;
use rr_core::parser::Registry;
use rr_core::walk::SourceFile;
use rr_core::RelPath;
use rr_git::map::BuildContext;
use rr_git::pipeline::Worker;

fn source(path: &str, lang: Lang) -> SourceFile {
    SourceFile::new(RelPath::new(path).unwrap(), lang, false)
}

/// An OID hashes content alone, so same bytes under two extensions collide on
/// everything but the language. Keying every entry as Rust — as the pipeline
/// did — served the Rust parse to the other file.
#[test]
fn a_file_is_never_served_another_languages_facts() {
    let root = tempfile::tempdir().unwrap();
    let identical = "pub fn a() {}\n";
    write(root.path(), "a.rs", identical);
    write(root.path(), "a.py", identical);

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());

    // Rust first, so its entry is the one sitting there when Python asks.
    let (rust, _) = worker.process(&source("a.rs", Lang::Rust), &cache).unwrap();
    assert!(
        !rust.unwrap().facts.defs().is_empty(),
        "the Rust file should have been parsed"
    );

    let (python, _) = worker
        .process(&source("a.py", Lang::Python), &cache)
        .unwrap();
    assert!(
        python.unwrap().facts.defs().is_empty(),
        "the Python file was served the Rust entry"
    );
    assert_eq!(cache.stats().hits(), 0, "no entry was shared");
}

#[test]
fn the_walk_allowlist_is_exactly_what_the_registry_supports() {
    let root = tempfile::tempdir().unwrap();
    let context = BuildContext::open(root.path(), 1).unwrap();
    assert_eq!(context.walk.languages, Some(Registry::supported()));
}

/// Facts made without an extractor describe rr's support, not the file, so a
/// later run that gained the extractor must not be handed them.
///
/// Since #31 the facts say so themselves. `NoExtractor` is a reason a reader
/// can act on, and the pipeline decides whether to cache by asking the status
/// rather than by remembering which branch it came from — so the two degrades
/// that used to share `ParserReturnedNone` can no longer be confused.
#[test]
fn an_unsupported_language_degrades_without_being_cached() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "script.py", "def greet():\n    pass\n");

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());
    let script = source("script.py", Lang::Python);

    let (input, _) = worker.process(&script, &cache).unwrap();
    let facts = input.unwrap().facts;
    assert!(matches!(
        facts.status(),
        ParseStatus::Degraded {
            reason: DegradedReason::NoExtractor,
            ..
        }
    ));
    assert!(!facts.status().is_cacheable());
    assert!(facts.defs().is_empty());

    worker.process(&script, &cache).unwrap();
    assert_eq!(
        cache.stats().hits(),
        0,
        "the degrade was stored and served back"
    );
}

/// A parse that failed is still a fact about the file, and is still cached.
///
/// The companion to the test above: separating the two reasons is only worth
/// anything if they now lead to different behaviour. Rust has an extractor, so
/// a file it cannot parse takes the other branch.
#[test]
fn a_file_its_own_parser_could_not_read_is_still_cached() {
    let root = tempfile::tempdir().unwrap();
    // Invalid UTF-8 is the degrade the Rust extractor reaches without needing a
    // pathological source: it is reproducible from these exact bytes.
    std::fs::write(root.path().join("broken.rs"), [0xFF, 0xFE, b'f', b'n']).unwrap();

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());
    let broken = source("broken.rs", Lang::Rust);

    let (input, _) = worker.process(&broken, &cache).unwrap();
    let status = input.unwrap().facts.status();
    assert!(matches!(
        status,
        ParseStatus::Degraded {
            reason: DegradedReason::InvalidUtf8,
            ..
        }
    ));
    assert!(status.is_cacheable());

    worker.process(&broken, &cache).unwrap();
    assert_eq!(
        cache.stats().hits(),
        1,
        "a degrade that describes the file was not reused"
    );
}
