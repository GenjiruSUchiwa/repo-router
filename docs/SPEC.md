# Radar-inspired Repository Navigator — Reimplementation Specification

> **Purpose:** hand this document to a coding agent as the implementation brief for an open, cross-platform repository navigation tool inspired by the publicly described behavior of Radar.
>
> **Important:** this is **not** a description of Radar's private source code. Radar's public site says the core implementation remains private during beta. This document separates **publicly confirmed behavior** from **proposed implementation choices / inference**.
>
> Target platform for the first implementation: **macOS ARM64 (Apple Silicon)**, Linux x86_64/ARM64, with a CLI usable by **Claude Code**, **Oh My Pi**, and any agent able to execute shell commands.

---

## 1. Executive summary

Build a **small, deterministic, local repository navigation engine** whose primary job is to answer:

> “Where, exactly, in this repository should the agent look?”

The tool should avoid returning a repository tour, a huge semantic context bundle, or a complete knowledge graph by default. It should instead return one or a few **source anchors** such as:

```text
FINAL SOURCE ANCHOR
src/auth/token.rs#verify_token
verified source span: lines 41-68
```

When requested with `--source`, return only the bounded, verified source span around that definition.

The core loop should be:

```text
repository
   │
   ├── parse changed files only
   │      ↓
   │   Tree-sitter facts
   │      ↓
   │   compact lexical + structural index
   │
query
   │
   ├── exact-symbol routing when possible
   ├── otherwise lexical/structural candidate ranking
   ├── abstain when confidence is weak
   │
   ↓
verify current file hash/span
   │
   ↓
small source anchor / bounded source packet
```

The retrieval path should **not require an LLM, embeddings, or a vector database**. The coding agent is the reasoning layer; this tool is the fast navigation layer.

---

## 2. What is publicly confirmed about Radar

The following behaviors are explicitly described on Radar's public site/evidence page and should be treated as the factual inspiration for this implementation:

1. Radar builds **committed maps** and **exact source routes** for repositories.
2. Its goal is to make coding agents read fewer files and consume less context.
3. It builds a small useful index and returns a **source pointer** rather than a large repository/context dump.
4. Parsing uses **Tree-sitter extraction**, retaining definitions and references rather than full source copies in the index.
5. It uses **lexical fingerprints** involving signals from names, paths, signatures, bodies, calls, and callers.
6. It supports **exact-symbol routing** when a symbol is unambiguous.
7. It can **abstain** and return candidates instead of pretending certainty.
8. It uses **verified source spans**: the current file is hash-checked before a bounded definition is returned.
9. It uses **content-addressed caches** so unchanged content can reuse deterministic parsed facts.
10. It has small **MAP routers** / committed pointers that guide navigation before reading source.
11. It advertises **Git-aware impact** signals including callers, tests, dependencies, and co-change history.
12. It is positioned as **local, deterministic, source-verifiable navigation**, intentionally narrower than a broad knowledge graph or semantic-context system.
13. The lookup path is designed to keep models out of the retrieval loop.

Public benchmark claims are useful as directional evidence but **must not be hard-coded as guarantees**. The site currently reports, for specific published workloads, figures including 40/40 expected anchors in top-three on one frozen source-location corpus and a 28.74 ms fresh deterministic query p95 on a generated 10,000-file corpus. Treat those numbers as reference points, not acceptance criteria for unrelated repositories.

Sources used for this reconstruction:

- https://radar.sanixdk.xyz/
- https://radar.sanixdk.xyz/evidence.html

Accessed: 2026-08-14.

---

## 3. What is inferred / proposed here

The public description does **not** reveal the private core implementation. Therefore, the following are our proposed engineering choices:

- exact on-disk formats;
- concrete Rust data structures;
- lexical normalization algorithm;
- ranking weights;
- confidence thresholds;
- cache file layout;
- exact Tree-sitter queries per language;
- how Git co-change is calculated;
- which local database/index library is used;
- MCP protocol surface;
- JSON schemas;
- command names beyond the publicly demonstrated commands.

The implementation agent is free to improve these choices as long as the design principles and output contract remain intact.

---

# 4. Product goal

The tool is a **repository router**, not a repository analyst.

Its default output should answer one of these questions quickly:

- Where is symbol `verify_token` defined?
- Where is JWT verification handled?
- Which source span implements the auth boundary?
- What are the most likely files/functions relevant to this coding task?
- Who calls this symbol?
- Which tests are likely related to this symbol/change?

The default output should **not** attempt to answer:

- explain the entire architecture of the repository;
- generate an exhaustive graph of every relationship;
- dump all related chunks;
- summarize every source file;
- replace the LSP;
- replace the coding agent;
- embed the whole repository.

If the best answer is one function, return one function.

---

# 5. Design principles

## 5.1 Minimal context by default

A successful query should ideally return tens to hundreds of tokens, not thousands.

Default:

```text
src/auth/token.rs#verify_token
```

Optional:

```text
--source
```

