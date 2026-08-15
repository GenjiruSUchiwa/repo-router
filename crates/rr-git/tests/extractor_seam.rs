mod common;

use common::write;
use rr_core::cache::FactCache;
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
        rr_core::facts::ParseStatus::Degraded { .. }
    ));
    assert!(facts.defs().is_empty());

    worker.process(&script, &cache).unwrap();
    assert_eq!(
        cache.stats().hits(),
        0,
        "the degrade was stored and served back"
    );
}
