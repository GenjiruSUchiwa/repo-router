# Contract A/B — August 2026

Does shipping an `rr` index to a coding agent make it cheaper? On six public
repositories, across two candidate agent contracts, the answer measured here is
**no** — and the reason is not the contract, it is what an index costs to carry.

## Method

108 Claude Sonnet sessions, `--effort low`, one question per session, no
follow-up turns:

**6 repositories × 3 tasks × 2 runs × 3 conditions = 108 sessions**, 36 per
condition.

| Repository | Language |
|---|---|
| `date-fns` | TypeScript |
| `axios` | JavaScript |
| `serde` | Rust |
| `cobra` | Go |
| `gson` | Java |
| `Dapper` | C# |

The three conditions differ only in what the repository contains when the
session starts:

- **control** — vanilla checkout. `git clean -fdx` before the run, no `rr`
  binary on `PATH`.
- **mapfirst** — `rr init && rr map`, with the contract that sends the agent to
  `MAP.md` first and to `rr query` second.
- **radarstyle** — same index, with the contract that sends the agent to
  `rr query` first and treats the map as orientation only.

Each task is a navigation question with one expected file. A session is correct
when the answer names that file. Scoring is on the basename, so a right answer
reached the wrong way still scores — this measures cost at equal correctness,
not quality.

## Results

| Metric (36 sessions/condition) | control | mapfirst | radarstyle |
|---|---|---|---|
| Correctness | 36/36 | 36/36 | 36/36 |
| Cost / session | **$0.0783** | $0.1083 (+38%) | $0.0933 (+19%) |
| Output tokens / session | 737 | 884 | 844 |
| Cache-read tokens / session | 145 174 | 189 211 | 188 273 |
| `rr query` calls | 2 | 21 | 34 |
| `grep` calls | 46 | 37 | 36 |
| File reads | 24 | 24 | 16 |

Every condition answered every question. The index changed the cost, not the
outcome.

## Where the money goes

The navigation improvement is real and it is small. The index tax is boring and
it is large.

Taking radarstyle against control, a delta of **$0.0150/session**:

| Component | Delta | Cost | Share |
|---|---|---|---|
| Cache read | +43 099 tokens | $0.01293 | 86% |
| Output | +107 tokens | $0.00161 | 11% |
| **Explained** | | **$0.01454** | **97%** |

At Sonnet rates ($0.30/MTok cache read, $15/MTok output), the delta is almost
entirely one thing: **the contract and the map enter the prompt prefix and are
re-read on every turn**. The agent does navigate better — `grep` drops 46 → 36,
file reads drop 24 → 16 — but those savings are worth roughly $0.002 against a
$0.013 prefix tax. Better navigation loses to carrying the index by an order of
magnitude.

The same decomposition against mapfirst closes only 51% of its
$0.0300/session delta. The residual is consistent with cache **writes**:
mapfirst pulls `MAP.md` into context during the session, which appends
new cacheable segments the other conditions never pay for.

## What this says about the two contracts

Radar-style is the cheaper contract, and it is not close: **$0.0933 vs $0.1083,
−14%**, at identical correctness. It gets there by asking `rr query` more (34
calls vs 21) and reading fewer files (16 vs 24). Asking the index is cheap;
loading the map into context is not.

That is the contract this branch adopts.

## Threats to validity

- **`Dapper` ran with a degraded index.** C# has no extractor
  (issue #67), so one of the six repositories exercised the rr conditions
  with almost no facts to route on. Its rr sessions paid the prefix and
  received nothing. Re-running it after #67 lands would move these numbers.
- **`n = 2` per repository/task cell.** No variance or spread is reported.
  The ranking (control < radarstyle < mapfirst) is consistent across the set;
  the magnitudes are not tight.
- **One turn per session.** The prefix tax is paid per turn, so it dominates
  short sessions and amortises over long ones. A multi-turn agent session
  would weight this differently, plausibly in the index's favour.
- **Basename scoring.** Correctness does not distinguish a precise anchor from
  a lucky grep, so this benchmark cannot see a quality gain if one exists.

## Layout

```
harness/      bench.sh, bench-control.sh, tasks.sh — the runner and the 18 questions
aggregates/   agg.json, agg3.json, report_by_repo.json, rows.json, rows_all.json
results/      <condition>/<repo>/task<i>-run<n>.{txt,expected,contract,session}
```

`.txt` is the agent's final answer, `.expected` the file the task wanted,
`.contract` the condition, `.session` the Claude session id the run came from.
Per-session token and cost figures were read from the session transcripts and
are summarised in `aggregates/`; the transcripts themselves are not vendored.
