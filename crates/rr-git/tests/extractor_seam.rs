use rr_core::cache::{CacheKey, CacheOutcome, FactCache};
use rr_core::lang::Lang;
use rr_core::parser::Registry;
use rr_core::walk::SourceFile;
use rr_core::{Facts, RelPath};
use rr_git::map::BuildContext;
use rr_git::oid_of;
use rr_git::pipeline::Worker;

#[test]
fn a_cache_entry_is_keyed_by_the_files_own_language() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(root.path().join("a.py"), "pub fn a() {}\n").unwrap();

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());

    let rs = SourceFile::new(RelPath::new("a.rs").unwrap(), Lang::Rust, false);
    let py = SourceFile::new(RelPath::new("a.py").unwrap(), Lang::Python, false);

    worker.process(&rs, &cache).unwrap();
    worker.process(&py, &cache).unwrap();

    assert_eq!(
        cache.stats().misses(),
        2,
        "two distinct entries, not one shared"
    );
    assert_eq!(
        cache.stats().hits(),
        0,
        "the second file must not reuse the first's entry"
    );

    let oid = oid_of(None, root.path(), &RelPath::new("a.rs").unwrap()).unwrap();
    let rust_facts = match cache.get::<Facts>(&CacheKey::new(oid, Lang::Rust)).unwrap() {
        CacheOutcome::Hit(facts) => facts,
        other => panic!("Rust entry missing: {other:?}"),
    };
    let py_facts = match cache
        .get::<Facts>(&CacheKey::new(oid, Lang::Python))
        .unwrap()
    {
        CacheOutcome::Hit(facts) => facts,
        other => panic!("Python entry missing: {other:?}"),
    };
    assert!(rust_facts.defs().len() > py_facts.defs().len());
}

#[test]
fn the_walk_allowlist_is_exactly_what_the_registry_supports() {
    let root = tempfile::tempdir().unwrap();
    let context = BuildContext::open(root.path(), 1).unwrap();
    assert_eq!(context.walk.languages, Some(Registry::supported()));
}

#[test]
fn an_unsupported_language_degrades_instead_of_failing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("script.py"), "def greet():\n    pass\n").unwrap();

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());
    let source = SourceFile::new(RelPath::new("script.py").unwrap(), Lang::Python, false);

    let (input, _stats) = worker.process(&source, &cache).unwrap();
    let facts = input.unwrap().facts;
    assert!(matches!(
        facts.status(),
        rr_core::facts::ParseStatus::Degraded { .. }
    ));
    assert!(facts.defs().is_empty());
}
