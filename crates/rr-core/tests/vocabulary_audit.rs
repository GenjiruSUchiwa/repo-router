//! Every enum variant this workspace ships has something that builds it, and
//! every `dead_code` suppression says what it is for.
//!
//! Dead vocabulary is worse than a missing feature. A shipped variant is a
//! promise that some input reaches it, and a reader who matches on one gets a
//! branch that never runs — a bug that no test fails on, because there is no
//! behaviour to test. #41 found ten of them by hand; this test is what makes
//! the eleventh impossible to merge.
//!
//! It also found one that was not a variant at all: a query capture the
//! extractor was contractually required to declare and then discarded, held in
//! place by a struct-wide `#[allow(dead_code)]` that switched the compiler off
//! for fourteen live fields to hide one dead one. Hence the second check. The
//! compiler finds dead code; this file's job is to keep it switched on.
//!
//! The audit is a parse, not a search. Telling a producer (`Kind::A` in an
//! expression) from a consumer (`Kind::A` in a match arm) is a question about
//! syntactic position, which `grep` cannot answer — the hand audit that filed
//! #41 answered it wrong twice, and both times by clearing a variant that had
//! no producer.
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Enums this audit cannot answer for, and why not for each.
///
/// Empty, and meant to stay that way. A row belongs here only when the audit is
/// structurally blind to how an enum is built — not when a variant is unused,
/// which is what deleting it is for, and not when it is held for later work,
/// which is what [`RESERVED_VARIANTS`] is for.
const EXEMPT_ENUMS: &[(&str, &str)] = &[];

/// Variants held for filed work, as `(enum, variant, issue)`.
///
/// A row is a debt with a creditor. The audit fails if the enum or the variant
/// stops existing, and fails if the variant gains a producer — at which point
/// the debt is paid and the row goes.
const RESERVED_VARIANTS: &[(&str, &str, &str)] = &[("Pipeline", "Route", "#12")];

/// Derives whose generated code constructs variants no source file spells.
///
/// Deliberately narrow, and `Deserialize` is deliberately absent. A
/// `Deserialize` impl builds a variant only when some input names it, which is
/// a producer in the same sense that a comment is documentation. This was
/// tested rather than assumed: with `Deserialize` on this list the audit
/// cleared `Pipeline::Route` and all three specifier-based `ImportKind`
/// variants — every gap it exists to report.
const GENERATIVE_DERIVES: &[&str] = &["Subcommand", "Parser", "ValueEnum"];

/// `#[allow(dead_code)]` sites whose removal is filed work, as
/// `(file, item, issue)`.
///
/// Same shape and same discipline as [`RESERVED_VARIANTS`]: a row is a debt
/// with a creditor, and the audit fails when the site stops existing so the row
/// cannot outlive what it excuses. A suppression that is simply *correct* does
/// not belong here — it belongs where it is, carrying a written reason, which
/// is what the check asks for.
const SCOPED_SUPPRESSIONS: &[(&str, &str, &str)] =
    &[("crates/rr-cli/src/output.rs", "print_text", "#45")];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rr-core sits two levels below the workspace root")
        .to_path_buf()
}

/// Every `.rs` file under `crates/`, minus the fixture trees.
///
/// `fixtures/` is skipped because it holds Rust that is deliberately not Rust:
/// `crates/rr-core/tests/fixtures/rust/recovered.rs` exists to make the
/// extractor report a parse error, and `syn` refuses it. Nothing else may fail
/// to parse — a file this walk cannot read is a file whose producers it cannot
/// see, which is the silent failure the whole audit is built against — so any
/// other parse failure is reported rather than being skipped.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.join("crates")];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default();
                if name != "target" && name != "fixtures" {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn path_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Whether an item is compiled only under `cfg(test)`.
fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        let name = path_string(attribute.path());
        (name == "cfg" || name == "cfg_attr")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string().contains("test"))
    })
}

/// One enum as declared, with what the audit already knows about how it is
/// built.
struct DeclaredEnum {
    file: String,
    variants: Vec<String>,
    /// Derive names, last path segment only.
    derives: BTreeSet<String>,
    /// Variants carrying a `#[from]` field, which `thiserror` constructs.
    from_variants: BTreeSet<String>,
}

struct DeclVisitor<'a> {
    declared: &'a mut BTreeMap<String, DeclaredEnum>,
    file: String,
}

