---
title: "M2-08 · Per-field BM25 ranking + calibrated abstention thresholds"
labels: ["milestone:M2", "type:core", "hard"]
---

## Why
The hardest part of the project. Radar published an abstention calibration
failure (4 wrong anchors out of 9 after a calibration that seemed safe)
and rolled back. We take the problem seriously from the design stage:
**no threshold without a frozen test corpus** (issue 14 provides the
harness; a 20-question mini-corpus is created right here).

## What
Lexical pipeline for queries with no exact identifier: candidates → BM25F
score → direct/candidates/none decision.

## How
1. Candidates: union of the postings of the query terms (all fields),
   capped at 64 by ascending doc-frequency (rare terms first).
2. BM25F score over synthetic documents per symbol (the fingerprints,
   NOT the source file — spec §11.4), weighted fields:
   name 8, qualified 5, path 5, signature 4, callees 3, body 1.5.
   ×0.5 penalty if `generated`. Slight bonus if kind ∈ {fn, method} when
   the query contains a verb.
3. Decision (initial values, to be recalibrated on the corpus, never
   hard-coded anywhere but `ranking.rs::THRESHOLDS`):
   - direct iff `score[0] > T_abs` AND `score[0]/score[1] > 1.6`;
   - otherwise top-3 candidates;
   - none if `score[0] < T_min`.
   The top1/top2 ratio is more robust than the absolute score — it is the
   margin that predicts correctness, not the magnitude.
4. Final deterministic tie-break: (score desc, SymbolId asc). Two runs =
   same output, always.
5. Mini-corpus `fixtures/queries.yaml`: 20 questions → expected anchor.
   `cargo test ranking_corpus` fails if top-3 < 18/20 or if a direct is wrong.
   **A wrong direct is worse than an abstention**: the test weights accordingly.

## Pseudo-code
```rust
let mut scored: Vec<_> = candidates(q, 64)
    .map(|s| (bm25f(q, s, &weights), s))
    .collect();
scored.sort_by(|a,b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
match decide(&scored, &THRESHOLDS) { Direct(..) | Candidates(..) | None }
```

## Best practices
- Every weight change = a dedicated commit with the corpus score diff in
  the message. The Git history becomes the calibration journal.
- `rr query --explain` (hidden flag): prints the features per candidate —
  indispensable for debugging the ranking without guessing.

## Acceptance criteria
- [ ] 20-question corpus: ≥ 18 top-3, 0 wrong directs.
- [ ] "where is token verification handled?" → direct `verify_token`.
- [ ] "security logic" (out of vocabulary) → candidates or none, NEVER a wrong direct.
- [ ] Determinism: 100 runs, identical outputs.
