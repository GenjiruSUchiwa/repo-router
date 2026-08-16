use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::facts::{Facts, ParseStatus, ReferenceKind};
use crate::lang::Lang;
use crate::lex::{append_source_terms, FieldTerm, InputKind, LexicalField, Lexicon};
use crate::oid::Oid;
use crate::path::RelPath;
use crate::ranking::CorpusStats;
use crate::{Error, Result};

use super::{
    ContentRepresentation, ExactRoute, FieldLengths, FieldPostings, FileId, FilePosting,
    FilePostingList, FileRecord, ImportRecord, PostingList, ReferenceRecord, Resolution, Snapshot,
    SnapshotMeta, StringId, SymbolId, SymbolPosting, SymbolRecord, TermId, TermRecord,
};

/// Bump whenever a snapshot built by this module gains, loses, or changes the
/// meaning of a field. Version 3 added the per-symbol display signature and the
/// per-file test classification — both because a text projection has to state
/// them, and neither can be re-derived later without opening a file the index
/// has already spoken for. Version 4 made qualification language-aware: the
/// module prefix strips the file's own extension and the segments join on the
/// language's separator, so a Python symbol is `service.Service.run` — a
/// spelling `rr query` accepts — rather than `service.py::Service.run`, which
/// no query can ever match.
pub const BUILD_VERSION: u32 = 4;

#[derive(Debug, Clone)]
pub struct FileInput {
    pub path: RelPath,
    pub oid: Oid,
    pub representation: ContentRepresentation,
    pub generated: bool,
    pub language: Lang,
    pub parse_status: ParseStatus,
    pub facts: Facts,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WorkerStats {
    pub clean_probes: u64,
    pub clean_blob_reads: u64,
    pub filtered_raw_reads: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_corrupt: u64,
    pub cache_write_failures: u64,
    pub parses: u64,
    pub complete: u64,
    pub recovered: u64,
    pub tags: u64,
    pub tags_recovered: u64,
    pub degraded: u64,
}

impl WorkerStats {
    pub fn add_assign(&mut self, other: Self) {
        self.clean_probes += other.clean_probes;
        self.clean_blob_reads += other.clean_blob_reads;
        self.filtered_raw_reads += other.filtered_raw_reads;
        self.cache_hits += other.cache_hits;
        self.cache_misses += other.cache_misses;
        self.cache_corrupt += other.cache_corrupt;
        self.cache_write_failures += other.cache_write_failures;
        self.parses += other.parses;
        self.complete += other.complete;
        self.recovered += other.recovered;
        self.tags += other.tags;
        self.tags_recovered += other.tags_recovered;
        self.degraded += other.degraded;
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BuildStats {
    pub files: u32,
    pub symbols: u32,
    pub references: u32,
    pub unresolved_refs: u32,
    pub ambiguous_refs: u32,
    pub imports: u32,
    pub unresolved_imports: u32,
    pub ambiguous_imports: u32,
    pub clean_probes: u64,
    pub clean_blob_reads: u64,
    pub filtered_raw_reads: u64,
    pub parses: u64,
    pub complete: u64,
    pub recovered: u64,
    pub tags: u64,
    pub tags_recovered: u64,
    pub degraded: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_corrupt: u64,
    pub cache_write_failures: u64,
    pub reparsed: u64,
    pub content_reads: u64,
}

#[derive(Debug)]
pub struct BuildReport {
    pub snapshot: Snapshot,
    pub stats: BuildStats,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexCounts {
    pub symbols: u32,
    pub references: u32,
    pub unresolved_refs: u32,
    pub ambiguous_refs: u32,
    pub imports: u32,
    pub unresolved_imports: u32,
    pub ambiguous_imports: u32,
}

struct StringInterner {
    values: Vec<String>,
    ids: HashMap<String, StringId>,
}

impl StringInterner {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            ids: HashMap::new(),
        }
    }

    fn intern(&mut self, value: &str) -> Result<StringId> {
        if let Some(id) = self.ids.get(value) {
            return Ok(*id);
        }
        let id = StringId::checked(self.values.len())?;
        let owned = value.to_owned();
        self.values.push(owned.clone());
        self.ids.insert(owned, id);
        Ok(id)
    }
}

pub struct SnapshotBuilder {
    meta: SnapshotMeta,
    strings: StringInterner,
    lexicon: Lexicon,
    files: Vec<FileRecord>,
    symbols: Vec<SymbolRecord>,
    references: Vec<ReferenceRecord>,
    imports: Vec<ImportRecord>,
    symbol_parent: Vec<Option<SymbolId>>,
    symbol_terms: Vec<[Vec<TermId>; LexicalField::COUNT]>,
    file_modules: Vec<Vec<String>>,
    file_path_terms: Vec<Vec<TermId>>,
    postings: FieldPostings,
    file_path_postings: Vec<FilePostingList>,
    exact_names: Vec<ExactRoute>,
    exact_qualified: Vec<ExactRoute>,
}

/// One import projected onto a file's symbols: owner, path, leaf name, alias.
///
/// The leaf sits beside the path because D2 keeps a specifier uncooked:
/// `from x import y` gives `("x", Some("y"))` and a whole-module import gives
/// `("os", None)`.
type FileImport = (Option<SymbolId>, String, Option<String>, Option<String>);

impl SnapshotBuilder {
    #[must_use]
    pub fn new(meta: SnapshotMeta) -> Self {
        Self {
            meta,
            strings: StringInterner::new(),
            lexicon: Lexicon::new(),
            files: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            imports: Vec::new(),
            symbol_parent: Vec::new(),
            symbol_terms: Vec::new(),
            file_modules: Vec::new(),
            file_path_terms: Vec::new(),
            postings: empty_postings(),
            file_path_postings: Vec::new(),
            exact_names: Vec::new(),
            exact_qualified: Vec::new(),
        }
    }

