---
title: "M4-14 · `rr impact`, `rr check`, frozen corpus and benchmarks"
labels: ["milestone:M4", "type:core", "quality"]
---

## Why
V1 closure: the "impact" value (the real day-to-day agent differentiator),
the `check` guardrail, and the measurement infrastructure without which no
ranking iteration is reliable (lesson from Radar's abstention rollback).

## What
Three blocks deliverable separately (a, b, c) in this order.

## How
### a) `rr impact` (on a Git change-set)
1. Delta: `gix diff HEAD..worktree` (or `--base <ref>`), files → symbols
   whose span intersects the hunks = "changed definitions".
2. Direct edges from the index (issue 06): incoming calls (callers),
   incoming imports (dependents), at depth 2 max (`--depth`).
3. Probable tests: (i) test files referencing the changed symbol's name
   (lexical, assumed), (ii) Git co-change: files committed together with
   the changed file in > 30% of its last 50 commits (gix log, computed
   on the fly, cached by HEAD).
4. Text output in the style of observed Radar (changed/edges/callers/tests
   sections) + `unresolved/ambiguous` counters displayed honestly + `--json`.

### b) `rr check`
Invariants: snapshot readable and at the right version; MAP.md present and
api_hash consistent; ROUTES.md parsable, `[ok]` anchors existing; MAP
budget respected. Exit codes: 0 ok, 1 warnings (stale), 2 invariants
violated, 3 snapshot missing. To be wired as a CI hook of the repo itself.

### c) Frozen corpus + bench
1. `fixtures/corpus/`: 3 real vendored repositories, frozen (small/medium/large
   Rust) + `queries.yaml` extended to 40 questions (like Radar) with
   hand-verified expected anchors.
2. `cargo test --release corpus`: top-3 ≥ 36/40, wrong directs = 0 (blocking).
3. Criterion: cold/warm map, query p50/p95. Radar's published ~28.74 ms/<30 ms
   figure is directional context, not an acceptance criterion for this
   workload — SPEC.md:73 forbids hard-coding a published benchmark claim as a
   guarantee, and the corpus here is not the corpus that figure was measured
   on. The gate is the regression rule in the GitHub issue body: a point
   estimate >10% slower whose bootstrap 95% lower bound is >5% slower.
4. The numbers go into `BENCHMARKS.md` with the exact command to reproduce
   them — never a number without its command.

## Best practices
- Impact never invents an edge: what is not resolved is counted, not
  guessed (spec principle §10.6, validated by the observation).
- The corpus is FROZEN: it is only touched via a dedicated PR explaining why.

## Acceptance criteria
- [ ] On the fixture: editing `verify_token` → `main` as caller,
      `users.rs` as dependent, the lexically related test listed (where Radar failed).
- [ ] `rr check` detects a corrupted ROUTES.md (exit 2).
- [ ] 40-question corpus: thresholds held, report printed.
- [ ] BENCHMARKS.md generated with reproducible commands.
