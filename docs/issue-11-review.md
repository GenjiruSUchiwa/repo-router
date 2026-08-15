# Issue #11 — implementation, self-review, and code-quality assessment

Written by me about my own work, not by the repo's code-review skill.

Scope: `rr-core::text` (the pure projection), `rr-git::refresh` (the guard
split), `rr-cli` (writing the artifacts). 543 tests pass; clippy pedantic and
`fmt` are clean.

---

## 1. Defects found by self-review

Every one of these was found after the code "worked" — by adversarial reading,
by running the binary against hand-built repositories, or by clippy. None came
from a failing test I had already written.

| # | Defect | How it would have shipped |
|---|---|---|
| 1 | `.rr/SYMBOLS.md` was repaired in place for **every** damaged state, including a foreign file and a merge conflict | rr seizes a path it does not own; the `declares_rr_artifact` sniff was dead code |
| 2 | A reserved path that is a **symlink** was judged on its target's bytes, then written through | rr's output lands outside the repo. On `.rr/SYMBOLS.md` — repaired in place — that is an offer to overwrite the target |
| 3 | A directory that lost its last source file kept its `MAP.md` **forever** | The orphan scan sought only overflow pages, and only in directories the *new* plan named. A vanished subtree was never revisited |
| 4 | Generated maps could **re-enter the index they describe** | Their own `index_hash` feeds back; no run converges. Latent today (Rust-only allowlist), so the guard sits in `walk::collected_lang` |
| 5 | Maps were written to `.rr/local/publication/…` | `guard.path()` is the lock file, not the working tree |
| 6 | An ordinary content change reported `SYMBOLS.md repaired` | "older" and "no longer valid" are different events; only the second is a repair |
| 7 | Rendering ran twice per invocation | Validation rendered to compare, then publication rendered again |
| 8 | Snapshot and text were publishable under **two** guards | A second run can publish between them, leaving a map naming a snapshot nobody has |

Findings 1, 2 and 3 each corrupt or lose user data. Findings 1 and 2 are the
same latent mistake twice: the repair path for `.rr/SYMBOLS.md` is the one
place rr writes without asking, and both times it nearly wrote somewhere it
had no claim to.

### What a later review pass found that this one missed

Six more, all fixed. Reproduced each against the pre-review binary before
accepting it — the first two are the ones I should have caught.

| # | Defect | Why my pass missed it |
|---|---|---|
| 9 | `Refresh::UpToDate` staged text against the **working directory**, not the work root. Every map reads as missing, so a nothing-to-do `rr refresh` from any subdirectory reaches for the publication guard | My `--root` probe used `rr map`, and Full mode never reaches the `UpToDate` branch. The reports are identical either way; the difference shows only against a held lock |
| 10 | `.rr/.gitignore` was written last and validated **never**. A malformed managed block failed the run *after* the snapshot and every map were replaced | `ConflictReason::ManagedIgnore` existed, so it looked handled. Nothing produced it |
| 11 | A bare `=======` was read as a conflict marker — which is also a Markdown setext underline, in a purpose slot that is human prose. A purpose of `Routing\n=======\nfor auth.` locks the repository out with a merge that never happened | I tested conflict detection with a synthetic conflict, never with legal prose |
| 12 | `unquote`'s `\u` escape accepted fewer than four digits, and a leading `+` | |
| 13 | `budget as usize * BYTES_PER_TOKEN as usize` overflows on 32-bit targets | Not reachable from the CLI; library API only |
| 14 | Freshness was a linear scan per file — a comparison per *pair* of artifacts | Fine at 44 pages, quadratic at thousands |

Verified independently rather than taken on trust: a real `git merge` conflict
(two bare `=======` separators in the file) is still detected through
`<<<<<<< ` and `>>>>>>> `, so #11's fix loses nothing; #9 reproduces exactly as
described — with the lock held, `rr refresh` exits 0 from the root and 1 from
`src/`; #10's fix leaves both maps and the snapshot untouched.

Findings 9 and 10 arrived with fixes but no regression tests. Added:
`a_no_op_refresh_takes_no_lock_from_any_directory`,
`a_refresh_from_a_subdirectory_still_repairs_the_whole_repository`, and
`a_malformed_managed_ignore_is_refused_before_anything_is_written`.

Still open, deliberately: **`--json` omits every text-artifact outcome.**
`text.clause()` is appended only on the human path, so `rr refresh --json` can
report `"outcome": "unchanged"` while it rewrote every committed map, and a
conflict exits 1 with empty stdout. Fixing it means adding fields to a
`schema_version: 1` contract — a versioning decision, not a bug fix.

### Verified negative — things that turned out to be fine

Probed and found correct, listed so the coverage is legible:

- **Markdown injection via directory names.** A directory named
  `a](http://evil)[x` renders as `- [a\](http:](<a%5D%28http%3A/MAP.md>)` —
  link text backslash-escaped, destination percent-encoded.
- **Concurrency.** Six simultaneous `rr map` runs: one wins the guard, five
  refuse with `another rr process is refreshing this repository`, and the
  repository converges with no dangling links.
- **`--root` into a subdirectory.** Resolves to the Git work root, so
  `rr map --root sub` and `rr map` agree; the second is a no-op.
- **Empty repository.** A valid root map with `_None._` sections.
- **Overflow trees.** 120 symbols → 44 pages across two levels; shrinking to 4
  removes 43, leaves no dangling router links, and settles on the next run.

---

## 2. Scenarios the issue did not consider

