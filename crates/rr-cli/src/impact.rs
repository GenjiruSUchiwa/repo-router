//! `rr impact` at the command line.
//!
//! This is the composition site, and composition is all it does. `rr-git`
//! resolves the two endpoints and reports the deltas between them, `rr-core`
//! overlays those deltas onto an index and traverses the resolved edges,
//! `rr-git` reads the bounded co-change window, and `rr-core` renders one
//! canonicalized value twice. Nothing here decides what an edge is, what a
//! source file is, or what a hunk touches: every such answer has an owner, and a
//! second answer in a command would be a second contract nobody validates.
//!
//! # Why the exit code does not come through `main`'s `finish`
//!
//! `finish` maps every failure to `1`. This command cannot use it, because `1`
//! is already the verdict for *a report was printed and part of it is missing*.
//! A caller that read `1` as a crash would treat a raced working tree as one. So
//! a failed invocation leaves by `2` — the code `clap` returns for a mistyped
//! flag — because both mean the same thing to a script: the command never got as
//! far as a report. The two verdicts that *are* reports keep `0` and `1`.
//!
//! Ranges live in the `value_parser` rather than in the body for the same
//! reason: `--depth 9` is then `clap`'s usage error with `clap`'s exit code,
//! instead of a hand-written refusal that a caller has to learn separately.
//!
//! SIGPIPE is restored to its default disposition once, in `main`. A consumer
//! that stops reading therefore kills the process rather than reaching any code
//! here, which is why this file contains no write-error handling for stdout
//! beyond letting the failure travel.
//!
//! # What each endpoint is read from
//!
//! An endpoint is a whole corpus, not a diff: the reverse closure asks who
//! points at a changed definition, and that question is about every file in the
//! repository. So each endpoint gets its own index, built from the working tree
//! for the paths the two endpoints agree about and from the endpoint's own blobs
//! for the paths they do not.
//!
//! Reading the deltas by object id is what makes the base side true. A modified
//! file's `HEAD` bytes are in the object database and nowhere else — the working
//! tree has moved on from them — so a base overlay built by re-reading the
//! working tree would describe the target twice and report no change at all.
//!
//! One approximation is left, and it is confined to `--head`. Where neither
//! endpoint is the working tree, the paths no delta names are read from the
//! working tree anyway: the two endpoints hold identical bytes for such a path,
//! this command can address it by path, and the report is about the difference
//! between the endpoints rather than about that ambient copy. A path either
//! endpoint actually differs on is never read that way — it is in the change
//! set, and the change set is read by oid.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Args as ClapArgs;
use rr_core::cache::CacheOutcome;
use rr_core::cancel::CancelToken;
use rr_core::facts::{DegradedReason, Facts};
use rr_core::impact::{
    impact, overlay, render_impact_json, render_impact_text, EndpointJson, FileChange, HunkRange,
    ImpactRequest, Side, DEFAULT_DEPTH, DEFAULT_LIMIT, MAX_DEPTH, MAX_LIMIT,
};
use rr_core::index::{ContentRepresentation, FileInput, SnapshotBuilder};
use rr_core::lang::Lang;
use rr_core::parser::{degraded_facts, Registry};
use rr_core::path::RelPath;
use rr_core::walk::{collected_lang, discover, WalkCfg};
use rr_core::{CacheKey, FactCache, Oid};
use rr_git::diff::{resolve_target, worktree_target, ChangeSet, ChangeTarget, FileDelta};
use rr_git::map::BuildContext;
use rr_git::{change_set, ContentProbe, GitRepo};

use crate::output::Output;

/// The exit code for an invocation that never reached a report.
///
/// Shared with `clap` on purpose; the module documentation holds the argument.
pub const NOT_EVALUATED: u8 = 2;

