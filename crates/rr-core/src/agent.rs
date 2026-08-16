//! What `rr` tells an agent about itself.
//!
//! One constant, installed into three files. It is a constant rather than a
//! template because everything in it is a fact about the CLI — exit codes,
//! output lines, artifact paths — and a fact that varies per repository is a
//! fact that will eventually be wrong in one of them.
//!
//! Every line quoted below is quoted from the code that prints it, not from
//! memory. The tests in this module are what keep them equal.

use crate::text::BlockMarkers;

/// Where the project-scoped Claude Code skill is installed.
///
/// The one path in this crate that is not derivable from this repository. If
/// Claude Code reads project skills from somewhere else, this is the whole fix.
///
/// The three are one path spelled at three depths, and the test
/// `the_skill_paths_are_one_path` is what keeps them from drifting apart: a
/// caller creating `SKILLS_ROOT` and writing `SKILL_PATH` has to be creating the
/// directory that file lands in.
pub const SKILLS_ROOT: &str = ".claude/skills";
/// The directory holding rr's own skill.
pub const SKILL_DIR: &str = ".claude/skills/rr";
/// The skill file itself.
pub const SKILL_PATH: &str = ".claude/skills/rr/SKILL.md";

/// The agent-instruction files `rr init` manages.
pub const AGENT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

pub const CONTRACT_BEGIN_MARKER: &str = "<!-- rr:begin agent contract -->";
pub const CONTRACT_END_MARKER: &str = "<!-- rr:end agent contract -->";

pub const CONTRACT_MARKERS: BlockMarkers = BlockMarkers {
    begin: CONTRACT_BEGIN_MARKER,
    end: CONTRACT_END_MARKER,
};

/// The version of the contract text, for the skill's stamp.
pub const AGENT_FORMAT_VERSION: u32 = 1;

/// The managed region installed into `AGENTS.md` and `CLAUDE.md`.
pub const CONTRACT_BLOCK: &str = concat!(
    "<!-- rr:begin agent contract -->\n",
    r#"## Finding code in this repository

This repository is indexed by `rr`. Ask it before you grep. `rr` answers with a
single anchor you can open, and it is the only thing here that knows which of
several same-named symbols is the one you want.

**Ask a question.**

```
rr query "where is the auth token verified"
```

It prints one line and exits `0`:

```
FINAL SOURCE ANCHOR (copy exactly): src/auth/token.rs#verify_token
```

Copy the anchor exactly. It is `path#symbol` with `%`, `#` and control bytes
percent-encoded, and it is what every other `rr` command accepts.

**The exit codes are the protocol.**

| code | output | what to do |
| --- | --- | --- |
| `0` | `FINAL SOURCE ANCHOR (copy exactly): <anchor>` | open it |
| `2` | `source candidates:` then up to three numbered anchors | pick one, or re-ask with more words |
| `3` | `NO ANCHOR (index has no match)` or `NO ANCHOR (confidence too low)` | re-ask, add `--path`, or run `rr map` if this repository was never indexed |
| `4` | a `STALE SOURCE` or other refusal line | run `rr refresh`, then ask again |
| `1` | `rr: query: <reason>` on stderr | read the reason; `index is stale; run 'rr refresh'` is the common one |

Those five are the only codes `rr query` chooses. If you see `141`, nothing went
wrong with `rr`: you piped it into something that stopped reading, such as
`rr query "…" --source | head -5`, and `rr` died on `SIGPIPE` the way every
well-behaved Unix program does. Read the whole output, or pipe into something
that consumes it.

**Four flags, and no others you need.**

- `--source` returns the anchor's own lines, verified against the indexed
  content. It refuses rather than returning bytes it cannot vouch for.
- `--json` emits one object instead of prose.
- `--path src/auth/token.rs` narrows to one file.
- `--explain` reports what the ranker did.

## Reading the repository without running anything

- **`MAP.md`** — one in every indexed directory, committed. It lists that
  directory's public API and links to its children. Start at the repository root
  and follow the links down. One directory is always one file: when a section
  holds more than the map budget, the page keeps the entries that fit and says
  so on the line `+ N more <things> omitted by the map budget`. Ask `rr query`
  for anything the page says it dropped.
- **`.rr/SYMBOLS.md`** — every indexed symbol, one TAB-separated row each:
  `symbol`, `visibility`, `map`, `source`, `line`, `api_hash`. Machine-local, not
  committed.

## What not to edit

Anything between an rr begin marker and its matching end marker is regenerated,
and an edit there is lost on the next `rr refresh` or `rr init`. Everything
outside those markers is yours and is preserved exactly.

One region is yours *inside* a generated file: the `## Purpose` slot of a
`MAP.md`, between `<!-- rr:slot purpose max=160 -->` and `<!-- /rr:slot -->`.
Write what the directory is for, in under 160 bytes. Nothing else in a `MAP.md`
survives a refresh.
"#,
    "<!-- rr:end agent contract -->\n",
);

