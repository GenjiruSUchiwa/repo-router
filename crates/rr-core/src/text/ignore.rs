//! The managed block inside `.rr/.gitignore`.
//!
//! Scoped to `.rr/` on purpose. A pattern written into the repository root's
//! `.gitignore` could hide a committed `MAP.md`, which is the one file this
//! whole issue exists to put under version control. Rooting the patterns to
//! `.rr/` makes that impossible rather than merely unlikely.
//!
//! Issue #12 adds `/ROUTES.md` through this same helper instead of maintaining
//! a second block: two managed regions in one file is two chances for a merge
//! to interleave them.

use super::{TextError, TextResult};

/// The first line of the managed region.
pub const IGNORE_BEGIN_MARKER: &str = "# rr:begin local artifacts";
/// The last line of the managed region.
pub const IGNORE_END_MARKER: &str = "# rr:end local artifacts";

/// The patterns this issue owns.
const OWNED_PATTERNS: [&str; 2] = ["/SYMBOLS.md", "/local/"];

/// The exact managed block for a set of patterns.
///
/// Patterns are sorted by UTF-8 bytes so that two callers registering the same
/// set in different orders produce the same block, and a later registration
/// does not reshuffle the file.
#[must_use]
pub fn managed_ignore_block(patterns: &[&str]) -> String {
    let mut sorted: Vec<&str> = patterns.to_vec();
    sorted.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    sorted.dedup();
    let mut out = String::from(IGNORE_BEGIN_MARKER);
    out.push('\n');
    for pattern in sorted {
        out.push_str(pattern);
        out.push('\n');
    }
    out.push_str(IGNORE_END_MARKER);
    out.push('\n');
    out
}

/// The block this issue registers.
#[must_use]
fn owned_block() -> String {
    managed_ignore_block(&OWNED_PATTERNS)
}

/// The contents `.rr/.gitignore` should have, given what it has now.
///
/// Unrelated lines are preserved exactly, including their order. A missing
/// block is appended after one blank separator line. Anything ambiguous —
/// duplicate markers, a closing marker before its opening one, an opening
/// marker that never closes — is refused rather than rewritten, because the
/// ambiguity usually means a merge left two versions of the block and
/// discarding either one loses somebody's edit.
///
/// # Errors
/// Returns [`TextError::ManagedIgnore`] for malformed or duplicated markers,
/// and [`TextError::Newline`] for content this crate cannot write back.
pub fn apply_managed_block(existing: Option<&str>) -> TextResult<String> {
    let block = owned_block();
    let Some(existing) = existing else {
        return Ok(block);
    };
    let existing = super::parse::normalize_newlines_public(existing)?;

    let begins: Vec<usize> = marker_lines(&existing, IGNORE_BEGIN_MARKER);
    let ends: Vec<usize> = marker_lines(&existing, IGNORE_END_MARKER);
    if begins.len() > 1 || ends.len() > 1 {
        return Err(TextError::ManagedIgnore {
            reason: "the managed block markers appear more than once",
        });
    }
    let lines: Vec<&str> = existing.lines().collect();
    match (begins.first(), ends.first()) {
        (None, None) => Ok(append_block(&existing, &block)),
        (Some(&begin), Some(&end)) if begin < end => {
            let mut out = String::new();
            for line in &lines[..begin] {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(&block);
            for line in &lines[end + 1..] {
                out.push_str(line);
                out.push('\n');
            }
            Ok(out)
        }
        _ => Err(TextError::ManagedIgnore {
            reason: "the managed block is missing a marker or its markers are inverted",
        }),
    }
}

fn marker_lines(text: &str, marker: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| *line == marker)
        .map(|(index, _)| index)
        .collect()
}

/// Appends the block after exactly one blank separator line.
fn append_block(existing: &str, block: &str) -> String {
    let mut out = String::with_capacity(existing.len() + block.len() + 2);
    out.push_str(existing);
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            out.push('\n');
        }
        if !existing.ends_with("\n\n") && !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_sorted_and_exact() {
        assert_eq!(
            owned_block(),
            "# rr:begin local artifacts\n/SYMBOLS.md\n/local/\n# rr:end local artifacts\n"
        );
        assert_eq!(
            managed_ignore_block(&["/local/", "/SYMBOLS.md"]),
            managed_ignore_block(&["/SYMBOLS.md", "/local/"])
        );
    }

    #[test]
    fn a_missing_file_becomes_the_block_alone() {
        assert_eq!(apply_managed_block(None).unwrap(), owned_block());
    }

    #[test]
    fn unrelated_lines_survive_on_both_sides() {
        let existing = format!("mine.txt\n{}\nkeep-me\n", owned_block().trim_end());
        let updated = apply_managed_block(Some(&existing)).unwrap();
        assert!(updated.starts_with("mine.txt\n"));
        assert!(updated.ends_with("keep-me\n"));
        assert!(updated.contains(&owned_block()));
    }

    #[test]
    fn a_file_without_the_block_gains_it_after_one_blank_line() {
        let updated = apply_managed_block(Some("mine.txt\n")).unwrap();
        assert_eq!(updated, format!("mine.txt\n\n{}", owned_block()));
    }

    #[test]
    fn applying_twice_changes_nothing() {
        let once = apply_managed_block(Some("mine.txt\n")).unwrap();
        assert_eq!(apply_managed_block(Some(&once)).unwrap(), once);
    }

    #[test]
    fn ambiguous_markers_are_refused_rather_than_rewritten() {
        let doubled = format!("{}{}", owned_block(), owned_block());
        assert!(apply_managed_block(Some(&doubled)).is_err());
        assert!(apply_managed_block(Some(IGNORE_BEGIN_MARKER)).is_err());
        assert!(apply_managed_block(Some(IGNORE_END_MARKER)).is_err());
        let inverted = format!("{IGNORE_END_MARKER}\n{IGNORE_BEGIN_MARKER}\n");
        assert!(apply_managed_block(Some(&inverted)).is_err());
    }
}