/// Parser threads one impact run uses.
///
/// One, which is what `rr refresh` uses when nobody says otherwise. A knob here
/// would be a third spelling of the same decision, and the report does not
/// change with it: the inputs are assembled in discovery order however many
/// threads produced them.
const THREADS: usize = 1;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Repository to inspect. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Revision the comparison starts from.
    #[arg(long, default_value = "HEAD")]
    pub base: String,
    /// Target endpoint; compares committed trees only.
    #[arg(long, conflicts_with = "worktree")]
    pub head: Option<String>,
    /// Compare against the working tree. The default.
    #[arg(long)]
    pub worktree: bool,
    /// Traversal depth; 0 reports incident edges without a closure.
    #[arg(
        long,
        default_value_t = DEFAULT_DEPTH,
        value_parser = clap::value_parser!(u8).range(0..=i64::from(MAX_DEPTH))
    )]
    pub depth: u8,
    /// Entries rendered per category; never truncates computation.
    #[arg(
        long,
        default_value_t = DEFAULT_LIMIT,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_LIMIT))
    )]
    pub limit: u32,
    /// Emit the report as one JSON object instead of the human report.
    #[arg(long)]
    pub json: bool,
}

/// Runs `rr impact` and returns the process exit code.
///
/// Deliberately not `anyhow::Result<u8>` like every other command: routing a
/// failure through `main`'s `finish` would spend `1` on it, and `1` is a report.
/// A failure prints one line on stderr and leaves by [`NOT_EVALUATED`].
#[must_use]
pub fn run(args: &Args) -> ExitCode {
    match report(args) {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            crate::diagnose(&format!("rr: impact: {}", crate::one_line(&err)));
            ExitCode::from(NOT_EVALUATED)
        }
    }
}

/// Resolves the endpoints, builds the two overlays, traverses, and renders.
///
/// The order is the contract. The endpoints are resolved before anything is
/// read, so two runs of one comparison read the same bytes; the change set is
/// taken once; both overlays are built from that one observation; the result is
/// canonicalized by [`impact`] before either renderer sees it, so the text and
/// the JSON cannot disagree about order.
///
/// # Errors
/// Returns the failures of resolving a revision, comparing the endpoints,
/// reading the working tree, indexing an overlay and writing the report. A
/// report that is merely incomplete is not among them: that is `status:
/// "partial"` and exit `1`.
fn report(args: &Args) -> anyhow::Result<u8> {
    let root = match args.root.clone() {
        Some(root) => root,
        None => std::env::current_dir().context("resolve current directory")?,
    };
    let context = BuildContext::open(&root, THREADS).context("open the repository")?;
    let repo = context
        .repo()
        .context("open the repository")?
        .ok_or_else(|| anyhow::anyhow!("not a Git repository"))?;

    let cancel = CancelToken::new();
    let base = resolve_target(&repo, &args.base).context("resolve the base endpoint")?;
    let target = match args.head.as_deref() {
        Some(spec) => resolve_target(&repo, spec).context("resolve the target endpoint")?,
        None => worktree_target(&repo).context("resolve the working tree")?,
    };
    let changes = change_set(&repo, &base, &target, &context.walk, &cancel)
        .context("compare the two endpoints")?;

    let cache = FactCache::open(&context.work_root).context("open the fact cache")?;
    let ambient = ambient_corpus(&context, &cache)?;
    let mut endpoints = Endpoints::new(&repo, &context.walk, &cache);
    let base_inputs = endpoints.inputs(&ambient, &changes, Side::Base)?;
    let target_inputs = endpoints.inputs(&ambient, &changes, Side::Target)?;

    let stamp = stamp(&context, &ambient)?;
    let base_snapshot = overlay(&stamp, base_inputs).context("index the base endpoint")?;
    let target_snapshot = overlay(&stamp, target_inputs).context("index the target endpoint")?;

    let seeds: Vec<RelPath> = changes
        .files
        .iter()
        .map(|delta| delta.path.clone())
        .collect();
    let travelled_with: Vec<RelPath> =
        rr_git::co_changed(&repo, &target, &seeds, &context.walk, &cancel)
            .context("read the co-change window")?
            .into_keys()
            .collect();
    let edits: Vec<FileChange> = changes.files.iter().map(file_change).collect();

    let result = impact(&ImpactRequest {
        base: endpoint_json(&base),
        target: endpoint_json(&target),
        base_snapshot: &base_snapshot,
        target_snapshot: &target_snapshot,
        changes: &edits,
        raced: &changes.raced,
        conflicted: &changes.conflicted,
        co_changed: &travelled_with,
        depth: args.depth,
    })
    .context("traverse the two endpoints")?;

    if args.json {
        Output::print_text(&render_impact_json(&result).context("render the report as JSON")?)?;
    } else {
        Output::print_text(render_impact_text(&result, args.limit).trim_end())?;
    }
    Ok(result.exit_code())
}