/// The start of the one line that carries the skill's stamp.
///
/// One constant, because [`unstamped`] and [`stamp_value`] have to agree about
/// which line they are looking at: a prefix that matched in one and not the
/// other would make a file rr wrote fail to verify as its own.
const STAMP_PREFIX: &str = "    generated_hash: \"";

/// The stamp line as it looks before the digest is written into it.
const BLANK_STAMP_LINE: &str = "    generated_hash: \"\"";

/// `CONTRACT_BLOCK` without its delimiter lines, under the skill title.
fn skill_body() -> String {
    let inner = CONTRACT_BLOCK
        .strip_prefix(CONTRACT_BEGIN_MARKER)
        .and_then(|text| text.strip_prefix('\n'))
        .and_then(|text| text.strip_suffix('\n'))
        .and_then(|text| text.strip_suffix(CONTRACT_END_MARKER))
        .and_then(|text| text.strip_suffix('\n'))
        .unwrap_or(CONTRACT_BLOCK);
    format!("# rr — ask this repository where its code is\n\n{inner}\n")
}

const DESCRIPTION: &str = "Find code in this repository by asking a question \
    instead of grepping. Use when you need the file and symbol that implements \
    something, when a grep would return too many hits to read, or when you need \
    a source excerpt verified against the index.";

/// The generated `SKILL.md`, ready to stamp.
///
/// `description` is written for a router that has to decide whether to load
/// this skill from one sentence, so it leads with the trigger rather than with
/// the tool's name.
fn skill_unstamped() -> String {
    let body = skill_body();
    format!(
        "---\n\
         name: rr\n\
         description: {DESCRIPTION}\n\
         metadata:\n\
         \x20 rr:\n\
         \x20   format: {AGENT_FORMAT_VERSION}\n\
         \x20   generated_hash: \"\"\n\
         ---\n\
         \n\
         {body}"
    )
}

