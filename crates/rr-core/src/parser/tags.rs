//! Generic facts extraction from a grammar's `queries/tags.scm`.

use std::ops::Range;
use std::sync::OnceLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, QueryMatch, StreamingIterator};
use tree_sitter_language::LanguageFn;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::facts::{
    def_key, import_key, reference_key, Def, DefKind, DegradedReason, Facts, Import, ImportKind,
    OwnerIndex, ParseStatus, Reference, ReferenceKind, Span, TestSignals, Visibility,
};
use crate::lang::Lang;
use crate::{Error, Result};

use super::{degraded_facts, scan_idents, Extractor};

const MAX_SIGNATURE_BYTES: usize = 180;

/// Static configuration for one grammar-backed tags extractor.
///
/// The query and grammar are compiled into their crates. Keeping the complete
/// specification static lets the registry retain a `const` table of bare
/// builder function pointers.
pub struct LanguageSpec {
    pub lang: Lang,
    pub language: LanguageFn,
    pub tags_query: &'static str,
    pub locals_query: &'static str,
    /// Definition capture suffixes, without the `definition.` prefix.
    pub kinds: &'static [(&'static str, DefKind)],
    /// Reference capture suffixes, without the `reference.` prefix.
    pub reference_kinds: &'static [(&'static str, ReferenceKind)],
    /// The imports pass, or `None` for a language rr extracts no imports from.
    pub imports: Option<&'static ImportSpec>,
    /// Visibility judged from the bare definition name — the only evidence a
    /// tags query surfaces.
    pub visibility: fn(&str) -> Visibility,
    /// Whether one attribute identifier marks the definition as a test.
    pub test_attribute: fn(&str) -> bool,
    /// Whether an enclosing definition of this name is a test scope.
    pub test_scope: fn(&str) -> bool,
    /// Last word on one definition, once its name, kind, signature and
    /// visibility are all in hand.
    ///
    /// `tree-sitter-tags` compiles text predicates and then never evaluates
    /// them, so `#eq?` and `#match?` cannot route a capture. Anything a
    /// language decides by reading the name or the declaration text — a
    /// `constructor` that is not an ordinary method, an accessor that is not
    /// either, an `= () =>` binding that is a function wearing a variable's
    /// syntax, a `private` modifier no capture can reach — is decided here
    /// instead.
    ///
    /// Runs before the definitions are sorted, because [`def_key`] holds the
    /// kind and a kind changed afterwards would leave the order it was sorted
    /// into.
    pub refine: fn(&mut Def),
    /// Whether this language's documentation is the string literal that opens
    /// a body, as Python's docstring is.
    ///
    /// `false` for a language whose documentation is a comment *preceding* the
    /// definition. That comment run is folded into the definition's span by
    /// [`preceding_comment_run_start`] and excluded from the body scan as a
    /// header region — so a container no longer absorbs its members' doc
    /// prose. Excluding a leading body string here would still be wrong: it
    /// would silently drop a `"use strict"` prologue's identifiers from a
    /// documented function.
    pub doc_is_leading_body_string: bool,
    /// Full-line comment prefixes for [`preceding_comment_run_start`].
    ///
    /// TypeScript uses `//`; Python uses `#`. They are not interchangeable:
    /// a TypeScript `#attempts` field starts with `#` and is a definition, not
    /// a comment. Block comments (`/* … */`) are recognized for every language.
    pub line_comment_prefixes: &'static [&'static str],
    /// The compiled tags query, shared by every worker that speaks this
    /// language. Compiling it costs milliseconds that rayon would otherwise
    /// repeat once per work split; only [`TagsContext`] stays per worker.
    pub config: OnceLock<std::result::Result<TagsConfiguration, String>>,
}

/// One language's import extraction: a plain `tree_sitter::Query` run over its
/// own parse of the same bytes.
///
/// Separate from [`LanguageSpec::tags_query`] because it has to be.
/// `tree-sitter-tags` accepts `@name`, `@ignore`, `@doc`, `@local.*`,
/// `@definition.*` and `@reference.*`, and rejects everything else with
/// `InvalidCapture`; there is no capture through which a tags query could name
/// an import, whatever the query says.
pub struct ImportSpec {
    /// The query source.
    pub query: &'static str,
    /// `@import.<suffix>` anchor suffixes and the kind each one produces.
    ///
    /// A suffix here may not be one of the reserved slot names `path`, `name`,
    /// `alias`, `glob`, `public`, `callee`.
    pub kinds: &'static [(&'static str, ImportKind)],
    /// Byte substrings without which the source cannot contain an import.
    ///
    /// Checked before parsing. Must be a *superset* test: every construct the
    /// query can match has to contain at least one of these literally, or a
    /// file with imports would be reported as having none.
    pub markers: &'static [&'static str],
    /// The function names `@import.callee` accepts.
    ///
    /// A call-based import is a call to a *particular* function, and which one
    /// is the language's business: JavaScript has `require`, Ruby has `require`
    /// and `require_relative`, Lua has `require`. Reading the list from the
    /// spec rather than comparing against a literal is what lets Ruby record
    /// the half of its imports that `require_relative` writes.
    ///
    /// Empty for a language whose query has no `@import.callee`.
    pub callee_names: &'static [&'static str],
    /// Compiled query and resolved capture indices, once per process.
    pub compiled: OnceLock<std::result::Result<CompiledImports, String>>,
}

/// An imports query with its capture indices resolved once, so the hot loop
/// compares `u32`s instead of capture names.
pub struct CompiledImports {
    query: Query,
    /// `(capture index, kind)` per `@import.<kind>` anchor.
    anchors: Vec<(u32, ImportKind)>,
    path: u32,
    name: Option<u32>,
    alias: Option<u32>,
    glob: Option<u32>,
    public: Option<u32>,
    callee: Option<u32>,
    callee_names: &'static [&'static str],
}

/// Stateful generic tags extractor: one tags parser/context per worker.
pub struct TagsExtractor {
    spec: &'static LanguageSpec,
    config: &'static TagsConfiguration,
    context: TagsContext,
    imports: Option<ImportsPass>,
}

/// The imports pass's own parser and cursor, one per worker.
///
/// Its own parser rather than `TagsContext::parser`: `generate_tags` borrows
/// the context mutably for as long as its tag iterator lives, and the pass has
/// to run after that borrow ends. Reusing the field would also lean on a public
/// field of a foreign crate that is an implementation detail rather than an
/// API. One `Parser` per worker per language is a cheap price for neither.
struct ImportsPass {
    compiled: &'static CompiledImports,
    parser: Parser,
    cursor: QueryCursor,
}

impl TagsExtractor {
    /// Compiles and validates one language's tags query, once per process.
    ///
    /// # Errors
    /// Returns a construction error when the query is invalid or contains a
    /// definition/reference capture with no kind mapping.
    pub fn new(spec: &'static LanguageSpec) -> std::result::Result<Self, String> {
        let config = spec.config.get_or_init(|| {
            let language: tree_sitter::Language = spec.language.into();
            let config = TagsConfiguration::new(language, spec.tags_query, spec.locals_query)
                .map_err(|error| error.to_string())?;
            validate_kind_maps(spec, &config)?;
            Ok(config)
        });
        match config {
            Ok(config) => {
                let imports = match spec.imports {
                    None => None,
                    Some(import_spec) => {
                        let language: tree_sitter::Language = spec.language.into();
                        let compiled = import_spec
                            .compiled
                            .get_or_init(|| compile_imports(&language, import_spec))
                            .as_ref()
                            .map_err(Clone::clone)?;
                        let mut parser = Parser::new();
                        parser
                            .set_language(&language)
                            .map_err(|error| error.to_string())?;
                        Some(ImportsPass {
                            compiled,
                            parser,
                            cursor: QueryCursor::new(),
                        })
                    }
                };
                Ok(Self {
                    spec,
                    config,
                    context: TagsContext::new(),
                    imports,
                })
            }
            Err(message) => Err(message.clone()),
        }
    }

    /// Extracts validated facts from exactly these bytes.
    ///
    /// Encoding, parser, and tags-iteration failures become lexical degraded
    /// facts. Structural invariant failures remain errors so they cannot be
    /// hidden as a plausible but incomplete API.
    ///
    /// # Errors
    /// Returns extraction invariant or facts validation failures.
    pub fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        if content.len() > u32::MAX as usize {
            return Ok(degraded_facts(content, DegradedReason::SourceTooLarge));
        }
        let Ok(source) = std::str::from_utf8(content) else {
            return Ok(degraded_facts(content, DegradedReason::InvalidUtf8));
        };
        let lines = LineIndex::new(content)?;
        let spec = self.spec;
        let config = self.config;
        let Ok((tags, parse_errors)) = self.context.generate_tags(config, content, None) else {
            return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
        };

        let mut defs: Vec<(Def, Vec<Span>)> = Vec::new();
        let mut references = Vec::new();
        for item in tags {
            let Ok(tag) = item else {
                return Ok(degraded_facts(content, DegradedReason::ParserReturnedNone));
            };
            if tag.name_range.start >= tag.name_range.end {
                continue;
            }
            let Some(name) = source.get(tag.name_range.clone()) else {
                continue;
            };
            if tag.is_definition {
                let Some(built) = definition_from_tag(spec, config, &tag, name, source, &lines)?
                else {
                    continue;
                };
                defs.push(built);
            } else {
                let Some(kind) = reference_kind(spec, config, tag.syntax_type_id) else {
                    continue;
                };
                references.push(Reference {
                    name: name.to_owned(),
                    qualified: None,
                    kind,
                    span: span_for_range(&tag.name_range, &lines, source)?,
                    owner: None,
                });
            }
        }

        defs.sort_by(|left, right| def_key(&left.0).cmp(&def_key(&right.0)));
        defs.dedup_by(|left, right| def_key(&left.0) == def_key(&right.0));
        let (mut defs, header_exclusions): (Vec<Def>, Vec<Vec<Span>>) = defs.into_iter().unzip();
        assign_nesting(&mut defs, &header_exclusions, source, spec);

        references.sort_by(|left, right| reference_key(left).cmp(&reference_key(right)));
        let owners = OwnerIndex::new(&defs);
        for reference in &mut references {
            reference.owner = owners.nearest(reference.span);
        }
        let imports = self.collect_imports(source, &lines, &owners)?;

        Facts::from_parts(
            defs,
            references,
            imports,
            ParseStatus::Tags { parse_errors },
        )
    }
}

impl Extractor for TagsExtractor {
    fn lang(&self) -> Lang {
        self.spec.lang
    }

    fn extract(&mut self, content: &[u8]) -> Result<Facts> {
        TagsExtractor::extract(self, content)
    }
}

fn validate_kind_maps(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
) -> std::result::Result<(), String> {
    for capture in config.query.capture_names() {
        if let Some(kind) = capture.strip_prefix("definition.") {
            let kind = kind.strip_prefix("local-").unwrap_or(kind);
            if !spec.kinds.iter().any(|(candidate, _)| *candidate == kind) {
                return Err(format!("missing definition kind mapping for `{kind}`"));
            }
        } else if let Some(kind) = capture.strip_prefix("reference.") {
            if !spec
                .reference_kinds
                .iter()
                .any(|(candidate, _)| *candidate == kind)
            {
                return Err(format!("missing reference kind mapping for `{kind}`"));
            }
        }
    }
    Ok(())
}

fn compile_imports(
    language: &tree_sitter::Language,
    spec: &'static ImportSpec,
) -> std::result::Result<CompiledImports, String> {
    let query = Query::new(language, spec.query).map_err(|error| {
        format!(
            "imports query at {}:{}: {}",
            error.row, error.column, error.message
        )
    })?;

    let mut anchors = Vec::new();
    let (mut path, mut name, mut alias) = (None, None, None);
    let (mut glob, mut public, mut callee) = (None, None, None);
    for (index, capture) in query.capture_names().iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| "capture index overflow".to_string())?;
        let Some(suffix) = capture.strip_prefix("import.") else {
            return Err(format!(
                "imports query capture `@{capture}` is not `@import.*`"
            ));
        };
        match suffix {
            "path" => path = Some(index),
            "name" => name = Some(index),
            "alias" => alias = Some(index),
            "glob" => glob = Some(index),
            "public" => public = Some(index),
            "callee" => callee = Some(index),
            _ => {
                let kind = spec
                    .kinds
                    .iter()
                    .find_map(|(candidate, kind)| (*candidate == suffix).then_some(*kind))
                    .ok_or_else(|| format!("missing import kind mapping for `{suffix}`"))?;
                anchors.push((index, kind));
            }
        }
    }

    let path = path.ok_or_else(|| "imports query has no @import.path".to_string())?;
    if anchors.is_empty() {
        return Err("imports query has no @import.<kind> anchor".to_string());
    }
    if spec.markers.is_empty() {
        return Err("imports spec has no markers prefilter".to_string());
    }
    if callee.is_some() && spec.callee_names.is_empty() {
        return Err("imports query has @import.callee but the spec names no callee".to_string());
    }
    Ok(CompiledImports {
        query,
        anchors,
        path,
        name,
        alias,
        glob,
        public,
        callee,
        callee_names: spec.callee_names,
    })
}

impl TagsExtractor {
    /// Extracts this file's imports, or nothing for a language with no imports
    /// query.
    ///
    /// Parses the source a second time. `tree-sitter-tags` builds its tree
    /// inside `generate_tags` and never yields it, so a second query over the
    /// same file needs a second tree. The `markers` prefilter is what keeps
    /// that off files that cannot contain an import at all.
    fn collect_imports(
        &mut self,
        source: &str,
        lines: &LineIndex,
        owners: &OwnerIndex,
    ) -> Result<Vec<Import>> {
        let Some(pass) = self.imports.as_mut() else {
            return Ok(Vec::new());
        };
        let markers = self.spec.imports.map_or(&[][..], |spec| spec.markers);
        if !markers.iter().any(|marker| source.contains(marker)) {
            return Ok(Vec::new());
        }

        let Some(tree) = pass.parser.parse(source.as_bytes(), None) else {
            return Err(Error::ExtractionInvariant {
                message: "imports pass could not parse a source the tags pass parsed",
            });
        };

        let mut imports = Vec::new();
        let mut matches =
            pass.cursor
                .matches(&pass.compiled.query, tree.root_node(), source.as_bytes());
        while let Some(query_match) = matches.next() {
            if let Some(import) = build_import(pass.compiled, query_match, source, lines, owners)? {
                imports.push(import);
            }
        }
        imports.sort_by(|left, right| import_key(left).cmp(&import_key(right)));
        Ok(imports)
    }
}

