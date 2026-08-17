//! Rust facts extraction via Tree-sitter.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::facts::{
    def_key, import_key, reference_key, Def, DefKind, DegradedReason, Facts, Import, ImportKind,
    OwnerIndex, ParseStatus, Reference, ReferenceKind, Span, TestSignals, Visibility,
};
use crate::lang::Lang;
use crate::{Error, Result};

use super::{degraded_facts, scan_idents};

const QUERY_SOURCE: &str = include_str!("queries/rust.scm");

/// Captures this extractor's query must declare, checked once at construction.
///
/// A capture belongs here when [`route_capture`] reads it. It is not a list of
/// everything the query captures and must not become one: an entry here is a
/// build-time obligation on `queries/rust.scm`, so a capture that is required
/// and unrouted forces every future editor of that file to keep a pattern alive
/// for matches that are discarded. `@doc` was exactly that until #41 removed
/// it — documentation reaches [`Def::doc_idents`] through
/// [`attached_metadata`], which reads an attachment rule no query can express.
const REQUIRED_CAPTURES: &[&str] = &[
    "def.item",
    "def.name",
    "def.body",
    "def.signature",
    "reference.call",
    "reference.method",
    "reference.macro",
    "reference.implementation",
    "reference.name",
    "import.declaration",
    "identifier",
    "attribute",
    "syntax.error",
    "syntax.missing",
];

/// Stateful Rust extractor: one Tree-sitter parser/query/cursor per worker.
pub struct RustExtractor {
    parser: Parser,
    query: Query,
    cursor: QueryCursor,
    capture: CaptureIds,
}

#[derive(Debug, Clone, Copy)]
struct CaptureIds {
    def_item: u32,
    def_name: u32,
    def_body: u32,
    def_signature: u32,
    reference_call: u32,
    reference_method: u32,
    reference_macro: u32,
    reference_implementation: u32,
    reference_name: u32,
    import_declaration: u32,
    identifier: u32,
    attribute: u32,
    syntax_error: u32,
    syntax_missing: u32,
}

/// Per-definition data needed only while assigning identifiers, dropped after
/// finalization.
struct DefExtras {
    name_span: Span,
    body_span: Option<Span>,
}

struct PendingReference {
    name: String,
    qualified: Option<String>,
    kind: ReferenceKind,
    span: Span,
}

struct PendingImport {
    kind: ImportKind,
    path: String,
    alias: Option<String>,
    is_public: bool,
    is_glob: bool,
    span: Span,
}

struct PendingIdent {
    text: String,
    span: Span,
}

/// Intermediate capture bag filled by one query pass.
#[derive(Default)]
struct CaptureBag {
    defs: BTreeMap<(u32, u32), (Def, DefExtras)>,
    references: Vec<PendingReference>,
    imports: Vec<PendingImport>,
    idents: Vec<PendingIdent>,
    attribute_spans: Vec<Span>,
    error_spans: Vec<Span>,
    error_keys: BTreeSet<(u32, u32)>,
    missing_keys: BTreeSet<(u32, u32)>,
}

/// Captures belonging to a single query match, before materialization.
#[derive(Default)]
struct MatchCaptures<'a> {
    def_item: Option<Node<'a>>,
    def_name: Option<Node<'a>>,
    def_body: Option<Node<'a>>,
    ref_kind: Option<ReferenceKind>,
    ref_name: Option<Node<'a>>,
    import_decl: Option<Node<'a>>,
}

impl CaptureBag {
    fn record_error(&mut self, span: Span) {
        if self.error_keys.insert((span.start_byte(), span.end_byte())) {
            self.error_spans.push(span);
        }
    }

    fn record_missing(&mut self, span: Span) {
        self.missing_keys
            .insert((span.start_byte(), span.end_byte()));
    }

    fn diagnostic_counts(&self) -> (u32, u32) {
        (
            u32::try_from(self.error_keys.len()).unwrap_or(u32::MAX),
            u32::try_from(self.missing_keys.len()).unwrap_or(u32::MAX),
        )
    }
}

/// Spans sorted by `(start_byte, end_byte)` supporting containment lookups in
/// O(log n + overlap) instead of a full scan per query.
struct SortedSpans {
    spans: Vec<Span>,
    /// `prefix_max_end[i]` is the maximum `end_byte` over `spans[0..=i]`.
    prefix_max_end: Vec<u32>,
}

impl SortedSpans {
    fn new(mut spans: Vec<Span>) -> Self {
        spans.sort_by_key(|span| (span.start_byte(), span.end_byte()));
        let mut prefix_max_end = Vec::with_capacity(spans.len());
        let mut max_end = 0_u32;
        for span in &spans {
            max_end = max_end.max(span.end_byte());
            prefix_max_end.push(max_end);
        }
        Self {
            spans,
            prefix_max_end,
        }
    }

    /// Returns true when any stored span fully contains `span`.
    fn contains(&self, span: Span) -> bool {
        let first_after = self
            .spans
            .partition_point(|candidate| candidate.start_byte() <= span.start_byte());
        for index in (0..first_after).rev() {
            if self.prefix_max_end[index] < span.end_byte() {
                return false;
            }
            if self.spans[index].contains(span) {
                return true;
            }
        }
        false
    }
}

impl RustExtractor {
    /// Sets the pinned Rust language, compiles the embedded query, and validates
    /// required capture names.
    ///
    /// # Errors
    /// Returns language setup, query compilation, or capture-contract errors.
    pub fn new() -> Result<Self> {
        Self::build(QUERY_SOURCE, None)
    }

    fn build(query_source: &str, match_limit: Option<u32>) -> Result<Self> {
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|err| Error::ExtractorLanguage {
                lang: Lang::Rust,
                message: err.to_string(),
            })?;

        let query = Query::new(&language, query_source).map_err(|err| Error::ExtractorQuery {
            lang: Lang::Rust,
            row: err.row,
            column: err.column,
            message: err.message,
        })?;

        let capture = CaptureIds::resolve(&query)?;
        let mut cursor = QueryCursor::new();
        if let Some(limit) = match_limit {
            cursor.set_match_limit(limit);
        }

        Ok(Self {
            parser,
            query,
            cursor,
            capture,
        })
    }

    /// Extracts facts from exactly these bytes.
    ///
    /// Syntax/encoding/resource failures return degraded [`Facts`]; only
    /// programmer/invariant failures return `Err`.
    ///
    /// # Errors
    /// Returns extraction invariant failures and facts validation errors.
    pub fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        if content.len() > u32::MAX as usize {
            return Ok(degraded_facts(content, DegradedReason::SourceTooLarge));
        }

        let Ok(source) = std::str::from_utf8(content) else {
            return Ok(degraded_facts(content, DegradedReason::InvalidUtf8));
        };

        let Some(tree) = self.parser.parse(content, None) else {
            return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
        };

        self.collect_facts(source, &tree)
    }

    fn collect_facts(&mut self, source: &str, tree: &Tree) -> Result<Facts> {
        let bag = self.collect_captures(source, tree.root_node())?;
        if self.cursor.did_exceed_match_limit() {
            return Ok(degraded_facts(
                source.as_bytes(),
                DegradedReason::QueryMatchLimit,
            ));
        }
        finalize_facts(bag)
    }

    fn collect_captures(&mut self, source: &str, root: Node<'_>) -> Result<CaptureBag> {
        let ids = self.capture;
        let mut bag = CaptureBag::default();
        let mut matches = self.cursor.matches(&self.query, root, source.as_bytes());
        while let Some(query_match) = matches.next() {
            ingest_match(ids, query_match, source, &mut bag)?;
        }
        Ok(bag)
    }
}

fn ingest_match(
    ids: CaptureIds,
    query_match: &tree_sitter::QueryMatch<'_, '_>,
    source: &str,
    bag: &mut CaptureBag,
) -> Result<()> {
    let mut view = MatchCaptures::default();
    for capture in query_match.captures {
        route_capture(ids, capture.index, capture.node, source, &mut view, bag)?;
    }
    materialize_match(&view, source, bag)
}

