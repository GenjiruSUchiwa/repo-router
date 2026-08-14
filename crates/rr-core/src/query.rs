use std::cmp::Ordering;

use smallvec::SmallVec;
use unicode_ident::{is_xid_continue, is_xid_start};

use crate::index::{FileId, Snapshot, SymbolId};
use crate::lex::{query_terms, QueryTerms, TermId};
use crate::path::RelPath;
use crate::ranking::{route_lexical, RankingEvidence, RankingProfile, RankingScratch};
use crate::result::{Candidate, Confidence, NoneReason, Pipeline, QueryResult, TargetId};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryRequest<'a> {
    pub text: &'a str,
    pub path: Option<&'a RelPath>,
}

impl<'a> QueryRequest<'a> {
    #[must_use]
    pub const fn new(text: &'a str, path: Option<&'a RelPath>) -> Self {
        Self { text, path }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactAtomKind {
    Qualified,
    Path,
    Symbol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactAtom<'a> {
    pub text: &'a str,
    pub kind: ExactAtomKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery<'a> {
    pub raw: &'a str,
    pub exact_atoms: SmallVec<[ExactAtom<'a>; 4]>,
    /// Normalized, deduplicated query terms shared by both pipelines.
    pub terms: QueryTerms,
    pub path: Option<&'a RelPath>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExactOutcome {
    Direct(Candidate),
    Candidates(SmallVec<[Candidate; 3]>),
    Miss,
}

/// Parses a query request into normalized terms and classified exact atoms.
///
/// # Errors
/// Returns [`crate::Error::InvalidQuery`] if the query is empty or whitespace-only.
pub fn parse_query<'a>(snapshot: &Snapshot, request: QueryRequest<'a>) -> Result<ParsedQuery<'a>> {
    let raw = request.text.trim();
    if raw.is_empty() {
        return Err(crate::Error::InvalidQuery {
            reason: "query cannot be empty or whitespace-only",
        });
    }

    let terms = query_terms(raw, snapshot);

    let cleaned_tokens: Vec<&str> = tokenize_whitespace(raw)
        .into_iter()
        .map(strip_surrounding_punctuation)
        .filter(|cleaned| !cleaned.is_empty())
        .collect();
    let single_token = cleaned_tokens.len() == 1;

    let mut exact_atoms = SmallVec::new();
    for cleaned in cleaned_tokens {
        if let Some(kind) = classify_atom(cleaned, single_token, snapshot) {
            exact_atoms.push(ExactAtom {
                text: cleaned,
                kind,
            });
        }
    }

    Ok(ParsedQuery {
        raw,
        exact_atoms,
        terms,
        path: request.path,
    })
}

fn tokenize_whitespace(input: &str) -> Vec<&str> {
    input.split_ascii_whitespace().collect()
}

fn strip_surrounding_punctuation(input: &str) -> &str {
    let mut s = input;
    loop {
        let trimmed = strip_one_surrounding_punctuation(s);
        if trimmed == s {
            return s;
        }
        s = trimmed;
    }
}

fn strip_one_surrounding_punctuation(input: &str) -> &str {
    if input.len() >= 2 {
        let first = input.as_bytes()[0];
        let last = input.as_bytes()[input.len() - 1];
        if matches!(
            (first, last),
            (b'`', b'`')
                | (b'"', b'"')
                | (b'\'', b'\'')
                | (b'(', b')')
                | (b'[', b']')
                | (b'{', b'}')
        ) {
            return &input[1..input.len() - 1];
        }
    }

    let start = input
        .as_bytes()
        .iter()
        .take_while(|byte| matches!(**byte, b',' | b';' | b'?' | b'!'))
        .count();
    let end = input
        .as_bytes()
        .iter()
        .rposition(|byte| !matches!(*byte, b',' | b';' | b'?' | b'!'))
        .map_or(start, |index| index + 1);
    &input[start..end]
}

fn classify_atom(token: &str, single_token: bool, snapshot: &Snapshot) -> Option<ExactAtomKind> {
    if token.contains('/') || is_indexed_file_path(token, snapshot) {
        return Some(ExactAtomKind::Path);
    }

    if (token.contains("::") || token.contains('.')) && is_qualified_identifier(token) {
        return Some(ExactAtomKind::Qualified);
    }

    if is_code_shaped_identifier(token) {
        return Some(ExactAtomKind::Symbol);
    }

    if single_token && is_valid_identifier_component(token) {
        return Some(ExactAtomKind::Symbol);
    }

    None
}

fn is_indexed_file_path(token: &str, snapshot: &Snapshot) -> bool {
    find_file_id_by_query_path(snapshot, token).is_some()
}

fn is_qualified_identifier(token: &str) -> bool {
    let separator = if token.contains("::") { "::" } else { "." };
    let mut parts = token.split(separator);
    let Some(first) = parts.next() else {
        return false;
    };
    if !is_valid_identifier_component(first) {
        return false;
    }
    let mut count = 1;
    for part in parts {
        if !is_valid_identifier_component(part) {
            return false;
        }
        count += 1;
    }
    count >= 2
}

fn is_code_shaped_identifier(token: &str) -> bool {
    if !is_valid_identifier_component(token) {
        return false;
    }

    token.contains('_') || has_lower_upper_transition(token)
}

fn is_valid_identifier_component(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first != '_' && !is_xid_start(first) {
        return false;
    }
    chars.all(|character| character == '_' || is_xid_continue(character))
}

fn has_lower_upper_transition(token: &str) -> bool {
    let mut previous = None;
    for current in token.chars() {
        if previous.is_some_and(char::is_lowercase) && current.is_uppercase() {
            return true;
        }
        previous = Some(current);
    }
    false
}

#[must_use]
pub fn route_exact(snapshot: &Snapshot, query: &ParsedQuery<'_>) -> ExactOutcome {
    let cli_path_file_id = match query.path {
        Some(rel_path) => match find_file_id_by_path(snapshot, rel_path.as_str()) {
            Some(id) => Some(id),
            None => return ExactOutcome::Miss,
        },
        None => None,
    };

    let query_path_file_id = query
        .exact_atoms
        .iter()
        .filter(|atom| atom.kind == ExactAtomKind::Path)
        .find_map(|atom| find_file_id_by_query_path(snapshot, atom.text));
    let effective_path_qualifier = cli_path_file_id.or(query_path_file_id);

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Qualified {
            continue;
        }
        if let Some(symbols) = lookup_exact_qualified(snapshot, atom.text) {
            match evaluate_symbol_candidates(
                snapshot,
                query,
                atom,
                symbols,
                effective_path_qualifier,
            ) {
                // Every match for this atom was filtered out by the path
                // qualifier; later atoms may still route.
                ExactOutcome::Miss => {}
                outcome => return outcome,
            }
        }
    }

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Symbol {
            continue;
        }
        if let Some(symbols) = lookup_exact_name(snapshot, atom.text) {
            match evaluate_symbol_candidates(
                snapshot,
                query,
                atom,
                symbols,
                effective_path_qualifier,
            ) {
                // Every match for this atom was filtered out by the path
                // qualifier; later atoms may still route.
                ExactOutcome::Miss => {}
                outcome => return outcome,
            }
        }
    }

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Path {
            continue;
        }
        if let Some(file_id) = find_file_id_by_query_path(snapshot, atom.text) {
            if let Some(cli_id) = cli_path_file_id {
                if file_id != cli_id {
                    continue;
                }
            }
            return ExactOutcome::Direct(Candidate::new(
                TargetId::File(file_id),
                Some(Confidence::ONE),
            ));
        }
    }

    ExactOutcome::Miss
}