returns a bounded source span.

Only explicit relationship/impact commands should produce more structure.

## 5.2 Source is authoritative

The index is never allowed to silently override the current working tree.

Before returning source:

1. locate indexed file record;
2. hash current content;
3. compare with indexed content hash;
4. if unchanged, use indexed span;
5. if changed, reparse that file or refuse stale output;
6. return only a verified span.

## 5.3 Deterministic retrieval

Given the same repository snapshot, index version, and query, retrieval should be deterministic.

Do not place an LLM in the routing path.

## 5.4 Exact before fuzzy

Try in this order:

1. exact fully qualified symbol;
2. exact symbol name;
3. exact normalized path/name tokens;
4. lexical ranking;
5. structural boosts from calls/callers/imports/tests;
6. return candidates if confidence is insufficient.

## 5.5 Do less work

Do not build a giant graph merely because a graph can be built.

Store the minimum structural facts useful for routing.

## 5.6 Incremental by content

Parsing should be content-addressed. Moving or renaming unchanged content should permit reuse where practical.

## 5.7 Agent-friendly contracts

Every command should have:

- terse human output;
- stable `--json` output;
- predictable exit codes;
- no interactive prompts during normal agent use;
- strict result limits.

---

# 6. High-level architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                     WORKING REPOSITORY                      │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           │ scan + ignore rules
                           ▼
                ┌───────────────────────┐
                │   File classification │
                │ language / generated  │
                └───────────┬───────────┘
                            │
                            ▼
                ┌───────────────────────┐
                │      Tree-sitter      │
                │ parse changed content │
                └───────────┬───────────┘
                            │
             ┌──────────────┼────────────────┐
             ▼              ▼                ▼
        definitions      references       imports
        methods          call sites       exports
        classes          identifiers      modules
             │              │                │
             └──────────────┼────────────────┘
                            ▼
                  ┌─────────────────────┐
                  │  Compact Fact Store │
                  └──────────┬──────────┘
                             │
           ┌─────────────────┼──────────────────┐
           ▼                 ▼                  ▼
    Exact Symbol Index   Lexical Index     Structural Index
     name → symbols      terms → records   calls/refs/tests
           │                 │                  │
           └─────────────────┼──────────────────┘
                             ▼
                    ┌─────────────────┐
                    │  Query Router   │
                    │ rank + abstain  │
                    └────────┬────────┘
                             │
                             ▼
                   ┌───────────────────┐
                   │ Source Verifier   │
                   │ hash + reparse    │
                   └────────┬──────────┘
                            │
                            ▼
                   FINAL SOURCE ANCHOR
```

---

# 7. Suggested Rust workspace

```text
repo-router/
├── Cargo.toml
├── crates/
│   ├── radar-cli/
│   ├── radar-core/
│   │   ├── src/parser/
│   │   ├── src/facts/
│   │   ├── src/index/
│   │   ├── src/query/
│   │   ├── src/verify/
│   │   ├── src/cache/
│   │   └── src/map/
│   ├── radar-git/
│   └── radar-mcp/              # later phase
├── fixtures/
├── benches/
└── docs/
```

Recommended initial dependencies:

- `clap` — CLI parsing;
- `tree-sitter` + language grammars;
- `ignore` — `.gitignore`-aware traversal;
- `blake3` — fast content hashes;
- `serde` — stable serialization structures;
- `rayon` — parallel parsing;
- `smallvec` — compact short collections where useful;
- `fst` or a compact custom dictionary — exact/prefix term lookup;
- `roaring` — compact postings if the lexical index needs bitmap sets;
- `memmap2` — optional later optimization for zero-copy map reads.

For V0, prefer a simple binary snapshot over introducing a database server.

---

# 8. Core data model

The exact representation can change, but preserve these logical concepts.

## 8.1 File record

```rust
struct FileRecord {
    id: FileId,
    relative_path: StringId,
    language: LanguageId,
    content_hash: [u8; 32],
    byte_len: u32,
    line_count: u32,
    generated: bool,
}
```

Do not store full file contents in the primary index.

## 8.2 Symbol record

```rust
struct SymbolRecord {
    id: SymbolId,
    file_id: FileId,
    name: StringId,
    qualified_name: Option<StringId>,
    kind: SymbolKind,

    start_byte: u32,
    end_byte: u32,
    start_line: u32,
    end_line: u32,

