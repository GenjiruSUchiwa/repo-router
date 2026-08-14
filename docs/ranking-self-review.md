# Self-review — lexical ranking (#8)

Written against the implementation in `crates/rr-core/src/ranking.rs` and its test
suites. Everything above the last section was found by hand, without the
repository's code-review tooling; the last section records what a subsequent
tooling pass added, kept separate so the two provenances stay distinguishable.

## Defects found and fixed

### 1. Every query failed whenever any lexical field was unpopulated

`Scorer::score` computed a length normalizer for all ten fields before scoring,
and `CorpusStats::average_length` returns `None` for a field no symbol populates.
The code treated that `None` as corruption, so `InvalidCorpusStats { "a matched
field has no average length" }` came back for **every** query against **every**
fixture repository — the first end-to-end run of the ranker returned nothing but
errors.

An unpopulated field cannot own a posting list, so its normalizer is never read.
It is now neutral (`1.0`) instead of fatal.

Regression: `ranking_scoring_survives_a_field_no_symbol_populates`.

### 2. `LexicalField::Qualified` was structurally dead

`index/build.rs` emitted the qualified field only when a definition had a
`local_qualified` scope — that is, only for nested definitions. Every top-level
definition, the overwhelming majority in every fixture, carried an empty
qualified field. The field is declared with boost 5.0, the joint-highest after
`name`, so a fifth of the intended ranking signal was silently absent.

Nothing failed. The tests passed, the calibration converged, and the thresholds
looked reasonable — which is exactly why this class of defect is dangerous: a
dead field does not error, it just quietly stops contributing.

Caught by `ranking_scoring_reads_every_declared_field`, which builds one source
populating all ten fields with the same term and scores each field in isolation
through a profile that zeroes the other nine. A field that cannot influence an
answer fails the test by name.

Fixing it moved every score in the corpus, which is why the shipped thresholds
differ from every earlier run.

### 3. Confidence was normalized by the score scale, not the margin scale

`MarginPpm::into_confidence` divided a parts-per-million margin by `SCALE_F64`,
a single constant that stood in for both `SCORE_SCALE` and `MARGIN_SCALE`.
Both are `1_000_000`, so the arithmetic was right by coincidence. Changing
either denominator independently — the only reason to have two named constants —
would have produced a silently wrong public confidence value. Split into
`SCORE_SCALE_F64` and `MARGIN_SCALE_F64`, each used where it belongs, and the
unit test now pins `MARGIN_SCALE_F64` to `MARGIN_SCALE` rather than to a literal.

### 4. A profile could demand a direct answer below its own abstention floor

`RankingProfile::validate` checked each field's parameters and the limits but
never the thresholds against each other. `direct_at_least < none_below` is
unsatisfiable — the decision order abstains before it can ever be read — so such
a profile is a silent misconfiguration, not a stricter one. Now rejected with
`"a direct answer must clear the abstention floor"`.

### 5. Corpus labels described the English sentence, not the query

Not a code defect, but it produced a false failure that took real work to
diagnose: the `http` fold sat at 83 % top-3 recall against a 90 % bar. The cause
was that cases were labelled by what the sentence asks a human, while the ranker
receives what survives normalization. "How do we hash a password key?" reduces to
the single term `key`, which `KeyValueStore` answers honestly — labelling that
case `none` asked the ranker to weigh words it never received. Every label now
describes the normalized query, and the rule is stated in the calibration module
documentation so the next corpus edit does not reintroduce it.

## Scenarios the issue did not consider

- **A field no symbol populates.** Defect 1 above. The issue specifies
  field-local statistics without saying what a field-local population of zero
  means.
- **A repository with nothing to rank.** An index containing no symbol at all —
  `rr map` in a repository with no supported sources. The ranker abstains rather
  than failing (`ranking_scoring_abstains_on_a_repository_with_nothing_to_rank`).
- **A corrupt index reaching the scorer.** Statistics that disagree with the
  postings must be reported, never scored into a confident wrong answer, since a
  confident wrong anchor is the single worst output this tool can produce
  (`ranking_scoring_reports_corrupt_corpus_statistics`).
- **Cross-validation requires every class in every fold.** The issue asks for
  held-out folds without saying that a difficulty class present in only one
  repository cannot be cross-validated: the fold holding that repository out fits
  thresholds blind to the class, then is scored on it.