fn evaluate_symbol_candidates(
    snapshot: &Snapshot,
    query: &ParsedQuery<'_>,
    matched_atom: &ExactAtom<'_>,
    symbols: &[SymbolId],
    path_qualifier: Option<FileId>,
) -> ExactOutcome {
    let mut count = 0usize;
    let mut only = None;
    for &symbol_id in symbols {
        if symbol_matches_path(snapshot, symbol_id, path_qualifier) {
            count += 1;
            only = Some(symbol_id);
            if count > 1 {
                break;
            }
        }
    }

    match (count, only) {
        (0, _) => ExactOutcome::Miss,
        (1, Some(symbol_id)) => ExactOutcome::Direct(Candidate::new(
            TargetId::Symbol(symbol_id),
            Some(Confidence::ONE),
        )),
        _ => disambiguate_candidates(snapshot, query, matched_atom, symbols, path_qualifier),
    }
}

fn symbol_matches_path(
    snapshot: &Snapshot,
    symbol_id: SymbolId,
    path_qualifier: Option<FileId>,
) -> bool {
    let Some(symbol) = snapshot.symbols.get(symbol_id.index()) else {
        return false;
    };
    match path_qualifier {
        Some(file_id) => symbol.file == file_id,
        None => true,
    }
}

struct ScoredSymbol<'a> {
    symbol_id: SymbolId,
    overlap: u32,
    qual_name: &'a str,
    path: &'a str,
    start_line: u32,
}