    signature_hash: u64,
    body_fingerprint: FingerprintId,
}
```

Possible `SymbolKind` values:

```text
function
method
class
struct
trait
interface
enum
module
constant
variable
route
test
```

V0 only needs the common language-appropriate subset.

## 8.3 Reference / call record

```rust
struct ReferenceRecord {
    source_symbol: Option<SymbolId>,
    target_name: StringId,
    file_id: FileId,
    line: u32,
    kind: ReferenceKind,
}
```

Kinds:

```text
call
import
use
read
write
inherit
implement
export
```

Do not require perfect name resolution in V0. Unresolved lexical references are still useful routing signals.

## 8.4 Lexical fingerprint

A record should expose a compact set of normalized terms derived from:

```text
symbol name
qualified name
file path
signature identifiers
body identifiers
imports
callees
caller names, when resolved
```

Example source:

```rust
pub fn verify_token(token: &str) -> Claims {
    jwt::decode(token)
}
```

Possible normalized terms:

```text
verify
token
auth
claims
jwt
decode
```

Store term IDs rather than repeated strings when practical.

## 8.5 Source anchor

```rust
struct SourceAnchor {
    path: String,
    symbol: Option<String>,
    start_line: Option<u32>,
    end_line: Option<u32>,
    indexed_hash: [u8; 32],
}
```

The anchor is the primary product output.

---

# 9. Repository map / storage layout

Proposed layout:

```text
.radar/
├── map                  # tiny committed navigation metadata
├── config.toml          # optional committed config
└── local/               # gitignored machine-local cache/index
    ├── snapshot.bin
    ├── strings.bin
    ├── postings.bin
    ├── facts/
    └── version
```

The public description says MAP pointers can be committed. A good design is therefore to separate:

- **small, stable, commit-friendly map metadata**, and
- **larger disposable local indexes/caches**.

Do not commit megabytes of generated graph data unless explicitly configured.

A possible committed `map` representation:

```toml
version = 1

[[route]]
name = "auth"
paths = ["src/auth", "src/middleware/auth.rs"]

[[route]]
name = "database"
paths = ["src/db", "src/models"]
```

This exact format is inferred, not public Radar behavior.

---

# 10. `map` indexing pipeline

Command:

```bash
radar map
```

## Step 1 — discover repository root

Prefer:

```text
git rev-parse --show-toplevel
```

with a filesystem fallback for non-Git directories.

## Step 2 — traverse files

Respect:

- `.gitignore`;
- `.ignore`;
- default excludes such as `.git/`;
- tool-specific excludes;
- generated/vendor directories when detectable/configured.

Do **not** blindly index:

```text
node_modules/
target/
dist/
build/
.venv/
vendor/
.git/
```

unless configuration explicitly includes them.

## Step 3 — hash before parse

For each candidate file:

```text
hash = BLAKE3(contents)
```

If cached facts already exist for the same content hash and parser/schema version, reuse them.

Cache key should include at least:

```text
content_hash
language_parser_version
fact_schema_version
```

## Step 4 — parse with Tree-sitter

Extract only useful routing facts.

Per-language query packages should extract:

- definitions;
- definition name;
- definition span;
- signature span or identifiers;
- call expressions;
- imports/use statements;
- exports when applicable;
- test markers when recognizable.

Avoid storing full ASTs after extraction.

## Step 5 — build lexical fingerprint

Normalize tokens using:

- camelCase splitting;
- PascalCase splitting;
- snake_case splitting;
- kebab-case splitting;
- lowercase;
- path component splitting;
- optional conservative stemming for English query terms.

Examples:

```text
verifyToken      → verify, token
JWTValidator     → jwt, validator
src/auth/token   → src, auth, token
```

Do not use aggressive stemming that makes code identifiers ambiguous.

## Step 6 — resolve cheap structural relations

Resolve calls/references when the target is unambiguous within known scope/module information.

If resolution is uncertain, keep the unresolved target name instead of inventing an edge.

## Step 7 — construct indexes

At minimum:

```text
exact_symbol_name -> [SymbolId]
qualified_name    -> [SymbolId]
term              -> postings(SymbolId/FileId)
file_path_terms   -> postings(FileId)
callee_name       -> postings(SymbolId)
caller_name       -> postings(SymbolId)
```

## Step 8 — write snapshot atomically

Use temp file + rename so interrupted indexing cannot corrupt the previous usable map.

---

# 11. Query pipeline

Command examples:

```bash
radar query "verify_token"
radar query "where is token verification handled?"
radar query "how does verify_token work?"
```

## 11.1 Query normalization

Input:

```text
where is token verification handled?
```

Normalize into conservative code-oriented terms:

```text
token
verification
verify
```

Potentially recognize explicit identifiers:

```text
verify_token
Foo::bar
AuthService.validate
src/auth/token.rs
```

## 11.2 Exact route first

If query contains an exact symbol candidate:

```text
verify_token
```

lookup:

```text
exact_symbol_index["verify_token"]
```

If one high-quality definition exists, return it directly.

If multiple exist, rank by query/path/signature context rather than guessing.

## 11.3 Lexical candidate retrieval

For natural-language queries, retrieve a small candidate set from compact postings.

Candidate generation should be cheap and broad enough to preserve recall.

Example scoring signals:

```text
exact symbol match
symbol token overlap
path token overlap
signature token overlap
body identifier overlap
callee overlap
caller overlap
test relationship
MAP route prior
```

## 11.4 Proposed ranking formula

This is only a starting point:

```text
score =
    12.0 * exact_symbol
  +  8.0 * symbol_name_overlap
  +  5.0 * qualified_name_overlap
  +  5.0 * path_overlap
  +  4.0 * signature_overlap
  +  3.0 * callee_overlap
  +  3.0 * caller_overlap
  +  1.5 * body_identifier_overlap
  +  route_prior