fn build_import(
    compiled: &CompiledImports,
    query_match: &QueryMatch<'_, '_>,
    source: &str,
    lines: &LineIndex,
    owners: &OwnerIndex,
) -> Result<Option<Import>> {
    let mut anchor: Option<(Node<'_>, ImportKind)> = None;
    let mut path_node: Option<Node<'_>> = None;
    let mut name_node: Option<Node<'_>> = None;
    let mut alias_node: Option<Node<'_>> = None;
    let mut callee_node: Option<Node<'_>> = None;
    let mut is_glob = false;
    let mut is_public = false;

    for capture in query_match.captures {
        let index = capture.index;
        if let Some((_, kind)) = compiled
            .anchors
            .iter()
            .find(|(candidate, _)| *candidate == index)
        {
            anchor = Some((capture.node, *kind));
        } else if index == compiled.path {
            path_node = Some(capture.node);
        } else if Some(index) == compiled.name {
            name_node = Some(capture.node);
        } else if Some(index) == compiled.alias {
            alias_node = Some(capture.node);
        } else if Some(index) == compiled.callee {
            callee_node = Some(capture.node);
        } else if Some(index) == compiled.glob {
            is_glob = true;
        } else if Some(index) == compiled.public {
            is_public = true;
        }
    }

    let (Some((anchor, kind)), Some(path_node)) = (anchor, path_node) else {
        return Err(Error::ExtractionInvariant {
            message: "imports query matched without both an anchor and a path",
        });
    };
    if let Some(callee) = callee_node {
        if !compiled.callee_names.contains(&node_text(callee, source)?) {
            return Ok(None);
        }
    }

    let path = unquote(node_text(path_node, source)?);
    if path.is_empty() {
        return Ok(None);
    }

    let span = span_for_range(&anchor.byte_range(), lines, source)?;
    Ok(Some(Import {
        kind,
        path: path.to_owned(),
        name: text_of(name_node, source)?,
        alias: text_of(alias_node, source)?,
        is_public,
        is_glob,
        span,
        owner: owners.nearest_import_owner(span),
    }))
}

fn text_of(node: Option<Node<'_>>, source: &str) -> Result<Option<String>> {
    node.map(|node| node_text(node, source).map(|text| unquote(text).to_owned()))
        .transpose()
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    source
        .get(node.byte_range())
        .ok_or(Error::ExtractionInvariant {
            message: "imports query node is not a source substring",
        })
}

/// A quoted specifier without its quotes; anything else unchanged.
///
/// Escape sequences are left as written. This field records what the file says,
/// not what a resolver would compute from it, and a specifier is not a path
/// until something resolves it.
fn unquote(text: &str) -> &str {
    let bytes = text.as_bytes();
    let (Some(&open), Some(&close)) = (bytes.first(), bytes.last()) else {
        return text;
    };
    if bytes.len() < 2 || open != close || !matches!(open, b'"' | b'\'' | b'`') {
        return text;
    }
    text.get(1..text.len() - 1).unwrap_or(text)
}

/// A definition capture split into its kind and whether the query said the
/// declaration is local to its file.
///
/// `@definition.local-function` is a [`DefKind::Function`] declared where nothing
/// outside the file can name it — what a TypeScript declaration says by leaving
/// `export` off, and what no other channel could carry: `tree-sitter-tags` allows
/// one `@definition.*` per pattern and returns one syntax type per tag.
///
/// Generic rather than TypeScript's, because the statement is not one language's
/// opinion. A query that marks a declaration local has made a stronger claim than
/// any name shape, so [`LanguageSpec::visibility`] is not consulted for one.
fn split_local(capture: &str) -> (&str, bool) {
    capture
        .strip_prefix("local-")
        .map_or((capture, false), |kind| (kind, true))
}

fn definition_kind(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
    syntax_type_id: u32,
) -> Option<(DefKind, bool)> {
    let (name, is_local) = split_local(config.syntax_type_name(syntax_type_id));
    spec.kinds
        .iter()
        .find_map(|(candidate, kind)| (*candidate == name).then_some((*kind, is_local)))
}

fn reference_kind(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
    syntax_type_id: u32,
) -> Option<ReferenceKind> {
    let name = config.syntax_type_name(syntax_type_id);
    spec.reference_kinds
        .iter()
        .find_map(|(candidate, kind)| (*candidate == name).then_some(*kind))
}

/// Byte offsets of every line start, including the empty line after a trailing
/// newline. Offsets are u32 because [`Span`] is u32-based.
struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    fn new(bytes: &[u8]) -> Result<Self> {
        let mut starts = vec![0];
        for (index, &byte) in bytes.iter().enumerate() {
            if byte == b'\n' {
                let next = index.checked_add(1).ok_or(Error::ExtractionInvariant {
                    message: "line offset overflow",
                })?;
                starts.push(u32::try_from(next).map_err(|_| Error::ExtractionInvariant {
                    message: "line offset exceeds u32::MAX",
                })?);
            }
        }
        Ok(Self { starts })
    }

    fn line_for(&self, offset: usize) -> Result<u32> {
        let offset = u32::try_from(offset).map_err(|_| Error::ExtractionInvariant {
            message: "line lookup offset exceeds u32::MAX",
        })?;
        let line = self.starts.partition_point(|start| *start <= offset);
        u32::try_from(line).map_err(|_| Error::ExtractionInvariant {
            message: "line number exceeds u32::MAX",
        })
    }

    /// The byte offset at which the line containing `offset` starts.
    fn line_start(&self, offset: usize) -> usize {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        let line = self.starts.partition_point(|start| *start <= offset);
        self.starts[line - 1] as usize
    }
}

fn span_for_range(range: &Range<usize>, lines: &LineIndex, source: &str) -> Result<Span> {
    if range.start > range.end || range.end > source.len() {
        return Err(Error::ExtractionInvariant {
            message: "tags range exceeds source length",
        });
    }
    if !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err(Error::ExtractionInvariant {
            message: "tags range is not on a UTF-8 boundary",
        });
    }
    let start_line = lines.line_for(range.start)?;
    let end_line = if range.start == range.end {
        start_line
    } else {
        lines.line_for(range.end - 1)?
    };
    Span::new(
        u32::try_from(range.start).map_err(|_| Error::ExtractionInvariant {
            message: "tags start offset exceeds u32::MAX",
        })?,
        u32::try_from(range.end).map_err(|_| Error::ExtractionInvariant {
            message: "tags end offset exceeds u32::MAX",
        })?,
        start_line,
        end_line,
    )
}

/// One tags definition: the expanded span, header, and refined [`Def`].
fn definition_from_tag(
    spec: &LanguageSpec,
    config: &TagsConfiguration,
    tag: &tree_sitter_tags::Tag,
    name: &str,
    source: &str,
    lines: &LineIndex,
) -> Result<Option<(Def, Vec<Span>)>> {
    let Some((kind, is_local)) = definition_kind(spec, config, tag.syntax_type_id) else {
        return Ok(None);
    };
    let decl_range = tag.range.clone();
    let span_start =
        preceding_comment_run_start(source, decl_range.start, spec.line_comment_prefixes)
            .unwrap_or(decl_range.start);
    let span = span_for_range(&(span_start..decl_range.end), lines, source)?;
    let header = header_for(
        span,
        decl_range.start,
        tag.name_range.start,
        name,
        tag.docs.is_some() && spec.doc_is_leading_body_string,
        source,
        lines,
    )?;
    let doc_idents = tag.docs.as_deref().map(scan_idents).unwrap_or_default();
    let test_signals = TestSignals {
        explicit_attribute: header
            .attribute_idents
            .iter()
            .any(|ident| (spec.test_attribute)(ident)),
        inside_cfg_test: false,
        inside_test_scope: false,
    };
    let mut def = Def {
        name: name.to_owned(),
        local_qualified: None,
        kind,
        visibility: if is_local {
            Visibility::Private
        } else {
            (spec.visibility)(name)
        },
        span,
        signature_span: header.signature_span,
        signature: header.signature,
        signature_idents: header.signature_idents,
        body_idents: Vec::new(),
        doc_idents,
        attribute_idents: header.attribute_idents,
        test_signals,
    };
    (spec.refine)(&mut def);
    Ok(Some((def, header.exclusions)))
}

/// The header of one tagged definition: its signature, its attached
/// attributes, and the regions the body scan must skip.
struct Header {
    signature_span: Span,
    signature: String,
    signature_idents: Vec<String>,
    attribute_idents: Vec<String>,
    /// Regions the body scan must not read: the absorbed comment run, attached
    /// attributes, and the captured documentation string. The signature span
    /// is excluded by the caller, which already holds it.
    exclusions: Vec<Span>,
}

