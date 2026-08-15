//! The one region of a generated file that a human owns.
//!
//! Everything else in a map is rewritten from the snapshot on every run. The
//! purpose slot is not: it is read back, validated, and copied through
//! byte-for-byte. That asymmetry is the whole reason maps are committed rather
//! than generated into `.rr/` alongside `SYMBOLS.md`.
//!
//! The rules are deliberately unforgiving. A slot that is malformed, oversize,
//! duplicated, or contains its own markers is a conflict, never something to
//! trim or repair — the alternative is a generator that silently edits prose
//! somebody wrote.

use std::collections::BTreeMap;
use std::path::Path;

use crate::lex::{append_source_terms, InputKind, LexicalField, Lexicon};

use super::model::{Scope, TextProjection};
use super::{TextError, TextResult, PURPOSE_MAX_BYTES};

/// The prefix that marks a purpose as still waiting for a human.
const PENDING_PREFIX: &str = "TODO:";
/// The seeded text when a scope has no names worth listing.
const SEED_BARE: &str = "TODO: describe this scope.";
/// The most terms a seed lists, however many the scope has.
const SEED_TERM_LIMIT: usize = 8;

/// Validated purpose text, keyed by the canonical map path that held it.
///
/// Only maps that exist and parse contribute. A map that could not be read at
/// all contributes nothing and is reported as a conflict by the caller — this
/// type never represents "we could not tell", because the difference between
/// "no purpose" and "unreadable purpose" decides whether a file may be
/// overwritten.
#[derive(Debug, Clone, Default)]
pub struct ExistingPurposes {
    by_map: BTreeMap<String, String>,
}

impl ExistingPurposes {
    /// An empty set, which makes every scope seed a fresh pending value.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The preserved purpose for one canonical map path.
    #[must_use]
    pub fn for_map(&self, map_path: &str) -> Option<&str> {
        self.by_map.get(map_path).map(String::as_str)
    }

    /// How many maps contributed text.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_map.len()
    }

    /// Whether nothing was preserved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_map.is_empty()
    }

    pub(crate) fn insert(&mut self, map_path: String, purpose: String) {
        self.by_map.insert(map_path, purpose);
    }
}

/// Reads the purpose slot of every router the plan will write.
///
/// Reads only the maps the new plan needs. A map for a directory that no longer
/// exists has no purpose to preserve, and reading it would only invite the
/// question of where that text should go.
///
/// # Errors
/// Returns an error only for a file that exists and cannot be read at all.
/// A file that parses but holds an unusable slot is left for
/// [`super::validate_text_artifacts`] to report as a conflict, because that is
/// the function whose job is to enumerate every problem rather than stop at the
/// first.
pub fn read_existing_purposes(
    root: &Path,
    projection: &TextProjection,
) -> crate::Result<ExistingPurposes> {
    let mut purposes = ExistingPurposes::default();
    for scope in projection.scopes() {
        let map_path = scope.path.map_path();
        let absolute = root.join(&map_path);
        let bytes = match std::fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(crate::Error::Io(error)),
        };
        // A map this crate cannot parse keeps its bytes and blocks the run
        // elsewhere; here it simply contributes nothing.
        if let Ok(parsed) = super::parse_map(&bytes) {
            if let Some(text) = parsed.purpose() {
                purposes.insert(map_path, text.to_owned());
            }
        }
    }
    Ok(purposes)
}

/// Whether this slot content is still the generator's placeholder.
///
/// Trimmed before the test so that a human who indented the placeholder while
/// editing around it does not accidentally convert it into a real description.
pub(crate) fn is_pending(purpose: &str) -> bool {
    purpose.trim_start().starts_with(PENDING_PREFIX)
}

/// Validates one slot's logical content.
///
/// # Errors
/// Returns [`TextError::Purpose`] for content that is oversize, contains a
/// marker, or contains a byte the format cannot carry.
pub(crate) fn validate(purpose: &str) -> TextResult<()> {
    if purpose.len() > PURPOSE_MAX_BYTES {
        return Err(TextError::Purpose {
            reason: "purpose exceeds 160 UTF-8 bytes",
        });
    }
    if purpose.contains(super::render::SLOT_OPEN) || purpose.contains(super::render::SLOT_CLOSE) {
        return Err(TextError::Purpose {
            reason: "purpose contains a slot marker",
        });
    }
    if purpose.contains('\u{0}') || purpose.contains('\r') {
        return Err(TextError::Purpose {
            reason: "purpose contains a NUL or a bare carriage return",
        });
    }
    Ok(())
}