```

Use normalized features, not raw token counts, to avoid large functions dominating.

A BM25-like lexical score is acceptable if applied to **compact synthetic documents/fingerprints**, not whole source files.

## 11.5 Direct answer vs abstention

Never force a direct answer solely because there is a top result.

Suggested direct conditions:

```text
top_score >= minimum_score
AND top_score - second_score >= margin
AND candidate has a valid source anchor
```

If not:

```text
CANDIDATES
1. src/auth/token.rs#verify_token
2. src/middleware/auth.rs#authenticate
3. src/session.rs#validate_session
```

Return a nonzero/structured status indicating ambiguity, but do not treat ambiguity as a fatal CLI failure.

---

# 12. Source verification

This is a required safety/correctness feature.

Given indexed anchor:

```text
src/auth/token.rs#verify_token
indexed hash = H1
lines = 41-68
```

Before `--source` output:

```text
acquire canonical content (no-follow open, one ODB/filter/raw branch)
   ↓
H1 == H2 ?
   ├── no  → stale, no content
   └── yes → apply the span and the budgets
                ↓
             reacquire and compare identity again
                ↓
             fresh → return the bounded window
             changed → raced, no content
```

`--source` never relocates an anchor. Reparsing to re-identify a moved symbol
would answer a question the caller did not ask and would hide that the index no
longer describes the file; a stale anchor is reported as stale and refreshed by
`rr refresh`. Old lines are never returned.

The final comparison re-derives the canonical identity rather than comparing
recorded metadata, because a same-size, same-timestamp overwrite is exactly what
a metadata comparison misses.

## Bounded source policy

`--source` returns the definition span plus a small context margin:

```text
context before:     3 lines
context after:      3 lines
max source lines:   120
max source bytes:   64 KiB
max verified input: 16 MiB
```

Anchor lines are served first and context only when the whole anchor fits, so
the budget is never spent on context around a definition that could not itself
be shown. A definition over the budget is truncated on a whole-line boundary
with an explicit marker naming how many anchor lines and bytes were omitted; a
first anchor line that alone exceeds the byte budget is refused as
`line-too-long` rather than cut mid-line.

Refusal statuses are `stale`, `missing`, `symlink`, `not-regular`, `too-large`,
`line-too-long`, `not-text`, and `raced`. Every one of them returns no content,
no preview, no current OID, and no relocated line number.

`not-text` is the one refusal that does not describe a change or a budget. The
mapper accepts a source file whose bytes are not UTF-8 — an embedded NUL, a
Latin-1 line — so such a file is indexed, routes normally, and answers `rr
query` with its anchor; only serving its bytes as text is impossible. Because
the identity matched, that condition is permanent and no refresh clears it, so
it is reported as a refusal the caller can read rather than as an execution
error. Staleness still outranks it: a text anchor overwritten with binary bytes
is `stale`, decided before anything is decoded.

---

# 13. Content-addressed cache

Cache extracted facts by content hash.

Conceptually:

```text
fact_cache[
  blake3(file_contents),
  grammar_version,
  extractor_version
] -> ParsedFacts
```

This means:

```text
10,000 files
9,995 unchanged
5 changed
```

should reparse approximately the changed files rather than the whole repository.

A rename of an unchanged file can reuse parsed content facts while updating path-dependent fingerprints.

---

# 14. Structural relations without becoming Graphify

Maintain a **small structural layer**, but do not turn the default system into a full knowledge graph.

Useful edges:

```text
CALLS
IMPORTS
DEFINES
REFERENCES
IMPLEMENTS
INHERITS
TESTS
```

These edges are supporting ranking/impact features.

They should not be dumped into the agent context unless explicitly requested.

For example:

```bash
radar callers verify_token
```

may return:

```text
src/middleware/auth.rs#authenticate_request
src/session/refresh.rs#refresh_session
```

But:

```bash
radar query "token verification"
```

should still return the best source anchor, not a graph report.

---

# 15. Git-aware impact

This can be a later phase.

Public Radar material mentions callers, tests, dependencies, and co-change history.

Implement impact as a combination of cheap evidence:

```text
static callers
static references
imports/dependencies
nearby tests
historical co-change
```

## 15.1 Co-change

For each commit in a bounded history window, collect changed source files.

For files `A` and `B`:

```text
cochange(A, B) = commits_containing(A AND B) / normalization
```

Possible normalization:

```text
Jaccard(A, B) = together / (commits_A + commits_B - together)
```

Ignore or down-weight huge formatting/vendor commits.

## 15.2 Test association

Signals can include:

- test file imports target module;
- test calls target symbol;
- matching path/name conventions;
- repeated Git co-change.

Return evidence, not unsupported certainty.

Example:

```bash
radar impact src/auth/token.rs
```

```text
CALLERS
src/middleware/auth.rs#authenticate_request