    /// Builds a validated deterministic snapshot from sorted or unsorted file inputs.
    ///
    /// # Errors
    /// Returns an error when ids, terms, ranges, or snapshot invariants overflow or fail.
    pub fn build(mut self, mut inputs: Vec<FileInput>) -> Result<(Snapshot, IndexCounts)> {
        inputs.sort_by(|left, right| left.path.cmp(&right.path));
        for input in &inputs {
            self.add_file(input)?;
        }
        self.derive_symbol_parents();
        self.resolve_references_and_imports()?;
        self.add_derived_terms()?;
        self.build_routes();
        self.build_postings()?;
        self.finish()
    }

    #[allow(clippy::too_many_lines)]
    fn add_file(&mut self, input: &FileInput) -> Result<()> {
        let file_id = FileId::checked(self.files.len())?;
        let path_id = self.strings.intern(input.path.as_str())?;
        let module = module_prefix(&input.path, input.language);
        self.file_modules.push(module.clone());

        let first_symbol = checked_u32(self.symbols.len(), "symbol offset")?;
        let first_reference = checked_u32(self.references.len(), "reference offset")?;
        let first_import = checked_u32(self.imports.len(), "import offset")?;

        let path_terms = collect_terms(
            &mut self.strings,
            &mut self.lexicon,
            LexicalField::Path,
            InputKind::Path,
            input.path.as_str(),
        )?;
        self.file_path_terms.push(path_terms.clone());

        let mut local_to_global = vec![None; input.facts.defs().len()];
        for (local_index, def) in input.facts.defs().iter().enumerate() {
            let symbol_id = SymbolId::checked(self.symbols.len())?;
            local_to_global[local_index] = Some(symbol_id);
            let name = self.strings.intern(&def.name)?;
            let qualified_text = qualify(
                &module,
                def.local_qualified.as_deref(),
                &def.name,
                input.language.qualified_separator(),
            );
            let qualified_name = self.strings.intern(&qualified_text)?;
            let mut terms: [Vec<TermId>; LexicalField::COUNT] = Default::default();
            append_into(
                &mut self.strings,
                &mut self.lexicon,
                &mut terms,
                LexicalField::Name,
                InputKind::Identifier,
                &def.name,
            )?;
            append_into(
                &mut self.strings,
                &mut self.lexicon,
                &mut terms,
                LexicalField::Qualified,
                InputKind::Qualified,
                &qualified_text,
            )?;
            for value in &def.signature_idents {
                append_into(
                    &mut self.strings,
                    &mut self.lexicon,
                    &mut terms,
                    LexicalField::Signature,
                    InputKind::Identifier,
                    value,
                )?;
            }
            for value in &def.body_idents {
                append_into(
                    &mut self.strings,
                    &mut self.lexicon,
                    &mut terms,
                    LexicalField::Body,
                    InputKind::Identifier,
                    value,
                )?;
            }
            for value in &def.doc_idents {
                append_into(
                    &mut self.strings,
                    &mut self.lexicon,
                    &mut terms,
                    LexicalField::Documentation,
                    InputKind::Prose,
                    value,
                )?;
            }
            for value in &def.attribute_idents {
                append_into(
                    &mut self.strings,
                    &mut self.lexicon,
                    &mut terms,
                    LexicalField::Attribute,
                    InputKind::Identifier,
                    value,
                )?;
            }
            terms[LexicalField::Path.index()].extend(path_terms.iter().copied());
            let signature = self.strings.intern(&def.signature)?;
            self.symbols.push(SymbolRecord {
                id: symbol_id,
                file: file_id,
                name,
                qualified_name,
                kind: def.kind,
                visibility: def.visibility.clone(),
                span: def.span,
                signature_span: def.signature_span,
                signature,
                test_signals: def.test_signals,
                field_lengths: FieldLengths::ZERO,
            });
            self.symbol_terms.push(terms);
        }

        for reference in input.facts.references() {
            let owner = reference
                .owner
                .and_then(|id| local_to_global.get(id.index()).copied().flatten());
            let name = self.strings.intern(&reference.name)?;
            let qualified = reference
                .qualified
                .as_deref()
                .map(|value| self.strings.intern(value))
                .transpose()?;
            self.references.push(ReferenceRecord {
                file: file_id,
                owner,
                name,
                qualified,
                kind: reference.kind,
                span: reference.span,
                resolution: Resolution::Unresolved,
            });
        }

        for import in input.facts.imports() {
            let owner = import
                .owner
                .and_then(|id| local_to_global.get(id.index()).copied().flatten());
            let path = self.strings.intern(&import.path)?;
            let name = import
                .name
                .as_deref()
                .map(|value| self.strings.intern(value))
                .transpose()?;
            let alias = import
                .alias
                .as_deref()
                .map(|value| self.strings.intern(value))
                .transpose()?;
            self.imports.push(ImportRecord {
                file: file_id,
                owner,
                path,
                name,
                alias,
                kind: import.kind,
                is_public: import.is_public,
                is_glob: import.is_glob,
                span: import.span,
                resolution: Resolution::Unresolved,
            });
        }

        let symbol_count = checked_u32(self.symbols.len(), "symbol count")? - first_symbol;
        let reference_count =
            checked_u32(self.references.len(), "reference count")? - first_reference;
        let import_count = checked_u32(self.imports.len(), "import count")? - first_import;
        self.files.push(FileRecord {
            id: file_id,
            path: path_id,
            language: input.language,
            content_oid: input.oid,
            representation: input.representation,
            generated: input.generated,
            is_test: input.language.path_indicates_test(input.path.as_str()),
            parse_status: input.parse_status,
            first_symbol,
            symbol_count,
            first_reference,
            reference_count,
            first_import,
            import_count,
        });
        Ok(())
    }

