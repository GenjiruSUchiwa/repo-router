---
title: "M3-13 · `rr init`: navigation contract + Claude Code SKILL.md"
labels: ["milestone:M3", "type:agent-interface"]
---

## Why
Lesson #2 from the observation: the instruction contract injected into the
agent IS half the product. Without it, the agent does not know that MAP.md,
SYMBOLS.md and `rr query` exist, and falls back to grep.

## What
`rr init` writes: a navigation block in `CLAUDE.md`/`AGENTS.md`
(created or updated between markers), `.claude/skills/rr-navigation/SKILL.md`,
and a commented `rr.toml` config file.

## How
1. Navigation block between `<!-- rr:begin navigation -->` / `<!-- rr:end -->`
   (idempotent: regenerating replaces the block, touches nothing else).
   Content — the procedure, in this order:
   1. `rr query "<full task>"` first (zero model calls); a
      `FINAL SOURCE ANCHOR` = the answer, copy exactly, stop;
   2. otherwise grep `.rr/ROUTES.md` (`[ok]` lines) then `.rr/SYMBOLS.md`;
   3. otherwise read MAP.md; multiple reads IN PARALLEL;
   4. after manual resolution: `rr route add "<task>" file#symbol`;
   5. maps route, source answers — always confirm in the code;
   6. if stale: `rr refresh` then retry once.
2. SKILL.md: same procedure in the Claude Code skill format (frontmatter
   name/description triggers: "navigate", "where is", "who calls").
3. `rr.toml`: additional excludes, languages, MAP budget — only keys that
   are actually honored (Radar pattern: document only what is real).
4. Detection: if `CLAUDE.md` exists → write there; otherwise `AGENTS.md`;
   otherwise create it. `--target <file>` to force.

## Best practices
- The contract text lives in `rr-cli/contracts/*.md` (include_str!), not
  in the code — re-readable, diffable, translatable.
- Every sentence of the contract must save the agent tokens: re-read each
  line asking "does this change its behavior?".

## Acceptance criteria
- [ ] `rr init` twice → second run is a no-op (idempotence).
- [ ] Content outside the markers never modified (test with an existing CLAUDE.md).
- [ ] Claude Code session on the fixture: the agent uses `rr query` as its
      first reflex (manual test documented in the PR).
