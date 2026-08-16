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
and could *find* an unrelated local symbol. That answer is settled rather than
deferred: for the tier-2 languages `path` is a specifier (`./Button`, `..`,
`react`), and no separator table turns one into a definition rr holds — that
takes a module graph (tsconfig paths, extension resolution, `node_modules`,
package layout), which is out of scope by decision. A row flips to `true` only
alongside a resolver that can follow that language's specifiers. Until then an
unresolved import is a true statement; a wrong one would not be.

Two entries did not land. `Visibility::Package` names Java's package-private and
Go's lowercase; `ImportKind::Include` names C's `#include`. Neither is reachable
from TypeScript or Python, and #31's rule was that a variant no extractor can
produce does not land. They are one additive bump away whenever a language that
has them arrives — the same bump this section just showed costs a rebuild and
nothing else.

### What #32 landed

The generic tags tier now ships with `tree-sitter-tags = 0.25.10` and the
`tree-sitter-python = 0.25.0` harness grammar. `FACT_SCHEMA_VERSION` is 4 and
`SNAPSHOT_SCHEMA_VERSION` is 7. The refresh report exposes a `tags` counter,
while maps publish the `syntax-tags` fidelity for files whose definitions came
from `tags.scm`.

There is no longer one `EXTRACTOR_VERSION`: #35 split it per language, so a
fix scoped to one grammar reparses that language's files and nobody else's.

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

## 5a. The verdict on step 4

