---
title: "M2-10 · `rr refresh`: git-gated fast path"
labels: ["milestone:M2", "type:core"]
---

## Why
Incrementality is what makes the tool invocable in a loop by an agent
without friction. Radar has it (`git-gated fast path`); ours is simpler
thanks to the OID cache (issue 03) which already does 90% of the work.

## What
`rr refresh`: detect the delta, re-parse the minimum, rewrite the snapshot.

## How
1. Fast path: compare `snapshot.repo_head_oid` + working tree status
   (gix status). If HEAD is identical and the tree is clean → "0 changed",
   exit 0, without touching the snapshot.
2. Delta: files modified/added/deleted/renamed since the snapshot
   (gix status + comparison against the snapshot's file list).
3. Rebuild the index FROM the facts cache: only the delta's files go
   through the parser; all others are cache hits.
   V1: full in-memory rebuild of the postings (fast); do NOT attempt
   in-place bitmap updates — unjustified complexity as long as the warm
   rebuild is < 300 ms on 10,000 files (measured in 06).
4. Output: `rr refresh — 1 reparsed, 41 cached, snapshot updated (12 ms)`.
5. `rr status` (10 lines of code once refresh is done): one line
   `git: dirty @ <sha> · snapshot: fresh|stale (N files) · unresolved: 12`.

## Best practices
- Refresh is idempotent and safe: interrupted at any point, the previous
  snapshot remains valid (atomic write from 06).
- The agent must NEVER have to decide between map and refresh:
  refresh does the right thing, map = alias forcing a full rebuild.

## Acceptance criteria
- [ ] Clean tree, unchanged HEAD: refresh < 5 ms, snapshot untouched (mtime).
- [ ] 1 edited file: exactly 1 re-parse.
- [ ] Deleting a file: its symbols disappear from the results.
- [ ] `rr status` correctly reflects clean/dirty/stale.