fn route_capture<'a>(
    ids: CaptureIds,
    id: u32,
    node: Node<'a>,
    source: &str,
    view: &mut MatchCaptures<'a>,
    bag: &mut CaptureBag,
) -> Result<()> {
    match id {
        i if i == ids.def_item || i == ids.def_signature => view.def_item = Some(node),
        i if i == ids.def_name => view.def_name = Some(node),
        i if i == ids.def_body => view.def_body = Some(node),
        i if i == ids.reference_call => view.ref_kind = Some(ReferenceKind::Call),
        i if i == ids.reference_method => view.ref_kind = Some(ReferenceKind::MethodCall),
        i if i == ids.reference_macro => view.ref_kind = Some(ReferenceKind::MacroCall),
        i if i == ids.reference_implementation => {
            view.ref_kind = Some(ReferenceKind::Implementation);
        }
        i if i == ids.reference_name => view.ref_name = Some(node),
        i if i == ids.import_declaration => view.import_decl = Some(node),
        i if i == ids.identifier => push_ident(node, source, &mut bag.idents)?,
        i if i == ids.attribute => bag.attribute_spans.push(node_span(node, source)?),
        i if i == ids.syntax_error => bag.record_error(node_span(node, source)?),
        i if i == ids.syntax_missing => bag.record_missing(node_span(node, source)?),
        _ => {}
    }
    Ok(())
}

fn materialize_match(view: &MatchCaptures<'_>, source: &str, bag: &mut CaptureBag) -> Result<()> {
    if let (Some(item), Some(name_node)) = (view.def_item, view.def_name) {
        try_insert_def(item, name_node, view.def_body, source, &mut bag.defs)?;
    }
    if let (Some(kind), Some(name_node)) = (view.ref_kind, view.ref_name) {
        bag.references
            .push(build_reference(kind, name_node, source)?);
    }
    if let Some(decl) = view.import_decl {
        expand_import_declaration(decl, source, &mut bag.imports)?;
    }
    Ok(())
}

fn push_ident(node: Node<'_>, source: &str, out: &mut Vec<PendingIdent>) -> Result<()> {
    let Some(text) = node_text(node, source) else {
        return Ok(());
    };
    out.push(PendingIdent {
        text: text.to_string(),
        span: node_span(node, source)?,
    });
    Ok(())
}

fn try_insert_def(
    item: Node<'_>,
    name_node: Node<'_>,
    body: Option<Node<'_>>,
    source: &str,
    defs: &mut BTreeMap<(u32, u32), (Def, DefExtras)>,
) -> Result<()> {
    let item_span = node_span(item, source)?;
    let key = (item_span.start_byte(), item_span.end_byte());
    let std::collections::btree_map::Entry::Vacant(slot) = defs.entry(key) else {
        return Ok(());
    };

    let name = required_text(name_node, source, "definition name is not valid UTF-8")?;
    let kind = classify_definition(item)?;
    let mut body_span = body.map(|node| node_span(node, source)).transpose()?;
    if kind == DefKind::Macro && body_span.is_none() {
        body_span = macro_body_span(item, source)?;
    }
    let expanded = expanded_item_span(item, source)?;
    let (doc_idents, attribute_idents) = metadata_idents(item, source);

    let signature_span = signature_span(kind, body_span, expanded, source)?;
    let def = Def {
        signature: declaration_line(item_span, signature_span, source, &name),
        name,
        local_qualified: local_qualified_name(item, source)?,
        kind,
        visibility: visibility(item, source),
        span: expanded,
        signature_span,
        signature_idents: Vec::new(),
        body_idents: Vec::new(),
        doc_idents,
        attribute_idents,
        test_signals: test_signals(item, source),
    };
    let extras = DefExtras {
        name_span: node_span(name_node, source)?,
        body_span,
    };
    slot.insert((def, extras));
    Ok(())
}

fn build_reference(
    kind: ReferenceKind,
    name_node: Node<'_>,
    source: &str,
) -> Result<PendingReference> {
    let full = required_text(name_node, source, "reference name is not valid UTF-8")?;
    let (name, qualified) = normalize_reference(kind, &full, name_node);
    Ok(PendingReference {
        name,
        qualified,
        kind,
        span: node_span(name_node, source)?,
    })
}

fn required_text(node: Node<'_>, source: &str, message: &'static str) -> Result<String> {
    node_text(node, source)
        .map(str::to_string)
        .ok_or(Error::ExtractionInvariant { message })
}

fn finalize_facts(bag: CaptureBag) -> Result<Facts> {
    let (error_nodes, missing_nodes) = bag.diagnostic_counts();
    let mut items: Vec<(Def, DefExtras)> = bag.defs.into_values().collect();
    items.sort_by(|left, right| def_key(&left.0).cmp(&def_key(&right.0)));
    let (mut defs, extras): (Vec<Def>, Vec<DefExtras>) = items.into_iter().unzip();

    let owners = OwnerIndex::new(&defs);
    let error_spans = SortedSpans::new(bag.error_spans);
    let references = finalize_references(bag.references, &error_spans, &owners);
    let imports = finalize_imports(bag.imports, &error_spans, &owners);
    assign_idents(
        bag.idents,
        &extras,
        &mut defs,
        &owners,
        &references,
        &imports,
        bag.attribute_spans,
        &error_spans,
    );

    let status = parse_status(error_nodes, missing_nodes);
    Facts::from_parts(defs, references, imports, status)
}

fn finalize_references(
    pending: Vec<PendingReference>,
    error_spans: &SortedSpans,
    owners: &OwnerIndex,
) -> Vec<Reference> {
    let mut references: Vec<Reference> = pending
        .into_iter()
        .filter(|item| !error_spans.contains(item.span))
        .map(|item| Reference {
            name: item.name,
            qualified: item.qualified,
            kind: item.kind,
            span: item.span,
            owner: owners.nearest(item.span),
        })
        .collect();
    references.sort_by(|left, right| reference_key(left).cmp(&reference_key(right)));
    references
}

fn finalize_imports(
    pending: Vec<PendingImport>,
    error_spans: &SortedSpans,
    owners: &OwnerIndex,
) -> Vec<Import> {
    let mut imports: Vec<Import> = pending
        .into_iter()
        .filter(|item| !error_spans.contains(item.span))
        .map(|item| Import {
            kind: item.kind,
            path: item.path,
            name: None,
            alias: item.alias,
            is_public: item.is_public,
            is_glob: item.is_glob,
            span: item.span,
            owner: owners.nearest_import_owner(item.span),
        })
        .collect();
    imports.sort_by(|left, right| import_key(left).cmp(&import_key(right)));
    imports
}

#[allow(clippy::too_many_arguments)]
fn assign_idents(
    idents: Vec<PendingIdent>,
    extras: &[DefExtras],
    defs: &mut [Def],
    owners: &OwnerIndex,
    references: &[Reference],
    imports: &[Import],
    attribute_spans: Vec<Span>,
    error_spans: &SortedSpans,
) {
    let name_spans: HashSet<(u32, u32)> = extras
        .iter()
        .map(|extra| (extra.name_span.start_byte(), extra.name_span.end_byte()))
        .collect();
    let reference_spans = SortedSpans::new(references.iter().map(|item| item.span).collect());
    let import_spans = SortedSpans::new(imports.iter().map(|item| item.span).collect());
    let attribute_spans = SortedSpans::new(attribute_spans);

    let mut signature_idents = vec![Vec::new(); defs.len()];
    let mut body_idents = vec![Vec::new(); defs.len()];

    for ident in idents {
        if name_spans.contains(&(ident.span.start_byte(), ident.span.end_byte()))
            || reference_spans.contains(ident.span)
            || import_spans.contains(ident.span)
            || attribute_spans.contains(ident.span)
            || error_spans.contains(ident.span)
        {
            continue;
        }
        let Some(owner) = owners.nearest(ident.span) else {
            continue;
        };
        let idx = owner.index();
        let body = extras[idx].body_span;
        if defs[idx].signature_span.contains(ident.span)
            && body.is_none_or(|body| !body.contains(ident.span))
        {
            signature_idents[idx].push(ident.text);
        } else if body.is_some_and(|body| body.contains(ident.span)) {
            body_idents[idx].push(ident.text);
        }
    }

    for (idx, def) in defs.iter_mut().enumerate() {
        def.signature_idents = std::mem::take(&mut signature_idents[idx]);
        def.body_idents = std::mem::take(&mut body_idents[idx]);
    }
}