fn compare_scored(left: &ScoredSymbol<'_>, right: &ScoredSymbol<'_>) -> Ordering {
    right
        .overlap
        .cmp(&left.overlap)
        .then_with(|| left.qual_name.as_bytes().cmp(right.qual_name.as_bytes()))
        .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
        .then_with(|| left.start_line.cmp(&right.start_line))
        .then_with(|| left.symbol_id.cmp(&right.symbol_id))
}

fn retain_top_three<'a>(top: &mut SmallVec<[ScoredSymbol<'a>; 3]>, candidate: ScoredSymbol<'a>) {
    let position = top
        .iter()
        .position(|existing| compare_scored(&candidate, existing) == Ordering::Less)
        .unwrap_or(top.len());
    if position >= 3 {
        return;
    }
    top.insert(position, candidate);
    if top.len() > 3 {
        top.pop();
    }
}

fn disambiguate_candidates(
    snapshot: &Snapshot,
    query: &ParsedQuery<'_>,
    matched_atom: &ExactAtom<'_>,
    candidates: &[SymbolId],
    path_qualifier: Option<FileId>,
) -> ExactOutcome {
    let atom_terms = query_terms(matched_atom.text, snapshot);
    let remaining_query_terms: SmallVec<[TermId; 8]> = query
        .terms
        .iter()
        .filter(|term| !atom_terms.as_slice().contains(term))
        .collect();

    let mut top: SmallVec<[ScoredSymbol<'_>; 3]> = SmallVec::new();
    for &symbol_id in candidates {
        if !symbol_matches_path(snapshot, symbol_id, path_qualifier) {
            continue;
        }
        let Some(sym) = snapshot.symbols.get(symbol_id.index()) else {
            continue;
        };
        let Some(file) = snapshot.files.get(sym.file.index()) else {
            continue;
        };
        let qual_name = snapshot
            .strings
            .get(sym.qualified_name.index())
            .map_or("", String::as_str);
        let file_path = snapshot
            .strings
            .get(file.path.index())
            .map_or("", String::as_str);
        let qual_terms = query_terms(qual_name, snapshot);
        let path_terms = query_terms(file_path, snapshot);
        let count = remaining_query_terms
            .iter()
            .filter(|term| {
                qual_terms.as_slice().contains(term) || path_terms.as_slice().contains(term)
            })
            .count();
        let overlap = u32::try_from(count).unwrap_or(u32::MAX);

        retain_top_three(
            &mut top,
            ScoredSymbol {
                symbol_id,
                overlap,
                qual_name,
                path: file_path,
                start_line: sym.span.start_line(),
            },
        );
    }

    if !top.is_empty() && top[0].overlap > 0 && (top.len() == 1 || top[0].overlap > top[1].overlap)
    {
        return ExactOutcome::Direct(Candidate::new(
            TargetId::Symbol(top[0].symbol_id),
            Some(Confidence::ONE),
        ));
    }

    let final_candidates: SmallVec<[Candidate; 3]> = top
        .into_iter()
        .map(|entry| Candidate::new(TargetId::Symbol(entry.symbol_id), None))
        .collect();
    if final_candidates.is_empty() {
        ExactOutcome::Miss
    } else {
        ExactOutcome::Candidates(final_candidates)
    }
}

fn find_file_id_by_query_path(snapshot: &Snapshot, path: &str) -> Option<FileId> {
    let normalized = RelPath::new(path).ok()?;
    find_file_id_by_path(snapshot, normalized.as_str())
}

fn find_file_id_by_path(snapshot: &Snapshot, path: &str) -> Option<FileId> {
    let result = snapshot.files.binary_search_by(|file| {
        let file_path = snapshot
            .strings
            .get(file.path.index())
            .map_or("", String::as_str);
        file_path.as_bytes().cmp(path.as_bytes())
    });
    result
        .ok()
        .and_then(|index| snapshot.files.get(index).map(|file| file.id))
}

fn lookup_route<'a>(
    routes: &'a [crate::index::ExactRoute],
    strings: &[String],
    key: &str,
) -> Option<&'a [SymbolId]> {
    let result = routes.binary_search_by(|route| {
        strings
            .get(route.key.index())
            .map_or("", String::as_str)
            .as_bytes()
            .cmp(key.as_bytes())
    });
    result
        .ok()
        .and_then(|index| routes.get(index).map(|route| route.symbols.as_slice()))
}

