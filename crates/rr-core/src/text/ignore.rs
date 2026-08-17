//! The managed block inside `.rr/.gitignore`.
//!
//! Scoped to `.rr/` on purpose: a pattern in the root `.gitignore` could hide a
//! committed `MAP.md`, the one file this issue exists to put under version
//! control.
//!
//! `/ROUTES.md` is registered through this same helper rather than through a
//! second managed block, which is why [`OWNED_PATTERNS`] has three entries and
//! not two lists.

use super::{BlockMarkers, TextResult};

/// The first line of the managed region.
pub const IGNORE_BEGIN_MARKER: &str = "# rr:begin local artifacts";
/// The last line of the managed region.
pub const IGNORE_END_MARKER: &str = "# rr:end local artifacts";

/// The markers this module's block is delimited by.
pub const IGNORE_MARKERS: BlockMarkers = BlockMarkers {
    begin: IGNORE_BEGIN_MARKER,
    end: IGNORE_END_MARKER,
};

/// The patterns this issue owns.
///
/// `/ROUTES.md` does not make that file private — `.rr/.gitignore` already
/// starts with the `*` that `workspace::ensure_private` stamps, and that prunes
/// the whole subtree. It is here because the stamp is a blanket a user may
/// legitimately replace (`ensure_private` leaves a hand-edited rule alone), and
/// this list is the part that still names, file by file, what rr claims.
const OWNED_PATTERNS: [&str; 3] = ["/ROUTES.md", "/SYMBOLS.md", "/local/"];

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
/// Unrelated lines are preserved exactly, in order; a missing block is appended
/// after one blank line. Duplicate, inverted, or unclosed markers are refused
/// rather than rewritten — they usually mean a merge left two versions of the
/// block, and picking one loses somebody's edit.
///
/// # Errors
/// Returns [`TextError::ManagedIgnore`] for malformed or duplicated markers,
/// and [`TextError::Newline`] for content this crate cannot write back.
pub fn apply_managed_block(existing: Option<&str>) -> TextResult<String> {
    super::block::apply_block(existing, IGNORE_MARKERS, &owned_block())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_sorted_and_exact() {
        assert_eq!(
            owned_block(),
            "# rr:begin local artifacts\n/ROUTES.md\n/SYMBOLS.md\n/local/\n\
             # rr:end local artifacts\n"
        );
        assert_eq!(
            managed_ignore_block(&["/local/", "/SYMBOLS.md", "/ROUTES.md"]),
            managed_ignore_block(&["/ROUTES.md", "/SYMBOLS.md", "/local/"])
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