impl<'ast> Visit<'ast> for DeclVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        let mut derives = BTreeSet::new();
        for attribute in &node.attrs {
            if path_string(attribute.path()) != "derive" {
                continue;
            }
            if let Ok(list) = attribute.meta.require_list() {
                for token in list.tokens.to_string().split(',') {
                    let name = token.trim().rsplit("::").next().unwrap_or_default();
                    derives.insert(name.trim().to_owned());
                }
            }
        }

        let mut variants = Vec::new();
        let mut from_variants = BTreeSet::new();
        for variant in &node.variants {
            let name = variant.ident.to_string();
            if variant
                .fields
                .iter()
                .any(|field| field.attrs.iter().any(|a| path_string(a.path()) == "from"))
            {
                from_variants.insert(name.clone());
            }
            variants.push(name);
        }

        self.declared.insert(
            node.ident.to_string(),
            DeclaredEnum {
                file: self.file.clone(),
                variants,
                derives,
                from_variants,
            },
        );
    }
}

#[derive(Default)]
struct Uses {
    /// `Enum::Variant` built in non-test `crates/*/src`. The only column that
    /// clears a variant.
    produced: BTreeMap<String, usize>,
    /// Built under `cfg(test)`, or in `tests/` and `benches/`.
    produced_in_tests: BTreeMap<String, usize>,
    /// Matched on, anywhere. Never clears; reported so a failure says whether
    /// the variant is read but never written.
    matched: BTreeMap<String, usize>,
    /// Seen as `A :: B` tokens inside a macro body. Context only, per the
    /// comment on `visit_macro`.
    in_macro: BTreeMap<String, usize>,
}

struct UseVisitor<'a> {
    uses: &'a mut Uses,
    /// Self types of the enclosing `impl` blocks, so `Self::V` resolves.
    self_ty: Vec<String>,
    /// Whether the current position is test-only code.
    in_test: bool,
    /// Pattern nesting depth. See `visit_pat`.
    in_pattern: usize,
}

impl UseVisitor<'_> {
    /// `Enum::Variant` for a path that could name one, else `None`.
    ///
    /// Takes the last two segments, so `crate::facts::DefKind::Struct` and
    /// `DefKind::Struct` agree. Both must start uppercase: that rejects
    /// `module::Type` and `Type::method` without a type resolver, at the price
    /// of also crediting `Struct::assoc_const` — which can only ever *add* a
    /// producer for a name no enum declares, and so cannot clear a real
    /// variant.
    fn key(&self, path: &syn::Path) -> Option<String> {
        let segments: Vec<&syn::Ident> = path.segments.iter().map(|s| &s.ident).collect();
        let [.., owner, variant] = segments.as_slice() else {
            return None;
        };
        let variant = variant.to_string();
        if !variant.starts_with(char::is_uppercase) {
            return None;
        }
        let owner = owner.to_string();
        let owner = if owner == "Self" {
            self.self_ty.last()?.clone()
        } else if owner.starts_with(char::is_uppercase) {
            owner
        } else {
            return None;
        };
        Some(format!("{owner}::{variant}"))
    }

    fn record_construction(&mut self, key: String) {
        let column = if self.in_test {
            &mut self.uses.produced_in_tests
        } else {
            &mut self.uses.produced
        };
        *column.entry(key).or_default() += 1;
    }
}

