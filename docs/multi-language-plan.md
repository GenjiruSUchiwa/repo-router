# Supporting every language rr already names

`lang.rs` names 27 languages. `parser/` has one extractor. This is the path
from one to many, in the order the work has to happen.

---

## 1. Grammars and queries are not the hard part

The ecosystem already solved this. Every mature tree-sitter grammar ships a
`queries/tags.scm` alongside it — the same file GitHub reads to build code
navigation for millions of repositories. The `tree-sitter-tags` crate consumes
those queries through one uniform API: `TagsConfiguration` per language,
`TagsContext` per thread.

So the per-language cost is **not** "write another 110-line `.scm`". It is
"declare the grammar crate and point at the `tags.scm` it already carries".

### One trap to avoid

`tree-sitter-language-pack` advertises 306 languages by **downloading and
caching grammars on demand**. That is disqualifying here. rr's product is
determinism: OID-verified sources, byte-identical snapshots, an index that
reproduces offline. A parser fetched at runtime makes the output depend on the
network and on when you ran it.

Pin statically instead — one `tree-sitter-<lang> = "=x.y.z"` per supported
language, exactly the way `tree-sitter-rust = "=0.24.2"` is pinned today. Fewer
languages, still reproducible. Determinism is the thing worth keeping.

---

## 2. The real blocker: the fact vocabulary is written in Rust

This is what actually stops a second language, and it is not visible from
`parser/`. `facts.rs` encodes Rust's own vocabulary:

| Type | Rust-only today | Missing for everyone else |
|---|---|---|
| `DefKind` | `Trait`, `TraitMethod`, `AssociatedType`, `Macro`, `Union` | `Class`, `Interface`, `Field`, `Property`, `Constructor`, `Namespace`, `Variable` |
| `Visibility` | `Crate`, `Restricted(String)` — i.e. `pub(crate)`, `pub(in path)` | `Protected`, `Internal`, `Package` |
| `ImportKind` | `Use`, `ExternCrate` | `Import`, `From`, `Require`, `Include` |
| `ReferenceKind` | `MacroCall`, `Implementation` | — |
| `TestSignals` | `inside_cfg_test` — `#[cfg(test)]` | a language-neutral test signal |

These types derive `Serialize`/`Deserialize`, so they are the on-disk format of
both the facts cache and the snapshot. Widening the vocabulary is a **format
break**.

That break is already affordable. `FACT_SCHEMA_VERSION = 2` and
`SNAPSHOT_SCHEMA_VERSION = 5` exist, and `snapshot.rs:218` refuses a snapshot
whose version does not match. Bump both, and every stale cache invalidates
itself into a full rebuild. The machinery for this migration was built before
anyone needed it.

### What #31 landed, and the two rows it did not

The versions are now 3 and 6. Every `DefKind` row above landed, along with
`Protected` and `Internal`, `Import` / `From` / `Require`, and a
`TestSignals::inside_test_scope` sitting beside `inside_cfg_test` rather than
replacing it. `DegradedReason::NoExtractor` landed too, which is not in the
table: it separates *rr has no extractor for this language* from *the parser was
asked and returned nothing*, so the first can be kept out of the fact cache by
the facts themselves instead of by a flag carried alongside them.

The new import kinds are inert on purpose. `ImportKind::resolves_by_path` answers
`false` for `Import`, `From`, and `Require`, because the resolver in
`index::build` splits a path on `::` and rejoins it onto the importing file's
module path — so `react` imported from `app` would be looked up as `app::react`
and could *find* an unrelated local symbol. Step 3 below flips those rows on as
it teaches the resolver each language's separators, one line and one test each.
Until then an unresolved import is a true statement; a wrong one would not be.

Two entries did not land. `Visibility::Package` names Java's package-private and
Go's lowercase; `ImportKind::Include` names C's `#include`. Neither is reachable
from TypeScript or Python, and #31's rule was that a variant no extractor can
produce does not land. They are one additive bump away whenever a language that
has them arrives — the same bump this section just showed costs a rebuild and
nothing else.

### What #32 landed

The generic tags tier now ships with `tree-sitter-tags = 0.25.10` and the
`tree-sitter-python = 0.25.0` harness grammar. `FACT_SCHEMA_VERSION` is 4,
`SNAPSHOT_SCHEMA_VERSION` is 7, and `EXTRACTOR_VERSION` is 3. The refresh
report exposes a `tags` counter, while maps publish the `syntax-tags` fidelity
for files whose definitions came from `tags.scm`.

