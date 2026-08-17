# repo-router

**`rr`** — a repository navigator for coding agents (Claude Code, etc.), inspired by the publicly documented behavior of [Radar](https://radar.dev). It indexes a repository (Tree-sitter + lexical fingerprints), then answers navigation questions ("where is X defined?", "who calls Y?") with **minimal context**: a precise route to the source, not a file dump.

> An open, cross-platform reimplementation written from Radar's publicly documented and observed behavior — not from its source code, which remains private. See the [Specification](https://github.com/GenjiruSUchiwa/repo-router/wiki/Specification) and the [Observations](https://github.com/GenjiruSUchiwa/repo-router/wiki/Observations) it is built from.

## Why

Coding agents burn their context window running `grep` and reading whole files. `rr` aims for the opposite:

- **Minimal context by default** — an answer fits in a few lines; source reading remains a separate, verified capability.
- **Exact before fuzzy** — exact routing (symbol, path) first; lexical ranking (per-field BM25) second; deliberate **abstention** when confidence is low, rather than a plausible-but-wrong answer.
- **Deterministic and incremental** — Git-OID-addressed index, atomic snapshots, fast git-gated `refresh`. No LLM in the retrieval loop.
- **Agent-friendly contracts** — stable dual text/JSON output, committed and greppable `MAP.md`/`SYMBOLS.md`, and `rr init` to install the navigation contract (including a SKILL.md for Claude Code).

## Commands

Shipped:

| Command | Role |
|---|---|
| `rr map` | Rebuild the whole snapshot (gitignore-aware traversal, Tree-sitter, fingerprints) |
| `rr refresh` | Bring the snapshot back into agreement with the repository, git-gated |
| `rr status` | Report how repository and snapshot relate, changing neither |
| `rr query [--json] [--explain] [--source] [--path <PATH>] <QUERY>` | Exact symbol/file query routing (text or versioned JSON v1); `--explain` reports the work the ranker did, `--source` returns the anchor's own verified bytes |
| `rr init [--root <path>] [--json]` | Install the agent navigation contract; safe to run again (shipped by #13, PR #50) |
| `rr version` | Version + build git SHA |

Planned, not yet implemented:

| Command | Role |
|---|---|
| `rr route` | Committable cache of resolved routes |
| `rr impact <sym>` | Change impact radius (transitive callers) |
| `rr check` | Guardrail: index/worktree consistency |


## Languages

`rr` reads a language at one of three tiers. `rr version --languages` prints
this table for the binary you have.

| Tier | What rr extracts |
|---|---|
| `complete` | A hand-written extractor: definitions, references, imports, visibility, test signals |
| `tags` | The grammar's own `tags.scm` plus an imports query: definitions, references, imports |
| `lexical` | Identifiers only |

| Language | Tier | Grammar size |
|---|---|---|
| Rust | `complete` | 1.1 MB |
| Python | `tags` | 0.5 MB |
| TypeScript, TSX | `tags` | 2.9 MB |
| JavaScript, JSX | `tags` | 0.4 MB |
| Go | `tags` | 0.2 MB |
| Java | `tags` | 0.4 MB |
| C | `tags` | 0.6 MB |
| C++ | `tags` | 3.5 MB |
| Ruby | `tags` | 2.1 MB |
| PHP | `tags` | 1.1 MB |
| Swift | `tags` | 3.8 MB |
| C# | `tags` | 6.0 MB |
| Lua | `tags` | 0.05 MB |
| Kotlin, Scala, Zig, Shell, SQL, Proto | `lexical` | — |
| TOML, JSON, YAML, Markdown, HTML, CSS | not indexed | — |

The six `lexical` languages are not an oversight. Kotlin's maintained crate
pins a Tree-sitter runtime this workspace cannot link, and its successor ships
no tags query; Scala, Zig and Bash ship none either, and Scala's grammar
additionally gives an import path no query can reconstruct; SQL has no
canonical crate, only dialect forks; the proto crate's tags query is commented
out upstream. C# stayed lexical until its extractor landed: the published
`tags.scm` ends in a bare `@module` capture `tree-sitter-tags` rejects, so `rr`
ships its own query instead of the grammar's.

Every grammar is compiled in. There is no feature switch. Prebuilt per-platform
release archives are how binary size stays off the user's machine.

## Project status

The V1 plan runs across six milestones — see the [issues](../../issues) and [milestones](../../milestones):

- **M0 Bootstrap** — workspace, CI, hygiene (done)
- **M1 Indexing** — traversal, OID cache, Tree-sitter extraction, fingerprints, snapshot (done)
- **M2 Query** — exact query contract, ranking, verified source, incremental refresh (done)
- **M3 Agent interface** — `MAP.md`/`SYMBOLS.md`, `rr route`, `rr init`
- **M4 Impact & quality** — `impact`, `check`, frozen corpus and benchmarks
- **M5 Multi-language** — the extractor seam, a language-neutral fact vocabulary, a generic `tags.scm` backend, and the languages it carries

## Development

Stable Rust. Three-crate workspace:

```
crates/
  rr-core/   # data model, indexes, ranking
  rr-git/    # traversal, OIDs, git integration
  rr-cli/    # the `rr` binary
```

```sh
cargo build --release   # binary at target/release/rr
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic
cargo fmt --check
```

CI (macOS arm64, Linux x64/arm64) enforces fmt + warning-free pedantic clippy + tests. Releases are automated: conventional commits → [release-plz](https://release-plz.dev) (version, changelog, tag) → multi-platform binaries attached to the GitHub Release.

## License

MIT.