fn header_for(
    span: Span,
    decl_start: usize,
    name_start: usize,
    name: &str,
    has_docs: bool,
    source: &str,
    lines: &LineIndex,
) -> Result<Header> {
    let span_start = span.start_byte() as usize;
    let span_end = span.end_byte() as usize;
    let decl_start = decl_start.clamp(span_start, span_end);
    let line_start = lines.line_start(name_start).max(decl_start);
    let item_start = source
        .get(line_start..span_end)
        .and_then(|text| text.find(|character: char| !character.is_ascii_whitespace()))
        .map_or(line_start, |offset| line_start + offset)
        .max(decl_start);

    let mut exclusions = Vec::new();
    if decl_start > span_start {
        exclusions.push(span_for_range(&(span_start..decl_start), lines, source)?);
    }
    let attribute_idents = if line_start > decl_start {
        exclusions.push(span_for_range(&(decl_start..line_start), lines, source)?);
        source
            .get(decl_start..line_start)
            .map(scan_idents)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let sig_end = signature_end(source, item_start, span_end);
    let signature_span = span_for_range(&(item_start..sig_end), lines, source)?;
    let raw = source.get(item_start..sig_end).unwrap_or_default();
    let signature = {
        let displayed = crate::facts::display_signature(raw);
        if displayed.is_empty() {
            name.to_owned()
        } else {
            displayed
        }
    };
    let scanned_name = name.trim_start_matches('#');
    let signature_idents = scan_idents(raw)
        .into_iter()
        .filter(|ident| ident != scanned_name)
        .collect();

    if has_docs {
        if let Some(range) = docstring_range(source, sig_end, span_end) {
            exclusions.push(span_for_range(&range, lines, source)?);
        }
    }

    Ok(Header {
        signature_span,
        signature,
        signature_idents,
        attribute_idents,
        exclusions,
    })
}

/// Start of the comment run immediately before `decl_start`, when one abuts
/// the declaration.
///
/// A tags-tier definition node's range begins at the declaration. A preceding
/// comment is not a child of that node, so without this walk it sits inside
/// the container's span and outside the member's — and the container's
/// `body_idents` absorb the member's doc prose. Folding the adjacent run into
/// the member's span is what makes the existing child-exclusion rule catch it.
///
/// Adjacency matches tree-sitter-tags' `#select-adjacent!`: comments may be
/// stacked with only horizontal whitespace between them, but a blank line or
/// any code breaks the run. Line-comment prefixes come from the language
/// (`//` for TypeScript, `#` for Python) so a TypeScript `#attempts` field is
/// never mistaken for a comment; block comments (`/* … */`) are universal.
fn preceding_comment_run_start(
    source: &str,
    decl_start: usize,
    line_comment_prefixes: &[&str],
) -> Option<usize> {
    let bytes = source.as_bytes();
    if decl_start == 0 || decl_start > bytes.len() {
        return None;
    }
    let mut cursor = decl_start;
    let mut absorbed = decl_start;
    loop {
        while cursor > 0 && matches!(bytes[cursor - 1], b' ' | b'\t') {
            cursor -= 1;
        }
        if cursor == 0 {
            break;
        }
        if cursor >= 2 && bytes[cursor - 2] == b'*' && bytes[cursor - 1] == b'/' {
            let Some(open) = block_comment_open(bytes, cursor - 1) else {
                break;
            };
            absorbed = open;
            cursor = open;
            continue;
        }
        if bytes[cursor - 1] != b'\n' {
            break;
        }
        let mut line_end = cursor - 1;
        if line_end > 0 && bytes[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        let mut line_start = line_end;
        while line_start > 0 && bytes[line_start - 1] != b'\n' {
            line_start -= 1;
        }
        let line = &source[line_start..line_end];
        let trim_offset = line.len() - line.trim_start_matches([' ', '\t']).len();
        let trimmed = &line[trim_offset..];
        if trimmed.is_empty() {
            break;
        }
        if line_comment_prefixes
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        {
            absorbed = line_start;
            cursor = line_start;
            continue;
        }
        if let Some(rel) = trimmed.rfind("*/") {
            let close_slash = line_start + trim_offset + rel + 1;
            let Some(open) = block_comment_open(bytes, close_slash) else {
                break;
            };
            absorbed = open;
            cursor = open;
            continue;
        }
        break;
    }
    (absorbed < decl_start).then_some(absorbed)
}

/// Byte offset of the `/` that opens the `/* … */` closed by the `*/` whose
/// `/` sits at `close_slash`.
fn block_comment_open(bytes: &[u8], close_slash: usize) -> Option<usize> {
    if close_slash == 0 || close_slash >= bytes.len() {
        return None;
    }
    if bytes[close_slash] != b'/' || bytes[close_slash - 1] != b'*' {
        return None;
    }
    let mut index = close_slash - 1;
    while index > 0 {
        index -= 1;
        if bytes[index] == b'*' && index > 0 && bytes[index - 1] == b'/' {
            return Some(index - 1);
        }
    }
    None
}

/// Where the declaration header ends: the first line break outside brackets
/// and strings, so a parameter list wrapped across lines stays one signature.
///
/// When no such break exists before the cap — an unbalanced, malformed header —
/// this falls back to the first line so the header cannot swallow the body.
fn signature_end(source: &str, start: usize, span_end: usize) -> usize {
    let cap = span_end.min(start.saturating_add(MAX_SIGNATURE_BYTES));
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut first_newline = None;
    let mut index = start;
    while index < cap {
        let byte = bytes[index];
        if let Some(open) = quote {
            match byte {
                b'\\' => index += 1,
                b'\n' => {
                    first_newline.get_or_insert(index);
                    break;
                }
                _ if byte == open => quote = None,
                _ => {}
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\n' => {
                    first_newline.get_or_insert(index);
                    if depth == 0 {
                        return index;
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    let mut end = first_newline.unwrap_or(cap);
    while end > start && !source.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The byte range of the string literal that opens the body after `from` —
/// the literal the tags query captured as documentation, which the body scan
/// must therefore not read as body identifiers.
///
/// `None` leaves the body scan untouched, for spans so malformed that the text
/// after the header is not a string literal after all.
fn docstring_range(source: &str, from: usize, span_end: usize) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < span_end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    while index < span_end && bytes[index].is_ascii_alphabetic() {
        index += 1;
    }
    if index >= span_end || index - start > 2 {
        return None;
    }
    let open = bytes[index];
    if open != b'"' && open != b'\'' {
        return None;
    }
    let triple = bytes
        .get(index..index + 3)
        .is_some_and(|quotes| quotes[1] == open && quotes[2] == open);
    if triple {
        let needle = if open == b'"' { "\"\"\"" } else { "'''" };
        let body = source.get(index + 3..span_end)?;
        let close = body.find(needle)?;
        return Some(start..index + 3 + close + 3);
    }
    let mut cursor = index + 1;
    while cursor < span_end {
        match bytes[cursor] {
            b'\\' => cursor += 1,
            b'\n' => return None,
            byte if byte == open => return Some(start..cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn assign_nesting(
    defs: &mut [Def],
    header_exclusions: &[Vec<Span>],
    source: &str,
    spec: &LanguageSpec,
) {
    let separator = spec.lang.qualified_separator();
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by(|left, right| {
        let left_span = defs[*left].span;
        let right_span = defs[*right].span;
        left_span
            .start_byte()
            .cmp(&right_span.start_byte())
            .then_with(|| right_span.end_byte().cmp(&left_span.end_byte()))
            .then_with(|| def_key(&defs[*left]).cmp(&def_key(&defs[*right])))
    });

    let mut direct_children: Vec<Vec<Span>> = vec![Vec::new(); defs.len()];
    let mut stack: Vec<usize> = Vec::new();
    for index in order {
        while stack
            .last()
            .is_some_and(|parent| !strictly_contains(defs[*parent].span, defs[index].span))
        {
            stack.pop();
        }
        if let Some(&parent) = stack.last() {
            let owners = naming_owners(defs, &stack, index);
            if !owners.is_empty() {
                let mut segments = owners
                    .iter()
                    .map(|owner| defs[*owner].name.as_str())
                    .collect::<Vec<_>>();
                segments.push(defs[index].name.as_str());
                defs[index].local_qualified = Some(segments.join(separator));
            }
            direct_children[parent].push(defs[index].span);
        }
        let inside_test_scope = stack
            .iter()
            .any(|ancestor| (spec.test_scope)(&defs[*ancestor].name));
        if inside_test_scope {
            defs[index].test_signals.inside_test_scope = true;
        }
        stack.push(index);
    }

    for (index, def) in defs.iter_mut().enumerate() {
        let mut exclusions =
            Vec::with_capacity(1 + header_exclusions[index].len() + direct_children[index].len());
        exclusions.push(def.signature_span);
        exclusions.extend(header_exclusions[index].iter().copied());
        exclusions.extend(direct_children[index].iter().copied());
        def.body_idents = scan_excluding(source, def.span, &exclusions);
    }
}

/// The definitions this one is *named* under, which is not always the ones it
/// sits inside.
///
/// Containment answers the question for everything rr indexes but one
/// construct: a TypeScript parameter property is written in the constructor's
/// parameter list and is a field of the class. `constructor(private repo: Repo)`
/// declares what the rest of the class reads as `this.repo` and what a caller
/// names `Service.repo`, so qualifying it `Service.constructor.repo` would file
/// it under a path nothing refers to it by.
///
/// Recognised through the vocabulary rather than through node names, which is
/// what makes it safe to run for every language: a field is state on a type,
/// no callable declares one, and `DefKind::Constructor` is produced by exactly
/// one `refine`. For Rust and Python this never fires.
fn naming_owners<'a>(defs: &[Def], stack: &'a [usize], index: usize) -> &'a [usize] {
    let hoisted = defs[index].kind == DefKind::Field
        && stack
            .last()
            .is_some_and(|parent| defs[*parent].kind == DefKind::Constructor);
    if hoisted {
        &stack[..stack.len() - 1]
    } else {
        stack
    }
}

fn strictly_contains(parent: Span, child: Span) -> bool {
    parent.start_byte() <= child.start_byte()
        && child.end_byte() <= parent.end_byte()
        && (parent.start_byte() < child.start_byte() || child.end_byte() < parent.end_byte())
}

fn scan_excluding(source: &str, container: Span, excluded: &[Span]) -> Vec<String> {
    let mut ranges = excluded.to_vec();
    ranges.sort_by_key(|span| (span.start_byte(), span.end_byte()));
    let container_start = container.start_byte() as usize;
    let container_end = container.end_byte() as usize;
    let mut cursor = container_start;
    let mut idents = Vec::new();
    for span in ranges {
        let start = (span.start_byte() as usize).max(container_start);
        let end = (span.end_byte() as usize).min(container_end);
        if end <= cursor {
            continue;
        }
        if start > cursor {
            if let Some(text) = source.get(cursor..start) {
                idents.extend(scan_idents(text));
            }
        }
        cursor = end;
    }
    if cursor < container_end {
        if let Some(text) = source.get(cursor..container_end) {
            idents.extend(scan_idents(text));
        }
    }
    idents
}

/// PEP 8 as [`Visibility`] documents it: a `__mangled` name is private, a
/// `_internal` name is a weak internal-use indicator, and a `__dunder__` name
/// is the public protocol a class exposes.
fn python_visibility(name: &str) -> Visibility {
    let is_dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
    if is_dunder {
        Visibility::Public
    } else if name.starts_with("__") {
        Visibility::Private
    } else if name.starts_with('_') {
        Visibility::Internal
    } else {
        Visibility::Public
    }
}

/// A decorator mentioning either standard test framework marks a test, the
/// tags-tier reading of what `#[test]` states in Rust.
fn python_test_attribute(ident: &str) -> bool {
    ident == "pytest" || ident == "unittest"
}

/// pytest collects the methods of `Test*`-named classes.
fn python_test_scope(name: &str) -> bool {
    name.starts_with("Test")
}

/// The last word on one Python definition.
///
/// The query alone tells `class`, `def` and a module-level assignment apart —
/// three node types — and PEP 8 visibility is readable from the name, so the
/// only thing left is the descriptor protocol. A `@property`, `@x.setter`,
/// `@x.getter` or `@x.deleter` declares a member reached like a field and
/// computed like a method, which is exactly [`DefKind::Property`].
///
/// Judged from the decorator identifiers rather than from what they resolve
/// to, the same evidence `typescript_refine` reads for a `get`/`set` accessor.
/// The limit is stated rather than hidden: this cannot see whether the
/// function is inside a class, so a module-level function decorated
/// `@property` is recorded as a property. That is a smaller error than
/// recording every real property as a plain function, which is what the tags
/// tier did before.
fn python_refine(def: &mut Def) {
    if def.kind == DefKind::Function && declares_a_descriptor(&def.attribute_idents) {
        def.kind = DefKind::Property;
    }
}

/// Whether the decorators on a definition spell one of the four descriptor
/// forms: `@property`, `@x.setter`, `@x.getter`, `@x.deleter`.
///
/// `attribute_idents` is flat — `@size.setter` contributes `size` and `setter`
/// — so the accessor forms are recognised by their trailing word. A decorator
/// called `deleter` that is not an accessor is a name collision this tier
/// cannot see through, and [`python_refine`] says so.
///
/// The list is wider than the decorator *names*: [`header_for`] scans every
/// identifier the decorator block holds, arguments and string contents
/// included. So `@register(getter=make)` and `@app.route("/property")` read as
/// descriptors and their functions are recorded as [`DefKind::Property`].
/// Narrowing that needs the decorator names themselves, which this tier does
/// not carry down to `refine`; it is the same class of collision as a
/// non-accessor `deleter`, and it is written down rather than papered over.
fn declares_a_descriptor(attributes: &[String]) -> bool {
    attributes
        .iter()
        .any(|ident| matches!(ident.as_str(), "property" | "setter" | "getter" | "deleter"))
}

pub(crate) static PYTHON: LanguageSpec = LanguageSpec {
    lang: Lang::Python,
    language: tree_sitter_python::LANGUAGE,
    tags_query: include_str!("queries/python.scm"),
    locals_query: "",
    kinds: &[
        ("constant", DefKind::Variable),
        ("class", DefKind::Class),
        ("function", DefKind::Function),
    ],
    reference_kinds: &[
        ("call", ReferenceKind::Call),
        ("method", ReferenceKind::MethodCall),
    ],
    imports: Some(&PYTHON_IMPORTS),
    visibility: python_visibility,
    test_attribute: python_test_attribute,
    test_scope: python_test_scope,
    refine: python_refine,
    doc_is_leading_body_string: true,
    line_comment_prefixes: &["#"],
    config: OnceLock::new(),
};

static PYTHON_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/python-imports.scm"),
    kinds: &[("import", ImportKind::Import), ("from", ImportKind::From)],
    markers: &["import"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

/// Every word TypeScript allows between the start of a member declaration and
/// its name.
///
/// Read as a prefix rather than searched for: a parameter called `privateKey`
/// and a field called `private` are both things a repository contains, and
/// neither declares a visibility.
const TYPESCRIPT_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "readonly",
    "abstract",
    "override",
    "accessor",
    "declare",
    "async",
    "get",
    "set",
];

/// The modifiers this declaration opens with, in source order.
///
/// [`crate::facts::display_signature`] has already folded every whitespace run
/// into one space, so splitting on it recovers the tokens exactly — once a
/// leading decorator is out of the way. `@Input() private name: string` states
/// `private`, and a scan that stopped at the decorator would report the class
/// default instead.
fn typescript_modifiers(signature: &str) -> impl Iterator<Item = &str> {
    strip_leading_decorations(signature)
        .split(' ')
        .take_while(|word| TYPESCRIPT_MODIFIERS.contains(word))
}

/// The visibility a member states, or `None` when it states none.
fn declared_visibility(signature: &str) -> Option<Visibility> {
    typescript_modifiers(signature).find_map(|word| match word {
        "private" => Some(Visibility::Private),
        "protected" => Some(Visibility::Protected),
        "public" => Some(Visibility::Public),
        _ => None,
    })
}

/// A `#name` is private to its class by the language, not by convention: the
/// `#` is part of the name, and reading it outside the class is a syntax
/// error. Everything else is public until a modifier says otherwise, which is
/// [`declared_visibility`]'s answer rather than this one's.
fn typescript_visibility(name: &str) -> Visibility {
    if name.starts_with('#') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// The initializer this binding assigns, or `None` when it assigns nothing.
///
/// The `=` that opens it is the first one that is not part of an operator:
/// `const f: (x: number) => void = …` annotates before it assigns, and
/// splitting on the `=` of that `=>` would read the *type* as the initializer.
fn initializer_of(signature: &str) -> Option<&str> {
    let bytes = signature.as_bytes();
    let assignment = (0..bytes.len()).find(|index| {
        if bytes[*index] != b'=' {
            return false;
        }
        let previous = index.checked_sub(1).map(|before| bytes[before]);
        let next = bytes.get(index + 1).copied();
        !matches!(next, Some(b'=' | b'>')) && !matches!(previous, Some(b'=' | b'!' | b'<' | b'>'))
    })?;
    signature.get(assignment + 1..)
}

/// The text after a leading type-parameter list, or `None` when the `<` does
/// not close before the string ends.
///
/// `<` is the one bracket that is not reliably a bracket: it opens a type
/// parameter list, a JSX element and a comparison alike. Matching it by depth
/// is what separates `<T extends Map<string, number>,>(a: T) => a`, whose list
/// closes onto the parameters, from `<Badge onClick={() => go()} />`, which is
/// a value that happens to contain an arrow.
fn after_type_parameters(initializer: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (offset, character) in initializer.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return initializer.get(offset + 1..);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether this binding's initializer is a function rather than a value.
///
/// Judged from the initializer's first token and not from `=>` appearing
/// anywhere, because `{ onEvent: () => … }` is an object and `(a) >= (b)` is a
/// comparison. A binding whose initializer starts on the next line is not
/// judged at all: the signature ends at the first line break outside brackets,
/// so there is nothing there to read, and it stays a [`DefKind::Variable`].
fn initializer_is_a_function(signature: &str) -> bool {
    let Some(initializer) = initializer_of(signature) else {
        return false;
    };
    let initializer = initializer.trim_start();
    let initializer = initializer
        .strip_prefix("async ")
        .map_or(initializer, str::trim_start);
    if initializer.starts_with("function") {
        return true;
    }
    let opens_parameters = if initializer.starts_with('<') {
        after_type_parameters(initializer).is_some_and(|rest| rest.trim_start().starts_with('('))
    } else {
        initializer.starts_with('(')
    };
    opens_parameters && initializer.contains("=>")
}

/// What the TypeScript query captured, plus what it structurally could not.
///
/// A `constructor`, a `get`/`set` accessor and an arrow-function binding are
/// all `method_definition`s or `variable_declarator`s to the grammar; only
/// their text tells them apart, and a tags query cannot read text. Nor can it
/// reach an `accessibility_modifier`, which is an unnamed sibling of the name.
fn typescript_refine(def: &mut Def) {
    if let Some(visibility) = declared_visibility(&def.signature) {
        def.visibility = visibility;
    }
    match def.kind {
        DefKind::Method => {
            if def.name == "constructor" {
                def.kind = DefKind::Constructor;
            } else if typescript_modifiers(&def.signature)
                .any(|word| word == "get" || word == "set")
            {
                def.kind = DefKind::Property;
            }
        }
        DefKind::Variable if initializer_is_a_function(&def.signature) => {
            def.kind = DefKind::Function;
        }
        _ => {}
    }
}

/// TypeScript declares no test intent on the definition.
///
/// `describe`, `it` and `test` are calls: not attributes, and not the names of
/// enclosing definitions. Reading them as a test scope here would mean
/// inventing a claim the tags tier never saw. What TypeScript does state is in
/// the file name, and [`Lang::path_indicates_test`] already reads
/// `.test.ts`/`.spec.ts`.
fn never_a_test_signal(_name: &str) -> bool {
    false
}

/// A language whose tags capture is already the right kind.
fn keep_as_captured(_def: &mut Def) {}

/// The declaration text with any leading annotation or attribute run removed.
///
/// [`crate::facts::display_signature`] folds every whitespace run into one
/// space, which is what lets the modifier scans below split on `' '` — but only
/// once the decoration in front of the modifiers is gone. Java writes
/// `@Override public void run()`, Swift writes `@objc public func run()` and
/// `@State private var count: Int`, PHP 8 writes `#[Route('/x')] public
/// function run()`; in every one of them the access modifier the scan is
/// looking for sits behind a decoration, and a scan that stopped at the first
/// non-modifier word would report the language's default instead of what the
/// file says.
///
/// Skipped by bracket depth rather than word by word, because an annotation
/// carrying arguments holds spaces of its own: `@RequestMapping(value = "/x")`
/// is one decoration and four space-separated words.
fn strip_leading_decorations(signature: &str) -> &str {
    let bytes = signature.as_bytes();
    let mut cursor = 0;
    loop {
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        if !bytes[cursor..].starts_with(b"#[") && bytes.get(cursor) != Some(&b'@') {
            return signature.get(cursor..).unwrap_or("");
        }
        let mut index = cursor + 1;

        let mut depth = 0usize;
        while let Some(&byte) = bytes.get(index) {
            match byte {
                b'(' | b'[' => depth += 1,
                b')' | b']' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    if depth == 0 {
                        index += 1;
                        break;
                    }
                }
                b' ' if depth == 0 => break,
                _ => {}
            }
            index += 1;
        }
        cursor = index;
    }
}

const TYPESCRIPT_KINDS: &[(&str, DefKind)] = &[
    ("function", DefKind::Function),
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("enum", DefKind::Enum),
    ("type", DefKind::TypeAlias),
    ("namespace", DefKind::Namespace),
    ("method", DefKind::Method),
    ("field", DefKind::Field),
    ("variable", DefKind::Variable),
];

const TYPESCRIPT_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("method", ReferenceKind::MethodCall),
];

/// One TypeScript specification, differing from the other only in the grammar
/// it compiles against.
///
/// Spelled once so the two cannot drift: everything below the first two fields
/// is the language, and the language is the same one.
const fn typescript_spec(
    lang: Lang,
    language: LanguageFn,
    imports: &'static ImportSpec,
) -> LanguageSpec {
    LanguageSpec {
        lang,
        language,
        tags_query: include_str!("queries/typescript.scm"),
        locals_query: "",
        kinds: TYPESCRIPT_KINDS,
        reference_kinds: TYPESCRIPT_REFERENCE_KINDS,
        imports: Some(imports),
        visibility: typescript_visibility,
        test_attribute: never_a_test_signal,
        test_scope: never_a_test_signal,
        refine: typescript_refine,
        doc_is_leading_body_string: false,
        line_comment_prefixes: &["//"],
        config: OnceLock::new(),
    }
}

const TYPESCRIPT_IMPORT_KINDS: &[(&str, ImportKind)] = &[
    ("import", ImportKind::Import),
    ("from", ImportKind::From),
    ("require", ImportKind::Require),
];

/// The same query source and the same kinds as [`TSX_IMPORTS`], and its own
/// `OnceLock`, for the reason [`TSX`] is its own static: a compiled query owns
/// its grammar.
static TYPESCRIPT_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/typescript-imports.scm"),
    kinds: TYPESCRIPT_IMPORT_KINDS,
    markers: &["import", "require", "from"],
    callee_names: &["require"],
    compiled: OnceLock::new(),
};

static TSX_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/typescript-imports.scm"),
    kinds: TYPESCRIPT_IMPORT_KINDS,
    markers: &["import", "require", "from"],
    callee_names: &["require"],
    compiled: OnceLock::new(),
};

pub(crate) static TYPESCRIPT: LanguageSpec = typescript_spec(
    Lang::TypeScript,
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    &TYPESCRIPT_IMPORTS,
);

/// TSX: the same query against a different grammar.
///
/// Tree-sitter ships two parsers because the languages disagree — `<T>(x)` is a
/// type assertion in one and an unclosed element in the other — so a `.tsx`
/// file parsed by the TypeScript grammar is a file full of syntax errors, not
/// a file that happens to also parse. The separate [`OnceLock`] is what keeps
/// them apart: a compiled `TagsConfiguration` holds its grammar, so one shared
/// between these two specs would hand every `.tsx` file the `.ts` parser —
/// which is why each of these is its own `static` rather than one built twice.
pub(crate) static TSX: LanguageSpec = typescript_spec(
    Lang::Tsx,
    tree_sitter_typescript::LANGUAGE_TSX,
    &TSX_IMPORTS,
);

const JAVASCRIPT_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("function", DefKind::Function),
    ("method", DefKind::Method),
    ("constant", DefKind::Const),
];

const JAVASCRIPT_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("class", ReferenceKind::Type),
];

/// A `#name` is private to its class by the language, exactly as in TypeScript.
///
/// Shared with TypeScript rather than duplicated: private class fields are one
/// proposal implemented once, and the `#` is part of the name in both grammars.
fn javascript_visibility(name: &str) -> Visibility {
    typescript_visibility(name)
}

/// What the JavaScript query captured, plus the two things its node types
/// cannot separate.
///
/// `constructor` and a `get`/`set` accessor are both `method_definition`s, and
/// upstream's query captures neither — so unlike TypeScript's, this hook has no
/// `DefKind::Method` to promote and only handles the arrow-binding case that
/// `definition.function` already reaches it as. Kept as a named function rather
/// than `keep_as_captured` so the day the query grows a `constructor` pattern
/// there is a place to put the promotion.
fn javascript_refine(def: &mut Def) {
    if def.name == "constructor" && def.kind == DefKind::Method {
        def.kind = DefKind::Constructor;
    }
}

/// One JavaScript specification, differing from the other only in the `Lang`
/// it reports and the `OnceLock` it fills.
///
/// Both compile the *same* grammar: `tree_sitter_javascript::LANGUAGE` parses
/// `.jsx` with no parse errors, which is why `Lang::Jsx` needs no second crate
/// and why JavaScript and JSX share one grammar crate. Kept as two statics all
/// the same, because `Registry` is keyed by `Lang` and a shared static would
/// file `.jsx` facts under `Lang::JavaScript`.
const fn javascript_spec(lang: Lang, imports: &'static ImportSpec) -> LanguageSpec {
    LanguageSpec {
        lang,
        language: tree_sitter_javascript::LANGUAGE,
        tags_query: tree_sitter_javascript::TAGS_QUERY,
        locals_query: "",
        kinds: JAVASCRIPT_KINDS,
        reference_kinds: JAVASCRIPT_REFERENCE_KINDS,
        imports: Some(imports),
        visibility: javascript_visibility,
        test_attribute: never_a_test_signal,
        test_scope: never_a_test_signal,
        refine: javascript_refine,
        doc_is_leading_body_string: false,
        line_comment_prefixes: &["//"],
        config: OnceLock::new(),
    }
}

const JAVASCRIPT_IMPORT_KINDS: &[(&str, ImportKind)] = &[
    ("import", ImportKind::Import),
    ("from", ImportKind::From),
    ("require", ImportKind::Require),
];

static JAVASCRIPT_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/javascript-imports.scm"),
    kinds: JAVASCRIPT_IMPORT_KINDS,
    markers: &["import", "require", "from"],
    callee_names: &["require"],
    compiled: OnceLock::new(),
};

static JSX_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/javascript-imports.scm"),
    kinds: JAVASCRIPT_IMPORT_KINDS,
    markers: &["import", "require", "from"],
    callee_names: &["require"],
    compiled: OnceLock::new(),
};

pub(crate) static JAVASCRIPT: LanguageSpec = javascript_spec(Lang::JavaScript, &JAVASCRIPT_IMPORTS);

pub(crate) static JSX: LanguageSpec = javascript_spec(Lang::Jsx, &JSX_IMPORTS);

const GO_KINDS: &[(&str, DefKind)] = &[
    ("function", DefKind::Function),
    ("method", DefKind::Method),
    ("type", DefKind::TypeAlias),
];

const GO_REFERENCE_KINDS: &[(&str, ReferenceKind)] =
    &[("call", ReferenceKind::Call), ("type", ReferenceKind::Type)];

/// Go's export rule, which is the whole of its visibility.
///
/// An initial upper-case rune exports; anything else is visible only inside the
/// declaring package. Judged on the first character rather than
/// `str::to_uppercase`, because a name starting with `_` or a digit is neither
/// upper nor lower and is package-scoped, and because Go's rule is about the
/// rune's Unicode category, not about round-tripping a case conversion.
fn go_visibility(name: &str) -> Visibility {
    match name.chars().next() {
        Some(first) if first.is_uppercase() => Visibility::Public,
        _ => Visibility::Package,
    }
}

/// Go states test intent in the file name and the function name, and rr already
/// reads the first: `Lang::path_indicates_test` handles `_test.go`. The second
/// is `TestXxx`, which is a name and not an attribute, so it belongs to
/// whichever tier can see the file name — not to this hook.
fn go_test_scope(_name: &str) -> bool {
    false
}

pub(crate) static GO: LanguageSpec = LanguageSpec {
    lang: Lang::Go,
    language: tree_sitter_go::LANGUAGE,
    tags_query: tree_sitter_go::TAGS_QUERY,
    locals_query: "",
    kinds: GO_KINDS,
    reference_kinds: GO_REFERENCE_KINDS,
    imports: Some(&GO_IMPORTS),
    visibility: go_visibility,
    test_attribute: never_a_test_signal,
    test_scope: go_test_scope,
    refine: keep_as_captured,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static GO_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/go-imports.scm"),
    kinds: &[("import", ImportKind::Import)],
    markers: &["import"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const JAVA_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("method", DefKind::Method),
    ("constructor", DefKind::Constructor),
    ("enum", DefKind::Enum),
    ("record", DefKind::Struct),
];

const JAVA_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("class", ReferenceKind::Type),
    ("implementation", ReferenceKind::Implementation),
];

/// Java states visibility with a modifier, and its *absence* is a visibility.
///
/// Returned from the name alone this can only be a placeholder, because a Java
/// name says nothing: `java_refine` overwrites it from the declaration text, the
/// way `typescript_refine` does. `Package` and not `Public` is the placeholder
/// because it is the language's default, so a declaration whose modifier the
/// signature slice truncated away is reported as the thing it most likely is.
fn java_visibility(_name: &str) -> Visibility {
    Visibility::Package
}

/// Every word Java allows between the start of a member declaration and its
/// return type, read as a prefix for the reason `TYPESCRIPT_MODIFIERS` gives:
/// a field named `final` and a parameter named `synchronized` are both things a
/// repository contains.
const JAVA_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "final",
    "abstract",
    "synchronized",
    "native",
    "transient",
    "volatile",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
];

/// The access modifier this declaration states, or `None` when it states none.
///
/// `display_signature` has already folded every whitespace run into one space,
/// so splitting on it recovers the tokens exactly — the same contract
/// `typescript_modifiers` relies on. Annotations are skipped rather than
/// treated as terminators: `@Override public void run()` states `public`, and
/// so does `@RequestMapping(value = "/x") public void run()`.
fn java_declared_visibility(signature: &str) -> Option<Visibility> {
    strip_leading_decorations(signature)
        .split(' ')
        .take_while(|word| JAVA_MODIFIERS.contains(word))
        .find_map(|word| match word {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            "protected" => Some(Visibility::Protected),
            _ => None,
        })
}

/// Whether a declaration that states no modifier at all is public regardless.
///
/// An interface method and an annotation element are public by the language,
/// and they are the only member declarations Java lets carry no modifier *and*
/// no body: inside a class a method without a body has to be `abstract` or
/// `native`, and both of those open the modifier run this requires to be empty.
/// So a captured member that ends in `;` and opens straight onto its return
/// type is an interface member, and `Visibility::Package` would mislabel every
/// method of every interface in the repository.
///
/// A class method with its brace on the next line never reaches here:
/// `signature_end` stops at that line break, so its signature ends in `)`.
fn java_states_no_modifier_and_is_public(signature: &str) -> bool {
    let signature = strip_leading_decorations(signature);
    signature.trim_end().ends_with(';')
        && !signature
            .split(' ')
            .next()
            .is_some_and(|word| JAVA_MODIFIERS.contains(&word))
}

/// What the Java query captured, plus the modifier no capture can reach.
///
/// An `@Test` annotation is an `attribute_ident`, which the extractor already
/// collects, so test detection needs no work here — `java_test_attribute` reads
/// it. What does need work is access: `modifiers` is an unnamed sibling of the
/// name in this grammar, so only the declaration text says whether a method is
/// public, and a method in a class that says nothing is package-private rather
/// than public — while the same words inside an interface are public.
fn java_refine(def: &mut Def) {
    if let Some(visibility) = java_declared_visibility(&def.signature) {
        def.visibility = visibility;
    } else if java_states_no_modifier_and_is_public(&def.signature) {
        def.visibility = Visibility::Public;
    }
}

/// `JUnit` 4 and 5 both spell it `@Test`; `TestNG` spells it the same.
fn java_test_attribute(ident: &str) -> bool {
    ident == "Test" || ident == "ParameterizedTest" || ident == "RepeatedTest"
}

pub(crate) static JAVA: LanguageSpec = LanguageSpec {
    lang: Lang::Java,
    language: tree_sitter_java::LANGUAGE,
    tags_query: include_str!("queries/java.scm"),
    locals_query: "",
    kinds: JAVA_KINDS,
    reference_kinds: JAVA_REFERENCE_KINDS,
    imports: Some(&JAVA_IMPORTS),
    visibility: java_visibility,
    test_attribute: java_test_attribute,
    test_scope: never_a_test_signal,
    refine: java_refine,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static JAVA_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/java-imports.scm"),
    kinds: &[("import", ImportKind::Import), ("from", ImportKind::From)],
    markers: &["import"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const CSHARP_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("struct", DefKind::Struct),
    ("record", DefKind::Class),
    ("enum", DefKind::Enum),
    ("type", DefKind::TypeAlias),
    ("method", DefKind::Method),
    ("function", DefKind::Function),
    ("constructor", DefKind::Constructor),
    ("property", DefKind::Property),
    ("field", DefKind::Field),
    ("module", DefKind::Module),
];

/// A C# `record` maps to `Class` where Java's maps to `Struct`, deliberately:
/// a C# record is a reference type the compiler shapes, and both spellings
/// `record` and `record struct` parse to the same node — the value-type form
/// is a modifier the grammar does not distinguish, so `Struct` would be a
/// guess on the wrong half of them.
const CSHARP_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("method-call", ReferenceKind::MethodCall),
    ("class", ReferenceKind::Type),
    ("implementation", ReferenceKind::Implementation),
];

/// A C# member that states no modifier is private, which is the language's
/// default; the placeholder has to say the same thing the file does.
fn csharp_visibility(_name: &str) -> Visibility {
    Visibility::Private
}

/// Every word C# allows between the start of a declaration and its type, read
/// as a prefix for the reason `JAVA_MODIFIERS` gives: a field named `sealed`
/// is a thing a repository contains.
const CSHARP_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "readonly",
    "sealed",
    "abstract",
    "virtual",
    "override",
    "async",
    "partial",
    "const",
    "unsafe",
    "extern",
    "ref",
    "required",
    "file",
];

