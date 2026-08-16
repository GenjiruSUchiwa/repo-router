# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/GenjiruSUchiwa/repo-router/releases/tag/v0.1.0) - 2026-08-16

### Added

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

- *(core, git, cli)* six defects from the review pass, with the tests

### Other

- *(cli)* make verification part of publishing, and share the test harness