LIKELY TESTS
 tests/auth/token_test.rs

CO-CHANGE
 src/session.rs    0.61
```

Keep default result limits small.

---

# 16. CLI contract

Required V0/V1 commands:

```bash
radar map
radar query <query>
radar status
radar doctor
rr init [--root <path>] [--json]
```

`rr init` installs the agent navigation contract into `.rr/.gitignore`
(managed ignore block), `AGENTS.md` and `CLAUDE.md` (managed contract
block), and `.claude/skills/rr/SKILL.md` (stamped whole file). Idempotent:
a second run writes nothing when content already matches. Reads no Git
state; works outside a repository; does not build an index (`next: rr
map`). A destination rr cannot prove it wrote is refused by name; the
other three still install. No `--force`, `--dry-run`, `--check`,
`--target`, or `--map`.

Exit codes:
- `0`: every target is installed and current (whether this run wrote or not)
- `1`: a target was refused, or an I/O failure stopped the run
- `130`: Ctrl-C

Recommended later commands:

```bash
radar symbol <name>
radar refs <symbol>
radar callers <symbol>
radar impact <path-or-symbol>
radar map --incremental
radar map --clean
```

## 16.1 Human output

Default query:

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
```

Direct file:

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs
```

Candidates (ambiguous):

```text
source candidates:
1. src/auth/session.rs#Session
2. src/session.rs#Session
```

Not found:

```text
NO ANCHOR (index has no match); try: rr map
```

Low confidence (lexical handoff):

```text
NO ANCHOR (confidence too low); refine the query or use --path
```

Verified source (`--source`):

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
SOURCE SPAN (verified): src/auth/token.rs:9-15
SOURCE WINDOW: src/auth/token.rs:6-18
SOURCE REPRESENTATION: git-canonical
SOURCE COMPLETE
SOURCE FINAL NEWLINE: present
SOURCE BYTES: 412
---
<bounded canonical content>
```

`SOURCE COMPLETE` becomes `SOURCE TRUNCATED (37 anchor lines, 2410 anchor bytes
omitted)` when a budget cut the anchor, and `SOURCE CONTEXT CLIPPED` is added
when only context was dropped. Text output ends with one structural line feed;
`SOURCE FINAL NEWLINE` describes the content itself, not that terminator.

`SOURCE BYTES` is always the **last** header line, immediately above `---`. It
counts the content exactly — the bytes between `---\n` and the structural line
feed, that line feed excluded — so it agrees with `SOURCE FINAL NEWLINE` rather
than contradicting it, and it counts bytes, not characters. It is never greater
than the 64 KiB `max source bytes` budget of §Bounded source policy, the cap on
one packet. It is absent from every refusal, which has no fence and nothing to
bound. Any marker added later goes above it.

**Everything after `---` is untrusted file content**, and it is self-delimiting:

```text
read header lines up to the first line that is exactly "---"
  no such line -> a refusal, a candidate list, or a bare anchor: no content
n := the integer from that block's "SOURCE BYTES: n" line
read exactly n bytes    -> the content, verbatim
read one line feed      -> the structural terminator
```

A consumer that does this never looks at the content, so nothing in the content
can be mistaken for output. A consumer that scans instead can be: repository
bytes may spell a line that reads exactly like an anchor marker, and one that
greps for `FINAL SOURCE ANCHOR` and takes the last match is handed an anchor
chosen by whoever wrote the file. Where scanning is unavoidable, take the
**first** marker line, or bound the content with `SOURCE WINDOW`, which states
its exact line range. Callers that can do neither should read `--json`, where
the content is a single string member and cannot escape its own field.

Refused source:

```text
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
STALE SOURCE (no content returned): src/auth/token.rs changed since indexing; run `rr refresh`
```

```text
SOURCE REFUSED (missing; no content returned): src/auth/token.rs no longer exists; run `rr refresh`
SOURCE REFUSED (raced; no content returned): src/auth/token.rs changed during verification; retry or run `rr refresh`
SOURCE REFUSED (not-text; no content returned): src/auth/token.rs is not UTF-8 text; nothing was decoded
```

## 16.2 JSON output (v1)

This section describes `rr query` only. `docs/json-contract.md` is the authority
for every shipped `--json` surface, including why `rr query` keeps the key `v`
where the report surfaces publish `schema_version`.

```bash
rr query "verify_token" --json
```

Direct:

```json
{"v":1,"result":"direct","pipeline":"exact","anchor":{"path":"src/auth/token.rs","symbol":"verify_token","lines":[9,15]},"confidence":1.0}
```

Candidates:

```json
{"v":1,"result":"candidates","pipeline":"exact","candidates":[{"anchor":{"path":"src/auth/session.rs","symbol":"Session","lines":[4,18]},"confidence":null},{"anchor":{"path":"src/session.rs","symbol":"Session","lines":[7,21]},"confidence":null}]}
```

