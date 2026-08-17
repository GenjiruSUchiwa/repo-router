# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/GenjiruSUchiwa/repo-router/releases/tag/v0.1.0) - 2026-08-17

### Added

- *(core, cli)* ship the remaining tags-tier languages
- *(core)* learn a local greppable route cache in .rr/ROUTES.md
- *(core, git, cli)* the JSON channel reports the run, not the snapshot
- *(core, cli)* idempotent rr init for the agent navigation contract
- *(core, git, cli)* ship the three unshipped #11 review decisions ([#49](https://github.com/GenjiruSUchiwa/repo-router/pull/49))
- *(core)* dead vocabulary audit, and the ten unproduced variants it retires ([#41](https://github.com/GenjiruSUchiwa/repo-router/pull/41)) ([#48](https://github.com/GenjiruSUchiwa/repo-router/pull/48))
- *(core)* TypeScript and TSX, and the verdict on #31's vocabulary ([#39](https://github.com/GenjiruSUchiwa/repo-router/pull/39))
- add generic tree-sitter tags extractor ([#38](https://github.com/GenjiruSUchiwa/repo-router/pull/38))
- *(core, git, cli)* write issue #11's maps under the refresh guard
- *(core, git, cli)* deterministic incremental refresh with a Git-gated fast path ([#28](https://github.com/GenjiruSUchiwa/repo-router/pull/28))
- *(core, git, cli)* bounded OID-verified source for rr query --source ([#26](https://github.com/GenjiruSUchiwa/repo-router/pull/26))
- *(core, cli)* report when the candidate cap cut through a tie ([#25](https://github.com/GenjiruSUchiwa/repo-router/pull/25))
- *(core, cli)* calibrated per-field BM25 lexical fallback (fixes #8) ([#23](https://github.com/GenjiruSUchiwa/repo-router/pull/23))
- *(core, cli)* exact query routing and dual text/JSON contract (fixes #7) ([#22](https://github.com/GenjiruSUchiwa/repo-router/pull/22))
- build deterministic index snapshot ([#21](https://github.com/GenjiruSUchiwa/repo-router/pull/21))
- bootstrap monorepo workspace, CI and basic hygiene (fixes #1) ([#15](https://github.com/GenjiruSUchiwa/repo-router/pull/15))

### Fixed

- *(core, cli, git)* name the snapshot by its bytes, and re-ask the HEAD question
- *(core, cli)* a memo is a claim about the bytes its value came from
- *(core, cli)* a route is stale on the corpus, and replays what it stored
- *(core)* read what each language actually declares
- *(cli)* a moved HEAD is not a stale index
- *(git)* the status scan runs the clean filter too
- *(git)* a clean filter that stops reading does not kill rr
- *(cli)* query anchors include the absorbed comment run
- *(cli)* release the publication claim for every signal that ends a run
- *(cli, git)* let go of the publication claim before SIGPIPE takes the run
- *(cli)* restore SIGPIPE so a closed pipe does not panic
- *(core, cli)* length-prefix --source with SOURCE BYTES
- *(core, cli)* preserve CRLF, key the restart note on the skill, write atomically
- *(core, cli)* report what rr init actually did, and to which file
- *(core, git, cli)* six defects from the review pass, with the tests

### Other

- remove orphaned blank lines after inline comments cleanup
- remove all inline comments across repository
- *(core, cli)* retire docs/, and file each survivor where its consumer is
- *(cli)* cargo fmt the new interrupt test
- *(cli)* cut the SIGPIPE novels and drop inline comments
- *(cli)* drop the diagnose SIGPIPE novel
- *(cli)* say the non-unix SIGPIPE stub is a no-op
- *(core, cli)* name the branch where there is no fence
- *(cli)* make verification part of publishing, and share the test harness
