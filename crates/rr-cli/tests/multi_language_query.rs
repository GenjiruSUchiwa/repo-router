//! `rr query` against a repository that is not written in one language.
//!
//! The unit tests prove the extractors read the files. This proves the whole
//! path holds: index a mixed repository through the binary, ask it for a
//! TypeScript symbol and a Python one by the spelling each language uses, and
//! get the file and line back.

#![allow(clippy::unwrap_used)]

mod common;

use common::{code, commit_all, empty_repo, json, run, stdout, write};
use tempfile::TempDir;

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

    /** Describes where this client points. */
    describe(): string {
        return this.secret;
    }
}
";

const PYTHON: &str = "\
class Service:
    \"\"\"Runs one request.\"\"\"

    def dispatch(self, value):
        return helper(value)
";

const RUST: &str = "\
/// Routes one request.
pub fn route(path: &str) -> usize {
    path.len()
}
";

fn mixed_repo() -> TempDir {
    let repo = empty_repo();
    write(repo.path(), "src/client.ts", TYPESCRIPT);
    write(repo.path(), "src/service.py", PYTHON);
    write(repo.path(), "src/router.rs", RUST);
    commit_all(repo.path(), "three languages");
    let map = run(repo.path(), &["map"]);
    assert_eq!(code(&map), 0, "rr map failed: {}", stdout(&map));
    repo
}

#[test]
fn rr_query_routes_into_typescript_and_python_definitions() {
    let repo = mixed_repo();

    let typescript = run(repo.path(), &["query", "--json", "client.Client.describe"]);
    assert_eq!(code(&typescript), 0, "{}", stdout(&typescript));
    let typescript = json(&typescript);
    assert_eq!(typescript["result"], "direct");
    assert_eq!(typescript["anchor"]["path"], "src/client.ts");
    assert_eq!(typescript["anchor"]["symbol"], "describe");
    assert_eq!(typescript["anchor"]["lines"], serde_json::json!([14, 17]));

    let python = run(
        repo.path(),
        &["query", "--json", "service.Service.dispatch"],
    );
    assert_eq!(code(&python), 0, "{}", stdout(&python));
    let python = json(&python);
    assert_eq!(python["result"], "direct");
    assert_eq!(python["anchor"]["path"], "src/service.py");
    assert_eq!(python["anchor"]["symbol"], "dispatch");

    let text = run(repo.path(), &["query", "client.Client.describe"]);
    assert_eq!(code(&text), 0);
    let text = stdout(&text);
    assert!(text.contains("src/client.ts"), "{text}");
    assert!(text.contains("describe"), "{text}");
}

/// The fidelity guarantee, at the surface a caller actually reads. A tags-tier
/// record must never be dressed up as a checked one: rr read `describe(): string`
/// out of a tags query, not out of a resolver that knows what `string` is.
#[test]
fn a_typescript_symbol_is_reported_at_the_tags_tier_not_as_checked() {
    let repo = mixed_repo();
    let map = common::read(repo.path(), "src/MAP.md");

    assert!(
        map.contains("fidelity: \"syntax-tags\""),
        "the mixed map does not state its fidelity:\n{map}"
    );
    assert!(
        !map.contains("checked"),
        "a tags-tier map claimed a checked fidelity:\n{map}"
    );
}