const fn parse_status(error_nodes: u32, missing_nodes: u32) -> ParseStatus {
    if error_nodes == 0 && missing_nodes == 0 {
        ParseStatus::Complete
    } else {
        ParseStatus::Recovered {
            error_nodes,
            missing_nodes,
        }
    }
}

impl CaptureIds {
    fn resolve(query: &Query) -> Result<Self> {
        let names = query.capture_names();
        let mut index_of: HashMap<&str, u32> = HashMap::new();
        for (idx, name) in names.iter().enumerate() {
            index_of.insert(
                name,
                u32::try_from(idx).map_err(|_| Error::ExtractionInvariant {
                    message: "capture index exceeds u32",
                })?,
            );
        }
        for required in REQUIRED_CAPTURES {
            if !index_of.contains_key(required) {
                return Err(Error::ExtractorQueryContract { capture: required });
            }
        }
        Ok(Self {
            def_item: index_of["def.item"],
            def_name: index_of["def.name"],
            def_body: index_of["def.body"],
            def_signature: index_of["def.signature"],
            reference_call: index_of["reference.call"],
            reference_method: index_of["reference.method"],
            reference_macro: index_of["reference.macro"],
            reference_implementation: index_of["reference.implementation"],
            reference_name: index_of["reference.name"],
            import_declaration: index_of["import.declaration"],
            identifier: index_of["identifier"],
            attribute: index_of["attribute"],
            syntax_error: index_of["syntax.error"],
            syntax_missing: index_of["syntax.missing"],
        })
    }
}

fn checked_u32(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::ExtractionInvariant {
        message: "value exceeds u32::MAX",
    })
}

fn node_span(node: Node<'_>, source: &str) -> Result<Span> {
    let start_byte = checked_u32(node.start_byte())?;
    let end_byte = checked_u32(node.end_byte())?;
    if end_byte as usize > source.len() {
        return Err(Error::ExtractionInvariant {
            message: "tree-sitter node exceeds source length",
        });
    }
    let bytes = source.as_bytes();
    if !source.is_char_boundary(start_byte as usize) || !source.is_char_boundary(end_byte as usize)
    {
        return Err(Error::ExtractionInvariant {
            message: "tree-sitter node is not on a UTF-8 boundary",
        });
    }

    let start_line = checked_u32(node.start_position().row)?
        .checked_add(1)
        .ok_or(Error::ExtractionInvariant {
            message: "start line overflow",
        })?;

    let end_row = checked_u32(node.end_position().row)?;
    let mut end_line = end_row.checked_add(1).ok_or(Error::ExtractionInvariant {
        message: "end line overflow",
    })?;
    if end_byte > start_byte && bytes[end_byte as usize - 1] == b'\n' {
        end_line = end_line.saturating_sub(1);
    }
    if end_byte == start_byte {
        end_line = start_line;
    }

    Span::new(start_byte, end_byte, start_line, end_line)
}

fn expanded_item_span(node: Node<'_>, source: &str) -> Result<Span> {
    let item_span = node_span(node, source)?;
    let meta = attached_metadata(node);
    let Some(first) = meta.first() else {
        return Ok(item_span);
    };
    let first_span = node_span(*first, source)?;
    Span::new(
        first_span.start_byte(),
        item_span.end_byte(),
        first_span.start_line(),
        item_span.end_line(),
    )
}

/// Whether this definition kind ends in `;` with no block body; its signature
/// is the whole item span.
const fn is_semicolon_definition(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::TypeAlias | DefKind::AssociatedType | DefKind::Const | DefKind::Static
    )
}

/// The one-line declaration text a reader is shown for this definition.
///
/// Runs from the item itself to the end of the signature region, so the
/// documentation and attributes that [`Def::span`] deliberately absorbs stay
/// out of a line that is meant to read like the declaration. A region that
/// holds nothing but whitespace — which only a recovered parse can produce —
/// falls back to the name, because every other consumer of this field may
/// assume it is non-empty.
fn declaration_line(item: Span, signature: Span, source: &str, name: &str) -> String {
    let start = item.start_byte() as usize;
    let end = signature.end_byte() as usize;
    let raw = source.get(start..end).unwrap_or_default();
    let line = crate::facts::display_signature(raw);
    if line.is_empty() {
        name.to_owned()
    } else {
        line
    }
}
#[allow(clippy::naive_bytecount)]
fn signature_span(
    kind: DefKind,
    body_span: Option<Span>,
    expanded: Span,
    source: &str,
) -> Result<Span> {
    let Some(body) = body_span else {
        return Ok(expanded);
    };
    if is_semicolon_definition(kind) {
        return Ok(expanded);
    }
    let bytes = source.as_bytes();
    let mut end = body.start_byte() as usize;
    while end > expanded.start_byte() as usize {
        let prev = end - 1;
        if bytes[prev].is_ascii_whitespace() {
            end = prev;
        } else {
            break;
        }
    }
    let end_byte = checked_u32(end)?;
    let end_line = if end_byte == expanded.start_byte() {
        expanded.start_line()
    } else {
        let start = expanded.start_byte() as usize;
        let newlines = bytes[start..end - 1]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        expanded
            .start_line()
            .saturating_add(u32::try_from(newlines).unwrap_or(u32::MAX))
    };
    Span::new(
        expanded.start_byte(),
        end_byte,
        expanded.start_line(),
        end_line,
    )
}

/// Body of a `macro_rules!` definition: the delimited rules block. The grammar
/// exposes the rules as a flat child list, so the query cannot capture one
/// body node.
fn macro_body_span(node: Node<'_>, source: &str) -> Result<Option<Span>> {
    let mut open = None;
    let mut close = None;
    let mut walk = node.walk();
    for child in node.children(&mut walk) {
        match child.kind() {
            "{" | "(" | "[" if open.is_none() => open = Some(child),
            "}" | ")" | "]" => close = Some(child),
            _ => {}
        }
    }
    let (Some(open), Some(close)) = (open, close) else {
        return Ok(None);
    };
    let open_span = node_span(open, source)?;
    let close_span = node_span(close, source)?;
    Ok(Some(Span::new(
        open_span.start_byte(),
        close_span.end_byte(),
        open_span.start_line(),
        close_span.end_line(),
    )?))
}

fn classify_definition(node: Node<'_>) -> Result<DefKind> {
    match node.kind() {
        "function_item" | "function_signature_item" => Ok(classify_function_like(node)),
        "struct_item" => Ok(DefKind::Struct),
        "enum_item" => Ok(DefKind::Enum),
        "union_item" => Ok(DefKind::Union),
        "trait_item" => Ok(DefKind::Trait),
        "type_item" => Ok(DefKind::TypeAlias),
        "associated_type" => Ok(DefKind::AssociatedType),
        "const_item" => Ok(DefKind::Const),
        "static_item" => Ok(DefKind::Static),
        "mod_item" => Ok(DefKind::Module),
        "macro_definition" => Ok(DefKind::Macro),
        _ => Err(Error::ExtractionInvariant {
            message: "unexpected definition node kind",
        }),
    }
}

fn classify_function_like(node: Node<'_>) -> DefKind {
    let Some(parent) = node.parent() else {
        return DefKind::Function;
    };
    if parent.kind() != "declaration_list" {
        return DefKind::Function;
    }
    match parent.parent().map(|n| n.kind()) {
        Some("impl_item") => DefKind::Method,
        Some("trait_item") => DefKind::TraitMethod,
        _ => DefKind::Function,
    }
}

fn visibility(node: Node<'_>, source: &str) -> Visibility {
    let Some(vis) = child_by_kind(node, "visibility_modifier") else {
        return Visibility::Private;
    };
    let mut walk = vis.walk();
    let has_in = vis.children(&mut walk).any(|child| child.kind() == "in");
    let mut walk = vis.walk();
    let mut named = vis.named_children(&mut walk);
    let Some(inner) = named.next() else {
        return Visibility::Public;
    };
    match inner.kind() {
        "crate" if !has_in => Visibility::Crate,
        "self" | "super" => Visibility::Restricted(inner.kind().to_string()),
        _ => {
            let text = collapse_ws(node_text(inner, source).unwrap_or(""));
            Visibility::Restricted(text)
        }
    }
}