/// Every file the working tree holds, indexed the way `rr refresh` indexes it.
///
/// The same discovery, the same pipeline and the same fact cache, because an
/// overlay whose corpus disagreed with the published index about which files
/// exist would report edges no `rr query` could ever follow. Keyed by path so
/// the deltas can replace entries by name.
fn ambient_corpus(
    context: &BuildContext,
    cache: &FactCache,
) -> anyhow::Result<BTreeMap<RelPath, FileInput>> {
    let files = discover(&context.work_root, &context.walk).context("discover source files")?;
    let built = context
        .run(&files, |worker, source| worker.process(source, cache))
        .context("read the working tree")?;
    Ok(built
        .inputs()
        .map(|input| (input.path.clone(), input.clone()))
        .collect())
}

/// The snapshot both overlays take their metadata from.
///
/// [`overlay`] stamps an endpoint with a published snapshot's metadata, and this
/// run publishes nothing — so what it needs is metadata that is *consistent*
/// with this binary, not metadata that is fresh. It is carried by an empty
/// snapshot rather than assembled twice: [`BuildContext::meta_for`] is the owner
/// of that computation, and an index built under two different stamps would make
/// the builder's own validation meaningless.
fn stamp(
    context: &BuildContext,
    ambient: &BTreeMap<RelPath, FileInput>,
) -> anyhow::Result<rr_core::index::Snapshot> {
    let indexed: BTreeSet<&RelPath> = ambient.keys().collect();
    let meta = context
        .meta_for(&indexed, &[], None)
        .context("stamp the overlay metadata")?;
    let (snapshot, _counts) = SnapshotBuilder::new(meta)
        .build(Vec::new())
        .context("stamp the overlay metadata")?;
    Ok(snapshot)
}

/// One endpoint's inputs, and the reader that gets the bytes it needs.
///
/// The extractor registry and the fact cache are held across both endpoints:
/// the same blob is often on both sides of a rename, and building a second
/// registry would parse it twice.
struct Endpoints<'run> {
    repo: &'run GitRepo,
    walk: &'run WalkCfg,
    cache: &'run FactCache,
    registry: Registry,
}

impl<'run> Endpoints<'run> {
    /// A reader for both endpoints of one comparison.
    fn new(repo: &'run GitRepo, walk: &'run WalkCfg, cache: &'run FactCache) -> Self {
        Self {
            repo,
            walk,
            cache,
            registry: Registry::new(),
        }
    }

    /// The whole corpus one endpoint holds.
    ///
    /// The ambient corpus with the endpoint's own deltas applied: a path the
    /// endpoint does not hold is removed, and a path whose bytes differ from the
    /// working tree's is re-read from the object database by the oid the delta
    /// named. A working-tree endpoint is returned unchanged, because the ambient
    /// corpus *is* that endpoint — its deltas' bytes are not in the object
    /// database at all, and asking for them by oid would fail on the one
    /// endpoint that needs no lookup.
    ///
    /// # Errors
    /// Returns object-database read and extraction failures.
    fn inputs(
        &mut self,
        ambient: &BTreeMap<RelPath, FileInput>,
        changes: &ChangeSet,
        side: Side,
    ) -> anyhow::Result<Vec<FileInput>> {
        let mut inputs = ambient.clone();
        if matches!(endpoint(changes, side), ChangeTarget::Worktree { .. }) {
            return Ok(inputs.into_values().collect());
        }
        for delta in &changes.files {
            let path = side_path(delta, side);
            match side_oid(delta, side) {
                None => {
                    inputs.remove(path);
                }
                Some(oid) => {
                    if ambient.get(path).is_some_and(|input| input.oid == oid) {
                        continue;
                    }
                    let generated = ambient.get(path).is_some_and(|input| input.generated);
                    if let Some(input) = self.input(path, oid, generated)? {
                        inputs.insert(path.clone(), input);
                    }
                }
            }
        }
        Ok(inputs.into_values().collect())
    }