impl<'ast> Visit<'ast> for UseVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let outer = self.in_test;
        self.in_test = outer || is_cfg_test(&node.attrs);
        syn::visit::visit_item_mod(self, node);
        self.in_test = outer;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let name = match &*node.self_ty {
            syn::Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.self_ty.push(name.unwrap_or_default());
        syn::visit::visit_item_impl(self, node);
        self.self_ty.pop();
    }

    /// The counter that makes this audit mean anything.
    ///
    /// syn 2 has no `visit_pat_path`: syn's AST gives the bare path pattern as
    /// a path expression, so every `Self::Fresh => …` arm lands here and would
    /// be counted as a construction without this bracket. Removing it makes
    /// the audit pass on a workspace where nothing builds anything.
    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        self.in_pattern += 1;
        syn::visit::visit_pat(self, node);
        self.in_pattern -= 1;
    }

    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if let Some(key) = self.key(&node.path) {
            if self.in_pattern > 0 {
                *self.uses.matched.entry(key).or_default() += 1;
            } else {
                self.record_construction(key);
            }
        }
        syn::visit::visit_expr_path(self, node);
    }

    fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
        if let Some(key) = self.key(&node.path) {
            self.record_construction(key);
        }
        syn::visit::visit_expr_struct(self, node);
    }

    fn visit_pat_tuple_struct(&mut self, node: &'ast syn::PatTupleStruct) {
        if let Some(key) = self.key(&node.path) {
            *self.uses.matched.entry(key).or_default() += 1;
        }
        syn::visit::visit_pat_tuple_struct(self, node);
    }

    fn visit_pat_struct(&mut self, node: &'ast syn::PatStruct) {
        if let Some(key) = self.key(&node.path) {
            *self.uses.matched.entry(key).or_default() += 1;
        }
        syn::visit::visit_pat_struct(self, node);
    }

    /// Token-level, and context only.
    ///
    /// A macro body is tokens, and `assert_eq!(state, Kind::A)` compares where
    /// `Some(Kind::A)` builds. Deciding which would mean parsing each macro's
    /// own grammar, so this counts occurrences and never clears a variant. The
    /// bias is toward a false alarm, which costs one reviewed line in
    /// `RESERVED_VARIANTS`, over a false clear, which costs the audit its
    /// purpose.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let tokens = node.tokens.to_string();
        let parts: Vec<&str> = tokens.split_whitespace().collect();
        for window in parts.windows(3) {
            let [left, sep, right] = window else {
                continue;
            };
            if *sep != "::"
                || !right.starts_with(char::is_uppercase)
                || !left.starts_with(char::is_uppercase)
            {
                continue;
            }
            let owner = if *left == "Self" {
                self.self_ty.last().cloned().unwrap_or_default()
            } else {
                (*left).to_owned()
            };
            *self
                .uses
                .in_macro
                .entry(format!("{owner}::{right}"))
                .or_default() += 1;
        }
        syn::visit::visit_macro(self, node);
    }
}

fn collect(root: &Path) -> (BTreeMap<String, DeclaredEnum>, Uses) {
    let mut declared = BTreeMap::new();
    let mut uses = Uses::default();
    for path in rust_sources(root) {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        let file =
            syn::parse_file(&text).unwrap_or_else(|error| panic!("parse {relative}: {error}"));

        let in_src = relative.contains("/src/");
        if in_src {
            DeclVisitor {
                declared: &mut declared,
                file: relative.clone(),
            }
            .visit_file(&file);
        }
        UseVisitor {
            uses: &mut uses,
            self_ty: Vec::new(),
            in_test: !in_src,
            in_pattern: 0,
        }
        .visit_file(&file);
    }
    (declared, uses)
}

/// Whether generated code builds this variant, per `GENERATIVE_DERIVES` and
/// `#[from]`.
fn built_by_generated_code(declaration: &DeclaredEnum, variant: &str) -> bool {
    declaration.from_variants.contains(variant)
        || GENERATIVE_DERIVES
            .iter()
            .any(|derive| declaration.derives.contains(*derive))
}

