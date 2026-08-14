use std::collections::HashSet;

use smallvec::SmallVec;

use crate::index::{FileId, Snapshot, SymbolId};
use crate::lex::{query_terms, TermId};
use crate::path::RelPath;
use crate::result::{Candidate, Confidence, NoneReason, Pipeline, QueryResult, TargetId};
use crate::{Error, Result};

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
    pub terms: SmallVec<[TermId; 8]>,
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
/// Returns [`Error::SnapshotInvariant`] if the query is empty or whitespace-only, or lexicon loading fails.
pub fn parse_query<'a>(snapshot: &Snapshot, request: QueryRequest<'a>) -> Result<ParsedQuery<'a>> {
    let raw = request.text.trim();
    if raw.is_empty() {
        return Err(Error::SnapshotInvariant {
            reason: "query cannot be empty or whitespace-only",
        });
    }

    let lexicon = snapshot.lexicon()?;
    let query_terms_vec = query_terms(raw, &lexicon);
    let terms = query_terms_vec.into_iter().collect();

    let tokens = tokenize_whitespace(raw);
    let single_token = tokens.len() == 1;

    let mut exact_atoms = SmallVec::new();
    for token in tokens {
        let cleaned = strip_surrounding_punctuation(token);
        if cleaned.is_empty() {
            continue;
        }

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
    input.split_whitespace().collect()
}

fn strip_surrounding_punctuation(input: &str) -> &str {
    let mut s = input;
    loop {
        let trimmed = strip_one_surrounding_punctuation(s);
        if trimmed == s {
            break;
        }
        s = trimmed;
    }
    s
}

fn strip_one_surrounding_punctuation(input: &str) -> &str {
    if input.len() >= 2 {
        let first = input.as_bytes()[0];
        let last = input.as_bytes()[input.len() - 1];
        if (first == b'`' && last == b'`')
            || (first == b'"' && last == b'"')
            || (first == b'\'' && last == b'\'')
            || (first == b'(' && last == b')')
            || (first == b'[' && last == b']')
            || (first == b'{' && last == b'}')
        {
            return &input[1..input.len() - 1];
        }
    }

    let trimmed_end =
        input.trim_end_matches([',', ';', '?', '!', ')', ']', '}', '>', ':', '`', '\'', '"']);
    let trimmed_start = trimmed_end.trim_start_matches(['(', '[', '{', '<', '`', '\'', '"']);
    trimmed_start
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
    find_file_id_by_path(snapshot, token).is_some()
}

fn is_qualified_identifier(token: &str) -> bool {
    let parts: Vec<&str> = if token.contains("::") {
        token.split("::").collect()
    } else {
        token.split('.').collect()
    };

    if parts.len() < 2 {
        return false;
    }

    for part in parts {
        if !is_valid_identifier_component(part) {
            return false;
        }
    }

    true
}

fn is_code_shaped_identifier(token: &str) -> bool {
    if !is_valid_identifier_component(token) {
        return false;
    }

    if token.contains('_') {
        return true;
    }

    has_lower_upper_transition(token)
}

fn is_valid_identifier_component(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }

    let bytes = token.as_bytes();
    let first = bytes[0];
    if !first.is_ascii_alphabetic() && first != b'_' {
        return false;
    }

    for &b in &bytes[1..] {
        if !b.is_ascii_alphanumeric() && b != b'_' {
            return false;
        }
    }

    true
}

fn has_lower_upper_transition(token: &str) -> bool {
    let bytes = token.as_bytes();
    for window in bytes.windows(2) {
        if window[0].is_ascii_lowercase() && window[1].is_ascii_uppercase() {
            return true;
        }
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
        .find(|atom| atom.kind == ExactAtomKind::Path)
        .and_then(|atom| find_file_id_by_path(snapshot, atom.text));

    let effective_path_qualifier = cli_path_file_id.or(query_path_file_id);

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Qualified {
            continue;
        }
        if let Some(symbols) = lookup_exact_qualified(snapshot, atom.text) {
            return evaluate_symbol_candidates(
                snapshot,
                query,
                atom,
                symbols,
                effective_path_qualifier,
            );
        }
    }

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Symbol {
            continue;
        }
        if let Some(symbols) = lookup_exact_name(snapshot, atom.text) {
            return evaluate_symbol_candidates(
                snapshot,
                query,
                atom,
                symbols,
                effective_path_qualifier,
            );
        }
    }

    for atom in &query.exact_atoms {
        if atom.kind != ExactAtomKind::Path {
            continue;
        }
        if let Some(file_id) = find_file_id_by_path(snapshot, atom.text) {
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
    let filtered_symbols: Vec<SymbolId> = if let Some(target_file) = path_qualifier {
        symbols
            .iter()
            .copied()
            .filter(|&symbol_id| {
                if let Some(sym) = snapshot.symbols.get(symbol_id.index()) {
                    sym.file == target_file
                } else {
                    false
                }
            })
            .collect()
    } else {
        symbols.to_vec()
    };

    match filtered_symbols.len() {
        0 => ExactOutcome::Miss,
        1 => ExactOutcome::Direct(Candidate::new(
            TargetId::Symbol(filtered_symbols[0]),
            Some(Confidence::ONE),
        )),
        _ => disambiguate_candidates(snapshot, query, matched_atom, &filtered_symbols),
    }
}

struct ScoredSymbol<'a> {
    symbol_id: SymbolId,
    overlap: u32,
    qual_name: &'a str,
    path: &'a str,
    start_line: u32,
}