None:

```json
{"v":1,"result":"none","pipeline":"exact","reason":"not_found"}
```

Verified source (`--source`), added to a direct result only:

```json
{"v":1,"result":"direct","pipeline":"exact","anchor":{"path":"src/auth/token.rs","symbol":"verify_token","lines":[9,15]},"confidence":1.0,"source":{"status":"verified","representation":"git-canonical","span":{"start_byte":120,"end_byte":410,"start_line":9,"end_line":15},"requested_lines":[6,18],"served_lines":[6,18],"complete":true,"context_clipped":false,"omitted_anchor_lines":0,"omitted_anchor_bytes":0,"ends_with_newline":true,"content":"<canonical content>"}}
```

Refused source, where every other member is absent rather than null:

```json
{"v":1,"result":"direct","pipeline":"exact","anchor":{"path":"src/auth/token.rs","symbol":"verify_token","lines":[9,15]},"confidence":1.0,"source":{"status":"stale"}}
```

Exit codes:
- `0`: direct, with or without verified source
- `1`: execution / I/O / snapshot error
- `2`: candidates
- `3`: none
- `4`: expected source refusal

A caller that stops reading before `rr` has finished writing sees none of these.
`rr` restores the default SIGPIPE disposition before its first print, so
`rr query … | head -1` is *terminated* by SIGPIPE and a shell reports `141` —
the same thing `head`, `grep` and `sort` do, and the reason the failure is
silent: nothing is written to stderr and no backtrace is produced. `rr` never
returns 141; it is killed, so `WIFSIGNALED` is what distinguishes it from an
ordinary exit. This applies to every command, `--help` and `--version` included,
and to a closed stderr exactly as to a closed stdout. Redirecting a stream is not
closing it: `rr … 2>/dev/null` is unaffected. On a platform without SIGPIPE the
write fails instead and the broken pipe is reported as the ordinary `1`.

Do not expose unstable internal ranking internals unless `--debug` is used.

---

# 17. Agent integration

The CLI itself is enough for initial Claude Code / Oh My Pi integration.

Suggested repository instruction:

```markdown
# Repository navigation

If Radar is available, use it before broad repository searches.

Prefer:

    radar query "<question>"

Use the returned source anchor before reading whole files or performing recursive searches.

Only fall back to ripgrep/find/LSP/native repository exploration when
Radar does not return enough information.
```

The tool should cooperate with LSP rather than compete with it:

```text
Radar: find likely source
LSP: precise language-aware navigation once localized
Agent: reason + edit
```

Recommended workflow:

```text
user task
   ↓
radar query
   ↓
1-3 likely anchors
   ↓
LSP / bounded reads
   ↓
agent edits
```

---

# 18. Token-efficiency rules

These are product requirements, not optional optimizations.

## Default query limits

Suggested:

```text
max direct anchors: 1
max candidate anchors: 3
max source lines: 80
max related nodes: 5
max impact entries per category: 5
```

## Never automatically generate

- `GRAPH_REPORT.md`;
- repository-wide prose summaries;
- giant JSON graphs;
- all call edges;
- all chunks related to a query;
- vector-search context bundles.

## Principle

If `rg` could answer a simple exact identifier query with a tiny output, our tool must not answer with 2,000 tokens of graph context.

The value is **routing precision + verified bounded source**, not verbosity.

---

# 19. Language support strategy

Do not start with 30 languages.

Recommended V0:

1. Rust
2. TypeScript / JavaScript
3. Python

V1:

4. Go
5. Java
6. C#

Each language adapter should implement a common trait:

```rust
trait LanguageExtractor {
    fn parse(&self, source: &[u8]) -> Result<ParsedTree>;
    fn definitions(&self, tree: &ParsedTree, source: &[u8]) -> Vec<Definition>;
    fn references(&self, tree: &ParsedTree, source: &[u8]) -> Vec<Reference>;
    fn imports(&self, tree: &ParsedTree, source: &[u8]) -> Vec<Import>;
}
```

Prefer language-specific Tree-sitter queries over large amounts of hand-written AST traversal.

---

# 20. Query examples that must work

Fixture repository should include auth/database/API/tests.

### Exact symbol

```bash
radar query "verify_token"
```

Expected: direct definition anchor.

### Natural language

```bash
radar query "where is token verification handled?"
```

Expected: `verify_token` in top 1-3.

### Path-related

```bash
radar query "authentication middleware"
```

Expected: auth middleware symbols/files ranked highly.

### Ambiguous name

Two definitions named `parse`.

```bash
radar query "parse"
```

Expected: candidates, not arbitrary certainty.

### Stale span

1. map repository;
2. edit file above target definition, changing line numbers;
3. query with `--source`.

Expected: the content identity no longer matches, so the tool refuses with
`stale` and exit `4`, never stale lines and never a relocated span.

### Cached map