---

## 3. Graduated degradation is what makes it affordable

`tags.scm` yields less than `rust.rs` does: a definition kind, a name, a span,
and a doc capture. No signature string, no visibility, no test signals.

Most of the gap closes generically:

| `Def` field | How a tags-based extractor fills it |
|---|---|
| `name`, `kind`, `span` | directly from the tags query |
| `signature` | source text sliced at `signature_span` — language-agnostic |
| `signature_idents`, `body_idents`, `doc_idents` | `scan_idents()`, which already exists in `parser/mod.rs` |
| `local_qualified` | derived from span nesting, language-agnostic |
| `visibility` | per-language default, refined later |
| `test_signals` | per-language default of `false` |

So there are three tiers, not two:

1. **Complete** — a hand-written extractor. Rust today.
2. **Tags** — kind, name, span, signature text, ident bags. Everything with a
   `tags.scm`.
3. **Lexical** — `degraded_facts()` / `lexical_idents()`. The current fallback.

`ParseStatus` already models `Complete | Recovered | Degraded`. Tier 2 is the
rung that is missing, and a `MAP.md` built from it has a populated `## API`
section — which is the whole point.

---

## 4. The plumbing is already multi-language

Worth knowing before estimating anything: the pipeline was built to carry a
language and then had Rust wired into it directly. Only **three** values are
hard-coded.

| Site | Today | Should be |
|---|---|---|
| `rr-git/src/pipeline.rs:42,52` | `extractor: Result<RustExtractor, String>` | a registry keyed by `Lang` |
| `rr-git/src/pipeline.rs:218` | `CacheKey::new(content.oid, Lang::Rust)` | `CacheKey::new(content.oid, source.lang)` |
| `rr-git/src/map.rs:176` | `languages: Some(vec![Lang::Rust])` | the set that has an extractor |

Everything around them is already generic:

- `walk::SourceFile` carries `lang: Lang` (`walk.rs:70`), computed by
  `collected_lang`.
- `Worker::process` already receives that `SourceFile`.
- `CacheKey::new(oid, lang)` already takes a language — the cache was designed
  for this.

### A latent bug in the second row

`CacheKey::new(content.oid, Lang::Rust)` keys every cache entry as Rust. Two
files with identical bytes and different extensions share an OID, so the moment
the allowlist in row three widens, one file's facts are served for the other.
It is invisible today because only Rust is ever walked. Fix row two **before**
row three, not after.

---

## 5. Order of work

1. **Seam.** `Extractor` trait in `parser/mod.rs` (`fn extract(&mut self,
   content: &[u8]) -> Result<Facts>` — the signature `RustExtractor` already
   has), plus a `Registry` holding one boxed extractor per `Lang`. Swap the
   three sites above. No behaviour change; the existing tests are the proof.
   The 30-odd `RustExtractor::new()` call sites in tests and benches stay as
   they are — the concrete type remains public.
2. **Vocabulary.** Widen `DefKind` / `Visibility` / `ImportKind` /
   `TestSignals`, bump `FACT_SCHEMA_VERSION` (2→3) and
   `SNAPSHOT_SCHEMA_VERSION` (5→6). `rust.rs` keeps its behaviour and its 2059
   lines of tests.
3. **Tags backend.** One generic extractor over `tree-sitter-tags`, one pinned
   `tree-sitter-<lang>` per supported language, filling `Def` as described in
   §3.
4. **Two languages, not twenty.** TypeScript and Python. They disagree with
   Rust and with each other about classes, visibility, and imports — which is
   exactly what a single implementation cannot reveal.

Step 4 validates steps 1–3. An abstraction with one implementation has never
been tested.

---

## 6. Cost of delay

Every milestone shipped on top of a Rust-shaped fact model deepens the format
break in §2. M4 adds change impact and quality gates — both read `DefKind`. The
cheapest moment to widen the vocabulary is before that, not after.

---

## Sources

- [tree-sitter-tags (crates.io)](https://crates.io/crates/tree-sitter-tags)
- [tree_sitter_tags docs](https://docs.rs/tree-sitter-tags)
- [What the `queries/` directories contain](https://github.com/tree-sitter/tree-sitter/discussions/1251)
- [tree-sitter-language-pack](https://github.com/kreuzberg-dev/tree-sitter-language-pack)
- [tree-sitter-loader](https://docs.rs/tree-sitter-loader/latest/tree_sitter_loader/)
