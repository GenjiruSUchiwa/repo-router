---
title: "M1-05 · Lexical fingerprints: token normalization"
labels: ["milestone:M1", "type:core"]
---

## Why
The bridge between human vocabulary ("token verification") and
identifiers (`verify_token`). The quality of this normalization bounds
the recall of all lexical ranking.

## What
`rr-core::lex`: pure function `terms(&SymbolRecord) -> SmallVec<TermId>`
and its query counterpart `query_terms(&str) -> Vec<TermId>`.

## How
1. Splitters: camelCase, PascalCase, snake_case, kebab-case, path
   components, attached digits (`utf8Decode` → `utf8`, `decode`). Lowercase.
2. Sources of a symbol's terms, **weighted by field** (the weight lives
   in issue 08; here we just tag the provenance): name, qualified name,
   path, signature identifiers, body identifiers, callees.
3. Stemming: conservative and English only — suffixes `s`, `ing`, `ion`
   → short form ONLY if the short form already exists in the corpus
   (`verification` → `verify` via a table of common pairs; never stem a
   code identifier). When in doubt: do not stem.
4. Interning: global `term → TermId (u32)` table serialized in the
   snapshot; everywhere else we manipulate u32s.
5. Query stop-words: `where`, `is`, `the`, `how`, `does`, `handled`...
   (short hard-coded list, ~40 words).

## Pseudo-code
```rust
fn split(ident: &str) -> impl Iterator<Item=&str> {
    // "JWTValidator" -> ["jwt", "validator"] ; "verify_token" -> ["verify","token"]
    boundaries(ident).map(|s| s.to_lowercase())
}
```

## Best practices
- 100% pure functions ⇒ exhaustive table-driven tests
  (`assert_eq!(split("XMLHttpRequest2"), ["xml","http","request","2"])`).
- Document every rule with the example that motivated it.

## Acceptance criteria
- [ ] Table of 30 split cases passes (including glued acronyms, digits, unicode).
- [ ] `query_terms("where is token verification handled?")` = `[token, verification, verify]` (stable order).
- [ ] No String allocation on the hot path (verify in the bench).
