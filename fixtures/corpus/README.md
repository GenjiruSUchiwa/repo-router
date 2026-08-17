# The frozen corpus

This directory is the evidence a release decision is argued from. It is **frozen**:
it changes only through a dedicated `corpus-update` pull request that says why, and
ordinary verification can never rewrite it.

Nothing here is vendored yet. `manifest.json` is absent, and while it is absent
`corpus_gate` skips with

```text
fixtures/corpus/manifest.json absent; see the corpus-update PR
```

That is deliberate and it is not a to-do left lying around. Copying three upstream
repositories needs network access and a redistribution decision — two things a
test suite must never take on itself — while corpus tests have to run offline and
byte-identically. So this branch ships the whole machine and none of the payload:
the manifest schema, the verifier, the generator, the oracle and golden formats,
the shard assignment, the aggregation, the gates, and the harness that drives the
compiled `rr` binary. Vendoring is the human step described below.

## What lives here

| Path | What it is |
|---|---|
| `manifest.json` | the lock: every byte of the corpus, checksummed. Absent until the corpus is vendored |
| `manifest.template.json` | the shape of that lock, ready to fill in |
| `repos/` | the three vendored repositories, one directory each |
| `licenses/` | the licence files copied beside them |
| `queries.json` | the oracle: 40 answerable questions plus at least 8 must-abstain cases |
| `impact-cases.json` | `rr impact` goldens |
| `check-cases.json` | `rr check` goldens |
| `adversarial/` | fixtures that must fail in bounded ways rather than panic or escape |
| `baselines/` | committed performance baselines, one per platform |

Generated evidence never lives here. It goes to `target/rr-quality/`, which is not
committed, because a report produced by a run is not an input to the next one.

## The generated repository is not in this tree

`repos/synthetic-10k/` is the 10,000-file performance repository, and it is
produced from two numbers — a generator version and a seed — by
`rr_core::quality::generate_synthetic`, into a scratch directory. It is not
committed, and the manifest locks `rr_core::quality::synthetic_digest` instead of
ten thousand per-file checksums.

Committing it would create a second copy of the generator's output beside the
generator that claims to produce it, and the two can disagree. Locking the digest
makes that disagreement the thing that fails.

## Licences

A vendored repository is declared with an SPDX expression from
`rr_core::quality::REDISTRIBUTABLE_SPDX` and at least one copied licence file
under `licenses/`. Both are checked. Anything else — an unlisted expression, a
missing licence file, a licence file whose bytes drifted — is
`RR0502_CORPUS_LICENSE_MISMATCH`.

The list is closed and is not a parser. Whether an arbitrary SPDX expression
permits redistribution is a legal question, and a checker that answered it from a
grammar would answer it wrongly and silently. Adding an expression to that list is
a human decision recorded in code.

## Vendoring: the human step

1. Choose three redistribution-compatible Rust repositories: small, medium, large.
   Note each one's immutable commit — a tag is not immutable.
2. Copy each working tree into `repos/<id>/`, without its `.git` directory. Copy
   its licence file(s) into `licenses/`.
3. Delete the `.gitkeep` placeholders from any directory that now holds real
   content. A stray file under `repos/` belongs to no repository entry, so the
   verifier reports it as an undeclared file — which is the right answer.
4. Copy `manifest.template.json` to `manifest.json` and fill in, for each
   repository: `id`, `tier`, the upstream URL, the commit, the retrieval date and
   the SPDX expression. Leave every `bytes` at `0` and every `digest` at its
   placeholder; step 5 computes them.
5. Regenerate the locks. This is the only mode that may write here:

   ```sh
   RR_ACCEPT_CORPUS_UPDATE=1 cargo test --release -p rr-cli --test corpus \
       -- --ignored --exact corpus_relock
   ```

   Without `RR_ACCEPT_CORPUS_UPDATE=1` the same command reports the drift and
   changes nothing. That asymmetry is the point: a harness that repaired its own
   expectations would report a clean run on exactly the change the corpus exists
   to catch.
6. Write the oracle and the goldens. `queries.json` needs 40 answerable questions
   spanning aliases, exact and qualified identifiers, duplicates, tests, and
   nested or moved definitions, each with its accepted **and** forbidden anchors
   and whether a single committed anchor is permitted; plus at least eight
   must-abstain cases. The must-abstain cases never enter the answerable
   denominator.
7. Re-run step 5, then run the gate on every shard:

   ```sh
   for shard in 0 1 2 3; do
     RR_CORPUS_SHARD=$shard/4 cargo test --release -p rr-cli --test corpus \
         -- --ignored --exact corpus_gate
   done
   ```

8. Open the pull request with the `corpus-update` label, quality CODEOWNER review,
   the provenance and licence evidence, the regenerated locks, and the before and
   after quality and performance figures.

## What the gates are

Blocking: top-three recall at least 36 of 40, zero false directs, direct precision
exactly one, required-abstention accuracy exactly one, every golden, corruption,
staleness and adversarial case passing, and — on one pinned platform only — a
performance decision that is not a regression.

Reported and not gated: top-one recall, the abstention rate, and the unresolved
count per repository beside its language mix. Top-one has one exception: it may
not lose more than one case against the approved baseline.

A performance regression blocks only when the point estimate is more than 10%
slower **and** the lower bound of the bootstrap 95% relative-change interval is
more than 5% slower. There is no absolute latency threshold anywhere in this
corpus, and `rr_core::quality::PerfDecision` has no field one could be written in.
A measurement is a fact about one machine, one corpus and one afternoon; the only
claim this evidence supports is "slower than we were".

Missing evidence is failure. A shard that produced no verdict blocks the release
rather than passing it.
