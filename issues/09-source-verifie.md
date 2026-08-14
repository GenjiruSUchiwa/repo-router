---
title: "M2-09 · `--source`: bounded span, hash-verified, refused if stale"
labels: ["milestone:M2", "type:core", "contract"]
---

## Why
The central promise: never stale source. The Radar observation (§5)
settled our spec debate: refusing + pointing to refresh is safer and
simpler than re-parsing on the fly. We copy that choice, with a
diff-based relocation option as a differentiating bonus.

## What
`rr query ... --source` and the `rr-core::verify` module.

## How
1. Verification: current OID of the file (via rr-git, free if the working
   tree is clean) vs `anchor.indexed_oid`.
   - identical → read only the span's lines (not the whole file in memory
     if avoidable), bounded to `MAX_SOURCE_LINES = 120` with a
     `SOURCE TRUNCATED (N more lines)` marker when applicable;
   - different → refusal:
     ```text
     STALE SOURCE (no content returned): src/auth/token.rs changed since indexing; run `rr refresh`
     ```
     exit 4 (unlike Radar which returns 0 — an agent must be able to
     branch without parsing the text).
2. `--relocate` option (bonus, may slip to M4): if stale, compute the diff
   indexed blob ↔ current content (gix diff), map the span's lines through
   the hunks; if the mapping is clean (whole span in an unchanged zone or
   shifted by a constant offset) → serve the relocated span marked
   `SOURCE SPAN (relocated)`; otherwise normal refusal. Never by default.
3. Text output (contract):
   ```text
   FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
   SOURCE SPAN (verified): src/auth/token.rs:9-15
   SOURCE COMPLETE
   ---
   <code>
   ```
   `--json`: same data + `"verified": true|"relocated"|false`.

## Best practices
- Verify re-reads the file at the moment T of the response — no snapshot
  data leaves for the agent without revalidation (spec §5.2, word for word).
- Race test: modify the file between the lookup and the read → the final
  hash is authoritative (re-read the OID AFTER reading the served bytes).

## Acceptance criteria
- [ ] Edit without refresh → STALE, no content, exit 4.
- [ ] After refresh → correct relocated span (2-line offset test).
- [ ] Span > 120 lines → truncated with marker.
- [ ] `--relocate` serves the span after inserting comments at the top.