    /// One committed file's input, read by the oid that names its bytes.
    ///
    /// `None` for a path discovery would not collect: a changed `Cargo.toml` is
    /// a fact about the repository and belongs in `changed_files`, but it has no
    /// extractor and therefore no definitions, and inventing a language for it
    /// would put lexical noise into a structural report.
    ///
    /// # Errors
    /// Returns object-database read and extraction failures.
    fn input(
        &mut self,
        path: &RelPath,
        oid: Oid,
        generated: bool,
    ) -> anyhow::Result<Option<FileInput>> {
        let Some(language) = collected_lang(path.as_str(), self.walk) else {
            return Ok(None);
        };
        let facts = self.facts(path, oid, language)?;
        Ok(Some(FileInput {
            path: path.clone(),
            oid,
            representation: ContentRepresentation::GitCanonical,
            generated,
            language,
            parse_status: facts.status(),
            facts,
        }))
    }

    /// The facts for exactly the bytes `oid` names.
    ///
    /// The fact cache first, because a refresh has usually already parsed the
    /// committed side of every file and the entry is keyed by the same oid,
    /// extractor version and fact schema this run would parse under. A miss, a
    /// corrupt entry and an unreadable cache all lead to the same work, so they
    /// lead to the same branch: the blob is read and parsed. A cache write that
    /// fails is ignored for the same reason — the facts are already in hand, and
    /// the cache is rebuildable by definition.
    ///
    /// # Errors
    /// Returns the object-database read failure and the extractor's own errors.
    fn facts(&mut self, path: &RelPath, oid: Oid, language: Lang) -> anyhow::Result<Facts> {
        let key = CacheKey::new(oid, language);
        if let Ok(CacheOutcome::Hit(facts)) = self.cache.get::<Facts>(&key) {
            return Ok(facts);
        }
        let content = self
            .repo
            .acquire_content(path, ContentProbe::CleanGitBlob(oid))
            .with_context(|| format!("read {path} at {}", oid.to_hex()))?
            .ok_or_else(|| anyhow::anyhow!("object {} is not in this repository", oid.to_hex()))?;
        let facts = match self.registry.for_lang(language) {
            Some(Ok(extractor)) => extractor
                .extract(&content.bytes)
                .with_context(|| format!("extract facts for {path}"))?,
            Some(Err(message)) => anyhow::bail!("{language} extractor unavailable: {message}"),
            None => degraded_facts(&content.bytes, DegradedReason::NoExtractor),
        };
        if facts.status().is_cacheable() {
            let _ = self.cache.put(&key, &facts);
        }
        Ok(facts)
    }
}

/// Which endpoint of a comparison one side names.
const fn endpoint(changes: &ChangeSet, side: Side) -> &ChangeTarget {
    match side {
        Side::Base => &changes.base,
        Side::Target => &changes.target,
    }
}

/// The path one delta has on one side, which a rename makes differ.
fn side_path(delta: &FileDelta, side: Side) -> &RelPath {
    match side {
        Side::Base => delta.source.as_ref().unwrap_or(&delta.path),
        Side::Target => &delta.path,
    }
}

/// The bytes one delta names on one side; `None` when that side has none.
const fn side_oid(delta: &FileDelta, side: Side) -> Option<Oid> {
    match side {
        Side::Base => delta.base_oid,
        Side::Target => delta.target_oid,
    }
}

/// How the report names one endpoint.
fn endpoint_json(target: &ChangeTarget) -> EndpointJson {
    match target {
        ChangeTarget::Tree { spec, commit } => EndpointJson::tree(spec, commit),
        ChangeTarget::Worktree { head } => EndpointJson::worktree(head.commit().as_ref()),
    }
}

/// One delta, in the vocabulary the graph layer reads deltas with.
///
/// [`FileDelta::kind`] is deliberately dropped: [`FileChange`] spells presence
/// with the two object ids instead, so impact cannot disagree with the delta it
/// was handed about which endpoint holds what. Translating the kind as well
/// would create a second, redundant way to say the same thing.
fn file_change(delta: &FileDelta) -> FileChange {
    FileChange {
        path: delta.path.clone(),
        source: delta.source.clone(),
        base_oid: delta.base_oid,
        target_oid: delta.target_oid,
        hunks: delta
            .hunks
            .iter()
            .map(|hunk| HunkRange {
                old_start: hunk.old_start,
                old_lines: hunk.old_lines,
                new_start: hunk.new_start,
                new_lines: hunk.new_lines,
            })
            .collect(),
    }
}
