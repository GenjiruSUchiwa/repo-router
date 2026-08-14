# repo-router

**`rr`** — a repository navigator for coding agents (Claude Code, etc.), inspired by the publicly documented behavior of [Radar](https://radar.dev). It indexes a repository (Tree-sitter + lexical fingerprints), then answers navigation questions ("where is X defined?", "who calls Y?") with **minimal context**: a precise route to the source, not a file dump.

> An open, cross-platform reimplementation written from Radar's publicly documented and observed behavior — not from its source code, which remains private. See [`docs/SPEC.md`](docs/SPEC.md) and [`docs/OBSERVATIONS.md`](docs/OBSERVATIONS.md).

## Why

Coding agents burn their context window running `grep` and reading whole files. `rr` aims for the opposite:

- **Minimal context by default** — an answer fits in a few lines; full source is only read on demand (`--source`), bounded and hash-verified.
- **Exact before fuzzy** — exact routing (symbol, path) first; lexical ranking (per-field BM25) second; deliberate **abstention** when confidence is low, rather than a plausible-but-wrong answer.
- **Deterministic and incremental** — Git-OID-addressed index, atomic snapshots, fast git-gated `refresh`. No LLM in the retrieval loop.
- **Agent-friendly contracts** — stable dual text/JSON output, committed and greppable `MAP.md`/`SYMBOLS.md`, and `rr init` to install the navigation contract (including a SKILL.md for Claude Code).

## Commands (V1 targets)

| Command | Role |
|---|---|
| `rr map` | Index the repository (gitignore-aware traversal, Tree-sitter, fingerprints) |
| `rr query <q>` | Answer: definitions, references, imports — text or `--json` |
| `rr query --source` | Return the exact source span, hash-verified, refused if stale |
| `rr refresh` | Update the index incrementally (git-gated) |
| `rr route` | Committable cache of resolved routes |
| `rr impact <sym>` | Change impact radius (transitive callers) |
| `rr check` | Guardrail: index/worktree consistency |
| `rr init` | Install the navigation contract into the repository |
| `rr version` | Version + build git SHA (implemented) |

## Project status

Bootstrap phase. The V1 plan is **14 issues across 5 milestones** — see the [issues](../../issues) and [milestones](../../milestones):

- **M0 Bootstrap** — workspace, CI, hygiene (done)
- **M1 Indexing** — traversal, OID cache, Tree-sitter (Rust first), fingerprints, snapshot
- **M2 Query** — `query`, ranking + abstention, `--source`, `refresh`
- **M3 Agent interface** — `MAP.md`/`SYMBOLS.md`, `rr route`, `rr init`
- **M4 Impact & quality** — `impact`, `check`, frozen corpus and benchmarks

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