Run `radar map` twice without changes.

Expected: second run performs near-zero parser work.

---

# 21. Testing requirements

## Unit tests

Test:

- token normalization;
- identifier splitting;
- exact symbol lookup;
- candidate scoring;
- confidence/abstention;
- content hash keys;
- span extraction;
- source verification;
- cache invalidation;
- serialization compatibility.

## Golden tests

For fixture repos, commit expected query results:

```text
query -> expected top anchors
```

Golden corpus should include natural-language aliases and ambiguous cases.

## Mutation tests for staleness

Modify source after index creation and ensure stale spans are never returned.

## Property tests

Useful invariants:

```text
verified=true => current content hash matches anchor basis
exact unique symbol => symbol appears in top result
candidate output count <= configured max
```

---

# 22. Benchmarking requirements

Do not optimize blindly. Measure these separately:

## Map build

```text
clean build time
incremental no-change time
incremental N-files-changed time
peak RSS
index bytes per source file
```

## Query

```text
exact-symbol p50/p95
natural-language lexical p50/p95
cold process start + query
loaded-process query
```

## Retrieval quality

Create a frozen corpus:

```text
question
expected source anchor(s)
```

Measure:

```text
top-1 recall
top-3 recall
false-direct rate
abstention rate
```

**False direct anchors are more harmful than reasonable abstention.**

## Context volume

Measure bytes/tokens returned by the tool, not just latency.

Target behavior:

```text
exact lookup → extremely small output
natural language → <= 3 anchors by default
--source → bounded span only
```

---

# 23. Suggested implementation phases

## Phase 0 — skeleton

Deliver:

```text
Rust workspace
CLI
repo root detection
file walker
BLAKE3 hashes
binary snapshot versioning
```

No natural-language query yet.

## Phase 1 — exact navigation MVP

Languages:

```text
Rust
Python
TypeScript
```

Implement:

```text
Tree-sitter parsing
definitions
exact symbol index
source anchors
```

Acceptance example:

```bash
radar map
radar query verify_token
```

works end-to-end on fixture repos.

## Phase 2 — lexical routing

Implement:

```text
fingerprints
term dictionary
postings
query tokenizer
ranking
candidate output
abstention
```

No embeddings.

## Phase 3 — references and callers

Implement:

```text
calls
imports
cheap resolution
callers
ranking boosts
```

## Phase 4 — incremental performance

Implement/optimize:

```text
content-addressed fact cache
parallel parse
mmap/compact snapshot if justified
atomic updates
```

## Phase 5 — Git-aware impact

Implement:

```text
co-change
likely tests
impact command
```

## Phase 6 — MCP

Expose a very small MCP surface:

```text
radar_query
radar_source
radar_callers
radar_impact
```

The MCP server should simply expose core deterministic operations. Do not add an LLM.

---

# 24. Possible internal API

```rust
pub struct Radar {
    repo: RepoContext,
    index: RepositoryIndex,
    cache: FactCache,
}

impl Radar {
    pub fn map(&mut self, options: MapOptions) -> Result<MapStats>;

    pub fn query(
        &self,
        query: &str,
        options: QueryOptions,
    ) -> Result<QueryResult>;

    pub fn verified_source(
        &mut self,
        anchor: &SourceAnchor,
        options: SourceOptions,
    ) -> Result<VerifiedSource>;

    pub fn callers(&self, symbol: &SymbolSelector) -> Result<Vec<SourceAnchor>>;

    pub fn impact(&self, target: &TargetSelector) -> Result<ImpactResult>;
}
```

Query result:

```rust
enum QueryResult {
    Direct {
        anchor: SourceAnchor,
        confidence: f32,
    },
    Candidates {
        candidates: Vec<RankedAnchor>,
    },
    NotFound,
}
```

---

# 25. Pseudocode — indexing

```rust
fn build_map(repo: &Path, previous: Option<&Snapshot>) -> Result<Snapshot> {
    let files = discover_source_files(repo)?;

    let parsed = files.par_iter().map(|path| {
        let bytes = fs::read(path)?;
        let hash = blake3::hash(&bytes);
        let language = detect_language(path)?;

        if let Some(facts) = cache.lookup(hash, language, EXTRACTOR_VERSION)? {
            return Ok((path, hash, facts));
        }

        let tree = parsers.parse(language, &bytes)?;
        let facts = extract_compact_facts(language, &tree, &bytes)?;
        cache.insert(hash, language, EXTRACTOR_VERSION, &facts)?;

        Ok((path, hash, facts))
    }).collect::<Result<Vec<_>>>()?;

    let mut index = RepositoryIndex::new();

    for (path, hash, facts) in parsed {
        index.add_file(path, hash, facts);
    }

    index.resolve_unambiguous_relations();
    index.build_exact_symbol_index();
    index.build_lexical_postings();

    write_snapshot_atomically(index)
}
```

---

# 26. Pseudocode — query

