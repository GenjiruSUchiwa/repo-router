---
title: "M1-03 · Git OID addressing and shareable facts cache"
labels: ["milestone:M1", "type:core", "differentiator"]
---

## Why
This is approach change #1 versus Radar. Git already computes a content
hash (blob OID) for every tracked file. Used as a cache key, the OID makes
parsed facts **shareable via a Git ref**: CI indexes once, the team clones
a warm index. Radar (local BLAKE3) cannot do that.

## What
`rr-git::oid` (OID computation/lookup) and `rr-core::cache` (key → value
facts store, local first, Git ref later).

## How
1. With `gitoxide` (the `gix` crate): for an **unmodified** file in the
   working tree, read the OID directly from the Git index (free, zero
   content reads). For a modified/untracked file, hash in memory using the
   Git object format (`blob <len>\0<bytes>`, SHA-1 or SHA-256 depending on the repo).
2. Full cache key: `(oid, lang, EXTRACTOR_VERSION, FACT_SCHEMA_VERSION)`.
   Bumping the extractor version ⇒ natural invalidation, no migration code.
3. Local store V1: files `.rr/local/facts/<aa>/<oid>.bin` (bincode or
   postcard), written via temporary file + rename (atomicity).
4. V1.5 (separate issue if needed): `rr cache push` / `rr cache pull` which
   serializes the facts into a blob attached to `refs/rr/facts` — does not
   block M1, but the OID key must be in place from now on.
5. Repo without Git: fallback custom hash in the same format, `no_git` flag
   in the snapshot (sharing is simply unavailable).

## Pseudo-code
```rust
fn facts_for(file: &SourceFile, repo: &GitRepo, cache: &FactCache) -> Facts {
    let oid = repo.oid_of(file)          // Git index if clean
        .unwrap_or_else(|| hash_as_git_blob(read(file)));
    let key = CacheKey { oid, lang: file.lang, ext: EXTRACTOR_VERSION };
    cache.get(&key).unwrap_or_else(|| {
        let facts = extract(file);        // issue 04
        cache.put(&key, &facts);
        facts
    })
}
```

## Best practices
- Never put file content in the cache — only facts.
- Measure and log (`--verbose`) the cache hit rate: it is THE health
  metric of incrementality.

## Acceptance criteria
- [ ] Second `rr map` with no modifications: 100% cache hits, 0 parses.
- [ ] `git mv` of an unmodified file: cache hit (same OID).
- [ ] Editing a file: only that file is re-parsed.
- [ ] Works in a directory without `.git`.