fn disambiguate_candidates(
    snapshot: &Snapshot,
    query: &ParsedQuery<'_>,
    matched_atom: &ExactAtom<'_>,
    candidates: &[SymbolId],
) -> ExactOutcome {
    let Ok(lexicon) = snapshot.lexicon() else {
        return ExactOutcome::Miss;
    };

    let atom_terms: HashSet<TermId> = query_terms(matched_atom.text, &lexicon)
        .into_iter()
        .collect();
    let remaining_query_terms: HashSet<TermId> = query
        .terms
        .iter()
        .copied()
        .filter(|term| !atom_terms.contains(term))
        .collect();

    let mut scored: Vec<ScoredSymbol<'_>> = Vec::with_capacity(candidates.len());
    for &symbol_id in candidates {
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

        let mut candidate_terms = HashSet::new();
        for term in query_terms(qual_name, &lexicon) {
            candidate_terms.insert(term);
        }
        for term in query_terms(file_path, &lexicon) {
            candidate_terms.insert(term);
        }

        let count = remaining_query_terms.intersection(&candidate_terms).count();
        let overlap = u32::try_from(count).unwrap_or(u32::MAX);

        scored.push(ScoredSymbol {
            symbol_id,
            overlap,
            qual_name,
            path: file_path,
            start_line: sym.span.start_line(),
        });
    }

    scored.sort_unstable_by(|left, right| {
        right
            .overlap
            .cmp(&left.overlap)
            .then_with(|| left.qual_name.as_bytes().cmp(right.qual_name.as_bytes()))
            .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });

    if !scored.is_empty()
        && scored[0].overlap > 0
        && (scored.len() == 1 || scored[0].overlap > scored[1].overlap)
    {
        return ExactOutcome::Direct(Candidate::new(
            TargetId::Symbol(scored[0].symbol_id),
            Some(Confidence::ONE),
        ));
    }

    scored.truncate(3);
    let final_candidates: SmallVec<[Candidate; 3]> = scored
        .into_iter()
        .map(|entry| Candidate::new(TargetId::Symbol(entry.symbol_id), None))
        .collect();

    ExactOutcome::Candidates(final_candidates)
}

fn find_file_id_by_path(snapshot: &Snapshot, path: &str) -> Option<FileId> {
    let result = snapshot.files.binary_search_by(|file| {
        let file_path = snapshot
            .strings
            .get(file.path.index())
            .map_or("", String::as_str);
        file_path.as_bytes().cmp(path.as_bytes())
    });
    result.ok().map(|index| snapshot.files[index].id)
}

fn lookup_exact_qualified<'a>(snapshot: &'a Snapshot, key: &str) -> Option<&'a [SymbolId]> {
    let result = snapshot.exact_qualified.binary_search_by(|route| {
        let route_key = snapshot
            .strings
            .get(route.key.index())
            .map_or("", String::as_str);
        route_key.as_bytes().cmp(key.as_bytes())
    });
    result
        .ok()
        .map(|index| snapshot.exact_qualified[index].symbols.as_slice())
}

fn lookup_exact_name<'a>(snapshot: &'a Snapshot, key: &str) -> Option<&'a [SymbolId]> {
    let result = snapshot.exact_names.binary_search_by(|route| {
        let route_key = snapshot
            .strings
            .get(route.key.index())
            .map_or("", String::as_str);
        route_key.as_bytes().cmp(key.as_bytes())
    });
    result
        .ok()
        .map(|index| snapshot.exact_names[index].symbols.as_slice())
}

#[must_use]
pub fn finish_exact(outcome: ExactOutcome) -> QueryResult {
    match outcome {
        ExactOutcome::Direct(candidate) => QueryResult::Direct {
            candidate,
            pipeline: Pipeline::Exact,
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