**a. Committing the maps costs the next refresh its fast path.** The
incremental delta is `git status` — working tree against `HEAD` — so a snapshot
is only incrementally usable while `HEAD` stands still. Issue #11 makes rr
produce files whose whole purpose is to be committed, so *every* generation is
now followed by a `HEAD` move that forces a full fallback. Before #11 this path
was incidental; now it is guaranteed for every user of every repository.

The fallback still converges off the cache and writes nothing, so the cost is a
full walk, not a full reparse. I did not fix it: a `HEAD`-to-`HEAD` tree diff is
a separate feature, not a detail of #11. Pinned as
`committing_the_generated_maps_moves_head_and_forces_a_full_fallback` so it is
visible rather than folklore.

**b. `rr map` now leaves the working tree dirty.** Unavoidable and correct, but
it changes what `rr status` says immediately after a successful run, and it
broke an existing test that assumed `clean`. Anything downstream asserting a
clean tree after `rr map` needs to know.

**c. Page density.** Each API record repeats the symbol name three times
(qualified name, full signature, source anchor), so ~250 bytes per record
against a 1000-byte body budget — about **three records per page**. 120
functions in one directory produced 44 files. The default budget of 250 tokens
is small relative to the record format; a real repository with a wide module
will generate a lot of files. Worth a decision before this ships widely.

**d. Case-insensitive filesystems.** On macOS a hand-written `map.md` occupies
`MAP.md`. rr refuses (`path is not owned by rr`) rather than clobbering — safe,
but the message names `MAP.md` while the file on disk is `map.md`, and rr can
never generate a map in that directory until it is renamed.

---

## 3. Code quality

### DRY

Fixed during this pass, not merely noted:

- `is_reserved_artifact_name` — one predicate for "a file rr writes", consulted
  by the walk, the orphan scan, and the plan. Previously three near-copies.
- `stage_text_artifacts` — one render, consumed by both validation and
  publication.
- `publish` + `confirm` were always called as a pair, once per caller.
  Verification is now *inside* publishing, so a caller cannot skip the check
  that catches a repository holding two generations at once.
- `tests/common/mod.rs` — the two integration-test binaries carried identical
  process-running helpers. Only the mechanics moved; each file keeps its own
  `repo()` fixture, because they genuinely want differently shaped trees.
- `workspace::STATE_DIR` reused instead of a literal `".rr"`.

Remaining, accepted: `TextReport::unchanged` and `publish` both count artifacts,
by different routes. Merging them would mean the up-to-date path constructing a
report it never writes.

### SRP

The split I am most confident in: `rr-core::text` cannot open a source file,
walk a worktree, or write anything. It turns one frozen `Snapshot` into bytes.
All I/O lives in `rr-cli::text_artifacts`; all locking in `rr-git::refresh`.
That is why the pure model has 30 unit tests that need no repository.

Weakest point: `validate.rs` at 598 lines carries three jobs — comparing disk
against the plan, scanning for orphans, and building `MapCatalog` for issue #12.
The catalog is there because it reuses the ownership check. It should move once
#12 exists and its real shape is known; splitting it now would be guessing.

### POR

Read as ownership boundaries and single source of truth — say if you meant
open/closed and I will redo this section.

The rule that drove the design: **exactly one place decides each fact.**
Ownership is decided by `ownership_of`, freshness by byte equality, "is this
mine" by `is_reserved_artifact_name`, "may I repair this" by `repairs_in_place`.
Defect #1 existed precisely because the repair decision had been inlined into a
match arm instead of being named; naming it made the bug visible in one line.

`repairs_in_place` is deliberately a *deny*-list of conflict reasons rather than
an allow-list of repairable ones, so a new `ConflictReason` defaults to "report,
do not seize". That default is what made adding `Symlink` safe.

### Nomenclature

Names state outcomes, not mechanics: `stale`, `missing`, `conflicts`,
`removable`, `fresh`. `ConflictReason` is a closed enum rather than a string
because the CLI prints it and issue #14 will branch on it — a message describes,
a decision is acted on.

Two names I changed because they lied: `PreparedRefresh::root()` →
`work_root()` (it was returning the lock-file path, which is defect #5), and
`SymbolsState::Written` vs `Repaired` (defect #6).

Test names are full sentences — `a_reserved_path_that_is_a_symlink_is_never_written_through`
— so a failure names the broken property, not the function under test.

### Readability

Honest numbers. Comment density (comment lines / total lines):

| File | Density | Note |
|---|---|---|
| repo-wide baseline (`rr-core` + `rr-git`) | **17%** | |
| `walk.rs` | 7% | the low outlier, not the norm |
| `rr-cli/text_artifacts.rs` | 16% | at baseline |
| `rr-core/text/validate.rs` | 21% | above |
| `rr-cli/refresh.rs` | 25% | pre-existing style |
| `rr-git/refresh.rs` | 31% | was 32% before I touched it |
| `rr-core/text/mod.rs` | 39% | structural |

Correcting myself: I previously told you the house baseline was 7–14%. It is
17%. `walk.rs` is the outlier.

On your "docstring ou prose" note — you were right, and I cut roughly twenty
blocks across `text/` in response. What is left is above baseline in
`validate.rs` (21% vs 17%) and I would trim further on another pass. The 39% in
`text/mod.rs` is not what the number suggests: the file is 140 lines of
`pub use` and `pub const`, where required doc comments dominate by
construction, and its header documents three places where the implementation
*reads* issue #11 rather than quoting it — which is the first thing a reviewer
of this change needs.

---

## 4. What I would not ship without a decision

1. **Page density** (§2c). Three records per page is a lot of files.
2. **The `HEAD`-moved full fallback** (§2a). Correct, but every user pays a full
   walk after every generation-and-commit cycle.
3. **`validate.rs` carrying `MapCatalog`** — fine until issue #12, then wrong.
