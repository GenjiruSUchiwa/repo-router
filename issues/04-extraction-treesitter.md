---
title: "M1-04 · Tree-sitter extraction: definitions, references, imports (Rust first)"
labels: ["milestone:M1", "type:core"]
---

## Why
This is the raw material for all routing. The lesson from spec §5.5:
extract the useful minimum, discard the AST immediately.

## What
`rr-core::parser`: for a file, produce `Facts { defs, refs, imports }`.
**A single language in V1: Rust** (the fixture's language and our own —
dogfooding). Python and TypeScript arrive at the end of M2, once the
pipeline is proven.

## How
1. `tree-sitter` + `tree-sitter-rust` crates (pinned versions — the grammar
   version is part of `EXTRACTOR_VERSION`, issue 03).
2. Write the extractions as **`.scm` query files**
   (embedded via `include_str!`), not manual traversal code:
   declarative, testable, and the standard pattern of the ecosystem.
3. Extract per definition: name, qualified name if derivable (module path),
   kind (fn/struct/enum/trait/impl-fn/const/mod), byte+line spans
   (start/end), signature identifiers, body identifiers, outgoing calls
   (callee name + line), test marker (`#[test]`, `tests/` path).
4. References: calls and `use` with line. **Do not resolve** here — we
   store names; cheap resolution comes in issues 06/14 (lesson from the
   observation: Radar accepts 12 unresolved refs and displays them).
5. Robustness: a file that fails to parse ⇒ degraded `Facts`
   (lexical identifiers only) + error counter, never an abort of the
   whole map.

## Pseudo-code (.scm query, excerpt)
```scheme
(function_item name: (identifier) @def.name) @def.body
(call_expression function: [(identifier) @call.name
                            (field_expression field: (field_identifier) @call.name)])
(use_declaration argument: (_) @import.path)
```

## Best practices
- Golden tests: input Rust file → YAML snapshot of the expected facts
  (`insta` crate). Any grammar evolution becomes a readable diff in review.
- Budget: target < 1 ms per average file in release (bench right away,
  criterion, 300-line file).

## Acceptance criteria
- [ ] On `fixtures/rust-basic`, extracts the 10 expected symbols with correct spans.
- [ ] `verify_token` correctly carries `decode_jwt` and `now` as outgoing calls.
- [ ] File with a syntax error: the full map still succeeds.
- [ ] Insta golden tests in place.
