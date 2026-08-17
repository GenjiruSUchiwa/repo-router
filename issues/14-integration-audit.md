# M4-14 — integration audit

Line-by-line pass over the issue's *Review rejection checklist* against the
merged branch `feat/14-impact-check-corpus`, plus the two items the slice agents
flagged for a human decision.

## The two flagged items

### C re-expressed the snapshot envelope in `cochange.rs`

**Decided: mutualised, with one magic per format.**

C's own justification had a hole. It shared `SNAPSHOT_MAGIC` and argued the two
files were told apart by their version words — but `SNAPSHOT_SCHEMA_VERSION` is
12 and rising while `COCHANGE_CONFIG_VERSION` is 1 and rising, counted
independently. The day they collide, a valid `cochange.bin` passes the
snapshot's magic *and* version check and is refused only by the payload decoder
— which is `rr check` reporting `RR0002_SNAPSHOT_CORRUPT` over a file that is
not a snapshot and is not corrupt.

- `crates/rr-core/src/envelope.rs` (new) owns the framing: magic (8) + version
  word (4) + payload length (8) + BLAKE3 over the payload (32). `wrap` and
  `unwrap` take the magic **from the caller**, so two formats can never occupy
  one namespace. `Framing` separates `LengthMismatch` from `TrailingBytes`
  because a caller that reports cares about the difference.
- `crates/rr-core/src/snapshot.rs` delegates. `SNAPSHOT_MAGIC` is unchanged, the
  order of checks is unchanged, and every `RebuildReason` maps one-to-one, so
  the bytes on disk and the diagnostics `rr check` prints are identical.
- `crates/rr-git/src/cochange.rs` declares `COCHANGE_MAGIC = *b"RRCOC\0\0\0"`
  and pins it: `the_cache_does_not_share_the_snapshot_magic`.

### B's `SchemaStamp` / `carries_over` are dead

**Decided: removed, and the reasoning moved to where the gate actually is.**

Not merely unused — redundant. All four agreements they restated are already
enforced upstream of any `FileInput`: `CacheKey { oid, lang, extractor, schema }`
makes facts read under any other combination a cache miss, and the snapshot
schema is proven by the envelope the loader decoded, which refuses another
version outright. A second stamp is a second place to bump a version, and the
failure that produces is one copy staying behind — a check that reads as a
guarantee while agreeing with nothing.

Removed from `impact.rs` and from the two `lib.rs` re-export lines; the
four-agreements paragraph now sits on `overlay`'s doc comment naming `CacheKey`
and `crate::envelope` as the owners.

## Rejection checklist

| # | Reject if the implementation… | Verdict | Evidence |
|---|---|---|---|
| 1 | treats diff as a commit range | pass | `ChangeTarget` is `Tree { spec, commit }` or `Worktree { head }` — two endpoints, never a range; no merge-base anywhere in `rr-git/src/diff.rs` |
| 2 | omits staged/untracked data | pass | `diff.rs:46` — "Index, unstaged tracked edits and eligible untracked sources, seen once" |
| 3 | mixes raced endpoints | pass | `IMPACT_WORKTREE_RACED` (`impact.rs:91`), `request.raced` threaded into `map_changes` |
| 4 | uses line-only/fuzzy definition matching | pass | `HunkRange` carries byte spans; matching is `(path, qualified name)` (`impact.rs:219`) |
| 5 | consumes stale graphs | pass | both endpoints are rebuilt through `overlay` from *their own* bytes (`rr-cli/src/impact.rs:177-178`); nothing is reused from the published graph |
| 6 | turns ambiguity into edges | pass | `impact.rs:1293,1305` — `let Resolution::Resolved(..) else { continue }`; unresolved and ambiguous only increment counters |
| 7 | guesses tests | pass | `TestReason` variants are documented "correlational, so it is labelled probable and never" an edge |
| 8 | hides counters/cycles/truncation | pass | `ResolutionCounts` publishes six counters; `ImpactResultV1::cycles`; `shown/total` truncation (`impact.rs:84`) |
| 9 | diverges text/JSON | pass | `ImpactResultV1::canonicalize` settles the order **both** renderers see (`impact.rs:43`) |
| 10 | emits nondeterministic data | pass | no `SystemTime`/`Instant`/`HashMap` in `impact.rs` or `check.rs`; co-change reads commits in graph order, no clock |
| 11 | mutates during check | pass | `check.rs:4` — "**It is read-only**"; no `fs::write`/`File::create`/`create_dir` in `rr-core/src/check.rs` or `rr-cli/src/check.rs` |
| 12 | drops malformed/trailing bytes | pass | `envelope::Framing::TrailingBytes` → `RebuildReason::TrailingBytes` → `check.rs:514` diagnostic; quality reports decode strictly (`quality.rs:29`) |
| 13 | changes rule/exit contracts | pass | `EMITTED_RULES`/`RESERVED_RULES` declared and tested; `rr impact` returns its own `ExitCode` without routing through `main::finish` (`rr-cli/src/impact.rs:11`) |
| 14 | duplicates an owning artifact/hash/anchor parser | pass | `check.rs:6` — "# Every rule delegates"; diagnostics are re-published in the owning module's own spelling |
| 15 | vendors unlicensed/unlocked code | pass | `REDISTRIBUTABLE_SPDX` is a closed list of 8, not a parser; `manifest.template.json` pins each repo by `commit` |
| 16 | rewrites goldens in CI | pass | the only non-test write in `quality.rs` is `materialize_synthetic`, into a caller-owned scratch dir; there is no baseline/golden write path |
| 17 | compares platforms | pass | `quality.rs:772` — the performance decision comes from "the one designated platform" |
| 18 | gates on one noisy run | pass | `PerfDecision::decide` blocks only when the point ratio **and** the bootstrap 95% lower bound both exceed their thresholds (>10% / >5%) |
| 19 | hard-codes Radar's 30 ms as our guarantee | pass | `PerfDecision` has no absolute-latency field by construction; only the two relative ratios |
| 20 | passes missing evidence | pass | `QualityFault::EvidenceMissing`, `MissingFile`, `MissingShard`, `MissingCase`; `CORPUS_ABSENT_SKIP` skips rather than passes |
| 21 | broadens M4 into UI/server/model work | pass | branch touches only `crates/`, `fixtures/corpus/`, `issues/`, CI config — no UI, server or model code |

Two items are outside what code can satisfy and stay open by design:

- **Corpus vendoring** is a human step (network + licence decision).
  `manifest.template.json` holds zero commits; `corpus_gate` is `#[ignore]`d
  until it is filled and the repos land under `fixtures/corpus/repos/`.
- **`BENCHMARKS.md`** is not generated. A number without the run that produced
  it is the thing the plan forbids, so the file waits for a real run on the
  designated platform.
