---
title: "M1-06 · Exact index + lexical postings + atomic snapshot"
labels: ["milestone:M1", "type:core"]
---

## Why
Turns the facts into queryable structures. Closes milestone M1:
`rr map` becomes usable end to end.

## What
`rr-core::index`: in-memory construction, serialization to `.rr/local/snapshot.bin`,
and the `rr map` CLI command that orchestrates issues 02→06.

## How
1. Structures (u32 IDs everywhere, cf. spec §8):
   - `exact: HashMap<TermId /*exact name*/, SmallVec<SymbolId>>`
   - `qualified: HashMap<TermId, SmallVec<SymbolId>>`
   - `postings: HashMap<TermId, RoaringBitmap /*SymbolId*/>` per field
     (name / path / signature / body / callees) — 5 maps, not a map of maps.
   - `files: Vec<FileRecord>`, `symbols: Vec<SymbolRecord>` (arena, index = ID).
2. Cheap relation resolution: a call `foo()` is resolved iff exactly one
   symbol named `foo` exists in the same file or module; otherwise it stays
   an unresolved name + counter (displayed by `rr map --verbose`).
3. Serialization: `postcard` or `bincode` + header
   `{ magic, SCHEMA_VERSION, repo_head_oid, created_at }`. Different version
   at load time ⇒ silent rebuild, never a migration.
4. Atomic write: temp file in the same directory + `rename` (spec §10.8).
5. `rr map`: one-line output, in the style of observed Radar:
   `rr map — 42 files, 310 symbols, 12 unresolved refs, 38 ms (cache 95%)`.

## Pseudo-code
```rust
fn build(root: &Path) -> Snapshot {
    let files = discover(root, &cfg);                     // 02
    let facts: Vec<_> = files.par_iter()
        .map(|f| (f, facts_for(f, &repo, &cache)))        // 03+04
        .collect();
    let mut ix = Index::default();
    for (f, facts) in facts { ix.add(f, facts, &mut interner /*05*/); }
    ix.resolve_unambiguous();
    ix.freeze_sorted()          // deterministic sort of all postings
}
```

## Best practices
- `freeze_sorted()` sorts every SmallVec/bitmap: two builds of the same
  tree yield **byte-identical** snapshots (golden determinism test).
- Memory budget: no duplicated String, everything goes through the interner.

## Acceptance criteria
- [ ] `rr map` twice in a row → byte-identical snapshots.
- [ ] Fixture snapshot < 50 KB.
- [ ] Bumped schema version ⇒ auto rebuild without error.
- [ ] 10,000 generated files (script provided): cold map < 5 s, warm < 300 ms.