/// The deterministic pending value a scope gets on its first generation.
///
/// Terms come from issue #5's normalizer over the scope's own map-visible
/// symbol names, counted once per symbol so that a name repeated across ten
/// overloads does not outvote ten distinct names. Ties break on raw bytes, so
/// two machines seed the same sentence.
///
/// Terms are added only while the rendered sentence still fits the slot. The
/// issue's eight-term ceiling is not itself a byte guarantee — eight long
/// identifiers overflow 160 bytes — and a seed that failed its own validator
/// would make the very first run a conflict.
pub(crate) fn seed(scope: &Scope) -> String {
    let terms = ranked_terms(scope);
    let mut chosen: Vec<&str> = Vec::new();
    for term in &terms {
        chosen.push(term);
        if render_seed(&chosen).len() > PURPOSE_MAX_BYTES {
            chosen.pop();
            break;
        }
        if chosen.len() == SEED_TERM_LIMIT {
            break;
        }
    }
    render_seed(&chosen)
}

fn render_seed(terms: &[&str]) -> String {
    if terms.is_empty() {
        return SEED_BARE.to_owned();
    }
    format!("{SEED_BARE} Key terms: {}.", terms.join(", "))
}

/// Scope symbol names, normalized and ordered by descending count then bytes.
fn ranked_terms(scope: &Scope) -> Vec<String> {
    let mut lexicon = Lexicon::new();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for record in &scope.api {
        let mut terms = smallvec::SmallVec::new();
        if append_source_terms(
            LexicalField::Name,
            InputKind::Qualified,
            &record.name,
            &mut lexicon,
            &mut terms,
        )
        .is_err()
        {
            continue;
        }
        // Once per symbol: a name whose lexemes repeat inside it says the term
        // once, not twice.
        let mut seen: Vec<&str> = Vec::new();
        for term in &terms {
            let Some(text) = lexicon.resolve(term.term) else {
                continue;
            };
            if seen.contains(&text) {
                continue;
            }
            seen.push(text);
            *counts.entry(text.to_owned()).or_default() += 1;
        }
    }

    let mut ranked: Vec<(String, u32)> = counts.into_iter().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.as_bytes().cmp(right.0.as_bytes()))
    });
    ranked.into_iter().map(|(term, _)| term).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_without_terms_is_the_bare_sentence() {
        assert_eq!(render_seed(&[]), "TODO: describe this scope.");
    }

    #[test]
    fn a_seed_lists_its_terms() {
        assert_eq!(
            render_seed(&["auth", "token", "claims"]),
            "TODO: describe this scope. Key terms: auth, token, claims."
        );
    }

    /// Eight terms is a count, not a size. This is the case the issue's rule
    /// does not cover on its own, and the seed has to satisfy its own validator
    /// or the first run of `rr map` would report a conflict it just created.
    #[test]
    fn a_seed_never_exceeds_the_slot_it_is_written_into() {
        let long: Vec<&str> = vec!["a_very_long_identifier_fragment_indeed"; 8];
        let mut chosen: Vec<&str> = Vec::new();
        for term in &long {
            chosen.push(term);
            if render_seed(&chosen).len() > PURPOSE_MAX_BYTES {
                chosen.pop();
                break;
            }
        }
        let seeded = render_seed(&chosen);
        assert!(seeded.len() <= PURPOSE_MAX_BYTES, "{seeded:?}");
        assert!(validate(&seeded).is_ok());
    }

    #[test]
    fn pending_is_decided_by_the_prefix_alone() {
        assert!(is_pending("TODO: describe this scope."));
        assert!(is_pending("  TODO: still mine"));
        assert!(!is_pending("Routes authentication tokens."));
        assert!(!is_pending("A TODO: list lives here"));
    }

    #[test]
    fn an_unusable_slot_is_refused_rather_than_trimmed() {
        assert!(validate(&"x".repeat(PURPOSE_MAX_BYTES)).is_ok());
        assert!(validate(&"x".repeat(PURPOSE_MAX_BYTES + 1)).is_err());
        assert!(validate("holds <!-- /rr:slot --> inside").is_err());
        assert!(validate("carriage\rreturn").is_err());
        assert!(validate("nul\u{0}byte").is_err());
        assert!(validate("multi\nline is fine").is_ok());
    }
}