- **The issue's own tie-break breaks its own acceptance criterion.** "Choose the
  largest value" always lands exactly on a case the corpus contains, so a
  held-out repository scoring a hair below loses every answer. Under that rule
  the `geometry` fold abstains on all three of its answerable cases and misses
  the issue's ≥90 % top-3 recall bar. The midpoint rule is documented as a
  deliberate deviation, with the issue's rule retained as fallback.
- **The decision boundaries' inclusivity is unstated.** `none_below` is
  exclusive, both direct thresholds are inclusive. Pinned by
  `ranking_decision_boundaries_are_exactly_inclusive` so a refactor cannot flip
  a boundary invisibly.
- **The builder's input order.** The snapshot must not depend on the order files
  are handed to it, or the same repository indexed twice ranks differently
  (`ranking_scoring_ignores_the_order_files_are_indexed`).

## Known limits, not fixed here

- **The corpus is small and self-authored.** 35 cases over five hand-written
  repositories, labelled by the same author as the fixtures. Perfect scores on
  every fold measure internal consistency, not accuracy — and mean the corpus is
  currently too easy to distinguish a good calibration from a lucky one. Highest
  value follow-up: more cases, from repositories nobody involved wrote.
- **Absolute thresholds are corpus-size sensitive.** IDF grows with the field
  population, so a four-symbol repository scores well under `none_below` and
  abstains on nearly everything. Honest, but a tiny repository will feel mute.
  A size-normalized decision statistic would fix it.
- **The candidate cap can drop the true top-scorer.** Retention orders by
  `(rarest_df, matched_terms, SymbolId)`, not by score. Inherent to the issue's
  bounded design. Measured and surfaced afterwards — see the closing sections.
- **Scoring dominates the cost, not the merge.** ~197 µs for 12 000 postings is
  64 candidates × 5 terms × 10 fields of binary search back into the posting
  lists. Carrying each stream's cursor into scoring, or scoring inside the merge,
  would cut it substantially — a separate, benchmark-driven change.

## Code quality

**Single responsibility.** The module separates cleanly: fixed-point types,
profile and statistics, scratch space, merge, scoring, decision. `rank` generates
and orders; `decide` maps to the public vocabulary; `route_lexical` composes the
two. Nothing in `decide` can change a score and nothing in `rank` can read a
threshold — which is what lets calibration explore thresholds against one frozen
snapshot, and why the scoring digest deliberately excludes them.

**Don't repeat yourself.** The counting allocator was duplicated between the
allocation test and the benchmark; it now lives in `tests/support/counting_alloc.rs`
and both include it via `#[path]`, so the asserted zero and the reported figure
come from one counter. Fixture loading is shared through `tests/support/mod.rs`,
so a fixture edit moves the goldens, the calibration report, and the benchmark
together instead of one of them. The calibration's `classify` deliberately
restates `decide` rather than calling it — a replay over pre-ranked observations
is what makes the threshold search finite — and
`ranking_calibration_matches_the_shipped_decision` pins the two together so the
restatement cannot drift.

**Principle of least surprise.** Errors describe what was violated rather than
where. Truncation is never silent: `candidates_dropped` is part of the evidence.
`u128` margin arithmetic and `u64` fixed-point scores mean no comparison depends
on float ordering. A snapshot carries the profile digest it was built under, so
it can never be read through a different scoring profile.

**Nomenclature.** `reserve_exact` was renamed `clear_and_reserve`: it cleared its
buffer and called `Vec::reserve`, not `reserve_exact`, so the name asserted an
allocation guarantee the code did not make. `Retained`, `Stream`, `StreamHead`,
`RankingScratch`, `MarginPpm`, `Score` all name what they are. Test names state
the claim they verify, so a failure reads as a sentence about the system
(`ranking_scoring_reads_every_declared_field`, not `test_fields`).

**Readability.** Per the working instruction, there are no inline comments: doc
comments carry the reasoning at the item level, and assertions carry it inside
test bodies as failure messages, where it appears exactly when it is needed. The
one remaining `#[allow]` in the scorer sits directly under the bound that proves
the cast exact.

