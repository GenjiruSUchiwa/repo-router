---
title: "M1-02 · Gitignore-aware traversal and file classification"
labels: ["milestone:M1", "type:core"]
---

## Why
Indexing `node_modules/` or `target/` destroys both relevance and speed.
The traversal is the foundation of the `map` pipeline: it decides what exists.

## What
A `rr-core::walk` module that produces the list of candidate source files,
with detected language and a `generated` flag.

## How
1. Use the `ignore` crate (ripgrep's engine): respects `.gitignore`,
   `.ignore`, global exclusions — do not rewrite this logic.
2. Hard-coded default exclusions: `.git/`, `.rr/`, `node_modules/`, `target/`,
   `dist/`, `build/`, `.venv/`, `vendor/` (overridable via `rr.toml` later).
3. Language detection by extension first (`.rs`, `.py`, `.ts`, `.tsx`);
   extensible table, no heavy content-based detection crate in V1.
4. `generated` heuristic: path contains `generated`/`.pb.`/`_pb2.py`,
   or first line contains `@generated` / `DO NOT EDIT`. Generated files
   are indexed but penalized at ranking time (issue 08).
5. Parallelism: `ignore::WalkBuilder::build_parallel()` with a crossbeam
   channel to the collector.

## Pseudo-code
```rust
pub struct SourceFile { pub path: RelPath, pub lang: Lang, pub generated: bool }

pub fn discover(root: &Path, cfg: &WalkCfg) -> Vec<SourceFile> {
    WalkBuilder::new(root)
        .standard_filters(true)          // gitignore, hidden, etc.
        .add_custom_ignore_rules(DEFAULT_EXCLUDES)
        .build_parallel()
        .collect_filter_map(|entry| classify(entry, cfg))
}
```

## Best practices
- Paths **relative to the repo root**, normalized with `/`, from this layer
  onward — the rest of the system never sees an absolute path (snapshot
  determinism across machines).
- Sort the final output by path: deterministic order regardless of
  parallelism (spec requirement §5.3).

## Acceptance criteria
- [ ] On the fixture, discovers exactly the expected files, in the same order on every run.
- [ ] A pattern added to `.gitignore` immediately excludes the file.
- [ ] Test: repo with a cyclic symlink → no infinite loop.
