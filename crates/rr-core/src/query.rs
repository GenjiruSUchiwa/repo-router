use std::cmp::Ordering;

use smallvec::SmallVec;
use unicode_ident::{is_xid_continue, is_xid_start};

use crate::facts::DefKind;
use crate::index::{FileId, FileRecord, Snapshot, SymbolId};
use crate::lex::{query_terms, LexicalField, QueryTerms, TermId};
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
    /// Bare name, with the path the whole of what an anchor can say.
    name: &'a str,
    kind: DefKind,
    start_line: u32,
}

/// Reports whether two scored symbols would print as one anchor.
fn share_one_anchor(left: &ScoredSymbol<'_>, right: &ScoredSymbol<'_>) -> bool {
    left.path == right.path && left.name == right.name
}

/// Ranks the kinds that can collide under one name, worst last.
///
/// A C# or Java constructor is named after the class it builds, so both answer
/// to `Fichier.cs#Foo`. Asked for `Foo`, a caller means the type.
const fn kind_precedence(kind: DefKind) -> u8 {
    match kind {
        DefKind::Class
        | DefKind::Interface
        | DefKind::Struct
        | DefKind::Enum
        | DefKind::TypeAlias
        | DefKind::Trait => 2,
        DefKind::Constructor => 0,
        _ => 1,
    }
}

/// Reports whether `candidate` should displace `held`, the two sharing an anchor.
fn displaces(candidate: &ScoredSymbol<'_>, held: &ScoredSymbol<'_>) -> bool {
    match candidate.overlap.cmp(&held.overlap) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => kind_precedence(candidate.kind) > kind_precedence(held.kind),
    }
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

/// Inserts one candidate into the sorted top three.
///
/// Two symbols in one file under one name — a class and its constructor, two
/// overloads under a tags-tier grammar — print as the same `path#name`, so a
/// list carrying both offers a choice the anchor cannot express, and spends two
/// of the three places the v1 contract has. Only the better survives, and it
/// stays one entry: a list of three identical lines answers nothing, and two
/// tied entries also defeat the margin that would have earned a direct answer.
fn retain_top_three<'a>(top: &mut SmallVec<[ScoredSymbol<'a>; 3]>, candidate: ScoredSymbol<'a>) {
    if let Some(held) = top
        .iter()
        .position(|entry| share_one_anchor(entry, &candidate))
    {
        if !displaces(&candidate, &top[held]) {
            return;
        }
        top.remove(held);
    }
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

/// Fields read when the name and the path fail to separate two candidates.
///
/// Ordered widest first is tempting; they are read as a set, and the count of
/// them is also the weight one name or path hit carries, so no run of content
/// hits can outrank a single hit on what the symbol is called.
const CONTENT_FIELDS: [LexicalField; 3] = [
    LexicalField::Signature,
    LexicalField::Body,
    LexicalField::Documentation,
];

/// Reports whether any symbol in `file` carries `term` in a content field.
///
/// The file, not the symbol, is the unit that can answer. What separates two
/// same-named types is usually written in one of their members — the word
/// `bulk` belongs to a `Convert(BulkPayload)` method, not to the class — and a
/// tags-tier body scan deliberately keeps a member's text out of the type that
/// encloses it. Asking the symbol alone would therefore never see the word.
/// Asking the file is also the honest unit: what this routine chooses between
/// is `path#name` anchors, so the file is already half of the identity.
///
/// A file owns a contiguous run of symbol ids, posting lists are sorted by term
/// text and their entries by symbol id, so this is two binary searches over
/// data the index already holds.
fn file_carries_term(snapshot: &Snapshot, file: &FileRecord, term: TermId) -> bool {
    let Some(text) = term_text(snapshot, term) else {
        return false;
    };
    let first = file.first_symbol as usize;
    let end = first.saturating_add(file.symbol_count as usize);
    CONTENT_FIELDS.iter().any(|&field| {
        let lists = snapshot.postings.lists(field);
        lists
            .binary_search_by(|list| term_text(snapshot, list.term).unwrap_or("").cmp(text))
            .is_ok_and(|position| {
                let entries = &lists[position].entries;
                let start = entries.partition_point(|entry| entry.symbol.index() < first);
                entries
                    .get(start)
                    .is_some_and(|entry| entry.symbol.index() < end)
            })
    })
}