    fn derive_symbol_parents(&mut self) {
        let mut parents = vec![None; self.symbols.len()];
        for file in &self.files {
            let start = file.first_symbol as usize;
            let end = start + file.symbol_count as usize;
            let file_symbols = &self.symbols[start..end];
            for child in file_symbols {
                let parent = file_symbols
                    .iter()
                    .filter(|candidate| {
                        candidate.id != child.id
                            && candidate.span.contains(child.span)
                            && candidate.span.byte_len() > child.span.byte_len()
                    })
                    .min_by_key(|candidate| candidate.span.byte_len())
                    .map(|candidate| candidate.id);
                parents[child.id.index()] = parent;
            }
        }
        self.symbol_parent = parents;
    }

    fn resolve_references_and_imports(&mut self) -> Result<()> {
        let mut by_qualified: HashMap<String, Vec<SymbolId>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<SymbolId>> = HashMap::new();
        for symbol in &self.symbols {
            by_qualified
                .entry(self.strings.values[symbol.qualified_name.index()].clone())
                .or_default()
                .push(symbol.id);
            by_name
                .entry(self.strings.values[symbol.name.index()].clone())
                .or_default()
                .push(symbol.id);
        }

        for index in 0..self.references.len() {
            let reference = &self.references[index];
            let name = self.strings.values[reference.name.index()].clone();
            let qualified = reference
                .qualified
                .map(|id| self.strings.values[id.index()].clone());
            let resolution = if reference.kind == ReferenceKind::MethodCall {
                Resolution::Unresolved
            } else {
                self.resolve_reference(
                    reference.file,
                    reference.owner,
                    &name,
                    qualified.as_deref(),
                    &by_qualified,
                    &by_name,
                )?
            };
            self.references[index].resolution = resolution;
        }

        for index in 0..self.imports.len() {
            let import = &self.imports[index];
            let resolution = if import.is_glob || !import.kind.resolves_by_path() {
                Resolution::Unresolved
            } else {
                let path = self.strings.values[import.path.index()].clone();
                let candidates = Self::resolve_path(
                    &path,
                    &self.file_modules[import.file.index()],
                    &by_qualified,
                );
                classify(candidates)?
            };
            self.imports[index].resolution = resolution;
        }
        Ok(())
    }