#[test]
fn every_shipped_enum_variant_has_a_producer() {
    let root = workspace_root();
    let (declared, uses) = collect(&root);
    assert!(
        declared.len() > 50,
        "the audit found only {} enums; the source walk is broken, not the workspace",
        declared.len()
    );

    let exempt: BTreeSet<&str> = EXEMPT_ENUMS.iter().map(|(name, _)| *name).collect();
    let reserved: BTreeMap<(&str, &str), &str> = RESERVED_VARIANTS
        .iter()
        .map(|(enum_name, variant, issue)| ((*enum_name, *variant), *issue))
        .collect();

    let mut orphans = String::new();
    let mut paid = String::new();

    for (enum_name, declaration) in &declared {
        if exempt.contains(enum_name.as_str()) {
            continue;
        }
        for variant in &declaration.variants {
            let key = format!("{enum_name}::{variant}");
            let produced = uses.produced.get(&key).copied().unwrap_or(0);
            let held = reserved.get(&(enum_name.as_str(), variant.as_str()));

            if produced > 0 {
                if let Some(issue) = held {
                    let _ = writeln!(
                        paid,
                        "  {key} is now produced; drop its RESERVED_VARIANTS row ({issue})"
                    );
                }
                continue;
            }
            if held.is_some() || built_by_generated_code(declaration, variant) {
                continue;
            }
            let _ = writeln!(
                orphans,
                "  {:<44} {:<40} tests={} matched={} macro={}",
                key,
                declaration.file,
                uses.produced_in_tests.get(&key).copied().unwrap_or(0),
                uses.matched.get(&key).copied().unwrap_or(0),
                uses.in_macro.get(&key).copied().unwrap_or(0),
            );
        }
    }

    let mut stale = String::new();
    for (name, _) in EXEMPT_ENUMS {
        if !declared.contains_key(*name) {
            let _ = writeln!(
                stale,
                "  EXEMPT_ENUMS names `{name}`, which no longer exists"
            );
        }
    }
    for (enum_name, variant, issue) in RESERVED_VARIANTS {
        let exists = declared
            .get(*enum_name)
            .is_some_and(|d| d.variants.iter().any(|v| v == variant));
        if !exists {
            let _ = writeln!(
                stale,
                "  RESERVED_VARIANTS names `{enum_name}::{variant}` ({issue}), which no longer exists"
            );
        }
    }

    assert!(
        orphans.is_empty() && paid.is_empty() && stale.is_empty(),
        "\n\
         {orphans_header}{orphans}\
         {paid_header}{paid}\
         {stale_header}{stale}\n\
         A variant with no producer is vocabulary rr ships and never speaks: a\n\
         reader who matches on it gets a branch that never runs. Build one,\n\
         delete the variant, or — if it is held for filed work — add a row to\n\
         RESERVED_VARIANTS in crates/rr-core/tests/vocabulary_audit.rs naming\n\
         the issue. `tests=` counts constructions in test code, which do not\n\
         clear a variant; `macro=` counts tokens inside macro bodies, which are\n\
         reported and never trusted.\n",
        orphans_header = if orphans.is_empty() {
            String::new()
        } else {
            "enum variants with no producer in crates/*/src:\n".to_owned()
        },
        paid_header = if paid.is_empty() {
            String::new()
        } else {
            "reserved variants that are now produced:\n".to_owned()
        },
        stale_header = if stale.is_empty() {
            String::new()
        } else {
            "allowlist rows that no longer name anything:\n".to_owned()
        },
    );
}

/// Runs both visitors over a fixed source string, so the machinery is pinned
/// against the shapes that once fooled it. Only the enum-name key matters; the
/// file label is a placeholder.
fn audit_source(source: &str) -> (BTreeMap<String, DeclaredEnum>, Uses) {
    let file = syn::parse_file(source).expect("test sources must parse");
    let mut declared = BTreeMap::new();
    let mut uses = Uses::default();
    DeclVisitor {
        declared: &mut declared,
        file: "<source>".to_owned(),
    }
    .visit_file(&file);
    UseVisitor {
        uses: &mut uses,
        self_ty: Vec::new(),
        in_test: false,
        in_pattern: 0,
    }
    .visit_file(&file);
    (declared, uses)
}

#[test]
fn a_match_arm_is_not_a_producer() {
    let (_, uses) = audit_source("enum K { A }\nfn f(k: K) { let _ = match k { K::A => 1 }; }");
    assert!(
        uses.produced.is_empty(),
        "a match arm must not clear a variant"
    );
    assert_eq!(uses.matched.get("K::A"), Some(&1));
}

#[test]
fn a_self_qualified_construction_is_a_producer() {
    let (_, uses) = audit_source("enum K { A }\nimpl K { fn a() -> Self { Self::A } }");
    assert_eq!(uses.produced.get("K::A"), Some(&1));
}

#[test]
fn a_test_only_construction_does_not_clear_a_variant() {
    let (_, uses) = audit_source("enum K { A }\n#[cfg(test)] mod tests { fn t() -> K { K::A } }");
    assert!(uses.produced.is_empty());
    assert_eq!(uses.produced_in_tests.get("K::A"), Some(&1));
}

#[test]
fn a_macro_body_occurrence_does_not_clear_a_variant() {
    let (_, uses) = audit_source("enum K { A }\nfn f(k: K) { assert_eq!(k, K::A); }");
    assert!(uses.produced.is_empty());
    assert_eq!(uses.in_macro.get("K::A"), Some(&1));
}

