//! What four languages in one repository have to produce: one snapshot, one
//! hierarchy, and no evidence in the Rust half that the other three arrived.

mod common;

use common::{git_add_and_commit, init_git_repo, write};
use rr_core::cancel::CancelToken;
use rr_core::facts::ParseStatus;
use rr_core::index::Snapshot;
use rr_core::lang::Lang;
use rr_core::refresh::RefreshMode;
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
use rr_core::text::{ExistingPurposes, TextProjection, DEFAULT_MAP_BUDGET};
use std::path::Path;
use std::sync::Arc;

const RUST: &str = "\
/// Routes one request.
pub struct Router {
    routes: usize,
}

impl Router {
    pub fn route(&self, path: &str) -> usize {
        self.routes + path.len()
    }
}
";

const TYPESCRIPT: &str = "\
/** Options the caller controls. */
export interface ClientOptions {
    readonly baseUrl: string;
}

/** A configured client. */
export class Client {
    private secret: string;

    constructor(options: ClientOptions) {
        this.secret = options.baseUrl;
    }

    describe(): string {
        return this.secret;
    }
}
";

const TSX: &str = "\
/** One badge. */
export const Badge = (props: { label: string }) => <span>{props.label}</span>;
";

const PYTHON: &str = "\
class Service:
    \"\"\"Runs one request.\"\"\"

    def run(self, value):
        return helper(value)

def _internal(value):
    return value
";

fn build(root: &Path) -> Arc<Snapshot> {
    rr_git::refresh(root, 1, RefreshMode::Full, &CancelToken::new()).unwrap();
    match SnapshotStore::new(root).load().unwrap() {
        LoadOutcome::Ready(snapshot) => snapshot,
        outcome => panic!("unexpected snapshot outcome: {outcome:?}"),
    }
}

fn record<'a>(snapshot: &'a Snapshot, path: &str) -> &'a rr_core::index::FileRecord {
    snapshot
        .file_by_path(path)
        .unwrap_or_else(|| panic!("{path} is not in the snapshot"))
}

/// The test the extractor seam exists for. Four languages, two extraction
/// tiers, four cache keys, and one snapshot that has to hold all of it without
/// any of them reaching into another's records.
#[test]
fn a_mixed_language_repository_produces_one_coherent_snapshot() {
    let temp = init_git_repo();
    write(temp.path(), "src/router.rs", RUST);
    write(temp.path(), "src/client.ts", TYPESCRIPT);
    write(temp.path(), "src/badge.tsx", TSX);
    write(temp.path(), "src/service.py", PYTHON);
    git_add_and_commit(temp.path(), "four languages");

    let snapshot = build(temp.path());
    snapshot.validate().unwrap();
    assert_eq!(snapshot.files.len(), 4);
    assert_eq!(record(&snapshot, "src/router.rs").language, Lang::Rust);
    assert_eq!(
        record(&snapshot, "src/client.ts").language,
        Lang::TypeScript
    );
    assert_eq!(record(&snapshot, "src/badge.tsx").language, Lang::Tsx);
    assert_eq!(record(&snapshot, "src/service.py").language, Lang::Python);
    assert!(matches!(
        record(&snapshot, "src/router.rs").parse_status,
        ParseStatus::Complete
    ));
    for path in ["src/client.ts", "src/badge.tsx", "src/service.py"] {
        assert!(
            matches!(
                record(&snapshot, path).parse_status,
                ParseStatus::Tags {
                    parse_errors: false
                }
            ),
            "{path} was not read at the tags tier: {:?}",
            record(&snapshot, path).parse_status
        );
    }
    for file in &snapshot.files {
        let path = snapshot.file_path(file).unwrap();
        assert!(file.symbol_count > 0, "{path} produced no symbols");
    }
    let projection = TextProjection::from_snapshot(&snapshot, DEFAULT_MAP_BUDGET).unwrap();
    let rendered = projection.render(&ExistingPurposes::none()).unwrap();
    let text = |path: &str| {
        let file = rendered
            .files()
            .iter()
            .find(|file| file.path() == path)
            .unwrap_or_else(|| {
                panic!(
                    "{path} is not among {:?}",
                    rendered
                        .files()
                        .iter()
                        .map(rr_core::text::RenderedFile::path)
                        .collect::<Vec<_>>()
                )
            });
        std::str::from_utf8(file.bytes()).unwrap().to_owned()
    };
    let mut roots: Vec<_> = rendered
        .files()
        .iter()
        .map(rr_core::text::RenderedFile::path)
        .filter(|path| path.ends_with("/MAP.md") || *path == "MAP.md")
        .collect();
    roots.sort_unstable();
    assert_eq!(roots, vec!["MAP.md", "src/MAP.md"], "not one hierarchy");
    assert!(
        rendered
            .files()
            .iter()
            .all(|file| !file.path().contains("MAP.rr-")),
        "a scope still produced overflow pages"
    );

    let src = text("src/MAP.md");
    for name in ["Router", "Client", "Badge", "Service"] {
        assert!(
            src.contains(name),
            "{name} is missing from the src map:\n{src}"
        );
    }

    assert!(src.contains("Client.describe"), "{src}");
    assert!(src.contains("Service.run"), "{src}");
    assert!(src.contains("Router::route"), "{src}");
    assert!(!src.contains("client.ts::"), "{src}");
    let symbols = text(".rr/SYMBOLS.md");
    for name in ["Router", "Client", "Badge", "Service"] {
        assert!(symbols.contains(name), "{name} is missing from SYMBOLS.md");
    }
}