fn lookup_exact_qualified<'a>(snapshot: &'a Snapshot, key: &str) -> Option<&'a [SymbolId]> {
    lookup_route(&snapshot.exact_qualified, &snapshot.strings, key)
}

fn lookup_exact_name<'a>(snapshot: &'a Snapshot, key: &str) -> Option<&'a [SymbolId]> {
    lookup_route(&snapshot.exact_names, &snapshot.strings, key)
}

/// Routes one parsed query through the whole pipeline: exact first, lexical
/// only on an exact miss.
///
/// Precedence lives here rather than in a caller so no consumer can invert it:
/// a [`ExactOutcome::Direct`] or [`ExactOutcome::Candidates`] outcome is final.
///
/// A `--path` qualifier is honored only by exact routing, which owns it. When a
/// path-qualified query misses, this abstains instead of ranking the whole
/// repository, because a lexical answer from another file would silently
/// contradict the qualifier the caller asked for.
///
/// The evidence is returned alongside the result rather than dropped here: it
/// is the only record of what the candidate cap discarded, and a caller that
/// cannot see it cannot tell an answer chosen from the whole repository apart
/// from one chosen among the first sixty-four members the merge happened to
/// meet. It is `None` for an exact route, which reads no posting list.
///
/// # Errors
/// Returns [`crate::Error::Ranking`] when the lexical fallback cannot compute a
/// route; the index is never partially answered.
pub fn route_query(
    snapshot: &Snapshot,
    query: &ParsedQuery<'_>,
    profile: &RankingProfile,
    scratch: &mut RankingScratch,
) -> Result<(QueryResult, Option<RankingEvidence>)> {
    match route_exact(snapshot, query) {
        ExactOutcome::Miss if query.path.is_none() => {
            let (result, evidence) = route_lexical(snapshot, &query.terms, profile, scratch)?;
            Ok((result, Some(evidence)))
        }
        outcome => Ok((finish_exact(outcome), None)),
    }
}

#[must_use]
pub fn finish_exact(outcome: ExactOutcome) -> QueryResult {
    match outcome {
        ExactOutcome::Direct(candidate) => QueryResult::Direct {
            candidate,
            pipeline: Pipeline::Exact,
            source: None,
        },
        ExactOutcome::Candidates(candidates) => QueryResult::Candidates {
            candidates,
            pipeline: Pipeline::Exact,
        },
        ExactOutcome::Miss => QueryResult::None {
            reason: NoneReason::NotFound,
            pipeline: Pipeline::Exact,
        },
    }
}