fn local_qualified_name(node: Node<'_>, source: &str) -> Result<Option<String>> {
    let mut segments: Vec<String> = Vec::new();
    let mut cursor = node.parent();
    while let Some(current) = cursor {
        match current.kind() {
            "source_file" => break,
            "impl_item" => {
                segments.push(impl_qualifier(current, source)?);
            }
            _ if is_definition_item(current) => {
                if let Some(name) = definition_name_text(current, source) {
                    segments.push(name.to_string());
                }
            }
            _ => {}
        }
        cursor = current.parent();
    }
    if segments.is_empty() {
        return Ok(None);
    }
    segments.reverse();
    let name = definition_name_text(node, source).unwrap_or("");
    segments.push(name.to_string());
    Ok(Some(segments.join("::")))
}

fn definition_name_text<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    field_text(node, "name", source)
}

fn impl_qualifier(node: Node<'_>, source: &str) -> Result<String> {
    let type_text = node
        .child_by_field_name("type")
        .and_then(|n| node_text(n, source))
        .map(collapse_ws)
        .ok_or(Error::ExtractionInvariant {
            message: "impl_item missing type",
        })?;
    if let Some(trait_node) = node.child_by_field_name("trait") {
        let trait_text = collapse_ws(node_text(trait_node, source).unwrap_or(""));
        Ok(format!("<{type_text} as {trait_text}>"))
    } else {
        Ok(type_text)
    }
}

fn test_signals(node: Node<'_>, source: &str) -> TestSignals {
    let mut explicit_attribute = false;
    let mut inside_cfg_test = false;
    for attr in attached_attributes(node) {
        let path = attribute_path(attr, source).unwrap_or_default();
        if is_explicit_test_attr(&path) {
            explicit_attribute = true;
        }
        if is_cfg_test_attr(attr, source, &path) {
            inside_cfg_test = true;
        }
    }
    let mut current = node.parent();
    while let Some(item) = current {
        if is_definition_item(item)
            || item.kind() == "impl_item"
            || item.kind() == "foreign_mod_item"
        {
            for attr in attached_attributes(item) {
                let path = attribute_path(attr, source).unwrap_or_default();
                if is_cfg_test_attr(attr, source, &path) {
                    inside_cfg_test = true;
                }
            }
        }
        if item.kind() == "source_file" {
            break;
        }
        current = item.parent();
    }

    TestSignals {
        explicit_attribute,
        inside_cfg_test,
        inside_test_scope: false,
    }
}

fn is_definition_item(node: Node<'_>) -> bool {
    classify_definition(node).is_ok()
}

fn is_explicit_test_attr(path: &str) -> bool {
    path == "test" || path.ends_with("::test") || path == "rstest" || path == "test_case"
}

/// Lexical `cfg(test)` signal per contract: the attribute's token tree contains
/// the identifier `test`. String literals (`feature = "test"`) never match
/// because their contents are `string_content` nodes, not identifiers.
fn is_cfg_test_attr(attr: Node<'_>, source: &str, path: &str) -> bool {
    if path != "cfg" && path != "cfg_attr" {
        return false;
    }
    let Some(attribute) = child_by_kind(attr, "attribute") else {
        return false;
    };
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return false;
    };
    let mut stack = vec![arguments];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && node_text(node, source) == Some("test") {
            return true;
        }
        if node.kind() == "token_tree" {
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                stack.push(child);
            }
        }
    }
    false
}

fn metadata_idents(node: Node<'_>, source: &str) -> (Vec<String>, Vec<String>) {
    let mut doc_idents = Vec::new();
    let mut attribute_idents = Vec::new();
    for meta in attached_metadata(node) {
        if is_outer_doc_comment(meta) {
            if let Some(text) = node_text(meta, source) {
                doc_idents.extend(scan_idents(text));
            }
        } else if is_outer_attribute(meta) {
            if is_doc_attribute(meta, source) {
                if let Some(content) = doc_attribute_string(meta, source) {
                    doc_idents.extend(scan_idents(content));
                }
            } else if let Some(text) = node_text(meta, source) {
                attribute_idents.extend(scan_idents(text));
            }
        }
    }
    (doc_idents, attribute_idents)
}

/// Outer metadata attached to `node`, in source order.
///
/// Attributes attach through interleaved ordinary comments (comments are
/// trivia to rustc); an ordinary comment between a doc block and the item
/// stops doc attachment only.
///
/// This walk is why the query has no documentation capture. A `@doc` pattern
/// tags every outer doc comment in a file and says nothing about which item it
/// attaches to, cannot express the blocking rule above, and never sees
/// `#[doc = "…"]` at all — that is an `attribute_item`. Both behaviours are
/// pinned: `plain_comment_blocks_docs_but_not_attributes` and
/// `raw_string_doc_attribute_is_documentation`.
fn attached_metadata(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut doc_blocked = false;
    let mut cursor = node.prev_named_sibling();
    while let Some(prev) = cursor {
        if is_outer_attribute(prev) {
            out.push(prev);
        } else if is_outer_doc_comment(prev) {
            if !doc_blocked {
                out.push(prev);
            }
        } else if is_plain_comment(prev) {
            doc_blocked = true;
        } else {
            break;
        }
        cursor = prev.prev_named_sibling();
    }
    out.reverse();
    out
}

fn attached_attributes(node: Node<'_>) -> Vec<Node<'_>> {
    attached_metadata(node)
        .into_iter()
        .filter(|n| is_outer_attribute(*n))
        .collect()
}

fn is_outer_attribute(node: Node<'_>) -> bool {
    node.kind() == "attribute_item"
}

fn is_comment(node: Node<'_>) -> bool {
    node.kind() == "line_comment" || node.kind() == "block_comment"
}

fn is_outer_doc_comment(node: Node<'_>) -> bool {
    if !is_comment(node) {
        return false;
    }
    let mut walk = node.walk();
    let mut children = node.named_children(&mut walk);
    children.any(|child| child.kind() == "outer_doc_comment_marker")
}

fn is_plain_comment(node: Node<'_>) -> bool {
    if !is_comment(node) {
        return false;
    }
    let mut walk = node.walk();
    let mut children = node.named_children(&mut walk);
    !children.any(|child| {
        child.kind() == "outer_doc_comment_marker" || child.kind() == "inner_doc_comment_marker"
    })
}

fn is_doc_attribute(node: Node<'_>, source: &str) -> bool {
    attribute_path(node, source).as_deref() == Some("doc")
}

fn doc_attribute_string<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    let attribute = child_by_kind(node, "attribute")?;
    let value = attribute.child_by_field_name("value")?;
    if !matches!(value.kind(), "string_literal" | "raw_string_literal") {
        return None;
    }
    match child_by_kind(value, "string_content") {
        Some(content) => node_text(content, source),
        None => Some(""),
    }
}

fn attribute_path(node: Node<'_>, source: &str) -> Option<String> {
    let attribute = child_by_kind(node, "attribute")?;
    let mut walk = attribute.walk();
    let mut named = attribute.named_children(&mut walk);
    let path_node = named.next()?;
    if matches!(
        path_node.kind(),
        "identifier" | "scoped_identifier" | "type_identifier"
    ) {
        Some(collapse_ws(node_text(path_node, source)?))
    } else {
        None
    }
}

fn normalize_reference(
    kind: ReferenceKind,
    full: &str,
    name_node: Node<'_>,
) -> (String, Option<String>) {
    match kind {
        ReferenceKind::MethodCall => {
            let name = if name_node.kind() == "field_identifier" {
                full.to_string()
            } else {
                terminal_name(full)
            };
            (name, None)
        }
        ReferenceKind::Call | ReferenceKind::MacroCall => {
            if name_node.kind() == "scoped_identifier" || full.contains("::") {
                (terminal_name(full), Some(collapse_ws(full)))
            } else {
                (full.to_string(), None)
            }
        }
        ReferenceKind::Implementation | ReferenceKind::Type => {
            let collapsed = collapse_ws(full);
            let name = terminal_type_name(&collapsed);
            (name, Some(collapsed))
        }
    }
}

