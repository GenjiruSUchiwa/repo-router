---
title: "M0-01 · Bootstrap the Cargo workspace, CI, and basic hygiene"
labels: ["milestone:M0", "type:infra"]
---

## Why
Everything else builds on this. A clean workspace from the start avoids
structural refactorings in the middle of development, and a CI that compiles
on all three targets guarantees we don't discover portability problems
(our argument #1: macOS ARM64) at release time.

## What
Create the Cargo monorepo with empty but compilable crates, linting, and CI.

```text
repo-router/
├── Cargo.toml            # workspace
├── crates/
│   ├── rr-cli/           # `rr` binary (clap)
│   ├── rr-core/          # parser / facts / index / query / verify / cache
│   └── rr-git/           # OIDs, refs, diff (gitoxide)
├── fixtures/             # test repositories (issue 14 will freeze them)
└── benches/
```

## How
1. `cargo new --lib crates/rr-core`, `crates/rr-git`; `cargo new crates/rr-cli`.
2. Workspace `Cargo.toml`: `resolver = "2"`, release profile `lto = true`,
   `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"`.
3. `rr-cli`: clap v4 in derive mode, an `rr version` command that prints
   version + compiled git sha (via `build.rs` + `vergen` or a simple equivalent).
4. GitHub Actions CI: matrix `macos-14` (ARM), `ubuntu-latest` (x86_64),
   `ubuntu-24.04-arm`; steps `cargo fmt --check`, `cargo clippy -- -D warnings`,
   `cargo test`, `cargo build --release`.
5. Handle SIGPIPE properly right away (lesson from observation §9.6):
   in `main()`, reset SIGPIPE to `SIG_DFL` before any print
   (`libc` crate, 3 lines, cfg(unix)) — otherwise `rr ... | head` will panic.

## Best practices
- `#![deny(unsafe_code)]` in rr-core (any unsafe will live in rr-git).
- Errors: `thiserror` in the libs, `anyhow` only in rr-cli.
- All user-facing output goes through a single `output.rs` layer in rr-cli
  (this prepares the dual text/JSON contract of issue 07).

## Acceptance criteria
- [ ] `cargo build --release` green on all 3 targets in CI.
- [ ] `rr version` prints `rr X.Y.Z (<sha>)`.
- [ ] `rr version | head -0` does not panic.
- [ ] clippy pedantic enabled with no warnings.