/// Resolves a term to its text, or `None` if the index does not hold it.
fn term_text(snapshot: &Snapshot, term: TermId) -> Option<&str> {
    let record = snapshot.terms.get(term.index())?;
    snapshot
        .strings
        .get(record.text.index())
        .map(String::as_str)
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
        let name = snapshot
            .strings
            .get(sym.name.index())
            .map_or("", String::as_str);
        let qual_terms = query_terms(qual_name, snapshot);
        let path_terms = query_terms(file_path, snapshot);
        let named = remaining_query_terms
            .iter()
            .filter(|term| {
                qual_terms.as_slice().contains(term) || path_terms.as_slice().contains(term)
            })
            .count();
        // The word that tells two same-named symbols apart is often in neither
        // the name nor the path: a `convert(BulkPayload)` overload puts it in
        // the signature. Those fields are indexed already, so consulting
        // them costs one binary search per term and settles ties the name alone
        // leaves open. They are counted below the name and the path, never
        // beside them, so a body word cannot outvote a path word.
        let carried = remaining_query_terms
            .iter()
            .filter(|term| {
                !qual_terms.as_slice().contains(term)
                    && !path_terms.as_slice().contains(term)
                    && file_carries_term(snapshot, file, **term)
            })
            .count();
        let overlap = u32::try_from(named * CONTENT_FIELDS.len() + carried).unwrap_or(u32::MAX);

        retain_top_three(
            &mut top,
            ScoredSymbol {
                symbol_id,
                overlap,
                qual_name,
                path: file_path,
                name,
                kind: sym.kind,
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

/// The symbol an anchor names, if the snapshot holds exactly one.
///
/// Exactly one is the whole contract. A file can hold two public definitions
/// with one name — a struct and its constructor function, two overloads under a
/// tags-tier grammar — and a cache that picked between them would answer a
/// question the user never asked. Two matches is a miss, and a miss costs one
/// ordinary query.
///
/// `None` also for a file anchor: an anchor with no `#` names a file, and a
/// file has no scope API identity to go stale against.
#[must_use]
pub fn resolve_route_anchor(snapshot: &Snapshot, anchor: &str) -> Option<SymbolId> {
    let (path, symbol_name) = crate::render::decode_anchor(anchor).ok()?;
    let name = symbol_name?;
    let file_id = find_file_id_by_path(snapshot, path.as_str())?;
    let symbols = lookup_exact_name(snapshot, &name)?;
    let mut matched = symbols
        .iter()
        .copied()
        .filter(|id| symbol_matches_path(snapshot, *id, Some(file_id)));
    let only = matched.next()?;
    if matched.next().is_some() {
        return None;
    }
    Some(only)
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::index::{ContentRepresentation, FileInput, SnapshotBuilder, SnapshotMeta};
    use crate::lang::Lang;
    use crate::oid::Oid;
    use crate::parser::RustExtractor;
    use crate::render::encode_anchor;
    use crate::result::resolve_anchor;

    fn snapshot(sources: &[(&str, &str)]) -> Snapshot {
        let mut extractor = RustExtractor::new().unwrap();
        let inputs = sources
            .iter()
            .map(|(path, code)| FileInput {
                path: RelPath::new(*path).unwrap(),
                oid: Oid::from_raw(blake3::hash(code.as_bytes()).as_bytes()).unwrap(),
                representation: ContentRepresentation::RawNoGit,
                generated: false,
                language: Lang::Rust,
                parse_status: crate::facts::ParseStatus::Complete,
                facts: extractor.extract(code.as_bytes()).unwrap(),
            })
            .collect();
        let (snapshot, _) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
            .build(inputs)
            .unwrap();
        snapshot
    }

    /// The round trip a stored route depends on: what `rr query` printed is what
    /// `.rr/ROUTES.md` holds, and reading it back names the same symbol.
    #[test]
    fn an_anchor_round_trips_to_the_symbol_it_named() {
        let snapshot = snapshot(&[
            (
                "src/auth/token.rs",
                "pub fn verify_token() -> bool { true }\n",
            ),
            ("src/auth/keys.rs", "pub fn rotate_signing_key() {}\n"),
        ]);

        for symbol in &snapshot.symbols {
            let anchor = resolve_anchor(&snapshot, TargetId::Symbol(symbol.id)).unwrap();
            let encoded = encode_anchor(anchor.path, anchor.symbol);
            assert_eq!(
                resolve_route_anchor(&snapshot, &encoded),
                Some(symbol.id),
                "the anchor {encoded} did not resolve back to the symbol it names"
            );
        }
    }

    /// Exactly one is the whole contract. Two public definitions of one name in
    /// one file make the anchor a question the snapshot cannot answer, and
    /// answering it with the first one would be a silently wrong route.
    #[test]
    fn an_ambiguous_anchor_resolves_to_nothing() {
        let snapshot = snapshot(&[(
            "src/auth/token.rs",
            "pub mod issue { pub fn verify() {} }\npub mod refresh { pub fn verify() {} }\n",
        )]);
        let named = snapshot
            .symbols
            .iter()
            .filter(|symbol| {
                snapshot
                    .strings
                    .get(symbol.name.index())
                    .is_some_and(|name| name == "verify")
            })
            .count();
        assert_eq!(named, 2, "the fixture must be ambiguous to test ambiguity");

        assert_eq!(
            resolve_route_anchor(&snapshot, "src/auth/token.rs#verify"),
            None
        );
    }

    fn route(snapshot: &Snapshot, text: &str) -> ExactOutcome {
        let parsed = parse_query(snapshot, QueryRequest::new(text, None)).unwrap();
        route_exact(snapshot, &parsed)
    }

    fn anchors(snapshot: &Snapshot, candidates: &[Candidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| {
                let anchor = resolve_anchor(snapshot, candidate.target).unwrap();
                encode_anchor(anchor.path, anchor.symbol)
            })
            .collect()
    }

    /// A C# class and its constructor, two overloads under a tags-tier grammar,
    /// two same-named functions under different scopes: all render one anchor.
    /// Offered twice it is not a choice, it spends a place the contract caps at
    /// three, and — the part that costs an answer — it ties with itself, so the
    /// margin that would have earned a direct route never appears.
    #[test]
    fn one_anchor_is_offered_once() {
        let snapshot = snapshot(&[(
            "src/auth/token.rs",
            "pub mod issue { pub fn verify() {} }\npub mod refresh { pub fn verify() {} }\n",
        )]);

        let ExactOutcome::Candidates(candidates) = route(&snapshot, "verify") else {
            panic!("two same-named symbols must be offered as candidates");
        };
        assert_eq!(
            anchors(&snapshot, &candidates),
            vec!["src/auth/token.rs#verify"],
            "the duplicate anchor was offered twice"
        );
    }

    /// The word that separates two same-named symbols is often in neither the
    /// name nor the path. Reading the signature and the body turns a list of
    /// candidates into the answer the caller asked for.
    #[test]
    fn a_body_word_separates_two_same_named_symbols() {
        let snapshot = snapshot(&[
            (
                "src/convert/single.rs",
                "pub fn convert_status() { let payload = 1; let _ = payload; }\n",
            ),
            (
                "src/convert/many.rs",
                "pub fn convert_status() { let bulk_payload = 1; let _ = bulk_payload; }\n",
            ),
        ]);

        assert!(
            matches!(
                route(&snapshot, "convert_status"),
                ExactOutcome::Candidates(_)
            ),
            "the bare name alone cannot separate the two"
        );

        let ExactOutcome::Direct(candidate) = route(&snapshot, "convert_status bulk") else {
            panic!("the body word must settle the choice");
        };
        let anchor = resolve_anchor(&snapshot, candidate.target).unwrap();
        assert_eq!(anchor.path, "src/convert/many.rs");
    }

    /// Pins D5: a file target has no owning scope record, so a file anchor could
    /// never be told it went stale and is never a route.
    #[test]
    fn a_file_anchor_resolves_to_nothing() {
        let snapshot = snapshot(&[("src/auth/token.rs", "pub fn verify_token() {}\n")]);
        assert_eq!(resolve_route_anchor(&snapshot, "src/auth/token.rs"), None);
    }
}