    fn resolve_reference(
        &self,
        file: FileId,
        owner: Option<SymbolId>,
        name: &str,
        qualified: Option<&str>,
        by_qualified: &HashMap<String, Vec<SymbolId>>,
        by_name: &HashMap<String, Vec<SymbolId>>,
    ) -> Result<Resolution> {
        if let Some(path) = qualified {
            let candidates =
                Self::resolve_path(path, &self.file_modules[file.index()], by_qualified);
            if !candidates.is_empty() {
                return classify(candidates);
            }
        }

        let imported = self.import_candidates(file, name, by_qualified);
        if !imported.is_empty() {
            return classify(imported);
        }

        let local = self.local_candidates(file, owner, name, by_name);
        if !local.is_empty() {
            return classify(local);
        }

        classify(self.file_candidates(file, name, by_name))
    }

    fn import_candidates(
        &self,
        file: FileId,
        name: &str,
        by_qualified: &HashMap<String, Vec<SymbolId>>,
    ) -> Vec<SymbolId> {
        let file_record = &self.files[file.index()];
        let start = file_record.first_import as usize;
        let end = start + file_record.import_count as usize;
        let mut candidates = Vec::new();
        for import in &self.imports[start..end] {
            if import.is_glob || !import.kind.resolves_by_path() {
                continue;
            }
            let path = &self.strings.values[import.path.index()];
            let leaf = path.rsplit("::").next().unwrap_or(path);
            let alias_matches = import
                .alias
                .is_some_and(|id| self.strings.values[id.index()] == name);
            if leaf == name || alias_matches {
                candidates.extend(Self::resolve_path(
                    path,
                    &self.file_modules[file.index()],
                    by_qualified,
                ));
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }

    fn local_candidates(
        &self,
        file: FileId,
        owner: Option<SymbolId>,
        name: &str,
        by_name: &HashMap<String, Vec<SymbolId>>,
    ) -> Vec<SymbolId> {
        let Some(named) = by_name.get(name) else {
            return Vec::new();
        };
        let mut scope = owner;
        loop {
            let candidates: Vec<SymbolId> = named
                .iter()
                .copied()
                .filter(|id| {
                    self.symbols[id.index()].file == file && self.symbol_parent[id.index()] == scope
                })
                .collect();
            if !candidates.is_empty() {
                return candidates;
            }
            let Some(current) = scope else {
                return Vec::new();
            };
            scope = self.symbol_parent[current.index()];
        }
    }

    fn file_candidates(
        &self,
        file: FileId,
        name: &str,
        by_name: &HashMap<String, Vec<SymbolId>>,
    ) -> Vec<SymbolId> {
        let Some(named) = by_name.get(name) else {
            return Vec::new();
        };
        let module = &self.file_modules[file.index()];
        named
            .iter()
            .copied()
            .filter(|id| self.file_modules[self.symbols[id.index()].file.index()] == *module)
            .collect()
    }

    /// Writes each symbol's visible imports into its `Import` field: the
    /// file's module-scope imports plus the ones this symbol's own body
    /// declares.
    ///
    /// Leaf names and aliases share one dedup list rather than two, because
    /// they share a field and an [`InputKind`]. `import { A as A } from "m"`
    /// otherwise emits `A` twice and ranks as if the file mentioned it twice.
    fn emit_symbol_imports(
        &mut self,
        file_imports: &[FileImport],
        symbols: std::ops::Range<usize>,
    ) -> Result<()> {
        for symbol in symbols {
            let symbol_id = self.symbols[symbol].id;
            let mut paths = Vec::new();
            let mut identifiers = Vec::new();
            for (owner, path, name, alias) in file_imports {
                if owner.is_none() || *owner == Some(symbol_id) {
                    if !paths.iter().any(|seen| seen == path) {
                        paths.push(path.clone());
                    }
                    for identifier in [name, alias].into_iter().flatten() {
                        if !identifiers.iter().any(|seen| seen == identifier) {
                            identifiers.push(identifier.clone());
                        }
                    }
                }
            }
            for path in paths {
                self.emit(symbol_id, LexicalField::Import, InputKind::Qualified, &path)?;
            }
            for identifier in identifiers {
                self.emit(
                    symbol_id,
                    LexicalField::Import,
                    InputKind::Identifier,
                    &identifier,
                )?;
            }
        }
        Ok(())
    }

    fn resolve_path(
        path: &str,
        module: &[String],
        by_qualified: &HashMap<String, Vec<SymbolId>>,
    ) -> Vec<SymbolId> {
        let segments: Vec<&str> = path.split("::").filter(|part| !part.is_empty()).collect();
        if segments.is_empty() {
            return Vec::new();
        }
        let mut base = module.to_vec();
        let rest: &[&str] = match segments[0] {
            "crate" => {
                base.clear();
                &segments[1..]
            }
            "self" => &segments[1..],
            "super" => {
                let count = segments.iter().take_while(|part| **part == "super").count();
                for _ in 0..count {
                    base.pop();
                }
                &segments[count..]
            }
            _ => &segments,
        };
        let mut expanded = base.join("::");
        if !rest.is_empty() {
            if !expanded.is_empty() {
                expanded.push_str("::");
            }
            expanded.push_str(&rest.join("::"));
        }
        by_qualified.get(&expanded).cloned().unwrap_or_default()
    }

    fn add_derived_terms(&mut self) -> Result<()> {
        let references: Vec<(Option<SymbolId>, String)> = self
            .references
            .iter()
            .map(|reference| {
                let name = self.strings.values[reference.name.index()].clone();
                let value = reference
                    .qualified
                    .map_or(name, |id| self.strings.values[id.index()].clone());
                (reference.owner, value)
            })
            .collect();
        for (owner, value) in references {
            if let Some(owner) = owner {
                self.emit(owner, LexicalField::Callee, InputKind::Qualified, &value)?;
            }
        }

        let file_ranges: Vec<(usize, usize, usize, usize)> = self
            .files
            .iter()
            .map(|file| {
                (
                    file.first_symbol as usize,
                    file.symbol_count as usize,
                    file.first_import as usize,
                    file.import_count as usize,
                )
            })
            .collect();
        for (first_symbol, symbol_count, first_import, import_count) in file_ranges {
            let file_imports: Vec<FileImport> = self.imports
                [first_import..first_import + import_count]
                .iter()
                .map(|import| {
                    (
                        import.owner,
                        self.strings.values[import.path.index()].clone(),
                        import
                            .name
                            .map(|id| self.strings.values[id.index()].clone()),
                        import
                            .alias
                            .map(|id| self.strings.values[id.index()].clone()),
                    )
                })
                .collect();
            self.emit_symbol_imports(&file_imports, first_symbol..first_symbol + symbol_count)?;
        }

        let caller_edges: Vec<(SymbolId, SymbolId)> = self
            .references
            .iter()
            .filter_map(|reference| match (reference.owner, reference.resolution) {
                (Some(source), Resolution::Resolved(target))
                    if reference.kind == ReferenceKind::Call =>
                {
                    Some((source, target))
                }
                _ => None,
            })
            .collect();
        for (source, target) in caller_edges {
            let qualified =
                self.strings.values[self.symbols[source.index()].qualified_name.index()].clone();
            self.emit(
                target,
                LexicalField::Caller,
                InputKind::Qualified,
                &qualified,
            )?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        symbol: SymbolId,
        field: LexicalField,
        kind: InputKind,
        value: &str,
    ) -> Result<()> {
        append_into(
            &mut self.strings,
            &mut self.lexicon,
            &mut self.symbol_terms[symbol.index()],
            field,
            kind,
            value,
        )?;
        Ok(())
    }

    fn build_routes(&mut self) {
        let mut names: HashMap<StringId, Vec<SymbolId>> = HashMap::new();
        let mut qualified: HashMap<StringId, Vec<SymbolId>> = HashMap::new();
        for symbol in &self.symbols {
            names.entry(symbol.name).or_default().push(symbol.id);
            qualified
                .entry(symbol.qualified_name)
                .or_default()
                .push(symbol.id);
        }
        self.exact_names = routes(names, &self.strings.values);
        self.exact_qualified = routes(qualified, &self.strings.values);
    }

    fn build_postings(&mut self) -> Result<()> {
        for field in LexicalField::ALL {
            let mut by_term: HashMap<TermId, Vec<SymbolPosting>> = HashMap::new();
            for (symbol_index, terms) in self.symbol_terms.iter().enumerate() {
                let mut frequencies: HashMap<TermId, u32> = HashMap::new();
                for term in &terms[field.index()] {
                    *frequencies.entry(*term).or_default() += 1;
                }
                let symbol = self.symbols[symbol_index].id;
                for (term, term_frequency) in frequencies {
                    by_term.entry(term).or_default().push(SymbolPosting {
                        symbol,
                        term_frequency,
                    });
                }
            }
            let mut lists: Vec<PostingList> = by_term
                .into_iter()
                .map(|(term, mut entries)| {
                    entries.sort_unstable_by_key(|entry| entry.symbol);
                    PostingList { term, entries }
                })
                .collect();
            lists.sort_unstable_by(|left, right| {
                self.lexicon
                    .resolve(left.term)
                    .unwrap_or_default()
                    .as_bytes()
                    .cmp(
                        self.lexicon
                            .resolve(right.term)
                            .unwrap_or_default()
                            .as_bytes(),
                    )
            });
            *self.postings.lists_mut(field) = lists;
        }

        let mut by_term: HashMap<TermId, Vec<FilePosting>> = HashMap::new();
        for (file_index, terms) in self.file_path_terms.iter().enumerate() {
            let mut frequencies: HashMap<TermId, u32> = HashMap::new();
            for term in terms {
                *frequencies.entry(*term).or_default() += 1;
            }
            let file = self.files[file_index].id;
            for (term, term_frequency) in frequencies {
                by_term.entry(term).or_default().push(FilePosting {
                    file,
                    term_frequency,
                });
            }
        }
        self.file_path_postings = by_term
            .into_iter()
            .map(|(term, mut entries)| {
                entries.sort_unstable_by_key(|entry| entry.file);
                FilePostingList { term, entries }
            })
            .collect();
        self.file_path_postings.sort_unstable_by(|left, right| {
            self.lexicon
                .resolve(left.term)
                .unwrap_or_default()
                .as_bytes()
                .cmp(
                    self.lexicon
                        .resolve(right.term)
                        .unwrap_or_default()
                        .as_bytes(),
                )
        });

        for (symbol, terms) in self.symbols.iter_mut().zip(&self.symbol_terms) {
            let mut lengths = [0u32; LexicalField::COUNT];
            for field in LexicalField::ALL {
                lengths[field.index()] =
                    checked_u32(terms[field.index()].len(), "symbol field length")?;
            }
            symbol.field_lengths = FieldLengths::from_ordinals(lengths);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(Snapshot, IndexCounts)> {
        let lexicon_terms: Vec<String> = self.lexicon.terms().map(str::to_owned).collect();
        let terms: Vec<TermRecord> = lexicon_terms
            .iter()
            .map(|term| self.strings.intern(term).map(|text| TermRecord { text }))
            .collect::<Result<Vec<_>>>()?;
        let mut snapshot = Snapshot {
            meta: self.meta,
            strings: self.strings.values,
            terms,
            files: self.files,
            symbols: self.symbols,
            references: self.references,
            imports: self.imports,
            exact_names: self.exact_names,
            exact_qualified: self.exact_qualified,
            postings: self.postings,
            file_path_postings: self.file_path_postings,
            corpus: CorpusStats::EMPTY,
        };
        snapshot.corpus = snapshot.compute_corpus_stats()?;
        snapshot.validate()?;
        let mut counts = IndexCounts {
            symbols: checked_u32(snapshot.symbols.len(), "symbol count")?,
            references: checked_u32(snapshot.references.len(), "reference count")?,
            imports: checked_u32(snapshot.imports.len(), "import count")?,
            ..IndexCounts::default()
        };
        for reference in &snapshot.references {
            match reference.resolution {
                Resolution::Unresolved => counts.unresolved_refs += 1,
                Resolution::Ambiguous { .. } => counts.ambiguous_refs += 1,
                Resolution::Resolved(_) => {}
            }
        }
        for import in &snapshot.imports {
            match import.resolution {
                Resolution::Unresolved => counts.unresolved_imports += 1,
                Resolution::Ambiguous { .. } => counts.ambiguous_imports += 1,
                Resolution::Resolved(_) => {}
            }
        }
        Ok((snapshot, counts))
    }
}

fn empty_postings() -> FieldPostings {
    FieldPostings {
        name: Vec::new(),
        qualified: Vec::new(),
        path: Vec::new(),
        signature: Vec::new(),
        body: Vec::new(),
        documentation: Vec::new(),
        attribute: Vec::new(),
        callee: Vec::new(),
        caller: Vec::new(),
        import: Vec::new(),
    }
}

fn append_into(
    strings: &mut StringInterner,
    lexicon: &mut Lexicon,
    terms: &mut [Vec<TermId>; LexicalField::COUNT],
    field: LexicalField,
    kind: InputKind,
    value: &str,
) -> Result<()> {
    let mut fields: SmallVec<[FieldTerm; 32]> = SmallVec::new();
    append_source_terms(field, kind, value, lexicon, &mut fields)?;
    for term in &fields {
        let text = lexicon.resolve(term.term).ok_or(Error::SnapshotInvariant {
            reason: "term id missing from lexicon",
        })?;
        strings.intern(text)?;
    }
    terms[field.index()].extend(fields.into_iter().map(|term| term.term));
    Ok(())
}

fn collect_terms(
    strings: &mut StringInterner,
    lexicon: &mut Lexicon,
    field: LexicalField,
    kind: InputKind,
    value: &str,
) -> Result<Vec<TermId>> {
    let mut fields: SmallVec<[FieldTerm; 32]> = SmallVec::new();
    append_source_terms(field, kind, value, lexicon, &mut fields)?;
    for term in &fields {
        let text = lexicon.resolve(term.term).ok_or(Error::SnapshotInvariant {
            reason: "term id missing from lexicon",
        })?;
        strings.intern(text)?;
    }
    Ok(fields.into_iter().map(|term| term.term).collect())
}

fn checked_u32(value: usize, label: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::IdSpaceExhausted(label))
}

fn module_prefix(path: &RelPath, lang: Lang) -> Vec<String> {
    let mut components: Vec<String> = path.as_str().split('/').map(str::to_owned).collect();
    if components.first().map(String::as_str) == Some("src") {
        components.remove(0);
    }
    // The extension is dropped only when it spells the file's own language, so
    // `service.py` becomes the module `service` while an unexpected suffix is
    // left visible rather than half-stripped.
    if let Some(file) = components.last_mut() {
        if let Some((stem, extension)) = file.rsplit_once('.') {
            if !stem.is_empty() && Lang::from_extension(extension) == Some(lang) {
                *file = stem.to_owned();
            }
        }
    }
    // File names that name their directory's module rather than their own.
    let implicit: &[&str] = match lang {
        Lang::Rust => &["lib", "main", "mod"],
        Lang::Python => &["__init__", "__main__"],
        _ => &[],
    };
    if components
        .last()
        .is_some_and(|part| implicit.contains(&part.as_str()))
    {
        components.pop();
    }
    components
}

fn qualify(module: &[String], local: Option<&str>, name: &str, separator: &str) -> String {
    let suffix = local.unwrap_or(name);
    if module.is_empty() {
        suffix.to_owned()
    } else {
        format!("{}{separator}{suffix}", module.join(separator))
    }
}

fn classify(mut candidates: Vec<SymbolId>) -> Result<Resolution> {
    candidates.sort_unstable();
    candidates.dedup();
    match candidates.len() {
        0 => Ok(Resolution::Unresolved),
        1 => Ok(Resolution::Resolved(candidates[0])),
        count => Ok(Resolution::Ambiguous {
            candidates: checked_u32(count, "resolution candidate count")?,
        }),
    }
}

fn routes(mut values: HashMap<StringId, Vec<SymbolId>>, strings: &[String]) -> Vec<ExactRoute> {
    let mut routes: Vec<ExactRoute> = values
        .drain()
        .map(|(key, mut symbols)| {
            symbols.sort_unstable();
            symbols.dedup();
            ExactRoute { key, symbols }
        })
        .collect();
    routes.sort_unstable_by(|left, right| {
        strings[left.key.index()]
            .as_bytes()
            .cmp(strings[right.key.index()].as_bytes())
    });
    routes
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::parser::RustExtractor;

    #[test]
    fn builder_creates_exact_routes_and_name_postings() {
        let bytes = b"fn alpha() { beta(); }\nfn beta() {}\n";
        let facts = RustExtractor::new().unwrap().extract(bytes).unwrap();
        let input = FileInput {
            path: RelPath::new("src/lib.rs").unwrap(),
            oid: Oid::from_hex("0000000000000000000000000000000000000000").unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: facts.status(),
            facts,
        };
        let (snapshot, counts) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
            .build(vec![input])
            .unwrap();
        assert_eq!(counts.symbols, 2);
        assert!(snapshot
            .exact_names
            .iter()
            .any(|route| snapshot.strings[route.key.index()] == "alpha"));
        assert!(!snapshot.postings.name.is_empty());
        snapshot.validate().unwrap();
    }

    #[test]
    fn module_prefix_keeps_file_module_stem() {
        assert_eq!(
            module_prefix(&RelPath::new("tests/auth.rs").unwrap(), Lang::Rust),
            ["tests", "auth"]
        );
        assert_eq!(
            module_prefix(&RelPath::new("src/auth/token.rs").unwrap(), Lang::Rust),
            ["auth", "token"]
        );
        assert_eq!(
            module_prefix(&RelPath::new("src/lib.rs").unwrap(), Lang::Rust),
            Vec::<String>::new()
        );
    }

    #[test]
    fn module_prefix_speaks_each_language_own_conventions() {
        assert_eq!(
            module_prefix(&RelPath::new("src/service.py").unwrap(), Lang::Python),
            ["service"]
        );
        assert_eq!(
            module_prefix(&RelPath::new("pkg/__init__.py").unwrap(), Lang::Python),
            ["pkg"]
        );
        // A foreign extension is not this language's to strip.
        assert_eq!(
            module_prefix(&RelPath::new("src/service.py").unwrap(), Lang::Rust),
            ["service.py"]
        );
    }

    #[test]
    fn qualification_uses_the_language_separator() {
        let module = vec!["service".to_owned()];
        assert_eq!(
            qualify(
                &module,
                Some("Service.run"),
                "run",
                Lang::Python.qualified_separator()
            ),
            "service.Service.run"
        );
        assert_eq!(
            qualify(
                &module,
                Some("Auth::new"),
                "new",
                Lang::Rust.qualified_separator()
            ),
            "service::Auth::new"
        );
    }
    #[test]
    fn nested_definition_resolution_walks_parent_scopes() {
        let facts = RustExtractor::new()
            .unwrap()
            .extract(b"fn outer() { fn inner() {} inner(); }")
            .unwrap();
        let input = FileInput {
            path: RelPath::new("src/lib.rs").unwrap(),
            oid: Oid::from_hex("0000000000000000000000000000000000000000").unwrap(),
            representation: ContentRepresentation::RawNoGit,
            generated: false,
            language: Lang::Rust,
            parse_status: facts.status(),
            facts,
        };
        let (snapshot, _) = SnapshotBuilder::new(SnapshotMeta::new(None, true, [0; 32]))
            .build(vec![input])
            .unwrap();
        let inner = snapshot
            .symbols
            .iter()
            .find(|symbol| snapshot.strings[symbol.name.index()] == "inner")
            .map(|symbol| symbol.id)
            .unwrap();
        assert!(snapshot.references.iter().any(|reference| {
            snapshot.strings[reference.name.index()] == "inner"
                && reference.resolution == Resolution::Resolved(inner)
        }));
    }
}