The weakest remaining spot is `ranking_calibration.rs`. At roughly 850 lines it
holds the corpus loader, the observation pass, two threshold fits, the metrics,
the JSON report, and the tests. It reads in order and each piece is small, but if
it grows again it should be split into a `calibration/` support module with the
tests kept separate.

## What the tooling pass added afterwards

A `/code-review high --fix` run over the whole branch found no defect producing a
wrong or panicking answer on a valid index. It did find four things worth fixing,
none of which the hand review above had caught:

1. **`Ord for Retained` documented the opposite of what it implements.** The
   comment claimed the ordering was the *exact reversal* of the retention key,
   worst first. It is the retention key itself, best first; correctness rests on
   `retain` evicting the max-heap root, which is the worst member kept. The
   comment was the trap, not the code: a maintainer trusting it would reverse the
   comparison and the cap would keep the 64 *least* promising union members with
   nothing failing. Reworded, and the unit test renamed to
   `ranking_retention_key_orders_best_first` so the name states the real claim.
2. **`decide` never validated its profile.** It is `pub` and re-exported, so a
   caller could hand it a `result_limit` above `RESULT_LIMIT` and get a candidate
   list the v1 contract cannot carry. Now validates first, pinned by
   `ranking_decide_rejects_an_invalid_profile`.
3. **A query-side defect was reported as index corruption.** Duplicated query
   terms returned `InvalidPostings { "corrupt postings" }`, which sends an
   operator to re-map their repository over a fault in the caller. Now
   `RankingError::InvalidQuery`.
4. **`compute_corpus_stats` walked the symbol arena ten times.** One pass per
   field, run on every snapshot load through `Snapshot::validate`. Now one pass
   accumulating all ten field counters.

The threshold-consistency check from defect 4 above also had no test of its own —
`ranking_profile_validation_rejects_every_invalid_parameter` covered seven of
`validate`'s eight rules. It now covers all eight.

One finding was left unfixed on that branch: `route_query` discarded the
`RankingEvidence` carrying `candidates_dropped`, so cap truncation was observable
inside `rr-core` but silent end to end. Surfacing it extends the v1 output
contract, which is a decision for the contract rather than a fix for a ranking
branch. The next section is that decision.

## Making truncation observable, and what measuring it showed

Following the finding up produced a better one. `candidates_dropped` is the wrong
number to surface: on the 10 001-symbol benchmark corpus, four of the five query
shapes discard more than 1 900 union members each. Truncation is the normal case,
not the exception, so a per-answer "truncated" flag would be true almost always
and inform nobody.

What actually threatens an answer is narrower. The retention key ranks on
`(rarest_df, matched_terms)` and breaks the remaining ties on `SymbolId` — the
order the files happened to be indexed in, which is not evidence about anything.
When hundreds of union members share a document frequency and a match count, the
cap keeps sixty-four of them by file order, and a discarded member could have
outscored every candidate that survived. That is the case worth reporting, and it
is not the same as "many were dropped":

| benchmark query | dropped | arbitrary cut |
|---|---|---|
| `rare_term` | 0 | no |
| `mixed_terms` | 3 736 | **yes** |
| `ubiquitous_term` | 9 937 | **yes** |
| `cap_overflow` | 1 936 | **yes** |
| `eight_terms` | 5 537 | **yes** |

So `RankingEvidence::cap_cut_a_tie` records whether the cap fell inside a group it
could not tell apart. It is decided exactly while holding a single extra member:
every discarded member ranks at or below the retention root of the moment it was
discarded, and that root only improves, so the best discarded member ties the
final root exactly when any discarded member does. The proof is in the
`merge_candidates` documentation, since it is the reason one `Retained` is enough.

`route_query` now returns the evidence alongside the result — a Rust API change
with one caller and no effect on either output contract — and `rr query --explain`
renders it. Default output is unchanged byte for byte, pinned by
`query_contract_default_output_is_unchanged_by_the_explain_flag`. In text mode the
diagnostic precedes the answer so the anchor stays the last line; in JSON it is
one added member, `null` for an exact route, which reads no posting list and so
has no cap to explain.

`docs/query.schema.json` gained the optional member. Nothing compared the schema
to the renderer before — a silent drift waiting to happen — so
`query_contract_json_carries_exactly_the_members_the_schema_declares` now reads
the published file and checks all three result shapes against it.