Step 4 shipped as Python (#38) and TypeScript with TSX (#33). Four languages
are now indexed across two extraction tiers. This section is what they proved,
written while the evidence is still in front of us, because the point of §5
step 4 was never the two languages — it was finding out whether #31's
vocabulary and #30's seam survive contact with a language that disagrees.

### The vocabulary held. The plumbing did not, and had to grow one hook.

Every `DefKind` row #31 added is now produced by something:

| Row | Produced by | Notes |
|---|---|---|
| `Class` | TypeScript, Python | |
| `Interface` | TypeScript | Kept apart from `Trait`, which stays Rust's |
| `Field` | TypeScript | Class fields, interface property signatures, and constructor parameter properties |
| `Property` | TypeScript | Getters and setters only |
| `Constructor` | TypeScript | |
| `Namespace` | TypeScript | `Module` stays what Rust means by `mod` |
| `Variable` | TypeScript, Python | `Const`/`Static` keep their Rust meanings |

`Visibility::Protected` is TypeScript's modifier; `Visibility::Internal` is
Python's `_name`. `Private` is produced by both, by three unrelated
mechanisms — a `#` in the name, a `private` modifier, and a `__` prefix — and
that is the point: the vocabulary names the *conclusion*, and each language
reaches it its own way.

The separator held too. `Client.describe` and `Service.run` are stored and
routable as written, and `Router::route` is stored as Rust writes it, out of
one `local_qualified` field and one `Lang::qualified_separator`.

What did **not** hold was the assumption underneath `LanguageSpec`: that a
language's behaviour is fully describable by a tags query plus a few function
pointers. `tree-sitter-tags` compiles text predicates — `#eq?`, `#match?` —
and then never evaluates them. So a query cannot say "this `method_definition`
whose name is `constructor` is a different kind", which is exactly the sort of
thing TypeScript needs said four times over: `constructor`, `get`/`set`
accessors, `const f = () => …`, and the `private`/`protected` modifiers that
are anonymous tokens no capture can reach.

That is what the `refine: fn(&mut Def)` hook is, and it is the one structural
change the second language cost. Python's is a no-op and documented as such.
It has to run before the definitions are sorted, because `def_key` holds the
kind and a kind changed afterwards would leave the order it was sorted into.

One further assumption gave way, in `assign_nesting` rather than in
`LanguageSpec`: that a definition is named under whatever contains it. A
TypeScript parameter property — `constructor(private readonly repo: Repo)` —
is written inside the constructor's parameter list and is a field of the
class, so containment alone files it as `Service.constructor.repo`, a path
nothing refers to it by. `naming_owners` separates the frames a definition is
*named* under from the frames it *sits* inside, and it is the only construct
across four languages where the two differ. It is expressed in the vocabulary
rather than in node names — a field is state on a type, and no callable
declares one — which is what makes it inert for Rust and Python instead of a
TypeScript special case leaking into shared code. The exclusion parent stays
the lexical one, because it is still the constructor's text that must not
count the parameter twice.

### What tier 2 does not claim

Recorded here rather than discovered later:

- **No imports, in either language.** `ImportKind::Import`/`From`/`Require`
  are still inert, and neither query has an import pattern. `import { X } from
  "y"` is invisible to rr today. This is the largest remaining gap and the
  obvious next piece of work. *(Landed in #40 as a second extraction pass:
  `tree-sitter-tags` accepts only its own capture vocabulary and there is no
  channel to widen, so each tier-2 language gained a plain `tree_sitter::Query`
  (`python-imports.scm`, `typescript-imports.scm`) run over its own second
  parse, and `Import::path` holds the specifier verbatim with the leaf in a
  separate `Import::name` field. Resolving either is a module graph and stays
  out of scope; `resolves_by_path` keeps answering false because a specifier
  is not a path the index's resolver follows.)*
- **`export` is not visibility.** A non-exported TypeScript declaration reads
  as `Public`, because visibility here is the `#` prefix and the modifier, not
  the export list.
- **No test signals for TypeScript at all.** `describe`/`it` are calls, and a
  call's meaning depends on what it resolves to. File naming already answers
  the question through `Lang::path_indicates_test`.
- **`Property` is never produced for Python.** `@property` is a decorator
  whose meaning depends on what it resolves to, and the tags tier does not
  resolve.
- **Not covered by the TypeScript query**, and stated in its header: `var` at
  module scope, enum members, destructured bindings, computed and string-named
  members, string-named ambient modules, and JSX component references.

### Four imprecisions worth knowing about

1. **A tags-tier span starts at the declaration, not at its documentation.**
   The span is the definition node's own range, so a decorator — a child of
   that node — is inside it and a preceding comment is not. Rust's
   hand-written extractor reaches back over both. The visible consequence is
   that a container's `body_idents` pick up its members' doc prose: a member's
   comment lies inside the container's span but outside the member's own, so
   the exclusion that removes members from their container misses it. Fixing
   it means backward-lexing in shared code, which is a real change and not
   this milestone's.

2. **Non-exported module bindings are indexed but not documented.** Those two
   patterns are anchored on a named parent, and a comment run inside a
   parent-anchored pattern only ever matches the *first* run under that
   parent — the query engine walks the parent's children once and does not
   restart the sequence. A documentation rule that holds until something above
   it is commented is worse than no rule, so those patterns read no comments
   at all. Exported bindings go through an anonymous group and have no such
   limit.

3. **A multi-line arrow binding stays a `Variable`.** The signature ends at
   the first line break, so `const later =\n    (x) => x;` offers no evidence
   that it is a function and none is invented.

4. **Python and TypeScript disagree about receiver calls, and Python is the
   one that is wrong.** `python.scm` maps an `attribute` call to
   `@reference.call`, so `obj.method()` becomes a `ReferenceKind::Call` and
   goes through the resolver, where it can bind to an unrelated same-named
   free function. TypeScript's equivalent is `ReferenceKind::MethodCall`,
   which `index::build` maps straight to `Resolution::Unresolved` — declining
   a resolution rr cannot make, which is what Rust does too. Correcting Python
   means bumping `PYTHON_EXTRACTOR_VERSION`, so it is named here and left for
   the change that can afford the reparse.

### What a Rust-only repository sees

Nothing, in every way that matters: same defs, same references, same
`Complete` status, `RUST_EXTRACTOR_VERSION` still 3, `FACT_SCHEMA_VERSION`
still 4, `SNAPSHOT_SCHEMA_VERSION` still 7. No Rust golden moved.

One thing does change, and it has to. `meta.discovery_digest` mixes in the
language allowlist and the per-language extractor versions, so growing the
registry moves the digest and the next `rr map` rebuilds. That is the
mechanism working: a repository that *already holds* `.ts` files needs to be
told its snapshot is now missing them, and a digest that ignored the language
set would leave it serving an incomplete index until some unrelated file
happened to change. The cost is one rebuild per repository, once.

### The measured cost of the next language

The one-time cost is paid. What a fifth language costs now:

- One `=`-pinned grammar crate and one `LanguageSpec`, roughly 40 lines.
- A `tags.scm`, if the upstream one is unusable. Python's is 55 lines;
  TypeScript's is 318, and TypeScript is the bad case — upstream's file is a
  supplement to `tree-sitter-javascript`'s and carries no function, class,
  method or field pattern at all, so the whole query had to be written.
- A `refine` function, between zero lines and TypeScript's 60, depending on
  how much of the language the query cannot say.
- Fixtures and a golden.

TypeScript came to 160 lines of extractor code across both specs, a 318-line
query, and 169 lines of unit tests. The `refine` hook it needed is shared and
already paid for. A language whose upstream `tags.scm` is usable and whose
kinds map cleanly should cost a small fraction of that.

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