/// The declaration text with any leading attribute run removed.
///
/// The C# grammar nests `[Fact]` inside the declaration node, so the signature
/// slice opens with the attribute rather than the modifier; `strip_leading_decorations`
/// knows `@` and `#[` but not a bare `[`, which no other language in this file
/// puts at that position.
fn csharp_strip_attributes(signature: &str) -> &str {
    let bytes = signature.as_bytes();
    let mut cursor = 0;
    loop {
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'[') {
            return signature.get(cursor..).unwrap_or("");
        }
        let mut index = cursor;
        let mut depth = 0usize;
        while let Some(&byte) = bytes.get(index) {
            match byte {
                b'[' | b'(' => depth += 1,
                b']' | b')' => {
                    depth -= 1;
                    if depth == 0 {
                        index += 1;
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        cursor = index;
    }
}

/// The access modifier this declaration states, or `None` when it states none.
///
/// `internal` reads as [`Visibility::Package`]: enforced by the compiler and
/// scoped to one compilation unit, like Java's default access. The compound
/// forms keep their first word — `protected internal` is at least `protected`,
/// `private protected` at least `private`.
fn csharp_declared_visibility(signature: &str) -> Option<Visibility> {
    csharp_strip_attributes(signature)
        .split(' ')
        .take_while(|word| CSHARP_MODIFIERS.contains(word))
        .find_map(|word| match word {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            "protected" => Some(Visibility::Protected),
            "internal" => Some(Visibility::Package),
            _ => None,
        })
}

/// What the C# query captured, plus the modifier no capture can reach.
fn csharp_refine(def: &mut Def) {
    if let Some(visibility) = csharp_declared_visibility(&def.signature) {
        def.visibility = visibility;
    }
}

/// xUnit `Fact`/`Theory`, `NUnit` `Test`/`TestCase`, `MSTest` `TestMethod`.
fn csharp_test_attribute(ident: &str) -> bool {
    matches!(
        ident,
        "Fact" | "Theory" | "Test" | "TestCase" | "TestMethod"
    )
}

pub(crate) static CSHARP: LanguageSpec = LanguageSpec {
    lang: Lang::CSharp,
    language: tree_sitter_c_sharp::LANGUAGE,
    tags_query: include_str!("queries/csharp.scm"),
    locals_query: "",
    kinds: CSHARP_KINDS,
    reference_kinds: CSHARP_REFERENCE_KINDS,
    imports: Some(&CSHARP_IMPORTS),
    visibility: csharp_visibility,
    test_attribute: csharp_test_attribute,
    test_scope: never_a_test_signal,
    refine: csharp_refine,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static CSHARP_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/csharp-imports.scm"),
    kinds: &[("import", ImportKind::Import)],
    markers: &["using"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const C_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Struct),
    ("function", DefKind::Function),
    ("type", DefKind::TypeAlias),
];

/// C has no visibility modifier reachable from a tags query.
///
/// `static` at file scope means internal linkage, which is exactly
/// `Visibility::Internal` — but `static` is an unnamed token this query never
/// captures, and the signature slice is not read here because C's `static` and
/// C++'s member `static` mean unrelated things. Everything is `Public` until a
/// tier that can see storage class says otherwise.
fn c_visibility(_name: &str) -> Visibility {
    Visibility::Public
}

pub(crate) static C: LanguageSpec = LanguageSpec {
    lang: Lang::C,
    language: tree_sitter_c::LANGUAGE,
    tags_query: tree_sitter_c::TAGS_QUERY,
    locals_query: "",
    kinds: C_KINDS,
    reference_kinds: &[],
    imports: Some(&C_IMPORTS),
    visibility: c_visibility,
    test_attribute: never_a_test_signal,
    test_scope: never_a_test_signal,
    refine: keep_as_captured,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static C_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/c-imports.scm"),
    kinds: &[("include", ImportKind::Include)],
    markers: &["include"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const CPP_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("function", DefKind::Function),
    ("method", DefKind::Method),
    ("type", DefKind::TypeAlias),
];

pub(crate) static CPP: LanguageSpec = LanguageSpec {
    lang: Lang::Cpp,
    language: tree_sitter_cpp::LANGUAGE,
    tags_query: tree_sitter_cpp::TAGS_QUERY,
    locals_query: "",
    kinds: CPP_KINDS,
    reference_kinds: &[],
    imports: Some(&CPP_IMPORTS),
    visibility: c_visibility,
    test_attribute: never_a_test_signal,
    test_scope: never_a_test_signal,
    refine: keep_as_captured,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static CPP_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/cpp-imports.scm"),
    kinds: &[("include", ImportKind::Include)],
    markers: &["include"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const RUBY_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("module", DefKind::Module),
    ("method", DefKind::Method),
];

const RUBY_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[("call", ReferenceKind::Call)];

/// Ruby's `private` is a method call whose effect runs to the end of the body,
/// which a tags query cannot see and this hook cannot reach either: `refine`
/// runs before `assign_nesting`, so it does not know which class a method is in,
/// let alone which `private` preceded it. A leading underscore is a convention
/// Ruby does not enforce, so it reads as internal and not as private.
fn ruby_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Internal
    } else {
        Visibility::Public
    }
}

/// Minitest names the enclosing class `TestFoo` or `FooTest`, which is the one
/// convention this hook can read.
///
/// `test_scope` is asked about an *enclosing* definition's name, so `RSpec`'s
/// `describe`/`it` blocks are out of reach — they are method calls, not
/// definitions, and nothing captures them. Minitest's `test_`-prefixed methods
/// are out of reach for the opposite reason: they are the definitions being
/// judged rather than the scope around them, and `Def::test_signals` has no
/// field for "this definition is itself a test".
fn ruby_test_scope(name: &str) -> bool {
    name.starts_with("Test") || name.ends_with("Test")
}

/// Ruby's constructor is `initialize`, reached through `Class.new` rather than
/// by its own name.
///
/// Promoted for the reason PHP's `__construct` and Swift's `init` are: the tags
/// query has one capture for every `def`, and a caller looking for the entry
/// point of a class should not have to know each language's spelling of it.
fn ruby_refine(def: &mut Def) {
    if def.name == "initialize" && def.kind == DefKind::Method {
        def.kind = DefKind::Constructor;
    }
}

pub(crate) static RUBY: LanguageSpec = LanguageSpec {
    lang: Lang::Ruby,
    language: tree_sitter_ruby::LANGUAGE,
    tags_query: tree_sitter_ruby::TAGS_QUERY,
    locals_query: "",
    kinds: RUBY_KINDS,
    reference_kinds: RUBY_REFERENCE_KINDS,
    imports: Some(&RUBY_IMPORTS),
    visibility: ruby_visibility,
    test_attribute: never_a_test_signal,
    test_scope: ruby_test_scope,
    refine: ruby_refine,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["#"],
    config: OnceLock::new(),
};

static RUBY_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/ruby-imports.scm"),
    kinds: &[("require", ImportKind::Require)],
    markers: &["require"],
    callee_names: &["require", "require_relative"],
    compiled: OnceLock::new(),
};

const LUA_KINDS: &[(&str, DefKind)] =
    &[("function", DefKind::Function), ("method", DefKind::Method)];

const LUA_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[("call", ReferenceKind::Call)];

/// Lua's only visibility is `local`, which is a statement keyword the query does
/// not capture and the signature slice does reach — but `local function f` and a
/// `local f = function()` differ in where the keyword sits, and reading only the
/// first would report the second as public. Everything is public until a tier
/// that can see the statement says otherwise.
fn lua_visibility(_name: &str) -> Visibility {
    Visibility::Public
}

pub(crate) static LUA: LanguageSpec = LanguageSpec {
    lang: Lang::Lua,
    language: tree_sitter_lua::LANGUAGE,
    tags_query: tree_sitter_lua::TAGS_QUERY,
    locals_query: "",
    kinds: LUA_KINDS,
    reference_kinds: LUA_REFERENCE_KINDS,
    imports: Some(&LUA_IMPORTS),
    visibility: lua_visibility,
    test_attribute: never_a_test_signal,
    test_scope: never_a_test_signal,
    refine: keep_as_captured,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["--"],
    config: OnceLock::new(),
};

static LUA_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/lua-imports.scm"),
    kinds: &[("require", ImportKind::Require)],
    markers: &["require"],
    callee_names: &["require"],
    compiled: OnceLock::new(),
};

const PHP_KINDS: &[(&str, DefKind)] = &[
    ("module", DefKind::Namespace),
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("field", DefKind::Field),
    ("function", DefKind::Function),
];

const PHP_REFERENCE_KINDS: &[(&str, ReferenceKind)] = &[
    ("call", ReferenceKind::Call),
    ("class", ReferenceKind::Type),
    ("implementation", ReferenceKind::Implementation),
];

const PHP_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "abstract",
    "final",
    "readonly",
];