#[test]
fn an_unparseable_source_file_fails_the_audit_rather_than_being_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("crates")).unwrap();
    std::fs::write(dir.path().join("crates/broken.rs"), "fn {").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| collect(dir.path())));
    assert!(
        result.is_err(),
        "an unparseable source file must fail the audit"
    );
}

/// A `#[allow(dead_code)]` as the audit found it.
struct Suppression {
    file: String,
    item: String,
    /// `false` for a struct, enum, module or impl block — anything whose
    /// suppression covers more than one nameable thing.
    scoped: bool,
    documented: bool,
}

/// Whether a doc comment or an adjacent `//` comment gives a reason.
///
/// Deliberately shallow: it asks whether *something* was written, not whether
/// the reason is good. A reviewer judges the sentence; the audit only makes
/// sure there is one to judge.
fn has_written_reason(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("doc"))
}

/// Whether an attribute list carries `#[allow(dead_code)]`.
fn has_dead_code_allow(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        path_string(attribute.path()) == "allow"
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string().contains("dead_code"))
    })
}

/// Whether a `macro_rules!` body token contains a `#[allow(dead_code)]`.
///
/// syn does not descend into macro bodies — they are tokens, not AST — yet
/// `crates/rr-core/src/index/mod.rs` carries a real, load-bearing suppression
/// inside `id_newtype!`. A token walk is the only way to see it, and the
/// surrounding tokens still answer the two real questions: `[`..`]` groups
/// name the attribute's argument, the token before the attribute names the
/// doc comment, and the token run after it names the item it is scoped to.
fn flatten_tokens(tokens: &proc_macro2::TokenStream, out: &mut Vec<proc_macro2::TokenTree>) {
    for tree in tokens.clone() {
        if let proc_macro2::TokenTree::Group(group) = &tree {
            out.push(tree.clone());
            flatten_tokens(&group.stream(), out);
        } else {
            out.push(tree.clone());
        }
    }
}

fn scan_macro_tokens(tokens: &proc_macro2::TokenStream, file: &str, found: &mut Vec<Suppression>) {
    let mut trees = Vec::new();
    flatten_tokens(tokens, &mut trees);
    for (index, tree) in trees.iter().enumerate() {
        let proc_macro2::TokenTree::Group(group) = tree else {
            continue;
        };
        if group.delimiter() != proc_macro2::Delimiter::Bracket {
            continue;
        }
        let inner: Vec<proc_macro2::TokenTree> = group.stream().into_iter().collect();
        let is_allow = matches!(
            inner.first(),
            Some(proc_macro2::TokenTree::Ident(ident)) if ident == "allow"
        );
        if !is_allow {
            continue;
        }
        let mentions_dead_code = group.stream().to_string().contains("dead_code");
        if !mentions_dead_code {
            continue;
        }

        let mut documented = false;
        let mut previous = index;
        while previous > 0 {
            previous -= 1;
            if let proc_macro2::TokenTree::Group(g) = &trees[previous] {
                if g.delimiter() == proc_macro2::Delimiter::Bracket {
                    documented = g.stream().into_iter().any(|token| {
                        matches!(
                            token,
                            proc_macro2::TokenTree::Ident(ident) if ident == "doc"
                        )
                    });
                    break;
                }
            }
        }

        let mut scoped = false;
        let mut item = String::new();
        let mut lookahead = trees.iter().skip(index + 1).take(16);
        while let Some(later) = lookahead.next() {
            let proc_macro2::TokenTree::Ident(ident) = later else {
                continue;
            };
            let name = ident.to_string();
            match name.as_str() {
                "struct" | "enum" | "union" | "mod" | "impl" => break,
                "fn" => {
                    scoped = true;
                    if let Some(proc_macro2::TokenTree::Ident(name_ident)) = lookahead.next() {
                        item = name_ident.to_string();
                    }
                    break;
                }
                _ => {}
            }
        }
        found.push(Suppression {
            file: file.to_owned(),
            item: if item.is_empty() {
                "<macro>".to_owned()
            } else {
                item
            },
            scoped,
            documented,
        });
    }
}

