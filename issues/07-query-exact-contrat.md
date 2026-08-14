---
title: "M2-07 · `rr query`: exact routing + dual text/JSON contract"
labels: ["milestone:M2", "type:core", "contract"]
---

## Why
First moment where an agent can consume the tool. The output contract is
a public commitment: we freeze it here and never break it again.

## What
`rr query "<question>"`: detection of explicit identifiers, exact lookup,
text output (compatible in spirit with observed Radar) + stable `--json`.

## How
1. Explicit identifier detection in the query: conservative regex
   (`[A-Za-z_][A-Za-z0-9_]*` containing `_` OR mixed case OR present as-is
   in the exact index; paths `a/b.rs`, forms `Foo::bar`, `x.y`).
2. Lookup `exact[name]`:
   - 1 result → direct answer;
   - N results → tie-break by overlap of the other query terms with
     path/qualified; if still ambiguous → candidates (max 3);
   - 0 → lexical pipeline (issue 08).
3. Text contract (stdout, nothing else):
   ```text
   FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
   ```
   Ambiguous: `source candidates:` + one line per anchor. Not found:
   `NO ANCHOR (index has no match); try: rr map` — a deliberate divergence
   from Radar which dumps the map (observation §4): our fallback stays tiny.
4. `--json` contract (versioned schema, field `v: 1`):
   ```json
   {"v":1,"result":"direct","anchor":{"path":"src/auth/token.rs","symbol":"verify_token","lines":[9,15]},"confidence":1.0}
   ```
   `result` variants: `direct` | `candidates` | `none`. Write the JSON Schema
   in `docs/query.schema.json`, tested in CI against the real output.
5. Exit codes: 0 = direct, 2 = candidates, 3 = none, 1 = execution error
   (deliberate divergence: Radar returns 0 everywhere; a scripted agent
   deserves better).

## Best practices
- The text contract and the JSON come from the SAME internal structure
  (`QueryResult`) via two renderers — never two computation paths.
- Contract tests: `tests/contract/*.txt` files compared verbatim.

## Acceptance criteria
- [ ] `rr query verify_token` → direct anchor, exit 0.
- [ ] `rr query session` (fixture) → 2 candidates, exit 2.
- [ ] `--json` validates against the schema in CI.
- [ ] Warm latency < 10 ms on the 10,000-file corpus.