/// The visibility this member states, and whether it stated anything at all.
///
/// A PHP 8 attribute is skipped rather than treated as a terminator, the way
/// Java's annotations are: `#[Route('/x')] public function run()` states
/// `public`.
fn php_declared_visibility(signature: &str) -> Option<Visibility> {
    strip_leading_decorations(signature)
        .split(' ')
        .take_while(|word| PHP_MODIFIERS.contains(word))
        .find_map(|word| match word {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            "protected" => Some(Visibility::Protected),
            _ => None,
        })
}

/// PHP's default is public, and it says so: a member with no modifier is public
/// by the language rather than package-scoped, which is why this is not Java's
/// `Package`.
fn php_visibility(_name: &str) -> Visibility {
    Visibility::Public
}

/// Whether this declaration opens with any modifier at all, visibility or not.
///
/// `abstract function run();` and `static function make()` are members even
/// though neither states an access level, so membership asks a wider question
/// than `php_declared_visibility` answers.
fn php_opens_with_modifier(signature: &str) -> bool {
    strip_leading_decorations(signature)
        .split(' ')
        .next()
        .is_some_and(|word| PHP_MODIFIERS.contains(&word))
}

/// Whether this declaration has no body, which in PHP means it is a member.
///
/// PHP has no forward declaration at file scope: a `function` that ends at `;`
/// instead of a body can only be an interface method or an abstract one. A free
/// function always carries its body, so `signature_end` leaves its signature
/// ending at the parameter list, the return type, or the closing brace — never
/// at a semicolon.
fn php_has_no_body(signature: &str) -> bool {
    strip_leading_decorations(signature)
        .trim_end()
        .ends_with(';')
}

/// What the PHP query captured, plus what one capture for two constructs cannot
/// say.
///
/// Upstream gives methods and free functions the same capture, so membership
/// has to be read off the declaration text. Two things settle it, and `refine`
/// runs before `assign_nesting` (`TagsExtractor::extract`) so neither may ask
/// who the owner is: a leading modifier, and the absence of a body. Between
/// them they cover the bare `function` an interface declares, which states no
/// modifier at all.
fn php_refine(def: &mut Def) {
    if let Some(visibility) = php_declared_visibility(&def.signature) {
        def.visibility = visibility;
    }
    if def.kind == DefKind::Function
        && (php_opens_with_modifier(&def.signature) || php_has_no_body(&def.signature))
    {
        def.kind = DefKind::Method;
    }
    if def.name == "__construct" && def.kind == DefKind::Method {
        def.kind = DefKind::Constructor;
    }
}