fn terminal_name(path: &str) -> String {
    path.rsplit("::").next().unwrap_or(path).trim().to_string()
}

fn terminal_type_name(path: &str) -> String {
    let base = path.split('<').next().unwrap_or(path).trim();
    terminal_name(base)
}

fn expand_import_declaration(
    decl: Node<'_>,
    source: &str,
    out: &mut Vec<PendingImport>,
) -> Result<()> {
    if decl.kind() == "extern_crate_declaration" {
        let name = field_text(decl, "name", source)
            .ok_or(Error::ExtractionInvariant {
                message: "extern crate missing name",
            })?
            .to_string();
        let alias = field_text(decl, "alias", source).map(str::to_string);
        let span = if let Some(alias_node) = decl.child_by_field_name("alias") {
            node_span(alias_node, source)?
        } else if let Some(name_node) = decl.child_by_field_name("name") {
            node_span(name_node, source)?
        } else {
            node_span(decl, source)?
        };
        out.push(PendingImport {
            kind: ImportKind::ExternCrate,
            path: name,
            alias,
            is_public: visibility(decl, source) == Visibility::Public,
            is_glob: false,
            span,
        });
        return Ok(());
    }

    let is_public = visibility(decl, source) == Visibility::Public;
    let Some(argument) = decl.child_by_field_name("argument") else {
        return Ok(());
    };
    expand_use_tree(argument, source, is_public, out)
}

/// Expands a use tree iteratively with an explicit worklist, so nesting depth
/// is bounded only by heap memory and can never overflow the stack.
#[allow(clippy::too_many_lines)]
fn expand_use_tree(
    root: Node<'_>,
    source: &str,
    is_public: bool,
    out: &mut Vec<PendingImport>,
) -> Result<()> {
    let mut work: Vec<(Node<'_>, Vec<String>)> = vec![(root, Vec::new())];
    while let Some((node, prefix)) = work.pop() {
        match node.kind() {
            "self" => {
                let path = if prefix.is_empty() {
                    "self".to_string()
                } else {
                    prefix.join("::")
                };
                out.push(PendingImport {
                    kind: ImportKind::Use,
                    path,
                    alias: None,
                    is_public,
                    is_glob: false,
                    span: node_span(node, source)?,
                });
            }
            "identifier" | "type_identifier" | "super" | "crate" | "metavariable" => {
                let segment = segment_text(node, source)?;
                let mut path_parts = prefix;
                path_parts.push(segment);
                out.push(PendingImport {
                    kind: ImportKind::Use,
                    path: path_parts.join("::"),
                    alias: None,
                    is_public,
                    is_glob: false,
                    span: node_span(node, source)?,
                });
            }
            "scoped_identifier" => {
                let mut path_parts = prefix;
                path_parts.extend(path_segments(node, source)?);
                out.push(PendingImport {
                    kind: ImportKind::Use,
                    path: path_parts.join("::"),
                    alias: None,
                    is_public,
                    is_glob: false,
                    span: node_span(node, source)?,
                });
            }
            "use_as_clause" => {
                let path_node =
                    node.child_by_field_name("path")
                        .ok_or(Error::ExtractionInvariant {
                            message: "use_as_clause missing path",
                        })?;
                let alias = field_text(node, "alias", source).map(str::to_string);
                let path = if path_node.kind() == "self" {
                    if prefix.is_empty() {
                        "self".to_string()
                    } else {
                        prefix.join("::")
                    }
                } else {
                    let mut path_parts = prefix;
                    path_parts.extend(path_segments(path_node, source)?);
                    path_parts.join("::")
                };
                out.push(PendingImport {
                    kind: ImportKind::Use,
                    path,
                    alias,
                    is_public,
                    is_glob: false,
                    span: node_span(node, source)?,
                });
            }
            "use_wildcard" => {
                let mut path_parts = prefix;
                if let Some(child) = first_named_child(node) {
                    path_parts.extend(path_segments(child, source)?);
                }
                let path = if path_parts.is_empty() {
                    "*".to_string()
                } else {
                    format!("{}::*", path_parts.join("::"))
                };
                let star = glob_star_token(node);
                out.push(PendingImport {
                    kind: ImportKind::Use,
                    path,
                    alias: None,
                    is_public,
                    is_glob: true,
                    span: node_span(star.unwrap_or(node), source)?,
                });
            }
            "scoped_use_list" => {
                let mut nested_prefix = prefix;
                if let Some(path_node) = node.child_by_field_name("path") {
                    nested_prefix.extend(path_segments(path_node, source)?);
                }
                if let Some(list) = node.child_by_field_name("list") {
                    work.push((list, nested_prefix));
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                for child in children.into_iter().rev() {
                    work.push((child, prefix.clone()));
                }
            }
            _ => {
                if let Some(text) = node_text(node, source) {
                    let mut path_parts = prefix;
                    path_parts.push(collapse_ws(text));
                    out.push(PendingImport {
                        kind: ImportKind::Use,
                        path: path_parts.join("::"),
                        alias: None,
                        is_public,
                        is_glob: false,
                        span: node_span(node, source)?,
                    });
                }
            }
        }
    }
    Ok(())
}

fn first_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut walk = node.walk();
    let mut named = node.named_children(&mut walk);
    named.next()
}

/// The anonymous `*` token of a `use_wildcard`; the contract span is the glob
/// token, not its path prefix.
fn glob_star_token(node: Node<'_>) -> Option<Node<'_>> {
    let mut walk = node.walk();
    let mut children = node.children(&mut walk);
    children.find(|child| child.kind() == "*")
}

/// Collects path segments iteratively; deep `a::a::a::...` chains never
/// recurse. A leading global-path `::` is preserved as an empty first segment
/// so `::std::io` and `std::io` stay distinct.
fn path_segments(node: Node<'_>, source: &str) -> Result<Vec<String>> {
    let mut reversed = Vec::new();
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" | "type_identifier" | "self" | "super" | "crate" => {
                reversed.push(segment_text(current, source)?);
                break;
            }
            "scoped_identifier" => {
                if let Some(name) = current.child_by_field_name("name") {
                    reversed.push(segment_text(name, source)?);
                }
                if let Some(path) = current.child_by_field_name("path") {
                    current = path;
                } else {
                    if current.child(0).is_some_and(|child| child.kind() == "::") {
                        reversed.push(String::new());
                    }
                    break;
                }
            }
            _ => {
                reversed.push(collapse_ws(node_text(current, source).unwrap_or("")));
                break;
            }
        }
    }
    reversed.reverse();
    Ok(reversed)
}

fn segment_text(node: Node<'_>, source: &str) -> Result<String> {
    match node.kind() {
        "self" | "super" | "crate" => Ok(node.kind().to_string()),
        _ => Ok(node_text(node, source)
            .ok_or(Error::ExtractionInvariant {
                message: "path segment is not valid UTF-8",
            })?
            .to_string()),
    }
}

fn field_text<'a>(node: Node<'a>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|child| node_text(child, source))
}

fn child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut walk = node.walk();
    let mut children = node.named_children(&mut walk);
    children.find(|c| c.kind() == kind)
}

fn node_text<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    source.get(node.start_byte()..node.end_byte())
}

fn collapse_ws(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::needless_raw_string_hashes,
    clippy::similar_names,
    clippy::redundant_clone,
    clippy::manual_let_else,
    clippy::range_plus_one
)]
mod tests {
    use super::*;
    use crate::facts::{LocalDefId, ParseStatus};

    impl RustExtractor {
        fn new_for_test(query_source: &str, match_limit: Option<u32>) -> Result<Self> {
            Self::build(query_source, match_limit)
        }
    }

    fn extract(src: &str) -> Facts {
        let mut extractor = RustExtractor::new().unwrap();
        extractor.extract(src.as_bytes()).unwrap()
    }

    fn def_names(facts: &Facts) -> Vec<&str> {
        facts.defs().iter().map(|d| d.name.as_str()).collect()
    }

    fn slice(src: &str, span: Span) -> &str {
        &src[span.start_byte() as usize..span.end_byte() as usize]
    }

    #[test]
    fn constructor_compiles_embedded_query() {
        RustExtractor::new().unwrap();
    }