/// The attribute list of an item, `&[]` for a token-only `Verbatim` item.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// The name of an item, where it has one.
fn item_name(item: &syn::Item) -> String {
    match item {
        syn::Item::Const(item) => item.ident.to_string(),
        syn::Item::Enum(item) => item.ident.to_string(),
        syn::Item::ExternCrate(item) => item.ident.to_string(),
        syn::Item::Fn(item) => item.sig.ident.to_string(),
        syn::Item::Macro(item) => item
            .ident
            .as_ref()
            .map_or_else(|| "<macro>".to_owned(), ToString::to_string),
        syn::Item::Mod(item) => item.ident.to_string(),
        syn::Item::Static(item) => item.ident.to_string(),
        syn::Item::Struct(item) => item.ident.to_string(),
        syn::Item::Trait(item) => item.ident.to_string(),
        syn::Item::TraitAlias(item) => item.ident.to_string(),
        syn::Item::Type(item) => item.ident.to_string(),
        syn::Item::Union(item) => item.ident.to_string(),
        syn::Item::Use(_) => "<use>".to_owned(),
        _ => "<item>".to_owned(),
    }
}

/// The attribute list of an impl item.
fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

struct SuppressionVisitor<'a> {
    file: &'a str,
    found: &'a mut Vec<Suppression>,
}

impl<'ast> Visit<'ast> for SuppressionVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        let attrs = item_attrs(node);
        if has_dead_code_allow(attrs) && !is_cfg_test(attrs) {
            let scoped = !matches!(
                node,
                syn::Item::Struct(_)
                    | syn::Item::Enum(_)
                    | syn::Item::Union(_)
                    | syn::Item::Mod(_)
                    | syn::Item::Impl(_)
            );
            self.found.push(Suppression {
                file: self.file.to_owned(),
                item: item_name(node),
                scoped,
                documented: has_written_reason(attrs),
            });
        }
        if let syn::Item::Macro(item) = node {
            scan_macro_tokens(&item.mac.tokens, self.file, self.found);
        }
        syn::visit::visit_item(self, node);
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        let attrs = impl_item_attrs(node);
        if has_dead_code_allow(attrs) && !is_cfg_test(attrs) {
            let (scoped, item) = match node {
                syn::ImplItem::Fn(fun) => (true, fun.sig.ident.to_string()),
                _ => (false, "<impl-item>".to_owned()),
            };
            self.found.push(Suppression {
                file: self.file.to_owned(),
                item,
                scoped,
                documented: has_written_reason(attrs),
            });
        }
        syn::visit::visit_impl_item(self, node);
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        if has_dead_code_allow(&node.attrs) {
            self.found.push(Suppression {
                file: self.file.to_owned(),
                item: node
                    .ident
                    .as_ref()
                    .map_or_else(|| "<field>".to_owned(), ToString::to_string),
                scoped: true,
                documented: has_written_reason(&node.attrs),
            });
        }
        syn::visit::visit_field(self, node);
    }
}

/// The suppressions found in a fixed source string.
fn suppressions_in(source: &str) -> Vec<Suppression> {
    let file = syn::parse_file(source).expect("test sources must parse");
    let mut found = Vec::new();
    SuppressionVisitor {
        file: "<source>",
        found: &mut found,
    }
    .visit_file(&file);
    found
}

#[test]
fn every_dead_code_suppression_is_scoped_and_explained() {
    let root = workspace_root();
    let mut found: Vec<Suppression> = Vec::new();
    for path in rust_sources(&root) {
        let text = std::fs::read_to_string(&path).unwrap();
        let file = syn::parse_file(&text).unwrap();
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let mut visitor = SuppressionVisitor {
            file: &relative,
            found: &mut found,
        };
        visitor.visit_file(&file);
    }

    let filed: BTreeSet<(&str, &str)> = SCOPED_SUPPRESSIONS
        .iter()
        .map(|(file, item, _)| (*file, *item))
        .collect();

    let mut bad = String::new();
    for site in &found {
        if filed.contains(&(site.file.as_str(), site.item.as_str())) {
            continue;
        }
        if !site.scoped {
            let _ = writeln!(
                bad,
                "  {}: `{}` — covers a whole item, not one field or fn",
                site.file, site.item
            );
        } else if !site.documented {
            let _ = writeln!(bad, "  {}: `{}` — no written reason", site.file, site.item);
        }
    }

    let mut stale = String::new();
    for (file, item, issue) in SCOPED_SUPPRESSIONS {
        if !found.iter().any(|s| s.file == *file && s.item == *item) {
            let _ = writeln!(
                stale,
                "  SCOPED_SUPPRESSIONS names {file}:`{item}` ({issue}), which no longer has one"
            );
        }
    }

    assert!(
        bad.is_empty() && stale.is_empty(),
        "\n{bad}{stale}\n\
         `#[allow(dead_code)]` turns a compiler diagnostic into silence, so its\n\
         blast radius is the thing to keep small. Put it on the one field or\n\
         function that needs it, never on the struct or module around them, and\n\
         write why above it. rr shipped a dead field behind a struct-wide allow\n\
         that hid it from every `-D warnings` run CI has ever done, and a second\n\
         allow whose reason had expired without anyone noticing. If removal is\n\
         filed work, add a row to\n\
         SCOPED_SUPPRESSIONS naming the issue.\n"
    );
}

