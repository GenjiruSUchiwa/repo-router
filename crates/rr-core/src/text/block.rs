//! Replacing one marked region of a file without disturbing the rest.
//!
//! Three files now carry an rr-owned region inside a file the user also owns:
//! `.rr/.gitignore`, and the two agent-instruction files `rr init` writes. The
//! surgery is the same for all three, and the part that must not be duplicated
//! is the refusal: duplicated or inverted markers usually mean a merge left two
//! versions of the block, and a caller that picked one would silently discard
//! somebody's edit.

use super::{TextError, TextResult};

/// The two lines that delimit one owned region.
///
/// Both are compared as whole lines, never as substrings. That is what lets a
/// block's own body talk *about* markers — the agent contract in
/// [`crate::agent`] does — without the surgery mistaking the prose for a
/// delimiter.
#[derive(Debug, Clone, Copy)]
pub struct BlockMarkers {
    pub begin: &'static str,
    pub end: &'static str,
}

/// The reason carried by a refusal for two begin or two end markers.
///
/// Exported so a caller can tell the two refusals apart by identity rather than
/// by matching on the prose, which [`TextError`]'s own documentation is explicit
/// about avoiding.
pub const DUPLICATE_MARKERS_REASON: &str = "the managed block markers appear more than once";

/// The reason carried by a refusal for an unpaired or inverted marker pair.
pub const MALFORMED_MARKERS_REASON: &str =
    "the managed block is missing a marker or its markers are inverted";

/// The contents a file should have, given what it has now.
///
/// Unrelated lines are preserved exactly, in order; a missing block is appended
/// after one blank line.
///
/// # Errors
/// Returns [`TextError::ManagedIgnore`] for malformed or duplicated markers, and
/// [`TextError::Newline`] for content this crate cannot write back.
pub fn apply_block(
    existing: Option<&str>,
    markers: BlockMarkers,
    block: &str,
) -> TextResult<String> {
    let Some(existing) = existing else {
        return Ok(block.to_owned());
    };
    let existing = super::parse::normalize_newlines_public(existing)?;

    let begins = marker_lines(&existing, markers.begin);
    let ends = marker_lines(&existing, markers.end);
    if begins.len() > 1 || ends.len() > 1 {
        return Err(TextError::ManagedIgnore {
            reason: DUPLICATE_MARKERS_REASON,
        });
    }
    let lines: Vec<&str> = existing.lines().collect();
    match (begins.first(), ends.first()) {
        (None, None) => Ok(append_block(&existing, block)),
        (Some(&begin), Some(&end)) if begin < end => {
            let mut out = String::new();
            for line in &lines[..begin] {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(block);
            for line in &lines[end + 1..] {
                out.push_str(line);
                out.push('\n');
            }
            Ok(out)
        }
        _ => Err(TextError::ManagedIgnore {
            reason: MALFORMED_MARKERS_REASON,
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
        // `out` now ends in exactly the newlines `existing` did, plus at most the
        // one just added, so one test covers both: a file already ending in a
        // blank line does not gain a second one.
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
    out.push_str(block);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const MARKERS: BlockMarkers = BlockMarkers {
        begin: "# rr:begin local artifacts",
        end: "# rr:end local artifacts",
    };

    const BLOCK: &str =
        "# rr:begin local artifacts\n/SYMBOLS.md\n/local/\n# rr:end local artifacts\n";

    const CONTRACT_MARKERS: BlockMarkers = BlockMarkers {
        begin: "<!-- rr:begin agent contract -->",
        end: "<!-- rr:end agent contract -->",
    };

    const CONTRACT_BLOCK: &str =
        "<!-- rr:begin agent contract -->\ncontract body\n<!-- rr:end agent contract -->\n";

    #[test]
    fn a_missing_block_is_appended_after_one_blank_line() {
        let updated = apply_block(Some("mine.txt\n"), MARKERS, BLOCK).unwrap();
        assert_eq!(updated, format!("mine.txt\n\n{BLOCK}"));
    }

    #[test]
    fn an_existing_block_is_replaced_and_the_rest_survives() {
        let existing = format!("before\n{}\nafter\n", BLOCK.trim_end());
        let replacement = "# rr:begin local artifacts\n/NEW.md\n# rr:end local artifacts\n";
        let updated = apply_block(Some(&existing), MARKERS, replacement).unwrap();
        assert!(updated.starts_with("before\n"));
        assert!(updated.ends_with("after\n"));
        assert!(updated.contains(replacement));
        assert!(!updated.contains("/SYMBOLS.md"));
        // Content before AND after the region is byte-identical afterwards.
        let before = "before\n";
        let after = "after\n";
        assert_eq!(&updated[..before.len()], before);
        assert_eq!(&updated[updated.len() - after.len()..], after);
    }

    #[test]
    fn duplicated_begin_markers_are_refused() {
        let doubled = format!("{BLOCK}{BLOCK}");
        assert!(apply_block(Some(&doubled), MARKERS, BLOCK).is_err());
    }

    #[test]
    fn inverted_markers_are_refused() {
        let inverted = format!("{}\n{}\n", MARKERS.end, MARKERS.begin);
        let err = apply_block(Some(&inverted), MARKERS, BLOCK).unwrap_err();
        assert!(matches!(err, TextError::ManagedIgnore { .. }));
    }

    #[test]
    fn an_absent_file_becomes_the_block_alone() {
        assert_eq!(apply_block(None, MARKERS, BLOCK).unwrap(), BLOCK);
    }

    #[test]
    fn two_marker_sets_do_not_see_each_other() {
        let existing = format!("{BLOCK}\n{CONTRACT_BLOCK}");
        let updated = apply_block(Some(&existing), MARKERS, BLOCK).unwrap();
        assert!(updated.contains(CONTRACT_BLOCK.trim_end()));
        assert!(updated.contains(BLOCK.trim_end()));

        let updated_contract =
            apply_block(Some(&existing), CONTRACT_MARKERS, CONTRACT_BLOCK).unwrap();
        assert!(updated_contract.contains(BLOCK.trim_end()));
        assert!(updated_contract.contains(CONTRACT_BLOCK.trim_end()));

        // Applying one must not disturb the other set's markers as whole lines.
        let other_begins: Vec<_> = updated
            .lines()
            .filter(|line| *line == CONTRACT_MARKERS.begin)
            .collect();
        let other_ends: Vec<_> = updated
            .lines()
            .filter(|line| *line == CONTRACT_MARKERS.end)
            .collect();
        assert_eq!(other_begins.len(), 1);
        assert_eq!(other_ends.len(), 1);
    }
}
