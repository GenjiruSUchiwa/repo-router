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

/// The sharper form of the test above, because here the collision is one a
/// real repository produces rather than one a test has to contrive. `.ts` and
/// `.tsx` are two grammars over one syntax: a file with no JSX in it is
/// legal, identical bytes under both, and a plausible thing to find committed
/// twice. They also share a query and differ only in the grammar it was
/// compiled against, so nothing but the cache key can keep them apart.
#[test]
fn an_identical_file_in_two_languages_does_not_share_a_cache_entry() {
    let root = tempfile::tempdir().unwrap();
    let identical =
        "export class Badge {\n    render(): string {\n        return \"x\";\n    }\n}\n";
    write(root.path(), "badge.ts", identical);
    write(root.path(), "badge.tsx", identical);

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());

    let (typescript, _) = worker
        .process(&source("badge.ts", Lang::TypeScript), &cache)
        .unwrap();
    let (tsx, _) = worker
        .process(&source("badge.tsx", Lang::Tsx), &cache)
        .unwrap();

    assert_eq!(cache.stats().hits(), 0, "the two grammars shared an entry");
    let typescript = typescript.unwrap().facts;
    let tsx = tsx.unwrap().facts;
    assert_eq!(typescript.defs().len(), 2);
    assert_eq!(tsx.defs().len(), 2);
    let (again, _) = worker
        .process(&source("badge.tsx", Lang::Tsx), &cache)
        .unwrap();
    assert_eq!(
        cache.stats().hits(),
        1,
        "the second read missed its own entry"
    );
    assert_eq!(again.unwrap().facts, tsx);
}

/// The companion to `an_unsupported_language_degrades_without_being_cached`,
/// which proves the entry is not written. This proves it was worth not
/// writing: the same OID, asked for again once rr has an extractor for it,
/// comes back as a real parse rather than as the empty record rr stored when
/// it had nothing to read the file with.
///
/// That is exactly what happens to every `.ts` file in every repository that
/// was indexed before this branch.
#[test]
fn facts_made_without_an_extractor_are_never_cached() {
    let root = tempfile::tempdir().unwrap();
    let content = "class Service:\n    def run(self):\n        return helper()\n";
    write(root.path(), "service.py", content);

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());
    let (unsupported, _) = worker
        .process(&source("service.py", Lang::Kotlin), &cache)
        .unwrap();
    let unsupported = unsupported.unwrap().facts;
    assert!(matches!(
        unsupported.status(),
        ParseStatus::Degraded {
            reason: DegradedReason::NoExtractor,
            ..
        }
    ));
    assert!(unsupported.defs().is_empty());
    let misses = cache.stats().misses();
    let (supported, _) = worker
        .process(&source("service.py", Lang::Python), &cache)
        .unwrap();
    let supported = supported.unwrap().facts;
    assert_eq!(cache.stats().hits(), 0, "the empty record was served back");
    assert!(
        cache.stats().misses() > misses,
        "the second read did not reach the cache at all"
    );
    assert!(matches!(supported.status(), ParseStatus::Tags { .. }));
    assert!(
        supported.defs().iter().any(|def| def.name == "run"),
        "the file rr can now read came back empty"
    );
}

#[test]
fn the_walk_allowlist_is_every_indexable_language() {
    let root = tempfile::tempdir().unwrap();
    let context = BuildContext::open(root.path(), 1).unwrap();
    assert_eq!(context.walk.languages, Some(Registry::indexable()));
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
    write(root.path(), "script.kt", "class Greeter {}\n");

    let cache = FactCache::open(root.path()).unwrap();
    let mut worker = Worker::new(root.path());
    let script = source("script.kt", Lang::Kotlin);
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