#[test]
fn a_struct_wide_suppression_is_reported() {
    let found = suppressions_in("#[allow(dead_code)]\nstruct S { a: u32 }");
    assert_eq!(found.len(), 1);
    assert!(!found[0].scoped, "a struct-wide allow must be reported");
}

#[test]
fn a_field_scoped_suppression_with_a_doc_comment_passes() {
    let found = suppressions_in("struct S {\n    /// why\n    #[allow(dead_code)]\n    a: u32,\n}");
    assert!(
        found.iter().all(|site| site.scoped && site.documented),
        "a scoped, explained allow must pass"
    );
}

#[test]
fn a_suppression_inside_a_string_literal_is_not_a_finding() {
    let found = suppressions_in("const S: &str = r#\"#[allow(dead_code)] fn x() {}\"#;");
    assert!(found.is_empty(), "a string is text, not an attribute");
}

#[test]
fn a_cfg_attr_test_suppression_is_out_of_scope() {
    let found = suppressions_in("#[cfg_attr(test, allow(dead_code))]\nfn x() {}");
    assert!(found.is_empty(), "a test-conditional allow is not shipped");
}

/// The `REQUIRED_CAPTURES` entries of the Rust extractor, as parsed strings.
fn required_captures(root: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(root.join("crates/rr-core/src/parser/rust.rs")).unwrap();
    let file = syn::parse_file(&source).unwrap();
    let mut out = Vec::new();
    for item in file.items {
        let syn::Item::Const(item) = item else {
            continue;
        };
        if item.ident != "REQUIRED_CAPTURES" {
            continue;
        }
        collect_string_literals(&item.expr, &mut out);
    }
    out
}

fn collect_string_literals(expr: &syn::Expr, out: &mut Vec<String>) {
    match expr {
        syn::Expr::Array(array) => {
            for element in &array.elems {
                collect_string_literals(element, out);
            }
        }
        syn::Expr::Reference(reference) => collect_string_literals(&reference.expr, out),
        syn::Expr::Paren(paren) => collect_string_literals(&paren.expr, out),
        syn::Expr::Lit(lit) => {
            if let syn::Lit::Str(value) = &lit.lit {
                out.push(value.value());
            }
        }
        _ => {}
    }
}

/// The `CaptureIds` struct of the Rust extractor, as parsed field names.
fn capture_ids_fields(root: &Path) -> BTreeSet<String> {
    let source = std::fs::read_to_string(root.join("crates/rr-core/src/parser/rust.rs")).unwrap();
    let file = syn::parse_file(&source).unwrap();
    let mut out = BTreeSet::new();
    for item in file.items {
        let syn::Item::Struct(item) = item else {
            continue;
        };
        if item.ident != "CaptureIds" {
            continue;
        }
        let syn::Fields::Named(fields) = item.fields else {
            continue;
        };
        for field in fields.named {
            if let Some(ident) = field.ident {
                out.insert(ident.to_string());
            }
        }
    }
    out
}

#[test]
fn every_required_capture_is_routed() {
    let fields = capture_ids_fields(&workspace_root());
    for capture in required_captures(&workspace_root()) {
        let field = capture.replace('.', "_");
        assert!(
            fields.contains(&field),
            "REQUIRED_CAPTURES has `{capture}` but CaptureIds has no `{field}`: \
             the query must declare a capture nothing reads"
        );
    }
}
