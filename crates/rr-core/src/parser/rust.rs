//! Rust facts extraction via Tree-sitter.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::facts::{
    nearest_owner, Def, DefKind, DegradedReason, Facts, Import, ImportKind, ParseStatus,
    Reference, ReferenceKind, Span, TestSignals, Visibility,
};
use crate::lang::Lang;
use crate::{Error, Result};

use super::degraded_facts;

const QUERY_SOURCE: &str = include_str!("queries/rust.scm");

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
    "doc",
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
#[allow(dead_code)]
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
    doc: u32,
    syntax_error: u32,
    syntax_missing: u32,
}

struct PendingDef {
    name: String,
    name_span: Span,
    kind: DefKind,
    visibility: Visibility,
    span: Span,
    signature_span: Span,
    body_span: Option<Span>,
    local_qualified: Option<String>,
    test_signals: TestSignals,
    doc_idents: Vec<String>,
    attribute_idents: Vec<String>,
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
        parser.set_language(&language).map_err(|err| Error::ExtractorLanguage {
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

    #[allow(clippy::too_many_lines)]
    fn collect_facts(&mut self, source: &str, tree: &Tree) -> Result<Facts> {
        let root = tree.root_node();
        let mut pending_defs: BTreeMap<(u32, u32), PendingDef> = BTreeMap::new();
        let mut pending_references: Vec<PendingReference> = Vec::new();
        let mut pending_imports: Vec<PendingImport> = Vec::new();
        let mut pending_idents: Vec<PendingIdent> = Vec::new();
        let mut error_spans: Vec<Span> = Vec::new();
        let mut missing_keys: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut error_keys: BTreeSet<(u32, u32)> = BTreeSet::new();

        {
            let mut matches = self.cursor.matches(&self.query, root, source.as_bytes());
            while let Some(query_match) = matches.next() {
                let mut def_item: Option<Node<'_>> = None;
                let mut def_name: Option<Node<'_>> = None;
                let mut def_body: Option<Node<'_>> = None;
                let mut ref_kind: Option<ReferenceKind> = None;
                let mut ref_name: Option<Node<'_>> = None;
                let mut import_decl: Option<Node<'_>> = None;

                for capture in query_match.captures {
                    let id = capture.index;
                    let node = capture.node;
                    if id == self.capture.def_item || id == self.capture.def_signature {
                        def_item = Some(node);
                    } else if id == self.capture.def_name {
                        def_name = Some(node);
                    } else if id == self.capture.def_body {
                        def_body = Some(node);
                    } else if id == self.capture.reference_call {
                        ref_kind = Some(ReferenceKind::Call);
                    } else if id == self.capture.reference_method {
                        ref_kind = Some(ReferenceKind::MethodCall);
                    } else if id == self.capture.reference_macro {
                        ref_kind = Some(ReferenceKind::MacroCall);
                    } else if id == self.capture.reference_implementation {
                        ref_kind = Some(ReferenceKind::Implementation);
                    } else if id == self.capture.reference_name {
                        ref_name = Some(node);
                    } else if id == self.capture.import_declaration {
                        import_decl = Some(node);
                    } else if id == self.capture.identifier {
                        let span = node_span(node, source)?;
                        if let Some(text) = node_text(node, source) {
                            pending_idents.push(PendingIdent {
                                text: text.to_string(),
                                span,
                            });
                        }
                    } else if id == self.capture.syntax_error {
                        let span = node_span(node, source)?;
                        if error_keys.insert((span.start_byte(), span.end_byte())) {
                            error_spans.push(span);
                        }
                    } else if id == self.capture.syntax_missing {
                        let span = node_span(node, source)?;
                        missing_keys.insert((span.start_byte(), span.end_byte()));
                    }
                }

                if let (Some(item), Some(name_node)) = (def_item, def_name) {
                    let item_span = node_span(item, source)?;
                    let key = (item_span.start_byte(), item_span.end_byte());
                    if let std::collections::btree_map::Entry::Vacant(slot) =
                        pending_defs.entry(key)
                    {
                        let name = node_text(name_node, source)
                            .ok_or(Error::ExtractionInvariant {
                                message: "definition name is not valid UTF-8",
                            })?
                            .to_string();
                        let name_span = node_span(name_node, source)?;
                        let expanded = expanded_item_span(item, source)?;
                        let body_span = match def_body {
                            Some(body) => Some(node_span(body, source)?),
                            None => None,
                        };
                        let signature = signature_span(body_span, expanded, source)?;
                        let (doc_idents, attribute_idents) = metadata_idents(item, source);
                        slot.insert(PendingDef {
                            name,
                            name_span,
                            kind: classify_definition(item)?,
                            visibility: visibility(item, source),
                            span: expanded,
                            signature_span: signature,
                            body_span,
                            local_qualified: local_qualified_name(item, source)?,
                            test_signals: test_signals(item, source),
                            doc_idents,
                            attribute_idents,
                        });
                    }
                }

                if let (Some(kind), Some(name_node)) = (ref_kind, ref_name) {
                    let span = node_span(name_node, source)?;
                    let full = node_text(name_node, source)
                        .ok_or(Error::ExtractionInvariant {
                            message: "reference name is not valid UTF-8",
                        })?;
                    let (name, qualified) = normalize_reference(kind, full, name_node, source);
                    pending_references.push(PendingReference {
                        name,
                        qualified,
                        kind,
                        span,
                    });
                }

                if let Some(decl) = import_decl {
                    expand_import_declaration(decl, source, &mut pending_imports)?;
                }
            }
        }

        if self.cursor.did_exceed_match_limit() {
            return Ok(degraded_facts(source.as_bytes(), DegradedReason::QueryMatchLimit));
        }

        let error_nodes = u32::try_from(error_keys.len()).unwrap_or(u32::MAX);
        let missing_nodes = u32::try_from(missing_keys.len()).unwrap_or(u32::MAX);

        let mut defs: Vec<PendingDef> = pending_defs.into_values().collect();
        defs.sort_by(|a, b| {
            (
                a.span.start_byte(),
                a.span.end_byte(),
                a.kind,
                a.name.as_str(),
            )
                .cmp(&(
                    b.span.start_byte(),
                    b.span.end_byte(),
                    b.kind,
                    b.name.as_str(),
                ))
        });

        let finalized_defs: Vec<Def> = defs
            .iter()
            .map(|d| Def {
                name: d.name.clone(),
                local_qualified: d.local_qualified.clone(),
                kind: d.kind,
                visibility: d.visibility.clone(),
                span: d.span,
                signature_span: d.signature_span,
                signature_idents: Vec::new(),
                body_idents: Vec::new(),
                doc_idents: d.doc_idents.clone(),
                attribute_idents: d.attribute_idents.clone(),
                test_signals: d.test_signals,
            })
            .collect();

        let name_spans: HashSet<(u32, u32)> = defs
            .iter()
            .map(|d| (d.name_span.start_byte(), d.name_span.end_byte()))
            .collect();

        let mut references = Vec::with_capacity(pending_references.len());
        for pending in pending_references {
            if inside_any_error(pending.span, &error_spans) {
                continue;
            }
            let owner = nearest_owner(pending.span, &finalized_defs);
            references.push(Reference {
                name: pending.name,
                qualified: pending.qualified,
                kind: pending.kind,
                span: pending.span,
                owner,
            });
        }
        references.sort_by(|a, b| {
            (
                a.span.start_byte(),
                a.span.end_byte(),
                a.kind,
                a.name.as_str(),
            )
                .cmp(&(
                    b.span.start_byte(),
                    b.span.end_byte(),
                    b.kind,
                    b.name.as_str(),
                ))
        });

        let mut imports = Vec::with_capacity(pending_imports.len());
        for pending in pending_imports {
            if inside_any_error(pending.span, &error_spans) {
                continue;
            }
            let owner = nearest_owner(pending.span, &finalized_defs);
            imports.push(Import {
                kind: pending.kind,
                path: pending.path,
                alias: pending.alias,
                is_public: pending.is_public,
                is_glob: pending.is_glob,
                span: pending.span,
                owner,
            });
        }
        imports.sort_by(|a, b| {
            (
                a.span.start_byte(),
                a.span.end_byte(),
                a.kind,
                a.path.as_str(),
                a.alias.as_deref(),
            )
                .cmp(&(
                    b.span.start_byte(),
                    b.span.end_byte(),
                    b.kind,
                    b.path.as_str(),
                    b.alias.as_deref(),
                ))
        });

        let reference_spans: Vec<Span> = references.iter().map(|r| r.span).collect();
        let import_spans: Vec<Span> = imports.iter().map(|i| i.span).collect();

        let mut signature_idents: Vec<Vec<String>> = vec![Vec::new(); defs.len()];
        let mut body_idents: Vec<Vec<String>> = vec![Vec::new(); defs.len()];

        for ident in pending_idents {
            if name_spans.contains(&(ident.span.start_byte(), ident.span.end_byte())) {
                continue;
            }
            if reference_spans.iter().any(|s| s.contains(ident.span)) {
                continue;
            }
            if import_spans.iter().any(|s| s.contains(ident.span)) {
                continue;
            }
            if inside_any_error(ident.span, &error_spans) {
                continue;
            }
            let Some(owner) = nearest_owner(ident.span, &finalized_defs) else {
                continue;
            };
            let idx = owner.index();
            let def = &defs[idx];
            if def.signature_span.contains(ident.span)
                && def
                    .body_span
                    .is_none_or(|body| !body.contains(ident.span))
            {
                signature_idents[idx].push(ident.text);
            } else if def
                .body_span
                .is_some_and(|body| body.contains(ident.span))
            {
                body_idents[idx].push(ident.text);
            }
        }

        let mut final_defs = finalized_defs;
        for (idx, def) in final_defs.iter_mut().enumerate() {
            def.signature_idents = std::mem::take(&mut signature_idents[idx]);
            def.body_idents = std::mem::take(&mut body_idents[idx]);
        }

        let status = if error_nodes == 0 && missing_nodes == 0 {
            ParseStatus::Complete
        } else {
            ParseStatus::Recovered {
                error_nodes,
                missing_nodes,
            }
        };

        Facts::from_parts(final_defs, references, imports, status)
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
            doc: index_of["doc"],
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
    if !source.is_char_boundary(start_byte as usize)
        || !source.is_char_boundary(end_byte as usize)
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
    let mut start_byte = item_span.start_byte();
    let mut start_line = item_span.start_line();

    let mut cursor = node.prev_named_sibling();
    while let Some(prev) = cursor {
        if is_outer_attribute(prev) || is_outer_doc_comment(prev) {
            let prev_span = node_span(prev, source)?;
            start_byte = prev_span.start_byte();
            start_line = prev_span.start_line();
            cursor = prev.prev_named_sibling();
        } else {
            break;
        }
    }

    Span::new(
        start_byte,
        item_span.end_byte(),
        start_line,
        item_span.end_line(),
    )
}

fn signature_span(
    body_span: Option<Span>,
    expanded: Span,
    source: &str,
) -> Result<Span> {
    let Some(body) = body_span else {
        return Ok(expanded);
    };
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
    } else if end > 0 {
        line_of_offset(bytes, end - 1)
    } else {
        1
    };
    Span::new(expanded.start_byte(), end_byte, expanded.start_line(), end_line)
}

fn line_of_offset(bytes: &[u8], offset: usize) -> u32 {
    let mut line = 1_u32;
    for &b in bytes.iter().take(offset) {
        if b == b'\n' {
            line = line.saturating_add(1);
        }
    }
    line
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
    let mut named = vis.named_children(&mut walk);
    let Some(inner) = named.next() else {
        return Visibility::Public;
    };
    match inner.kind() {
        "crate" => Visibility::Crate,
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
            "mod_item" => {
                if let Some(name) = field_text(current, "name", source) {
                    segments.push(name.to_string());
                }
            }
            "function_item" | "function_signature_item" | "struct_item" | "enum_item"
            | "union_item" | "trait_item" | "type_item" | "const_item" | "static_item"
            | "macro_definition" | "associated_type" => {
                if let Some(name) = definition_name_text(current, source) {
                    segments.push(name.to_string());
                }
            }
            "impl_item" => {
                segments.push(impl_qualifier(current, source)?);
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

    let mut current = Some(node);
    while let Some(item) = current {
        if is_definition_item(item) || item.kind() == "impl_item" || item.kind() == "foreign_mod_item"
        {
            for attr in attached_attributes(item) {
                let path = attribute_path(attr, source).unwrap_or_default();
                if is_explicit_test_attr(&path) {
                    explicit_attribute = true;
                }
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
    }
}

fn is_definition_item(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_item"
            | "function_signature_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "trait_item"
            | "type_item"
            | "associated_type"
            | "const_item"
            | "static_item"
            | "mod_item"
            | "macro_definition"
    )
}

fn is_explicit_test_attr(path: &str) -> bool {
    path == "test"
        || path.ends_with("::test")
        || path == "rstest"
        || path == "test_case"
}

fn is_cfg_test_attr(attr: Node<'_>, source: &str, path: &str) -> bool {
    if path != "cfg" && path != "cfg_attr" {
        return false;
    }
    let text = node_text(attr, source).unwrap_or("");
    scan_idents(text).into_iter().any(|ident| ident == "test")
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

fn attached_metadata(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = node.prev_named_sibling();
    while let Some(prev) = cursor {
        if is_outer_attribute(prev) || is_outer_doc_comment(prev) {
            out.push(prev);
            cursor = prev.prev_named_sibling();
        } else {
            break;
        }
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

fn is_outer_doc_comment(node: Node<'_>) -> bool {
    if node.kind() != "line_comment" && node.kind() != "block_comment" {
        return false;
    }
    let mut walk = node.walk();
    let mut children = node.named_children(&mut walk);
    children.any(|child| child.kind() == "outer_doc_comment_marker")
}

fn is_doc_attribute(node: Node<'_>, source: &str) -> bool {
    attribute_path(node, source).as_deref() == Some("doc")
}

fn doc_attribute_string<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    let attribute = child_by_kind(node, "attribute")?;
    let value = attribute.child_by_field_name("value")?;
    if value.kind() != "string_literal" {
        return None;
    }
    let text = node_text(value, source)?;
    let stripped = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            text.strip_prefix("r\"")
                .and_then(|s| s.strip_suffix('"'))
        })
        .unwrap_or(text);
    Some(stripped)
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
    _source: &str,
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
        ReferenceKind::Implementation => {
            let collapsed = collapse_ws(full);
            let name = terminal_type_name(&collapsed);
            (name, Some(collapsed))
        }
    }
}

fn terminal_name(path: &str) -> String {
    path.rsplit("::")
        .next()
        .unwrap_or(path)
        .trim()
        .to_string()
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
            is_public: visibility(decl, source) != Visibility::Private,
            is_glob: false,
            span,
        });
        return Ok(());
    }

    let is_public = visibility(decl, source) != Visibility::Private;
    let Some(argument) = decl.child_by_field_name("argument") else {
        return Ok(());
    };
    expand_use_tree(argument, source, &mut Vec::new(), is_public, out)
}

#[allow(clippy::too_many_lines)]
fn expand_use_tree(
    node: Node<'_>,
    source: &str,
    prefix: &mut Vec<String>,
    is_public: bool,
    out: &mut Vec<PendingImport>,
) -> Result<()> {
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
            let mut path_parts = prefix.clone();
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
            let mut path_parts = prefix.clone();
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
            let path_node = node
                .child_by_field_name("path")
                .ok_or(Error::ExtractionInvariant {
                    message: "use_as_clause missing path",
                })?;
            let alias = field_text(node, "alias", source).map(str::to_string);
            let mut path_parts = prefix.clone();
            path_parts.extend(path_segments(path_node, source)?);
            let span = if let Some(alias_node) = node.child_by_field_name("alias") {
                node_span(alias_node, source)?
            } else {
                node_span(node, source)?
            };
            out.push(PendingImport {
                kind: ImportKind::Use,
                path: path_parts.join("::"),
                alias,
                is_public,
                is_glob: false,
                span,
            });
        }
        "use_wildcard" => {
            let mut path_parts = prefix.clone();
            let mut leaf = node;
            let mut walk = node.walk();
            let mut named = node.named_children(&mut walk);
            if let Some(child) = named.next() {
                path_parts.extend(path_segments(child, source)?);
                leaf = child;
            }
            let path = if path_parts.is_empty() {
                "*".to_string()
            } else {
                format!("{}::*", path_parts.join("::"))
            };
            out.push(PendingImport {
                kind: ImportKind::Use,
                path,
                alias: None,
                is_public,
                is_glob: true,
                span: node_span(leaf, source)?,
            });
        }
        "scoped_use_list" => {
            if let Some(path_node) = node.child_by_field_name("path") {
                prefix.extend(path_segments(path_node, source)?);
            }
            if let Some(list) = node.child_by_field_name("list") {
                expand_use_tree(list, source, prefix, is_public, out)?;
            }
            if let Some(path_node) = node.child_by_field_name("path") {
                let n = path_segments(path_node, source)?.len();
                for _ in 0..n {
                    prefix.pop();
                }
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                expand_use_tree(child, source, prefix, is_public, out)?;
            }
        }
        _ => {
            if let Some(text) = node_text(node, source) {
                let mut path_parts = prefix.clone();
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
    Ok(())
}

fn path_segments(node: Node<'_>, source: &str) -> Result<Vec<String>> {
    match node.kind() {
        "identifier" | "type_identifier" | "self" | "super" | "crate" => {
            Ok(vec![segment_text(node, source)?])
        }
        "scoped_identifier" => {
            let mut parts = Vec::new();
            if let Some(path) = node.child_by_field_name("path") {
                parts.extend(path_segments(path, source)?);
            }
            if let Some(name) = node.child_by_field_name("name") {
                parts.push(segment_text(name, source)?);
            }
            Ok(parts)
        }
        _ => Ok(vec![collapse_ws(node_text(node, source).unwrap_or(""))]),
    }
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

fn scan_idents(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            if let Ok(ident) = std::str::from_utf8(&bytes[start..i]) {
                out.push(ident.to_string());
            }
        } else {
            i += 1;
        }
    }
    out
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn inside_any_error(span: Span, errors: &[Span]) -> bool {
    errors.iter().any(|error| error.contains(span))
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
        assert!(matches!(
            err,
            Error::ExtractorQueryContract { capture: _ }
        ));
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
        let by_name: HashMap<_, _> = facts.defs().iter().map(|d| (d.name.as_str(), d.kind)).collect();
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
        assert!(facts
            .defs()
            .iter()
            .any(|d| d.name == "verify" && d.kind == DefKind::Function && d.local_qualified.as_deref() == Some("auth::verify")));
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
                && d.local_qualified.as_deref()
                    == Some("auth::<TokenService as Validator>::verify")
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
        let whole = &src[def.span.start_byte() as usize..def.span.end_byte() as usize];
        assert!(whole.starts_with("/// docs here"));
        assert!(whole.contains("#[inline]"));
        let sig = &src[def.signature_span.start_byte() as usize..def.signature_span.end_byte() as usize];
        assert!(sig.contains("fn foo()"));
        assert!(!sig.contains("let x"));
        assert!(!def.doc_idents.is_empty());
        assert!(def.attribute_idents.iter().any(|i| i == "inline"));
    }

    #[test]
    fn normal_comment_is_not_documentation() {
        let src = "// plain comment\nfn foo() {}\n";
        let facts = extract(src);
        let def = facts.defs().iter().find(|d| d.name == "foo").unwrap();
        assert!(def.doc_idents.is_empty());
        let sliced = &src[def.span.start_byte() as usize..def.span.end_byte() as usize];
        assert!(!sliced.contains("plain comment"));
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
        assert_eq!(
            by_name["f"],
            &Visibility::Restricted("crate::auth".into())
        );
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
        let inner = facts
            .defs()
            .iter()
            .position(|d| d.name == "inner")
            .unwrap();
        let outer = facts
            .defs()
            .iter()
            .position(|d| d.name == "outer")
            .unwrap();
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
        assert!(paths.iter().any(|(p, a, g, ..)| *p == "crate::a::b" && a.is_none() && !*g));
        assert!(paths.iter().any(|(p, a, g, ..)| *p == "crate::a" && a.is_none() && !*g));
        assert!(paths.iter().any(|(p, a, ..)| *p == "crate::a::c" && *a == Some("d")));
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
        let z = facts.imports().iter().find(|i| i.path == "crate::z").unwrap();
        assert!(z.owner.is_some());
        let b_count = facts.imports().iter().filter(|i| i.path == "crate::a::b" && i.alias.is_none() && !i.is_glob).count();
        assert!(b_count >= 2);
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
    fn error_node_suppresses_internal_references_and_imports() {
        let src = "fn broken( { use crate::x; foo(); }\nfn ok() { bar(); }\n";
        let facts = extract(src);
        assert!(matches!(facts.status(), ParseStatus::Recovered { .. }));
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
        let (idents, _, truncated) = super::super::lexical_idents(
            &{
                let mut v = Vec::new();
                for i in 0..(super::super::MAX_FALLBACK_IDENTIFIERS + 1) {
                    if i > 0 {
                        v.push(b' ');
                    }
                    v.extend_from_slice(b"x");
                }
                v
            },
        );
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
        let sliced = &src[def.span.start_byte() as usize..def.span.end_byte() as usize];
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