/// The file with its own stamp slot emptied.
///
/// A hash cannot cover itself, so it covers the file it will be written into
/// with the slot blank — the same trick `text::render::seal` uses
/// (`text/render.rs:398-408`). Done by rewriting the one line rather than by
/// searching for a digest, because the file this must be able to hash first is
/// the one that has no digest in it yet.
///
/// Returns `None` when there is no stamp line at all, which is the primary
/// signal that somebody else wrote this file.
fn unstamped(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut found = false;
    for line in text.lines() {
        if line.starts_with(STAMP_PREFIX) && line.ends_with('"') {
            out.push_str(BLANK_STAMP_LINE);
            out.push('\n');
            found = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    found.then_some(out)
}

/// The digest stored in `generated_hash`, if that line is present.
fn stamp_value(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(STAMP_PREFIX) {
            if let Some(value) = rest.strip_suffix('"') {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// The `SKILL.md` this version of rr installs.
#[must_use]
pub fn skill_document() -> String {
    let blank = skill_unstamped();
    let digest = crate::text::Digest::of_bytes(blank.as_bytes());
    blank.replacen(
        BLANK_STAMP_LINE,
        &format!("{STAMP_PREFIX}{}\"", digest.to_text()),
        1,
    )
}

/// Whether this text is a `SKILL.md` rr wrote and nobody has edited.
///
/// The question `rr init` has to answer before it overwrites a file. A `false`
/// here is not an accusation — a user is entitled to write their own skill at
/// that path — it is the reason `rr init` reports the path and leaves it alone.
#[must_use]
pub fn is_rr_written_skill(text: &str) -> bool {
    let Some(blank) = unstamped(text) else {
        return false;
    };
    let Some(stored) = stamp_value(text) else {
        return false;
    };
    crate::text::Digest::parse(&stored)
        .is_ok_and(|stored| stored == crate::text::Digest::of_bytes(blank.as_bytes()))
}

/// One row of the `rr init` report.
///
/// `outcome` is one of `created`, `updated`, `unchanged`, `refused`; `reason` is
/// serialized only when present.
#[derive(Debug, serde::Serialize)]
pub struct InitTarget<'a> {
    pub path: &'a str,
    pub outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
}

#[derive(Debug, serde::Serialize)]
struct InitReport<'a> {
    v: u32,
    targets: &'a [InitTarget<'a>],
}

/// The `--json` report, as one line.
///
/// Serialized rather than concatenated so that escaping, and the field order the
/// struct declares, are the serializer's problem rather than this function's.
#[must_use]
pub fn render_init_json(targets: &[InitTarget<'_>]) -> String {
    let report = InitReport { v: 1, targets };
    // A struct of `&str` has no map key, no non-finite float and no non-string
    // key, which are the only three things `to_string` can fail on.
    serde_json::to_string(&report).unwrap_or_else(|_| String::from("{\"v\":1,\"targets\":[]}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileId;
    use crate::result::{Candidate, Confidence, NoneReason, Pipeline, QueryResult, TargetId};
    use crate::text::{apply_block, MAP_FILE_NAME, SYMBOLS_PATH};
    use crate::verify::{SourceResult, SourceStatus};
    use smallvec::smallvec;

    #[test]
    fn the_skill_paths_are_one_path() {
        assert_eq!(SKILL_DIR, format!("{SKILLS_ROOT}/rr"));
        assert_eq!(SKILL_PATH, format!("{SKILL_DIR}/SKILL.md"));
    }

    /// The stamp `skill_document` writes has to be the stamp `unstamped` blanks;
    /// otherwise a file rr just wrote would not verify as its own.
    #[test]
    fn the_blank_stamp_is_the_line_the_renderer_emits() {
        assert!(skill_unstamped().contains(BLANK_STAMP_LINE));
        assert!(BLANK_STAMP_LINE.starts_with(STAMP_PREFIX));
    }

    #[test]
    fn the_contract_block_starts_and_ends_with_its_markers() {
        assert!(CONTRACT_BLOCK.starts_with("<!-- rr:begin agent contract -->\n"));
        assert!(CONTRACT_BLOCK.ends_with("<!-- rr:end agent contract -->\n"));
    }

    #[test]
    fn the_contract_block_contains_no_bare_marker_line() {
        let lines: Vec<&str> = CONTRACT_BLOCK.lines().collect();
        assert_eq!(lines.first().copied(), Some(CONTRACT_BEGIN_MARKER));
        assert_eq!(lines.last().copied(), Some(CONTRACT_END_MARKER));
        for line in &lines[1..lines.len() - 1] {
            assert_ne!(*line, CONTRACT_BEGIN_MARKER);
            assert_ne!(*line, CONTRACT_END_MARKER);
        }
    }

    #[test]
    fn applying_the_contract_twice_is_a_fixed_point() {
        let once = apply_block(None, CONTRACT_MARKERS, CONTRACT_BLOCK).unwrap();
        let twice = apply_block(Some(&once), CONTRACT_MARKERS, CONTRACT_BLOCK).unwrap();
        assert_eq!(twice, once);
    }

    #[test]
    fn a_freshly_rendered_skill_verifies_its_own_stamp() {
        assert!(is_rr_written_skill(&skill_document()));
    }

    #[test]
    fn an_edited_skill_does_not_verify() {
        let mut edited = skill_document();
        edited.push('!');
        assert!(!is_rr_written_skill(&edited));
    }

    #[test]
    fn a_skill_with_no_stamp_line_does_not_verify() {
        assert!(!is_rr_written_skill("# somebody else's skill\n"));
    }

    #[test]
    fn a_skill_with_a_malformed_digest_does_not_verify() {
        let skill = skill_document();
        let stored = stamp_value(&skill).expect("a freshly rendered skill has a stamp");
        let malformed = skill.replacen(
            stored.as_str(),
            "blake3:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            1,
        );
        assert!(!is_rr_written_skill(&malformed));
    }

    #[test]
    fn the_stamp_is_stable_across_calls() {
        assert_eq!(skill_document(), skill_document());
    }

    #[test]
    fn the_skill_body_and_the_agent_block_describe_one_tool() {
        let skill = skill_document();
        for line in CONTRACT_BLOCK.lines() {
            if line.contains("rr query") {
                assert!(
                    skill.contains(line),
                    "skill is missing contract line: {line}"
                );
            }
        }
    }

    #[test]
    fn every_documented_exit_code_matches_the_result_type() {
        let mut documented: Vec<u8> = CONTRACT_BLOCK
            .lines()
            .filter_map(|line| {
                let mut cells = line.split('|').skip(1);
                let code = cells.next()?.trim();
                let code = code.strip_prefix('`')?.strip_suffix('`')?;
                code.parse().ok()
            })
            .collect();
        documented.sort_unstable();
        assert_eq!(documented, [0, 1, 2, 3, 4]);

        let target = TargetId::File(FileId::from_index(0));
        let direct = QueryResult::Direct {
            candidate: Candidate::new(target, Some(Confidence::ONE)),
            pipeline: Pipeline::Exact,
            source: None,
        };
        let candidates = QueryResult::Candidates {
            candidates: smallvec![Candidate::new(target, None)],
            pipeline: Pipeline::Exact,
        };
        let none = QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Exact,
        };
        let refused = QueryResult::Direct {
            candidate: Candidate::new(target, Some(Confidence::ONE)),
            pipeline: Pipeline::Exact,
            source: Some(SourceResult::Refused {
                status: SourceStatus::Stale,
            }),
        };
        assert_eq!(direct.exit_code(), 0);
        assert_eq!(candidates.exit_code(), 2);
        assert_eq!(none.exit_code(), 3);
        assert_eq!(refused.exit_code(), 4);

        assert!(include_str!("../../rr-cli/src/main.rs").contains("rr: query:"));
        assert!(
            include_str!("../../rr-cli/src/query.rs").contains("index is stale; run 'rr refresh'")
        );
    }

    #[test]
    fn the_contract_documents_the_sigpipe_disposition() {
        assert!(CONTRACT_BLOCK.contains("141"));
        assert!(include_str!("../../rr-cli/src/main.rs").contains("SIGPIPE"));
    }

    #[test]
    fn every_path_named_in_the_contract_is_a_constant_in_this_crate() {
        assert_eq!(MAP_FILE_NAME, "MAP.md");
        assert_eq!(SYMBOLS_PATH, ".rr/SYMBOLS.md");
        assert!(CONTRACT_BLOCK.contains(MAP_FILE_NAME));
        assert!(CONTRACT_BLOCK.contains(SYMBOLS_PATH));
    }

    #[test]
    fn the_contract_describes_one_map_file_per_directory() {
        assert!(!CONTRACT_BLOCK.contains("MAP.rr-"));
        assert!(CONTRACT_BLOCK.contains("omitted by the map budget"));
    }
}
