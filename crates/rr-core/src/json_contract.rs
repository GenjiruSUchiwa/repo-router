//! The rule every `--json` surface follows.
//!
//! Every `--json` surface is a contract with a program, not a formatting
//! option. This module is the rule those contracts follow. It is normative for
//! surfaces that already ship and for surfaces that do not exist yet.
//!
//! It declares nothing. It lives beside the constants it governs rather than in
//! a file elsewhere in the tree, so that a rule about what a version number
//! means is read from the same place the number is defined.
//!
//! # The version key
//!
//! New surfaces publish `schema_version`, an integer, as the first key of the
//! object.
//!
//! `rr query` is the one exception and stays as it is. It publishes `v` and its
//! shape is pinned by `crates/rr-cli/tests/query.schema.json` at a stable
//! `$id`, where every object is `additionalProperties: false` and every
//! `required` list names `v`. Renaming that key, or adding `schema_version`
//! beside it, changes a published schema at a pinned URL and breaks every
//! validator pointed at it — to publish two keys that mean the same thing for
//! ever.
//!
//! # One version per surface
//!
//! A version belongs to one command's object, not to the binary.
//!
//! | Constant | Surfaces | Value |
//! |---|---|---|
//! | [`REFRESH_SCHEMA_VERSION`](crate::REFRESH_SCHEMA_VERSION) | `rr refresh`, `rr map` | 4 |
//! | [`STATUS_SCHEMA_VERSION`](crate::STATUS_SCHEMA_VERSION) | `rr status` | 3 |
//! | [`INIT_SCHEMA_VERSION`](crate::agent::INIT_SCHEMA_VERSION) | `rr init` | 1 |
//! | [`CHECK_SCHEMA_VERSION`](crate::CHECK_SCHEMA_VERSION) | `rr check` | 1 |
//! | [`IMPACT_SCHEMA_VERSION`](crate::IMPACT_SCHEMA_VERSION) | `rr impact` | 1 |
//!
//! `rr refresh` and `rr map` share one because they share a report: `rr map` is
//! `rr refresh --full` under another name.
//!
//! A surface's version is seeded at the highest value that surface has already
//! published, never restarted at 1. `rr status` published 3 before it had a
//! constant of its own, so its constant starts at 3.
//!
//! A shared number is not a smaller version of this rule, it is a broken one: a
//! field added to `refresh` under a shared number bumps the version a
//! `rr status` consumer sees for an object that did not change, and a consumer
//! trained to see bumps that mean nothing cannot act on the one that does.
//!
//! # What a bump means
//!
//! **rr's report objects are open.** A consumer must read by key and ignore
//! keys it does not recognize. In exchange:
//!
//! - **Adding a key does not bump the version.** It cannot break a consumer
//!   that honours the paragraph above.
//! - **A bump means a consumer that ignored it would now be wrong.** A key
//!   removed, renamed, or retyped. A key whose meaning changed under the same
//!   name. A new spelling in a published enum, which breaks an exhaustive
//!   match.
//! - Versions never decrease and are never reused.
//!
//! Openness is what makes that rule available. A closed object — `rr query`,
//! whose schema says `additionalProperties: false` — must bump on every
//! addition, because a validator rejects the new key. That is the trade each
//! surface makes once, and the report surfaces make it in favour of additive
//! evolution.
//!
//! # The frozen v1 query surface
//!
//! **v1 of `rr query` is closed.** No member is added, removed, renamed or
//! reordered; no variant is added to a serialized enum; no exit code is
//! re-meant; no byte of a text marker changes. Anything else ships as `v: 2`
//! beside v1 for one minor release, so that a consumer has a release in which
//! both answers are available and it can be moved without being broken first.
//!
//! Five things are frozen, and each has a test rather than a promise:
//!
//! 1. the `v: 1` literal and the three response shapes
//!    ([`render_json`](crate::render::render_json));
//! 2. `crates/rr-cli/tests/query.schema.json` byte for byte — its `$id`, its
//!    title `RepoRouterQueryResultV1`, and `additionalProperties: false` at
//!    every object;
//! 3. the exit codes [`QueryResult::exit_code`](crate::result::QueryResult::exit_code)
//!    chooses — `0` direct, `2` candidates, `3` none, `4` refused — plus the `1`
//!    the CLI returns for an error, which must never collide with the `2` that
//!    means the answer arrived and is not one to act on;
//! 4. the closed inventory of text markers in [`crate::render::marker`];
//! 5. this list.
//!
//! The trade named under *What a bump means* is why the promise has to be this
//! strict. The report surfaces are open, so they may grow a key. `rr query` is
//! closed, so it may not: an added member does not extend the answer, it makes
//! every validator reject the answer — silently, inside an agent's parser, on a
//! response whose other members are all correct. That is a worse failure than a
//! version bump, and it is why v1 grows a sibling instead of growing a member.
//!
//! # Enum spellings
//!
//! A published enum spelling is part of the contract. In code, `as_str()`
//! returns that spelling and is identical to the serde name; `as_text()`
//! returns the human phrase for the summary line. A test asserts the first for
//! every published enum; nothing may serialize the second.
//!
//! # Version log
//!
//! ## `rr refresh` / `rr map`
//!
//! - **1** — the original counters.
//! - **2** — adds `tags`.
//! - **3** — adds `tags_recovered`.
//! - **4** — `outcome` is the outcome of the *run* rather than of the snapshot
//!   alone, and gains the spelling `refused`; the snapshot's own verdict remains
//!   at `snapshot_updated`. The object also gains `text`, and a refusal now
//!   emits an object on stdout instead of nothing. Only the first of those is a
//!   breaking change; under the rule above the other two would not have bumped
//!   anything.
//!
//! Versions 1–3 were issued under a stricter rule that treated the object as
//! closed, which is why an added counter bumped them. That history stands as it
//! happened; the rule changed here, not the past.
//!
//! ## `rr status`
//!
//! - **3** — the value it has published from the beginning, under the shared
//!   constant it no longer uses.
//!
//! ## `rr init`
//!
//! - **1** — the surface as #13 shipped it. It published this value under the
//!   key `v` for the few hours between PR #50 and this change; the object is
//!   otherwise unchanged, so the version does not move.
//!
//! ## `rr check`
//!
//! - **1** — the surface as #14 shipped it: `schema_version`, `command`,
//!   `status`, `counts`, `diagnostics`, and `quality` when a `--quality-report`
//!   was adjudicated. A rule id added later adds no key and does not bump this;
//!   a *rule id spelling* removed or re-meant does, because a pipeline greps for
//!   those the way a consumer matches on an enum.
//!
//! ## `rr impact`
//!
//! - **1** — the surface as #14 shipped it: `schema_version`, `command`,
//!   `status`, `base`, `target`, `depth`, `changed_files`,
//!   `changed_definitions`, `direct_edges`, `affected`, `dependencies`,
//!   `cycles`, `tests`, `resolution`, `diagnostics`, `unfollowed_imports`. A
//!   counter added to `resolution` adds a key and does not bump this; a widening
//!   of what counts as an edge does, because a consumer that read `affected` as
//!   "resolved edges only" would now be wrong about every entry.
//!
//! ## `rr query`
//!
//! - **v1** — pinned by `crates/rr-cli/tests/query.schema.json`; closed, and
//!   versioned in its title rather than by a key of this name.