    #[test]
    fn broken_query_returns_extractor_query() {
        let err = match RustExtractor::new_for_test("(function_item", None) {
            Ok(_) => panic!("expected query error"),
            Err(err) => err,
        };
        match err {
            Error::ExtractorQuery {
                lang: Lang::Rust,
                message,
                ..
            } => assert!(!message.is_empty()),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn missing_required_capture_returns_contract_error() {
        let query = r#"
            (function_item name: (identifier) @def.name) @def.item
            (ERROR) @syntax.error
            (MISSING) @syntax.missing
        "#;
        let err = match RustExtractor::new_for_test(query, None) {
            Ok(_) => panic!("expected contract error"),
            Err(err) => err,
        };
        assert!(matches!(err, Error::ExtractorQueryContract { capture: _ }));
    }

    #[test]
    fn extracts_every_def_kind_once() {
        let src = r#"
struct S;
enum E { A }
union U { a: u32 }
trait T { type Item; fn req(&self); fn def(&self) {} }
type Alias = u32;
const C: u32 = 1;
static ST: u32 = 2;
mod m {}
macro_rules! mac { () => {} }
fn free() {}
impl S { fn method(&self) {} }
impl T for S { fn req(&self) {} type Item = u32; }
extern "C" { fn foreign(); }
"#;
        let facts = extract(src);
        let kinds: BTreeSet<_> = facts.defs().iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DefKind::Struct));
        assert!(kinds.contains(&DefKind::Enum));
        assert!(kinds.contains(&DefKind::Union));
        assert!(kinds.contains(&DefKind::Trait));
        assert!(kinds.contains(&DefKind::AssociatedType));
        assert!(kinds.contains(&DefKind::TraitMethod));
        assert!(kinds.contains(&DefKind::TypeAlias));
        assert!(kinds.contains(&DefKind::Const));
        assert!(kinds.contains(&DefKind::Static));
        assert!(kinds.contains(&DefKind::Module));
        assert!(kinds.contains(&DefKind::Macro));
        assert!(kinds.contains(&DefKind::Function));
        assert!(kinds.contains(&DefKind::Method));
        assert!(matches!(facts.status(), ParseStatus::Complete));
    }

    #[test]
    fn classifies_free_impl_trait_foreign_functions() {
        let src = r#"
fn free() {}
impl S { fn method(&self) {} }
trait T { fn req(&self); fn def(&self) {} }
extern "C" { fn foreign(); }
struct S;
"#;
        let facts = extract(src);
        let by_name: HashMap<_, _> = facts
            .defs()
            .iter()
            .map(|d| (d.name.as_str(), d.kind))
            .collect();
        assert_eq!(by_name["free"], DefKind::Function);
        assert_eq!(by_name["method"], DefKind::Method);
        assert_eq!(by_name["req"], DefKind::TraitMethod);
        assert_eq!(by_name["def"], DefKind::TraitMethod);
        assert_eq!(by_name["foreign"], DefKind::Function);
    }

    #[test]
    fn local_qualification_formats() {
        let src = r#"
mod auth {
    fn verify() {}
    struct TokenService;
    impl TokenService {
        fn verify(&self) {}
    }
    trait Validator {
        fn verify(&self);
    }
    impl Validator for TokenService {
        fn verify(&self) {}
    }
}
fn outer() {
    fn inner() {}
}
"#;
        let facts = extract(src);
        assert!(facts.defs().iter().any(|d| d.name == "verify"
            && d.kind == DefKind::Function
            && d.local_qualified.as_deref() == Some("auth::verify")));
        assert!(facts.defs().iter().any(|d| {
            d.name == "verify"
                && d.kind == DefKind::Method
                && d.local_qualified.as_deref() == Some("auth::TokenService::verify")
        }));
        assert!(facts.defs().iter().any(|d| {
            d.name == "verify"
                && d.kind == DefKind::TraitMethod
                && d.local_qualified.as_deref() == Some("auth::Validator::verify")
        }));
        assert!(facts.defs().iter().any(|d| {
            d.name == "verify"
                && d.kind == DefKind::Method
                && d.local_qualified.as_deref() == Some("auth::<TokenService as Validator>::verify")
        }));
        assert!(facts.defs().iter().any(|d| {
            d.name == "inner" && d.local_qualified.as_deref() == Some("outer::inner")
        }));
    }

    #[test]
    fn spans_include_attrs_and_docs_signature_excludes_body() {
        let src = "/// docs here\n#[inline]\npub fn foo() {\n    let x = 1;\n}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "foo").unwrap();
        def.span.validate_for(src).unwrap();
        def.signature_span.validate_for(src).unwrap();
        let whole = slice(src, def.span);
        assert!(whole.starts_with("/// docs here"));
        assert!(whole.contains("#[inline]"));
        let sig = slice(src, def.signature_span);
        assert!(sig.contains("fn foo()"));
        assert!(!sig.contains("let x"));
        assert!(!def.doc_idents.is_empty());
        assert!(def.attribute_idents.iter().any(|i| i == "inline"));
    }

    #[test]
    fn attribute_idents_are_not_duplicated_into_signature_idents() {
        let src = "#[inline]\n#[cfg(not(loom))]\npub fn foo(x: u32) {}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "foo").unwrap();
        assert_eq!(def.attribute_idents, vec!["inline", "cfg", "not", "loom"]);
        assert!(!def.signature_idents.iter().any(|i| i == "inline"));
        assert!(!def.signature_idents.iter().any(|i| i == "cfg"));
        assert!(!def.signature_idents.iter().any(|i| i == "not"));
        assert!(def.signature_idents.iter().any(|i| i == "x"));
    }

    #[test]
    fn normal_comment_is_not_documentation() {
        let src = "// plain comment\nfn foo() {}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "foo").unwrap();
        assert!(def.doc_idents.is_empty());
        assert!(!slice(src, def.span).contains("plain comment"));
    }

    #[test]
    fn plain_comment_blocks_docs_but_not_attributes() {
        let src = "/// blocked doc\n// plain comment\n#[test]\nfn t() {}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "t").unwrap();
        assert!(def.test_signals.explicit_attribute);
        assert!(def.attribute_idents.iter().any(|i| i == "test"));
        assert!(def.doc_idents.is_empty());

        let src = "#[test]\n// TODO: flaky\nfn t2() {}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "t2").unwrap();
        assert!(def.test_signals.explicit_attribute);
        assert!(slice(src, def.span).starts_with("#[test]"));
    }

    #[test]
    fn visibility_variants() {
        let src = r#"
fn a() {}
pub fn b() {}
pub(crate) fn c() {}
pub(self) fn d() {}
pub(super) fn e() {}
pub(in crate::auth) fn f() {}
pub(in crate) fn g() {}
"#;
        let facts = extract(src);
        let by_name: HashMap<_, _> = facts
            .defs()
            .iter()
            .map(|d| (d.name.as_str(), &d.visibility))
            .collect();
        assert_eq!(by_name["a"], &Visibility::Private);
        assert_eq!(by_name["b"], &Visibility::Public);
        assert_eq!(by_name["c"], &Visibility::Crate);
        assert_eq!(by_name["d"], &Visibility::Restricted("self".into()));
        assert_eq!(by_name["e"], &Visibility::Restricted("super".into()));
        assert_eq!(by_name["f"], &Visibility::Restricted("crate::auth".into()));
        assert_eq!(by_name["g"], &Visibility::Restricted("crate".into()));
    }

    #[test]
    fn test_signal_matrix() {
        let src = r#"
#[test]
fn t1() {}
#[tokio::test]
async fn t2() {}
#[rstest]
fn t3() {}
#[test_case]
fn t4() {}
#[cfg(test)]
mod tests {
    fn inside() {}
}
fn plain() {}
"#;
        let facts = extract(src);
        let t1 = facts.defs().iter().find(|d| d.name == "t1").unwrap();
        assert!(t1.test_signals.explicit_attribute);
        let t2 = facts.defs().iter().find(|d| d.name == "t2").unwrap();
        assert!(t2.test_signals.explicit_attribute);
        let t3 = facts.defs().iter().find(|d| d.name == "t3").unwrap();
        assert!(t3.test_signals.explicit_attribute);
        let t4 = facts.defs().iter().find(|d| d.name == "t4").unwrap();
        assert!(t4.test_signals.explicit_attribute);
        let inside = facts.defs().iter().find(|d| d.name == "inside").unwrap();
        assert!(inside.test_signals.inside_cfg_test);
        let plain = facts.defs().iter().find(|d| d.name == "plain").unwrap();
        assert!(!plain.test_signals.any());
    }