pub(crate) static PHP: LanguageSpec = LanguageSpec {
    lang: Lang::Php,
    language: tree_sitter_php::LANGUAGE_PHP,
    tags_query: tree_sitter_php::TAGS_QUERY,
    locals_query: "",
    kinds: PHP_KINDS,
    reference_kinds: PHP_REFERENCE_KINDS,
    imports: Some(&PHP_IMPORTS),
    visibility: php_visibility,
    test_attribute: never_a_test_signal,
    test_scope: never_a_test_signal,
    refine: php_refine,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//", "#"],
    config: OnceLock::new(),
};

static PHP_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/php-imports.scm"),
    kinds: &[
        ("import", ImportKind::Import),
        ("from", ImportKind::From),
        ("require", ImportKind::Require),
    ],
    markers: &["use", "require", "include"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

const SWIFT_KINDS: &[(&str, DefKind)] = &[
    ("class", DefKind::Class),
    ("interface", DefKind::Interface),
    ("method", DefKind::Method),
    ("property", DefKind::Property),
    ("function", DefKind::Function),
];

const SWIFT_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "internal",
    "fileprivate",
    "open",
    "package",
    "static",
    "class",
    "final",
    "override",
    "mutating",
    "lazy",
    "weak",
    "unowned",
];

/// The access level this declaration states, or `None` when it states none.
///
/// Attributes are skipped rather than treated as terminators: Swift writes them
/// on the declaration's own line far more often than Java does, and
/// `@objc public func run()` states `public` while `@State private var count`
/// states `private`.
fn swift_declared_visibility(signature: &str) -> Option<Visibility> {
    strip_leading_decorations(signature)
        .split(' ')
        .take_while(|word| SWIFT_MODIFIERS.contains(word))
        .find_map(|word| match word {
            "public" | "open" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            "internal" => Some(Visibility::Internal),
            "fileprivate" => Some(Visibility::FilePrivate),
            "package" => Some(Visibility::Package),
            _ => None,
        })
}

/// Swift's default is `internal` — visible inside the module and nowhere else.
/// Reported as `Visibility::Internal` and not `Public`, because unlike PHP the
/// language's unwritten default really is narrower than public.
fn swift_visibility(_name: &str) -> Visibility {
    Visibility::Internal
}

fn swift_refine(def: &mut Def) {
    if let Some(visibility) = swift_declared_visibility(&def.signature) {
        def.visibility = visibility;
    }
    if def.name == "init" && def.kind == DefKind::Method {
        def.kind = DefKind::Constructor;
    }
}

pub(crate) static SWIFT: LanguageSpec = LanguageSpec {
    lang: Lang::Swift,
    language: tree_sitter_swift::LANGUAGE,
    tags_query: tree_sitter_swift::TAGS_QUERY,
    locals_query: "",
    kinds: SWIFT_KINDS,
    reference_kinds: &[],
    imports: Some(&SWIFT_IMPORTS),
    visibility: swift_visibility,
    test_attribute: never_a_test_signal,
    test_scope: never_a_test_signal,
    refine: swift_refine,
    doc_is_leading_body_string: false,
    line_comment_prefixes: &["//"],
    config: OnceLock::new(),
};

