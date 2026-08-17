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
//! ## `rr query`
//!
//! - **v1** — pinned by `crates/rr-cli/tests/query.schema.json`; closed, and
//!   versioned in its title rather than by a key of this name.