    #[test]
    fn nested_helper_does_not_inherit_explicit_test() {
        let src = "#[test]\nfn outer() {\n    fn helper() {}\n}\n";
        let facts = extract(src);
        let outer = facts.defs().iter().find(|d| d.name == "outer").unwrap();
        assert!(outer.test_signals.explicit_attribute);
        let helper = facts.defs().iter().find(|d| d.name == "helper").unwrap();
        assert!(!helper.test_signals.explicit_attribute);
        assert!(!helper.test_signals.inside_cfg_test);
    }

    #[test]
    fn cfg_test_signal_matches_identifier_tokens_only() {
        let src = r#"
#[cfg(feature = "test")]
fn feature_string() {}
#[cfg(feature = "integration-test")]
fn feature_hyphen() {}
#[cfg(test)]
fn direct() {}
#[cfg(all(test, unix))]
fn nested_all() {}
#[cfg(not(test))]
fn negated() {}
#[cfg_attr(test, allow(dead_code))]
fn via_cfg_attr() {}
"#;
        let facts = extract(src);
        let signal = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|d| d.name == name)
                .unwrap()
                .test_signals
                .inside_cfg_test
        };
        assert!(!signal("feature_string"));
        assert!(!signal("feature_hyphen"));
        assert!(signal("direct"));
        assert!(signal("nested_all"));
        assert!(signal("negated"));
        assert!(signal("via_cfg_attr"));
    }

    #[test]
    fn semicolon_definitions_use_whole_item_signature() {
        let src = "pub type Alias = crate::Target;\nconst C: u32 = 1;\nstatic S: u32 = 2;\ntrait T { type Item: Copy; }\n";
        let facts = extract(src);
        let sig = |name: &str| {
            let def = facts.defs().iter().find(|d| d.name == name).unwrap();
            slice(src, def.signature_span)
        };
        assert_eq!(sig("Alias"), "pub type Alias = crate::Target;");
        assert_eq!(sig("C"), "const C: u32 = 1;");
        assert_eq!(sig("S"), "static S: u32 = 2;");
        assert_eq!(sig("Item"), "type Item: Copy;");
    }

    #[test]
    fn macro_definition_has_body_idents() {
        let src = "macro_rules! route {\n    ($h:ident) => { inner_call(); };\n}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "route").unwrap();
        assert_eq!(slice(src, def.signature_span), "macro_rules! route");
        assert!(def.body_idents.iter().any(|i| i == "inner_call"));
    }

    #[test]
    fn raw_string_doc_attribute_is_documentation() {
        let src = "#[doc = r#\"authentication token\"#]\nfn verify() {}\n#[doc = \"plain words\"]\nfn plain() {}\n";
        let facts = extract(src);
        let verify = facts.defs().iter().find(|d| d.name == "verify").unwrap();
        assert_eq!(verify.doc_idents, vec!["authentication", "token"]);
        let plain = facts.defs().iter().find(|d| d.name == "plain").unwrap();
        assert_eq!(plain.doc_idents, vec!["plain", "words"]);
    }

    #[test]
    fn reference_normalization() {
        let src = r#"
fn f() {
    foo();
    module::foo();
    foo::<u8>();
    Type::new();
    value.foo();
    println!();
    crate::route!();
}
impl Trait for Type {}
"#;
        let facts = extract(src);
        let refs: Vec<_> = facts
            .references()
            .iter()
            .map(|r| (r.name.as_str(), r.qualified.as_deref(), r.kind))
            .collect();
        assert!(refs.contains(&("foo", None, ReferenceKind::Call)));
        assert!(refs.contains(&("foo", Some("module::foo"), ReferenceKind::Call)));
        assert!(refs.contains(&("foo", None, ReferenceKind::Call)));
        assert!(refs.contains(&("new", Some("Type::new"), ReferenceKind::Call)));
        assert!(refs.contains(&("foo", None, ReferenceKind::MethodCall)));
        assert!(refs.contains(&("println", None, ReferenceKind::MacroCall)));
        assert!(refs.contains(&("route", Some("crate::route"), ReferenceKind::MacroCall)));
        assert!(refs.iter().any(|(n, q, k)| {
            *n == "Trait" && q.is_some() && *k == ReferenceKind::Implementation
        }));
    }

    #[test]
    fn same_line_calls_remain_distinct() {
        let src = "fn f() { a(); b(); }\n";
        let facts = extract(src);
        let owned: Vec<_> = facts
            .references()
            .iter()
            .filter(|r| r.name == "a" || r.name == "b")
            .collect();
        assert_eq!(owned.len(), 2);
        assert!(owned[0].span.start_byte() < owned[1].span.start_byte());
    }

    #[test]
    fn nested_definition_owns_its_references() {
        let src = r#"
fn outer() {
    fn inner() {
        call_inner();
    }
    call_outer();
}
"#;
        let facts = extract(src);
        let inner = facts.defs().iter().position(|d| d.name == "inner").unwrap();
        let outer = facts.defs().iter().position(|d| d.name == "outer").unwrap();
        let inner_id = LocalDefId::from_index(u32::try_from(inner).unwrap());
        let outer_id = LocalDefId::from_index(u32::try_from(outer).unwrap());
        let inner_refs: Vec<_> = facts
            .references_from(inner_id)
            .map(|r| r.name.as_str())
            .collect();
        let outer_refs: Vec<_> = facts
            .references_from(outer_id)
            .map(|r| r.name.as_str())
            .collect();
        assert!(inner_refs.contains(&"call_inner"));
        assert!(outer_refs.contains(&"call_outer"));
        assert!(!outer_refs.contains(&"call_inner"));
    }

    #[test]
    fn import_expansion_matrix() {
        let src = r#"
use crate::a::b;
use crate::a::{self, b, c as d, nested::{e, *}};
pub use crate::internal::Client as ApiClient;
extern crate alloc as allocator;
use super::x;
use self::y;
fn block_local() {
    use crate::z;
}
use crate::a::b;
use a as _;
"#;
        let facts = extract(src);
        let paths: Vec<_> = facts
            .imports()
            .iter()
            .map(|i| {
                (
                    i.path.as_str(),
                    i.alias.as_deref(),
                    i.is_glob,
                    i.is_public,
                    i.kind,
                )
            })
            .collect();
        assert!(paths
            .iter()
            .any(|(p, a, g, ..)| *p == "crate::a::b" && a.is_none() && !*g));
        assert!(paths
            .iter()
            .any(|(p, a, g, ..)| *p == "crate::a" && a.is_none() && !*g));
        assert!(paths
            .iter()
            .any(|(p, a, ..)| *p == "crate::a::c" && *a == Some("d")));
        assert!(paths.iter().any(|(p, ..)| *p == "crate::a::nested::e"));
        assert!(paths
            .iter()
            .any(|(p, _, g, ..)| *p == "crate::a::nested::*" && *g));
        assert!(paths.iter().any(|(p, a, _, pub_, kind)| {
            *p == "crate::internal::Client"
                && *a == Some("ApiClient")
                && *pub_
                && *kind == ImportKind::Use
        }));
        assert!(paths.iter().any(|(p, a, _, _, kind)| {
            *p == "alloc" && *a == Some("allocator") && *kind == ImportKind::ExternCrate
        }));
        assert!(paths.iter().any(|(p, ..)| *p == "super::x"));
        assert!(paths.iter().any(|(p, ..)| *p == "self::y"));
        assert!(paths.iter().any(|(p, a, ..)| *p == "a" && *a == Some("_")));
        let z = facts
            .imports()
            .iter()
            .find(|i| i.path == "crate::z")
            .unwrap();
        assert!(z.owner.is_some());
        let b_count = facts
            .imports()
            .iter()
            .filter(|i| i.path == "crate::a::b" && i.alias.is_none() && !i.is_glob)
            .count();
        assert!(b_count >= 2);
    }

    #[test]
    fn import_spans_slice_exact_leaf_clauses() {
        let src = "use crate::a::{c as d, nested::*};\n";
        let facts = extract(src);
        let aliased = facts
            .imports()
            .iter()
            .find(|i| i.alias.as_deref() == Some("d"))
            .unwrap();
        assert_eq!(slice(src, aliased.span), "c as d");
        let glob = facts.imports().iter().find(|i| i.is_glob).unwrap();
        assert_eq!(slice(src, glob.span), "*");
    }

    #[test]
    fn aliased_import_path_does_not_leak_into_body_idents() {
        let src = "fn f() {\n    use crate::alpha::beta as g;\n}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "f").unwrap();
        assert!(def.body_idents.is_empty(), "leaked: {:?}", def.body_idents);
    }

    #[test]
    fn self_alias_resolves_to_accumulated_prefix() {
        let src = "use crate::a::{self as a_alias};\n";
        let facts = extract(src);
        let import = facts
            .imports()
            .iter()
            .find(|i| i.alias.as_deref() == Some("a_alias"))
            .unwrap();
        assert_eq!(import.path, "crate::a");
        assert_eq!(slice(src, import.span), "self as a_alias");
    }

    #[test]
    fn global_path_prefix_is_preserved() {
        let src = "use ::std::io;\nuse std::io;\n";
        let facts = extract(src);
        let paths: Vec<_> = facts.imports().iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"::std::io"));
        assert!(paths.contains(&"std::io"));
    }

    #[test]
    fn restricted_imports_are_not_public() {
        let src = "pub(crate) use crate::internal::Secret;\npub use crate::internal::Open;\nuse crate::internal::Hidden;\n";
        let facts = extract(src);
        let is_public = |leaf: &str| {
            facts
                .imports()
                .iter()
                .find(|i| i.path.ends_with(leaf))
                .unwrap()
                .is_public
        };
        assert!(!is_public("Secret"));
        assert!(is_public("Open"));
        assert!(!is_public("Hidden"));
    }

    #[test]
    fn module_scope_import_has_no_owner() {
        let src = "mod holder {\n    use crate::inside_mod::Item;\n}\nfn f() {\n    use crate::block_scope::Local;\n}\n";
        let facts = extract(src);
        let module_import = facts
            .imports()
            .iter()
            .find(|i| i.path.ends_with("Item"))
            .unwrap();
        assert!(module_import.owner.is_none());
        let block_import = facts
            .imports()
            .iter()
            .find(|i| i.path.ends_with("Local"))
            .unwrap();
        let owner = block_import.owner.unwrap();
        assert_eq!(facts.def(owner).unwrap().name, "f");
    }

    #[test]
    fn deep_use_paths_and_nesting_do_not_overflow_the_stack() {
        let mut flat = String::from("use a");
        for _ in 0..50_000 {
            flat.push_str("::a");
        }
        flat.push_str(";\n");
        let facts = extract(&flat);
        assert_eq!(facts.imports().len(), 1);
        assert!(matches!(facts.status(), ParseStatus::Complete));

        let depth = 5_000;
        let mut nested = String::from("use a::{");
        for _ in 0..depth {
            nested.push_str("b::{");
        }
        nested.push('c');
        for _ in 0..depth {
            nested.push('}');
        }
        nested.push_str("};\n");
        let mut extractor = RustExtractor::new().unwrap();
        let facts = extractor.extract(nested.as_bytes()).unwrap();
        drop(facts);
    }

    #[test]
    fn recovered_keeps_surrounding_definitions() {
        let src = "fn before() {}\nfn broken( { let x = ; }\nfn after() {}\n";
        let facts = extract(src);
        assert!(matches!(facts.status(), ParseStatus::Recovered { .. }));
        let names = def_names(&facts);
        assert!(names.contains(&"before"));
        assert!(names.contains(&"after"));
    }

    #[test]
    fn missing_node_marks_recovered_and_keeps_valid_neighbors() {
        let src = "fn broken( { use crate::x; foo(); }\nfn ok() { bar(); }\n";
        let facts = extract(src);
        assert!(matches!(facts.status(), ParseStatus::Recovered { .. }));
        assert!(facts.references().iter().any(|r| r.name == "bar"));
    }

    #[test]
    fn error_node_suppresses_internal_references_and_imports() {
        let src = "fn ok() { bar(); }\nmatch { use crate::x; foo(); }\n";
        let facts = extract(src);
        let ParseStatus::Recovered { error_nodes, .. } = facts.status() else {
            panic!("expected recovered status, got {:?}", facts.status());
        };
        assert!(error_nodes >= 1);
        assert!(!facts.imports().iter().any(|i| i.path == "crate::x"));
        assert!(!facts.references().iter().any(|r| r.name == "foo"));
        assert!(facts.references().iter().any(|r| r.name == "bar"));
    }

    #[test]
    fn invalid_utf8_degrades() {
        let mut extractor = RustExtractor::new().unwrap();
        let content = b"fn foo() {}\xfffn bar() {}";
        let facts = extractor.extract(content).unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Degraded {
                reason: DegradedReason::InvalidUtf8,
                ..
            }
        ));
        assert!(facts.defs().is_empty());
        assert!(facts.lexical_idents().iter().any(|i| i == "foo"));
        assert!(facts.lexical_idents().iter().any(|i| i == "bar"));
    }

    #[test]
    fn fallback_caps_set_truncated() {
        let (idents, _, truncated) = super::super::lexical_idents(&{
            let mut v = Vec::new();
            for i in 0..(super::super::MAX_FALLBACK_IDENTIFIERS + 1) {
                if i > 0 {
                    v.push(b' ');
                }
                v.extend_from_slice(b"x");
            }
            v
        });
        assert_eq!(idents.len(), super::super::MAX_FALLBACK_IDENTIFIERS);
        assert!(truncated);
    }

    #[test]
    fn query_match_limit_degrades_without_partial_facts() {
        let src = r#"
fn a() { x(); }
fn b() { y(); }
fn c() { z(); }
"#;
        let mut extractor = RustExtractor::new_for_test(QUERY_SOURCE, Some(1)).unwrap();
        let facts = extractor.extract(src.as_bytes()).unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Degraded {
                reason: DegradedReason::QueryMatchLimit,
                ..
            }
        ));
        assert!(facts.defs().is_empty());
        assert!(facts.references().is_empty());
    }

    #[test]
    fn crlf_spans_slice_same_buffer() {
        let src = "fn foo() {\r\n    bar();\r\n}\r\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "foo").unwrap();
        def.span.validate_for(src).unwrap();
        let sliced = slice(src, def.span);
        assert!(sliced.contains("fn foo()"));
        assert!(sliced.contains("\r\n"));
        for r in facts.references() {
            r.span.validate_for(src).unwrap();
        }
    }

    #[test]
    fn unicode_identifiers_survive() {
        let src = "fn méthode() { valeur(); }\n";
        let facts = extract(src);
        assert!(facts.defs().iter().any(|d| d.name == "méthode"));
        assert!(facts.references().iter().any(|r| r.name == "valeur"));
    }

    #[test]
    fn checked_u32_rejects_overflow() {
        let err = checked_u32(usize::try_from(u32::MAX).unwrap() + 1).unwrap_err();
        assert!(matches!(
            err,
            Error::ExtractionInvariant {
                message: "value exceeds u32::MAX"
            }
        ));
    }

    #[test]
    fn extract_is_deterministic() {
        let src = r#"
mod auth {
    pub fn verify_token(t: &str) -> bool {
        decode(t).is_some()
    }
}
fn decode(t: &str) -> Option<u32> { None }
"#;
        let mut extractor = RustExtractor::new().unwrap();
        let a = extractor.extract(src.as_bytes()).unwrap();
        let b = extractor.extract(src.as_bytes()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn some_constructor_is_retained_as_call() {
        let src = "fn f() { let x = Some(1); }\n";
        let facts = extract(src);
        assert!(facts
            .references()
            .iter()
            .any(|r| r.name == "Some" && r.kind == ReferenceKind::Call));
    }
}