static SWIFT_IMPORTS: ImportSpec = ImportSpec {
    query: include_str!("queries/swift-imports.scm"),
    kinds: &[("import", ImportKind::Import)],
    markers: &["import"],
    callee_names: &[],
    compiled: OnceLock::new(),
};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn line_index_matches_newline_and_eof_boundaries() {
        let source = "one\ntwo\n";
        let lines = LineIndex::new(source.as_bytes()).unwrap();
        assert_eq!(lines.line_for(0).unwrap(), 1);
        assert_eq!(lines.line_for(3).unwrap(), 1);
        assert_eq!(lines.line_for(4).unwrap(), 2);
        assert_eq!(lines.line_for(source.len()).unwrap(), 3);
        assert_eq!(lines.line_start(0), 0);
        assert_eq!(lines.line_start(3), 0);
        assert_eq!(lines.line_start(5), 4);
        assert_eq!(
            span_for_range(&(0..4), &lines, source).unwrap().end_line(),
            1
        );
        assert_eq!(
            span_for_range(&(0..5), &lines, source).unwrap().end_line(),
            2
        );
        assert_eq!(
            span_for_range(&(0..source.len()), &lines, source)
                .unwrap()
                .end_line(),
            2
        );

        let empty = LineIndex::new(b"").unwrap();
        let span = span_for_range(&(0..0), &empty, "").unwrap();
        assert_eq!((span.start_line(), span.end_line()), (1, 1));
    }

    #[test]
    fn an_imports_spec_with_a_callee_and_no_callee_names_is_rejected() {
        static IMPORTS: ImportSpec = ImportSpec {
            query: "(call_expression function: (identifier) @import.callee arguments: (arguments . (string) @import.path)) @import.require",
            kinds: &[("require", ImportKind::Require)],
            markers: &["require"],
            callee_names: &[],
            compiled: OnceLock::new(),
        };
        static SPEC: LanguageSpec = LanguageSpec {
            lang: Lang::JavaScript,
            language: tree_sitter_javascript::LANGUAGE,
            tags_query: tree_sitter_javascript::TAGS_QUERY,
            locals_query: "",
            kinds: JAVASCRIPT_KINDS,
            reference_kinds: JAVASCRIPT_REFERENCE_KINDS,
            imports: Some(&IMPORTS),
            visibility: javascript_visibility,
            test_attribute: never_a_test_signal,
            test_scope: never_a_test_signal,
            refine: keep_as_captured,
            doc_is_leading_body_string: false,
            line_comment_prefixes: &["//"],
            config: OnceLock::new(),
        };
        let Err(error) = TagsExtractor::new(&SPEC) else {
            panic!("an empty callee_names list was accepted");
        };
        assert!(error.contains("names no callee"), "{error}");
    }

    #[test]
    fn ruby_records_require_relative() {
        let mut extractor = TagsExtractor::new(&RUBY).unwrap();
        let facts = extractor
            .extract(b"require_relative '../lib/thing'\n")
            .unwrap();
        assert_eq!(facts.imports().len(), 1);
        assert_eq!(facts.imports()[0].path, "../lib/thing");
        assert_eq!(facts.imports()[0].kind, ImportKind::Require);
    }

    #[test]
    fn ruby_records_initialize_as_the_constructor() {
        let mut extractor = TagsExtractor::new(&RUBY).unwrap();
        let facts = extractor
            .extract(b"class Service\n  def initialize(name)\n    @name = name\n  end\nend\n")
            .unwrap();
        let initialize = facts
            .defs()
            .iter()
            .find(|def| def.name == "initialize")
            .expect("initialize was not extracted");
        assert_eq!(initialize.kind, DefKind::Constructor);
    }

    #[test]
    fn a_decoration_run_is_skipped_whatever_it_holds() {
        assert_eq!(
            strip_leading_decorations("public void run();"),
            "public void run();"
        );
        assert_eq!(
            strip_leading_decorations("@Override public void run()"),
            "public void run()"
        );
        assert_eq!(
            strip_leading_decorations("@RequestMapping(value = \"/x\") public void run()"),
            "public void run()"
        );
        assert_eq!(
            strip_leading_decorations("#[Route('/x')] public function run()"),
            "public function run()"
        );
        assert_eq!(
            strip_leading_decorations("@objc @MainActor public func run()"),
            "public func run()"
        );
        assert_eq!(strip_leading_decorations("@Override(value"), "");
        assert_eq!(strip_leading_decorations("@"), "");
    }

    #[test]
    fn an_annotated_java_member_still_states_its_access() {
        assert_eq!(
            java_declared_visibility("@Override public void run() {"),
            Some(Visibility::Public)
        );
        assert_eq!(
            java_declared_visibility("@RequestMapping(value = \"/x\") private void run() {"),
            Some(Visibility::Private)
        );
        assert_eq!(java_declared_visibility("void run() {"), None);
    }

    #[test]
    fn a_bodyless_java_member_with_no_modifier_is_public() {
        assert!(java_states_no_modifier_and_is_public("void run();"));
        assert!(java_states_no_modifier_and_is_public(
            "@Override java.util.List<String> names();"
        ));
        assert!(!java_states_no_modifier_and_is_public(
            "abstract void run();"
        ));
        assert!(!java_states_no_modifier_and_is_public("native void run();"));
        assert!(!java_states_no_modifier_and_is_public("void run() {}"));
        assert!(!java_states_no_modifier_and_is_public("void run()"));
    }

    #[test]
    fn an_attributed_swift_declaration_still_states_its_access() {
        assert_eq!(
            swift_declared_visibility("@objc public func run() {"),
            Some(Visibility::Public)
        );
        assert_eq!(
            swift_declared_visibility("@State private var count: Int"),
            Some(Visibility::Private)
        );
        assert_eq!(swift_declared_visibility("func run() {"), None);
    }

    #[test]
    fn swift_fileprivate_is_not_the_module_wide_default() {
        assert_eq!(
            swift_declared_visibility("fileprivate var shared: Int"),
            Some(Visibility::FilePrivate)
        );
        assert_eq!(
            swift_declared_visibility("internal var shared: Int"),
            Some(Visibility::Internal)
        );
        assert_eq!(swift_visibility("shared"), Visibility::Internal);
    }

    #[test]
    fn the_java_query_reaches_what_upstream_leaves_out() {
        let mut extractor = TagsExtractor::new(&JAVA).unwrap();
        let facts = extractor
            .extract(
                b"public class Service {\n    public Service() {}\n}\n\
                  enum Mode { FAST }\n\
                  public record Point(int x, int y) {}\n",
            )
            .unwrap();
        let kind = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .map(|def| def.kind)
        };
        assert_eq!(kind("Service"), Some(DefKind::Class));
        assert_eq!(
            facts
                .defs()
                .iter()
                .filter(|def| def.name == "Service")
                .count(),
            2
        );
        assert!(facts
            .defs()
            .iter()
            .any(|def| def.name == "Service" && def.kind == DefKind::Constructor));
        assert_eq!(kind("Mode"), Some(DefKind::Enum));
        assert_eq!(kind("Point"), Some(DefKind::Struct));
    }

    #[test]
    fn a_decorated_typescript_member_still_states_its_access() {
        assert_eq!(
            declared_visibility("@Input() private name: string;"),
            Some(Visibility::Private)
        );
        assert_eq!(declared_visibility("name: string;"), None);
    }

    #[test]
    fn an_attributed_php_member_still_states_its_access() {
        assert_eq!(
            php_declared_visibility("#[Route('/x')] public function run(): void"),
            Some(Visibility::Public)
        );
        assert_eq!(php_declared_visibility("function run(): void"), None);
    }

    #[test]
    fn a_php_declaration_without_a_body_is_a_member() {
        assert!(php_has_no_body("function run(): void;"));
        assert!(!php_opens_with_modifier("function run(): void;"));
        assert!(!php_has_no_body("function bare(string $name): string"));
        assert!(!php_has_no_body("function bare(): string { return 'x'; }"));
        assert!(php_opens_with_modifier("abstract function run();"));
        assert!(php_opens_with_modifier("static function make(): self"));
        assert!(!php_opens_with_modifier("function bare(): string"));
    }

    #[test]
    fn a_spaced_include_directive_is_still_an_include() {
        let mut extractor = TagsExtractor::new(&C).unwrap();
        let facts = extractor.extract(b"#  include <stdio.h>\n").unwrap();
        assert_eq!(facts.imports().len(), 1);
        assert_eq!(facts.imports()[0].path, "<stdio.h>");
        assert_eq!(facts.imports()[0].kind, ImportKind::Include);
    }

    #[test]
    fn a_grammar_without_a_usable_tags_query_falls_back_to_lexical() {
        static ILLEGAL: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(module) @illegal",
            locals_query: "",
            kinds: &[],
            reference_kinds: &[],
            imports: None,
            visibility: python_visibility,
            test_attribute: python_test_attribute,
            test_scope: python_test_scope,
            refine: python_refine,
            doc_is_leading_body_string: true,
            line_comment_prefixes: &["#"],
            config: OnceLock::new(),
        };
        static INCOMPLETE: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: "(module) @definition.module",
            locals_query: "",
            kinds: &[],
            reference_kinds: &[],
            imports: None,
            visibility: python_visibility,
            test_attribute: python_test_attribute,
            test_scope: python_test_scope,
            refine: python_refine,
            doc_is_leading_body_string: true,
            line_comment_prefixes: &["#"],
            config: OnceLock::new(),
        };
        assert!(TagsExtractor::new(&ILLEGAL).is_err());
        assert!(TagsExtractor::new(&INCOMPLETE).is_err());
    }

    #[test]
    fn python_tags_produce_nested_defs_and_references() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    value = 1\n    def run(self):\n        result = helper()\n        def nested():\n            return hidden()\n        return result\n",
            )
            .unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags {
                parse_errors: false
            }
        ));
        let method = facts.defs().iter().find(|def| def.name == "run").unwrap();
        assert_eq!(method.local_qualified.as_deref(), Some("Service.run"));
        assert!(method.body_idents.iter().any(|ident| ident == "helper"));
        assert!(!method.body_idents.iter().any(|ident| ident == "hidden"));
        assert!(facts
            .references()
            .iter()
            .any(|reference| reference.name == "helper"));
    }

    #[test]
    fn tags_docs_are_scanned_into_doc_idents_and_out_of_body_idents() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"def documented():\n    \"\"\"Prose mentions widget.\"\"\"\n    return helper()\n",
            )
            .unwrap();
        let documented = facts.defs().first().unwrap();
        assert!(documented.doc_idents.iter().any(|ident| ident == "widget"));
        assert!(documented.body_idents.iter().any(|ident| ident == "helper"));
        assert!(!documented.body_idents.iter().any(|ident| ident == "widget"));
    }

    #[test]
    fn a_multi_line_header_is_one_signature_and_not_body() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(b"def configure(\n    host,\n    port,\n):\n    return host\n")
            .unwrap();
        let configure = facts.defs().first().unwrap();
        assert_eq!(configure.signature, "def configure( host, port, ):");
        assert!(configure
            .signature_idents
            .iter()
            .any(|ident| ident == "host"));
        assert!(configure
            .signature_idents
            .iter()
            .any(|ident| ident == "port"));
        assert!(!configure.body_idents.iter().any(|ident| ident == "port"));
    }

    #[test]
    fn decorators_are_attributes_of_the_decorated_definition() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    @staticmethod\n    def run(value):\n        return value\n",
            )
            .unwrap();
        let run = facts.defs().iter().find(|def| def.name == "run").unwrap();
        assert!(run
            .attribute_idents
            .iter()
            .any(|ident| ident == "staticmethod"));
        assert_eq!(run.signature, "def run(value):");
        assert!(!run.body_idents.iter().any(|ident| ident == "staticmethod"));
        let service = facts
            .defs()
            .iter()
            .find(|def| def.name == "Service")
            .unwrap();
        assert!(!service
            .body_idents
            .iter()
            .any(|ident| ident == "staticmethod"));
    }

    #[test]
    fn python_names_carry_their_conventional_visibility() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"class Service:\n    def __init__(self):\n        pass\n    def __mangled(self):\n        pass\n    def _hidden(self):\n        pass\n    def run(self):\n        pass\n",
            )
            .unwrap();
        let visibility_of = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .visibility
                .clone()
        };
        assert_eq!(visibility_of("__init__"), Visibility::Public);
        assert_eq!(visibility_of("__mangled"), Visibility::Private);
        assert_eq!(visibility_of("_hidden"), Visibility::Internal);
        assert_eq!(visibility_of("run"), Visibility::Public);
    }

    #[test]
    fn python_test_conventions_set_test_signals() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor
            .extract(
                b"import pytest\n\nclass TestService:\n    def test_run(self):\n        pass\n\n@pytest.fixture\ndef service():\n    return None\n\ndef plain():\n    pass\n",
            )
            .unwrap();
        let signals_of = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .test_signals
        };
        assert!(signals_of("test_run").inside_test_scope);
        assert!(signals_of("service").explicit_attribute);
        assert!(!signals_of("plain").any());
    }

    #[test]
    fn malformed_python_is_tags_recovered_not_complete() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor.extract(b"def broken(:\n    pass\n").unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags { parse_errors: true }
        ));
    }

    #[test]
    fn invalid_utf8_degrades_lexically() {
        let mut extractor = TagsExtractor::new(&PYTHON).unwrap();
        let facts = extractor.extract(b"def bad(\xff").unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Degraded {
                reason: DegradedReason::InvalidUtf8,
                ..
            }
        ));
    }

    fn typescript(source: &str) -> Facts {
        TagsExtractor::new(&TYPESCRIPT)
            .unwrap()
            .extract(source.as_bytes())
            .unwrap()
    }

    fn python(source: &str) -> Facts {
        TagsExtractor::new(&PYTHON)
            .unwrap()
            .extract(source.as_bytes())
            .unwrap()
    }

    #[test]
    fn a_containers_body_idents_do_not_absorb_member_doc_prose() {
        let facts = typescript(
            "/** Service docs mention container. */\nexport class Service {\n  /** Mentions widget and frobnicate. */\n  run(): void {\n    helper();\n  }\n}\n",
        );
        let service = facts
            .defs()
            .iter()
            .find(|def| def.name == "Service")
            .unwrap();
        let run = facts.defs().iter().find(|def| def.name == "run").unwrap();
        assert!(run.doc_idents.iter().any(|ident| ident == "widget"));
        assert!(run.doc_idents.iter().any(|ident| ident == "frobnicate"));
        assert!(run.span.start_byte() < run.signature_span.start_byte());
        assert!(!service.body_idents.iter().any(|ident| ident == "widget"));
        assert!(!service
            .body_idents
            .iter()
            .any(|ident| ident == "frobnicate"));
        assert!(!service.body_idents.iter().any(|ident| ident == "helper"));
        assert!(run.body_idents.iter().any(|ident| ident == "helper"));
        assert!(service.doc_idents.iter().any(|ident| ident == "container"));
        assert!(!service.body_idents.iter().any(|ident| ident == "container"));
    }

    #[test]
    fn preceding_comment_run_absorbs_stacked_line_and_block_comments() {
        let source = "/* block about alpha */\n// line about beta\nfunction target(): void {}\n";
        let decl = source.find("function").unwrap();
        let start = preceding_comment_run_start(source, decl, &["//"]).unwrap();
        assert_eq!(
            &source[start..decl],
            "/* block about alpha */\n// line about beta\n"
        );

        let gapped = "/* orphan */\n\nfunction other(): void {}\n";
        assert_eq!(
            preceding_comment_run_start(gapped, gapped.find("function").unwrap(), &["//"]),
            None,
            "a blank line breaks adjacency"
        );
        let private_field = "    #attempts = 0;\n    private secret: string;\n";
        let secret_at = private_field.find("private").unwrap();
        assert_eq!(
            preceding_comment_run_start(private_field, secret_at, &["//"]),
            None,
            "private fields must not be absorbed as comments"
        );
        let py = "# note about widget\ndef run():\n    pass\n";
        let def_at = py.find("def").unwrap();
        let start = preceding_comment_run_start(py, def_at, &["#"]).unwrap();
        assert_eq!(&py[start..def_at], "# note about widget\n");
    }

    fn kind_of(facts: &Facts, name: &str) -> String {
        facts
            .defs()
            .iter()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("no definition named {name}"))
            .kind
            .to_string()
    }

    /// The three member modifiers no capture can reach, because they are
    /// anonymous tokens the grammar does not give a field to. `refine` reads
    /// them back off the signature text, and only off its leading words — a
    /// `private` in a parameter list or a type is not this class's business.
    #[test]
    fn typescript_member_visibility_comes_off_the_leading_modifiers() {
        let facts = typescript(
            "class C {\n    private a: string;\n    protected b: string;\n    public c: string;\n    d: string;\n    e = (private_thing: string) => 1;\n}\n",
        );
        let visibility_of = |name: &str| {
            &facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .visibility
        };
        assert_eq!(visibility_of("a"), &Visibility::Private);
        assert_eq!(visibility_of("b"), &Visibility::Protected);
        assert_eq!(visibility_of("c"), &Visibility::Public);
        assert_eq!(visibility_of("d"), &Visibility::Public);
        assert_eq!(visibility_of("e"), &Visibility::Public);
    }

    /// A `#` name is private to the runtime rather than to the type checker,
    /// and it is the one visibility TypeScript spells in the name itself.
    #[test]
    fn a_typescript_hash_name_is_private_without_a_modifier() {
        let facts = typescript("class C {\n    #hidden = 0;\n    shown = 0;\n}\n");
        let hidden = facts
            .defs()
            .iter()
            .find(|def| def.name == "#hidden")
            .unwrap();
        assert_eq!(hidden.visibility, Visibility::Private);
        assert!(!hidden
            .signature_idents
            .iter()
            .any(|ident| ident == "hidden"));
        assert_eq!(
            facts
                .defs()
                .iter()
                .find(|def| def.name == "shown")
                .unwrap()
                .visibility,
            Visibility::Public
        );
    }

    /// `constructor` and the accessors are `method_definition` nodes like any
    /// other, so the grammar cannot tell them apart and `refine` must.
    #[test]
    fn typescript_constructors_and_accessors_are_not_ordinary_methods() {
        let facts = typescript(
            "class C {\n    constructor() {}\n    get size(): number { return 0; }\n    set size(value: number) {}\n    resize(): void {}\n}\n",
        );
        assert_eq!(kind_of(&facts, "constructor"), "constructor");
        assert_eq!(kind_of(&facts, "resize"), "method");
        let sizes: Vec<_> = facts
            .defs()
            .iter()
            .filter(|def| def.name == "size")
            .map(|def| def.kind.to_string())
            .collect();
        assert_eq!(sizes, vec!["property", "property"]);
    }

    /// An arrow or `function` initializer makes a binding a function; anything
    /// else leaves it the variable it was written as. The signature ends at the
    /// first line break, so an initializer that starts on the next line is not
    /// evidence of anything and is not treated as if it were.
    #[test]
    fn a_typescript_binding_is_refined_only_on_what_its_first_line_shows() {
        let facts = typescript(
            "const arrow = (a: string) => a;\nconst generic = <T,>(a: T): T => a;\nconst plain = function (a: string) { return a; };\nconst value = 3;\nconst wrapped = compute(() => 1);\nconst split =\n    (a: string) => a;\n",
        );
        assert_eq!(kind_of(&facts, "arrow"), "function");
        assert_eq!(kind_of(&facts, "generic"), "function");
        assert_eq!(kind_of(&facts, "plain"), "function");
        assert_eq!(kind_of(&facts, "value"), "variable");
        assert_eq!(kind_of(&facts, "wrapped"), "variable");
        assert_eq!(kind_of(&facts, "split"), "variable");
    }

    /// The two shapes the first `=` and the first token get wrong on their own:
    /// an annotation that spells a function type *before* the assignment, and a
    /// `<` that opens an element rather than a type parameter list.
    #[test]
    fn a_typescript_initializer_is_read_past_its_annotation_and_its_element() {
        let facts = typescript(
            "const annotated: (x: number) => void = (x) => {};\nconst asserted = <Foo>bar.map((y) => y);\nconst nested = <T extends Map<string, number>,>(a: T): T => a;\nconst compared = left >= right;\n",
        );
        assert_eq!(kind_of(&facts, "annotated"), "function");
        assert_eq!(kind_of(&facts, "asserted"), "variable");
        assert_eq!(kind_of(&facts, "nested"), "function");
        assert_eq!(kind_of(&facts, "compared"), "variable");
        let mut tsx = TagsExtractor::new(&TSX).unwrap();
        let facts = tsx
            .extract(b"const element = <Badge onClick={() => go()} />;\n")
            .unwrap();
        assert_eq!(kind_of(&facts, "element"), "variable");
    }

    /// Nesting is spelled with the separator the language uses, which is the
    /// whole reason [`Lang::qualified_separator`] exists.
    #[test]
    fn typescript_nesting_is_qualified_with_a_dot() {
        let facts = typescript(
            "namespace Outer {\n    export class Inner {\n        run(): void {}\n    }\n}\n",
        );
        let qualified = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .local_qualified
                .clone()
        };
        assert_eq!(qualified("Inner").as_deref(), Some("Outer.Inner"));
        assert_eq!(qualified("run").as_deref(), Some("Outer.Inner.run"));
    }

    /// A local `const` is not API, and the surface rr publishes would be noise
    /// if every temporary inside every function body landed in it.
    #[test]
    fn a_typescript_binding_inside_a_function_body_is_not_a_definition() {
        let facts = typescript(
            "const exported = 1;\nfunction run(): number {\n    const local = 2;\n    return local + exported;\n}\n",
        );
        assert!(facts.defs().iter().any(|def| def.name == "exported"));
        assert!(!facts.defs().iter().any(|def| def.name == "local"));
    }

    /// A parameter property declares a field from inside the parameter list,
    /// which is the one construct rr indexes whose lexical parent is not its
    /// owner. Both halves are asserted here: that the field is found at all,
    /// and that it is named `Service.repo` rather than the
    /// `Service.constructor.repo` containment alone would give it.
    #[test]
    fn a_typescript_parameter_property_is_a_field_of_the_class_not_the_constructor() {
        let facts = typescript(
            "class Service {\n    constructor(private readonly repo: Repo, public label: string, protected retries = 0, readonly opened?: Date, plain: string, fallbackish: X = defaulted) {}\n}\n",
        );
        let field = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("{name} was not extracted"))
        };

        for name in ["repo", "label", "retries", "opened"] {
            assert_eq!(kind_of(&facts, name), "field");
            assert_eq!(
                field(name).local_qualified.as_deref(),
                Some(format!("Service.{name}").as_str()),
                "{name} was not named as a member of its class"
            );
        }
        assert!(!facts.defs().iter().any(|def| def.name == "plain"));
        assert!(!facts.defs().iter().any(|def| def.name == "fallbackish"));
        assert!(!facts.defs().iter().any(|def| def.name == "defaulted"));
        assert_eq!(field("repo").visibility, Visibility::Private);
        assert_eq!(field("label").visibility, Visibility::Public);
        assert_eq!(field("retries").visibility, Visibility::Protected);
        assert_eq!(field("opened").visibility, Visibility::Public);
    }

    /// The hoist is keyed on the constructor, so an ordinary nested definition
    /// must be unaffected by it — a class declared inside a method is still
    /// named through that method.
    #[test]
    fn only_a_parameter_property_is_lifted_out_of_the_member_it_sits_in() {
        let facts = typescript(
            "class Outer {\n    build() {\n        class Inner {\n            held = 1;\n        }\n        return Inner;\n    }\n}\n",
        );
        let qualified = |name: &str| {
            facts
                .defs()
                .iter()
                .find(|def| def.name == name)
                .unwrap()
                .local_qualified
                .clone()
        };
        assert_eq!(qualified("build").as_deref(), Some("Outer.build"));
        assert_eq!(qualified("Inner").as_deref(), Some("Outer.build.Inner"));
        assert_eq!(
            qualified("held").as_deref(),
            Some("Outer.build.Inner.held"),
            "a class-body field was hoisted out of its class"
        );
    }

    /// A receiver-typed call is one rr cannot resolve without knowing the
    /// receiver's type, so it is recorded as what it is and left for the index
    /// to decline rather than guessed at here.
    #[test]
    fn a_typescript_call_through_a_receiver_is_a_method_reference() {
        let facts = typescript("function run(): void {\n    plain();\n    obj.member();\n}\n");
        let kind_of_reference = |name: &str| {
            &facts
                .references()
                .iter()
                .find(|reference| reference.name == name)
                .unwrap()
                .kind
        };
        assert_eq!(kind_of_reference("plain"), &ReferenceKind::Call);
        assert_eq!(kind_of_reference("member"), &ReferenceKind::MethodCall);
    }

    /// A receiver call is one rr cannot resolve without knowing the receiver's
    /// type. Python used to record it as a plain call and let the index bind it to
    /// whatever free function shared the name; this is that claim withdrawn.
    #[test]
    fn a_python_call_through_a_receiver_is_a_method_reference() {
        let facts = python("def run(obj):\n    plain()\n    obj.method()\n    a.b.c()\n");
        let kind_of_reference = |name: &str| {
            &facts
                .references()
                .iter()
                .find(|reference| reference.name == name)
                .unwrap()
                .kind
        };
        assert_eq!(kind_of_reference("plain"), &ReferenceKind::Call);
        assert_eq!(kind_of_reference("method"), &ReferenceKind::MethodCall);
        assert_eq!(kind_of_reference("c"), &ReferenceKind::MethodCall);
    }

    #[test]
    fn a_python_plain_call_stays_a_call() {
        let facts = python("def run():\n    helper(value)\n");
        let helper = facts
            .references()
            .iter()
            .find(|reference| reference.name == "helper")
            .unwrap();
        assert_eq!(helper.kind, ReferenceKind::Call);
    }

    #[test]
    fn a_python_decorator_call_through_a_receiver_is_a_method_reference() {
        let facts = python("@app.route(\"/x\")\ndef handler():\n    pass\n");
        let route = facts
            .references()
            .iter()
            .find(|reference| reference.name == "route")
            .unwrap();
        assert_eq!(route.kind, ReferenceKind::MethodCall);
    }

    #[test]
    fn a_python_receiver_call_records_the_member_not_the_receiver() {
        let facts = python("def run(obj):\n    obj.method()\n");
        assert!(facts
            .references()
            .iter()
            .any(|reference| reference.name == "method"));
        assert!(!facts
            .references()
            .iter()
            .any(|reference| reference.name == "obj"));
    }

    #[test]
    fn a_python_property_is_a_property() {
        let facts = python("class C:\n    @property\n    def size(self):\n        return 0\n");
        assert_eq!(kind_of(&facts, "size"), "property");
    }

    #[test]
    fn a_python_property_accessor_is_a_property() {
        let facts = python("class C:\n    @size.setter\n    def size(self, v):\n        pass\n");
        assert_eq!(kind_of(&facts, "size"), "property");
    }

    #[test]
    fn a_python_non_property_decorator_leaves_the_kind_alone() {
        let facts = python("class C:\n    @staticmethod\n    def run():\n        pass\n");
        assert_eq!(kind_of(&facts, "run"), "function");
    }

    #[test]
    fn a_python_class_is_not_refined_into_a_property() {
        let facts = python("@property\nclass C:\n    pass\n");
        assert_eq!(kind_of(&facts, "C"), "class");
    }

    #[test]
    fn a_module_level_property_is_recorded_as_one_and_that_is_written_down() {
        let facts = python("@property\ndef size():\n    return 0\n");
        assert_eq!(kind_of(&facts, "size"), "property");
    }

    /// `DefKind::Property` was documented as "a TypeScript getter or setter, a
    /// Python `@property`" long before Python produced one. This is that sentence
    /// made checkable.
    #[test]
    fn every_language_the_property_doc_names_produces_one() {
        let python_facts =
            python("class C:\n    @property\n    def size(self):\n        return 0\n");
        let typescript_facts = typescript("class C {\n    get size(): number { return 0; }\n}\n");
        assert!(python_facts
            .defs()
            .iter()
            .any(|def| def.kind == DefKind::Property));
        assert!(typescript_facts
            .defs()
            .iter()
            .any(|def| def.kind == DefKind::Property));
    }

    /// TSX is a second grammar rather than a second file extension, and the
    /// shared query has to compile against both. A `TagsConfiguration` owns the
    /// grammar it was built with, so this is also the test that would fail if
    /// the two specs ever came to share one.
    #[test]
    fn the_shared_query_compiles_against_both_typescript_grammars() {
        let source = "export const Badge = (props: P) => <b>{props.label}</b>;\n";
        let mut tsx = TagsExtractor::new(&TSX).unwrap();
        let facts = tsx.extract(source.as_bytes()).unwrap();
        assert!(matches!(
            facts.status(),
            ParseStatus::Tags {
                parse_errors: false
            }
        ));
        assert_eq!(kind_of(&facts, "Badge"), "function");
        assert!(matches!(
            typescript(source).status(),
            ParseStatus::Tags { parse_errors: true }
        ));
    }

    /// `import os.path` spells one path that already ends in its leaf, so the
    /// whole path is `path` and `name` stays `None` — the same convention Rust
    /// `use` follows.
    #[test]
    fn a_python_import_records_the_module_verbatim() {
        let facts = python("import os.path\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::Import);
        assert_eq!(imports[0].path, "os.path");
        assert!(imports[0].name.is_none());
        assert!(imports[0].alias.is_none());
        assert!(!imports[0].is_glob);
        assert!(!imports[0].is_public);
    }

    #[test]
    fn a_python_aliased_import_records_the_local_name() {
        let facts = python("import os.path as p\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, "os.path");
        assert_eq!(imports[0].alias.as_deref(), Some("p"));
    }

    #[test]
    fn a_python_from_import_separates_source_from_leaf() {
        let facts = python("from x import y\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].path, "x");
        assert_eq!(imports[0].name.as_deref(), Some("y"));
        assert!(imports[0].alias.is_none());
    }

    /// `..pkg` is relative to the importing module's package, which rr does
    /// not resolve. The specifier is stored verbatim, and the predicate agrees
    /// that it is not a path this index's resolver can follow.
    #[test]
    fn a_python_relative_import_is_not_presented_as_canonical() {
        let facts = python("from ..pkg import y\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, "..pkg");
        assert_eq!(imports[0].name.as_deref(), Some("y"));
        assert!(!imports[0].kind.resolves_by_path());
    }

    #[test]
    fn a_python_star_import_is_a_glob() {
        let facts = python("from x import *\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].path, "x");
        assert!(imports[0].name.is_none());
        assert!(imports[0].is_glob);
    }

    /// `from __future__ import annotations` enables a compiler flag rather
    /// than naming a module rr could index, and its statement has no node
    /// whose text is `__future__` to point `@import.path` at. Recorded as a
    /// decision in the query header; pinned here so a grammar change cannot
    /// silently start extracting it.
    #[test]
    fn a_python_future_import_is_not_recorded() {
        let facts = python("from __future__ import annotations\n");
        assert!(facts.imports().is_empty());
    }

    #[test]
    fn a_typescript_default_import_is_an_import_kind() {
        let facts = typescript("import x from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::Import);
        assert_eq!(imports[0].path, "m");
        assert!(imports[0].name.is_none());
        assert_eq!(imports[0].alias.as_deref(), Some("x"));
        assert!(!imports[0].is_glob);
    }

    #[test]
    fn a_typescript_named_import_is_a_from_kind() {
        let facts = typescript("import { A } from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].path, "m");
        assert_eq!(imports[0].name.as_deref(), Some("A"));
        assert!(imports[0].alias.is_none());
    }

    /// Without the `!alias` negation the plain pattern also matches `{ A as B }`
    /// and the declaration lands twice.
    #[test]
    fn a_typescript_aliased_specifier_is_recorded_once() {
        let facts = typescript("import { A as B } from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].name.as_deref(), Some("A"));
        assert_eq!(imports[0].alias.as_deref(), Some("B"));
    }

    /// D5: `import * as ns from "m"` binds exactly one name; `is_glob` marks a
    /// declaration that brings in names it does not write down, and this one
    /// writes its name down.
    #[test]
    fn a_typescript_namespace_import_is_not_a_glob() {
        let facts = typescript("import * as ns from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::Import);
        assert_eq!(imports[0].path, "m");
        assert_eq!(imports[0].alias.as_deref(), Some("ns"));
        assert!(!imports[0].is_glob);
    }

    /// The `.` anchor is what separates a bare import from every clause form:
    /// a bare import's first named child is its source. One match, no alias.
    #[test]
    fn a_typescript_side_effect_import_is_recorded_once() {
        let bare = typescript("import \"m\";\n");
        assert_eq!(bare.imports().len(), 1);
        assert_eq!(bare.imports()[0].kind, ImportKind::Import);
        assert_eq!(bare.imports()[0].path, "m");
        assert!(bare.imports()[0].alias.is_none());

        let clause = typescript("import x from \"m\";\n");
        assert_eq!(clause.imports().len(), 1);
        assert_eq!(clause.imports()[0].alias.as_deref(), Some("x"));
    }

    /// `export default "hi"` opens with a string too, under the `value` field;
    /// the `source:` field requirement is what keeps it out of the `export *`
    /// pattern.
    #[test]
    fn an_export_default_string_is_not_an_import() {
        let facts = typescript("export default \"hi\";\n");
        assert!(facts.imports().is_empty());
    }

    /// D3: `require` is an ordinary identifier, and only a call to the *global*
    /// The query matches every call taking one string; the callee name is read
    /// in Rust, not through a query predicate, and is what drops the rest.
    #[test]
    fn a_typescript_require_is_matched_by_its_callee_name() {
        let facts = typescript("const cj = require(\"m\");\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::Require);
        assert_eq!(imports[0].path, "m");
        assert!(imports[0].alias.is_none());
        assert!(imports[0].owner.is_none());
        assert!(typescript("describe(\"a case\");\n").imports().is_empty());
        assert!(typescript("thing.require(\"m\");\n").imports().is_empty());
        assert_eq!(
            typescript("function require(p: string) { return p; }\nrequire(\"./shim\");\n")
                .imports()
                .len(),
            1
        );
    }

    #[test]
    fn a_typescript_re_export_is_public() {
        let facts = typescript("export { A } from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].name.as_deref(), Some("A"));
        assert!(imports[0].is_public);
    }

    #[test]
    fn a_typescript_star_re_export_is_a_glob() {
        let facts = typescript("export * from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].path, "m");
        assert!(imports[0].name.is_none());
        assert!(imports[0].is_glob);
        assert!(imports[0].is_public);
    }

    /// One declaration, two facts: the default clause has no leaf, the named
    /// clause does, and each anchor keeps its own span so their keys differ.
    #[test]
    fn a_typescript_mixed_default_and_named_import_is_two_facts() {
        let facts = typescript("import x, { A } from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 2);
        let by_kind = |kind: ImportKind| {
            imports
                .iter()
                .find(|import| import.kind == kind)
                .unwrap_or_else(|| panic!("no {kind:?} import"))
        };
        let plain = by_kind(ImportKind::Import);
        assert_eq!(plain.path, "m");
        assert_eq!(plain.alias.as_deref(), Some("x"));
        let named = by_kind(ImportKind::From);
        assert_eq!(named.path, "m");
        assert_eq!(named.name.as_deref(), Some("A"));
    }

    /// Fact order is the sort key's order, not the source's, and every import
    /// carries the smallest enclosing definition when one exists.
    #[test]
    fn imports_are_sorted_and_owned_by_the_nearest_definition() {
        let facts = typescript(
            "import top from \"m-top\";\nnamespace N {\n    import ir = require(\"m-nested\");\n}\n",
        );
        let imports = facts.imports();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "m-top");
        assert!(imports[0].owner.is_none());
        assert_eq!(imports[1].path, "m-nested");
        assert_eq!(
            imports[1]
                .owner
                .and_then(|id| facts.def(id))
                .map(|def| def.name.as_str()),
            Some("N")
        );
    }

    /// The imports query is compiled against both TypeScript grammars, and a
    /// `.tsx` file's imports arrive the same way a `.ts` file's do.
    #[test]
    fn the_imports_pass_runs_against_both_typescript_grammars() {
        let source =
            "import { A } from \"m\";\nexport const Badge = (props: P) => <b>{props.label}</b>;\n";
        let mut tsx = TagsExtractor::new(&TSX).unwrap();
        let facts = tsx.extract(source.as_bytes()).unwrap();
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert_eq!(imports[0].name.as_deref(), Some("A"));
    }

    /// The `None` arm: a language rr extracts no imports from reports none
    /// rather than failing.
    #[test]
    fn a_language_with_no_imports_query_extracts_no_imports() {
        static NO_IMPORTS: LanguageSpec = LanguageSpec {
            lang: Lang::Python,
            language: tree_sitter_python::LANGUAGE,
            tags_query: include_str!("queries/python.scm"),
            locals_query: "",
            kinds: &[
                ("constant", DefKind::Variable),
                ("class", DefKind::Class),
                ("function", DefKind::Function),
            ],
            reference_kinds: &[
                ("call", ReferenceKind::Call),
                ("method", ReferenceKind::MethodCall),
            ],
            imports: None,
            visibility: python_visibility,
            test_attribute: python_test_attribute,
            test_scope: python_test_scope,
            refine: python_refine,
            doc_is_leading_body_string: true,
            line_comment_prefixes: &["#"],
            config: OnceLock::new(),
        };
        let mut extractor = TagsExtractor::new(&NO_IMPORTS).unwrap();
        let facts = extractor
            .extract(b"import os\ndef f():\n    pass\n")
            .unwrap();
        assert!(facts.imports().is_empty());
        assert!(facts.defs().iter().any(|def| def.name == "f"));
    }

    /// The marker prefilter is a superset test: a file whose only "import" is
    /// a substring of an identifier passes the prefilter and still yields no
    /// imports, and a file carrying no marker at all is never parsed twice.
    #[test]
    fn an_import_free_file_is_not_parsed_twice() {
        assert!(python("result = imported_count + 1\n").imports().is_empty());
        assert!(python("x = 1\n").imports().is_empty());
        assert!(typescript("const harvested = 1;\n").imports().is_empty());
    }

    /// `export { A } from "m"` carries neither `import` nor `require`; `from`
    /// in the marker set is what keeps the prefilter from skipping it.
    #[test]
    fn a_re_export_only_file_is_not_skipped_by_the_prefilter() {
        let facts = typescript("export { A } from \"m\";\n");
        let imports = facts.imports();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].kind, ImportKind::From);
        assert!(imports[0].is_public);
    }
}
