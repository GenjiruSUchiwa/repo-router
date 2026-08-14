---
title: "M3-12 · Committable cache of resolved routes (`rr route`)"
labels: ["milestone:M3", "type:agent-interface", "differentiator"]
---

## Why
Radar's best idea (auto-bootstrapped ROUTES.md + `route add`) is locked
inside a gitignored directory: every developer relearns everything.
Approach change #3: turn it into a **versioned team memory**. Six months
of real questions → verified answers, cleanly invalidated by api_hash.

## What
Committed `.rr/ROUTES.md` + `rr route add|find|list` subcommands +
automatic consultation by `rr query` before ranking.

## How
1. Line format (stable, sorted by keywords):
   `[state] keywords | file#symbol | api_hash | hits`
   states: `auto` (seeded by rr from symbol names), `ok` (validated —
   added by an agent/human and anchor verified), `stale` (api_hash ≠ current).
2. `rr route add "<task>" <file#symbol>`: normalizes the task (issue 05),
   VERIFIES that the anchor exists in the current index (refusal otherwise —
   no pollution), writes `[ok]`, sorts the file.
3. `rr query`: before the lexical pipeline, look for a strong overlap
   (≥ 2/3 of the query terms) with a fresh `[ok]` route → direct answer
   marked `(from route cache)` in the JSON, `hits += 1`.
   `[auto]` routes do not short-circuit the ranking (they only boost,
   +2.0 to the score); `stale` ones are ignored.
4. `rr refresh` re-marks the states according to the current api_hash and
   re-seeds the missing `[auto]` entries. Never auto-delete an `[ok]`
   (even stale: a human removes it or re-validates it).
5. Local vs committed policy: everything in the same committed file —
   simplicity first; if the diff noise becomes a problem, we split later.

## Best practices
- The file remains readable AND hand-editable: that is a feature, not an
  internal format (PR review of routes added by agents!).
- Systematic deterministic sort after every write.

## Acceptance criteria
- [ ] `route add` with a nonexistent anchor → refusal, exit 1.
- [ ] Query matching an `[ok]` route → answer < 5 ms, hits incremented.
- [ ] API change → route becomes `[stale]`, ignored by query.
- [ ] File diff-stable after repeated operations.