```rust
fn query(index: &RepositoryIndex, raw: &str) -> QueryResult {
    let q = normalize_query(raw);

    if let Some(symbol) = q.explicit_symbol.as_ref() {
        let exact = index.exact_symbol(symbol);

        if exact.len() == 1 {
            return QueryResult::Direct {
                anchor: exact[0].anchor(),
                confidence: 1.0,
            };
        }
    }

    let candidates = index.lexical_candidates(&q, 64);

    let mut ranked = candidates
        .into_iter()
        .map(|candidate| score_candidate(index, &q, candidate))
        .collect::<Vec<_>>();

    ranked.sort_by(descending_score_then_stable_id);

    if is_confident_direct(&ranked) {
        return QueryResult::Direct {
            anchor: ranked[0].anchor.clone(),
            confidence: ranked[0].confidence,
        };
    }

    if ranked.is_empty() {
        QueryResult::NotFound
    } else {
        QueryResult::Candidates {
            candidates: ranked.into_iter().take(3).collect(),
        }
    }
}
```

---

# 27. Pseudocode — verified source

```rust
fn verified_source(anchor: &SourceAnchor, repo: &Path) -> Result<VerifiedSource> {
    let path = repo.join(&anchor.path);
    let bytes = fs::read(&path)?;
    let current_hash = blake3::hash(&bytes);

    let current_anchor = if current_hash.as_bytes() == &anchor.indexed_hash {
        anchor.clone()
    } else {
        reparse_and_relocate_symbol(&path, &bytes, anchor.symbol.as_deref())?
            .ok_or(Error::StaleAnchor)?
    };

    let source = bounded_lines(
        &bytes,
        current_anchor.start_line,
        current_anchor.end_line,
        MAX_SOURCE_LINES,
    )?;

    Ok(VerifiedSource {
        anchor: current_anchor,
        source,
        verified: true,
    })
}
```

---

# 28. What not to implement in the first version

Avoid these distractions:

- vector database;
- embedding model downloads;
- LLM query rewriting;
- repository chat UI;
- full program-analysis engine;
- perfect cross-language symbol resolution;
- complete data-flow/security analysis;
- full Graphify-style knowledge graph;
- IDE plugin;
- cloud service;
- telemetry dependency required for operation.

A fast, reliable 80% router is more valuable than an unfinished universal code intelligence platform.

---

# 29. macOS ARM64 requirements

The implementation must build natively on Apple Silicon:

```text
aarch64-apple-darwin
```

Avoid dependencies requiring an x86-only external runtime.

CI matrix should include at least:

```text
macos-14 / arm64 or equivalent Apple Silicon runner if available
ubuntu / x86_64
ubuntu / arm64
```

If native ARM CI is unavailable, still ensure Rust dependencies are portable and add a real-device release test before tagging releases.

Packaging options later:

```text
cargo install
GitHub release binaries
Homebrew tap
```

---

# 30. Definition of done for the first useful release

A V1 is useful when all of the following are true:

1. `radar map` works on Rust, Python, and TypeScript fixture repositories.
2. A second unchanged `radar map` reuses cached parsing work.
3. `radar query <exact-symbol>` returns the correct definition anchor.
4. Basic natural-language code-location queries return the expected definition in top 3 on a frozen test corpus.
5. Ambiguous symbols return candidates rather than arbitrary direct answers.
6. The future source capability never returns a stale span after source edits.
7. Default output is intentionally small.
8. `--json` is stable enough for agents/tools.
9. The binary runs natively on macOS ARM64.
10. No LLM, embedding API, network service, or vector DB is required for indexing/querying.

---

# 31. The most important implementation insight

Do **not** think of this as “build a code knowledge graph.”

Think of it as:

> **Build the cheapest possible deterministic routing layer that can confidently tell an agent which source definition to inspect next.**

A full graph can represent more information, but that information has a context cost when exposed to an agent. The design should optimize for this pipeline:

```text
QUESTION
   ↓
small deterministic router
   ↓
ONE VERIFIED SOURCE ANCHOR
   ↓
coding agent reads only what it needs
```

For harder relational questions, expose a deliberately bounded structural view:

```text
"where is X?"            → 1 anchor
"who calls X?"           → <= 5 anchors
"what may X break?"      → <= 5 callers/tests/co-change hints per class
"show the whole graph"   → not a default operation
```

That distinction is the core product philosophy to preserve.

---

# 32. Instruction to the implementation agent

Implement this incrementally and benchmark each phase.

Priorities, in order:

```text
correct source anchor
    > stale-source safety
    > retrieval recall
    > tiny output
    > incremental speed
    > richer graph features
```

When uncertain between two designs, choose the one that:

1. reads less source;
2. stores fewer derived facts;
3. returns fewer tokens;
4. remains deterministic;
5. makes stale output impossible or explicit;
6. works locally and cross-platform.

Do not claim compatibility or equivalence with Radar's private implementation. This is an independent reimplementation based on its publicly described mechanisms and product behavior.