/// The promise this branch had to keep. Adding three languages may not change
/// what rr reads out of a repository that has none of them in it.
///
/// The one thing that does change is `meta.discovery_digest`, and it has to:
/// it mixes in the language allowlist, so growing that allowlist is exactly
/// what tells a repository *already holding* `.ts` files that its snapshot is
/// now missing them. A digest that ignored the language set would leave those
/// repositories serving an incomplete index until some unrelated file
/// happened to change. So the invariant worth asserting is about the records,
/// not about the bytes of the whole file.
#[test]
fn a_rust_only_repository_is_indexed_exactly_as_before() {
    let temp = init_git_repo();
    write(temp.path(), "src/router.rs", RUST);
    git_add_and_commit(temp.path(), "rust only");

    let snapshot = build(temp.path());
    snapshot.validate().unwrap();

    assert_eq!(snapshot.files.len(), 1);
    let router = record(&snapshot, "src/router.rs");
    assert_eq!(router.language, Lang::Rust);
    assert!(matches!(router.parse_status, ParseStatus::Complete));
    assert_eq!(rr_core::extractor_version(Lang::Rust), 3);
    for file in &snapshot.files {
        assert!(
            !matches!(file.parse_status, ParseStatus::Tags { .. }),
            "a Rust file was routed to the tags tier"
        );
    }
    let path = SnapshotStore::new(temp.path()).path().to_path_buf();
    let first = std::fs::read(&path).unwrap();
    drop(build(temp.path()));
    let second = std::fs::read(&path).unwrap();
    assert_eq!(first, second, "two builds of one repository disagreed");
}

#[test]
fn a_polyglot_repository_indexes_deterministically() {
    let temp = init_git_repo();
    write(temp.path(), "src/router.rs", RUST);
    write(temp.path(), "src/client.ts", TYPESCRIPT);
    write(temp.path(), "src/badge.tsx", TSX);
    write(temp.path(), "src/service.py", PYTHON);
    write(
        temp.path(),
        "src/app.js",
        "export function run() { return 1; }\n",
    );
    write(
        temp.path(),
        "src/icon.jsx",
        "export const Icon = () => <i/>;\n",
    );
    write(temp.path(), "src/main.go", "package app\nfunc Run() {}\n");
    write(
        temp.path(),
        "src/Service.java",
        "public class Service { public void run() {} }\n",
    );
    write(temp.path(), "src/run.c", "int run(void) { return 0; }\n");
    write(temp.path(), "src/run.cpp", "int run() { return 0; }\n");
    write(temp.path(), "src/app.rb", "def run; end\n");
    write(temp.path(), "src/app.lua", "function run() end\n");
    write(
        temp.path(),
        "src/Service.php",
        "<?php class Service { public function run() {} }\n",
    );
    write(temp.path(), "src/App.swift", "func run() {}\n");
    write(temp.path(), "src/Greeter.cs", "class Greeter {}\n");
    git_add_and_commit(temp.path(), "polyglot");

    let first = build(temp.path());
    first.validate().unwrap();
    assert!(
        first.files.len() >= 15,
        "expected every shipped language plus C#"
    );
    let csharp = record(&first, "src/Greeter.cs");
    assert!(
        matches!(
            csharp.parse_status,
            ParseStatus::Degraded {
                reason: rr_core::DegradedReason::NoExtractor,
                ..
            }
        ),
        "C# must be walked and degraded, not skipped"
    );

    let path = SnapshotStore::new(temp.path()).path().to_path_buf();
    let first_bytes = std::fs::read(&path).unwrap();
    drop(build(temp.path()));
    let second_bytes = std::fs::read(&path).unwrap();
    assert_eq!(first_bytes, second_bytes);
}
