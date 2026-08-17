//! The `rr query` command: route once, optionally verify source, render once.
//!
//! Verification is a strict sequence and every step can only refuse or fail
//! closed, so it lives here as one readable function rather than spread across
//! the renderer or the acquisition layer. `rr-core` decides *what* the answer
//! is; `rr-git` decides *what the file says*; this module is the only place
//! that puts the two together.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use rr_core::index::Snapshot;
use rr_core::path::RelPath;
use rr_core::query::{parse_query, resolve_route_anchor, route_query, QueryRequest};
use rr_core::ranking::{RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::render::{
    encode_anchor, render_json, render_json_explained, render_text, render_text_explained,
};
use rr_core::result::{resolve_anchor, Candidate, Pipeline, QueryResult, TargetId};
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
use rr_core::text::{
    encode_map_destination, load_routes, projected_map_catalog, update_routes, RouteKey,
    RouteRecord, DEFAULT_MAP_BUDGET,
};
use rr_core::verify::{
    finish_source, resolve_indexed_source, verify_source, PendingSource, SourceResult,
};
use rr_git::{acquire_for_source, revalidate_source, AcquireOutcome, GitRepo};

use crate::output::Output;

#[derive(clap::Args, Debug)]
pub struct Args {
    #[arg(long, value_name = "REL_PATH")]
    path: Option<RelPath>,
    #[arg(long)]
    json: bool,
    /// Report the work the ranker did, including whether the candidate cap
    /// discarded members it could not tell apart from the ones it kept.
    #[arg(long)]
    explain: bool,
    /// Return the anchor's own source, verified against the indexed content
    /// identity, when routing lands on exactly one direct anchor.
    #[arg(long)]
    source: bool,
    query: String,
}

/// Runs one query and returns its exit code.
///
/// # Errors
/// Returns an execution error for repository, snapshot, and I/O failures.
/// Expected worktree states are refusals inside the result, not errors here.
pub fn run(args: &Args) -> anyhow::Result<u8> {
    let workspace = Workspace::open()?;

    let request = QueryRequest::new(&args.query, args.path.as_ref());
    let parsed = parse_query(&workspace.snapshot, request).map_err(anyhow::Error::new)?;
    let mut scratch = RankingScratch::new();

    // A cache hit is only a shortcut, so it must cost less than the work it
    // skips: `route_lookup` reads one file and projects the snapshot, while
    // `route_query` on a miss ranks the whole corpus. A `--path` query is
    // neither read nor learned — the qualifier is part of the question and is
    // not part of the key, so caching one would let a later unqualified
    // question receive a qualified answer.
    let key = args
        .path
        .is_none()
        .then(|| RouteKey::new(&args.query))
        .flatten();
    let cached = key.as_ref().and_then(|key| route_lookup(&workspace, key));

    let (mut result, evidence) = match cached {
        Some(candidate) => (
            QueryResult::Direct {
                candidate,
                pipeline: Pipeline::Route,
                source: None,
            },
            None,
        ),
        None => route_query(
            &workspace.snapshot,
            &parsed,
            &DEFAULT_RANKING_PROFILE,
            &mut scratch,
        )
        .map_err(anyhow::Error::new)?,
    };

    if cached.is_none() {
        learn_route(&workspace, key.as_ref(), &result);
    }

    if args.source {
        attach_source(&workspace, &mut result)?;
    }

    let rendered = match (args.json, args.explain) {
        (true, false) => render_json(&workspace.snapshot, &result),
        (true, true) => render_json_explained(&workspace.snapshot, &result, evidence.as_ref()),
        (false, false) => render_text(&workspace.snapshot, &result),
        (false, true) => render_text_explained(&workspace.snapshot, &result, evidence.as_ref()),
    }
    .map_err(|err| anyhow::anyhow!("{err}"))?;

    Output::print_raw(&rendered).context("write query result")?;
    Ok(result.exit_code())
}

/// The repository and snapshot a query runs against, already agreed with each
/// other: a snapshot built elsewhere never reaches routing.
struct Workspace {
    repo: Option<GitRepo>,
    root: PathBuf,
    snapshot: Arc<Snapshot>,
}

impl Workspace {
    fn open() -> anyhow::Result<Self> {
        let current_dir = std::env::current_dir().context("resolve current directory")?;
        let canonical = current_dir
            .canonicalize()
            .context("canonicalize current directory")?;
        let repo = GitRepo::discover(&canonical).context("discover repository")?;

        let (root, head_oid) = match &repo {
            Some(repo) => (
                repo.workdir().to_path_buf(),
                repo.head_oid().context("resolve HEAD commit")?,
            ),
            None => (canonical, None),
        };

        let snapshot = match SnapshotStore::new(&root)
            .load()
            .map_err(|err| anyhow::anyhow!("{err}"))?
        {
            LoadOutcome::Ready(snapshot) => snapshot,
            LoadOutcome::Missing => bail!("index missing; run 'rr map'"),
            LoadOutcome::NeedsRebuild(_) => bail!("index invalid; run 'rr map'"),
        };

        if repo.is_some() {
            if snapshot.meta.no_git {
                bail!("index repository mismatch; run 'rr map'");
            }
            if snapshot.meta.repo_head_oid != head_oid {
                // The route cache is not slow in this window, it is
                // unreachable: routing never runs, so committing the maps and
                // then asking a question makes the cache look broken on the
                // first day of use. Whether this refusal stays at all is #44's
                // HEAD-to-HEAD fast path to decide, so the refusal names it
                // rather than quietly implementing an answer here.
                bail!("index is stale; run 'rr refresh' (until then no query is answered, cached or not; see #44)");
            }
        } else if !snapshot.meta.no_git {
            bail!("index repository mismatch; run 'rr map'");
        }

        Ok(Self {
            repo,
            root,
            snapshot,
        })
    }
}

/// The cached answer to this question, if there is one that is still true.
///
/// Three things have to hold, and each of them is a way a route dies: the
/// record exists, its anchor still names exactly one symbol, and the scope that
/// owns that symbol still hashes to what the record was learned against. The
/// third is the one that matters: an `api_hash` covers path, name, anchor name,
/// kind, visibility and signature of every public record in the directory, so a
/// renamed or re-signed neighbour retires this route without anybody having to
/// notice.
fn route_lookup(workspace: &Workspace, key: &RouteKey) -> Option<Candidate> {
    let (table, _) = load_routes(&workspace.root);
    let record = table.get(key)?;
    let symbol = resolve_route_anchor(&workspace.snapshot, &record.anchor)?;
    // Projected, not validated: the weaker guarantee is that the map may not be
    // on disk, which costs a reader one `MAP.md` they cannot open, against
    // reading every artifact in the repository on every query.
    let catalog = projected_map_catalog(&workspace.snapshot, DEFAULT_MAP_BUDGET).ok()?;
    let identity = catalog.owner(symbol)?;
    if identity.api_hash().digest() != record.api_hash {
        return None;
    }
    Some(Candidate::new(
        TargetId::Symbol(symbol),
        Some(record.confidence),
    ))
}

/// Records the answer, if it is the kind of answer that can be recorded.
///
/// Best-effort in the strongest sense: this returns `()` and every failure
/// inside it is dropped. A query that answered correctly and then could not
/// write its cache has still answered correctly, and turning that into a
/// non-zero exit would make the cache a liability.
fn learn_route(workspace: &Workspace, key: Option<&RouteKey>, result: &QueryResult) {
    let Some(key) = key else { return };
    let QueryResult::Direct { candidate, .. } = result else {
        return;
    };
    // Only a symbol: a file target has no owning scope and so could never be
    // told it went stale.
    let TargetId::Symbol(symbol) = candidate.target else {
        return;
    };
    let Some(confidence) = candidate.confidence else {
        return;
    };
    let Ok(catalog) = projected_map_catalog(&workspace.snapshot, DEFAULT_MAP_BUDGET) else {
        return;
    };
    let Some(identity) = catalog.owner(symbol) else {
        return;
    };
    let Ok(anchor) = resolve_anchor(&workspace.snapshot, candidate.target) else {
        return;
    };

    let record = RouteRecord {
        key: key.clone(),
        // The spelling `rr query` prints, so what is cached and what was shown
        // are one string.
        anchor: encode_anchor(anchor.path, anchor.symbol),
        map: encode_map_destination(identity.path().as_str()),
        api_hash: identity.api_hash().digest(),
        confidence,
    };
    let _ = update_routes(&workspace.root, |table| table.insert(record));
}

/// Verifies and attaches the anchor's source, in the one order that keeps
/// staleness ahead of format diagnosis and content behind the final check.
///
/// A result that is not a single direct anchor is left untouched, so asking for
/// source can never turn candidates or a no-match into filesystem work.
fn attach_source(workspace: &Workspace, result: &mut QueryResult) -> anyhow::Result<()> {
    let QueryResult::Direct {
        candidate, source, ..
    } = result
    else {
        return Ok(());
    };

    let indexed = resolve_indexed_source(&workspace.snapshot, candidate.target)
        .map_err(anyhow::Error::new)
        .context("resolve indexed source")?;
    let path = indexed.path();

    let acquired = acquire_for_source(workspace.repo.as_ref(), &workspace.root, path)
        .with_context(|| format!("acquire source for {}", path.as_str()))?;

    let pending = match verify_source(&indexed, acquired.as_source())
        .map_err(anyhow::Error::new)
        .with_context(|| format!("verify source for {}", path.as_str()))?
    {
        PendingSource::Refused(status) => {
            *source = Some(SourceResult::Refused { status });
            return Ok(());
        }
        PendingSource::Pending(pending) => pending,
    };

    // A pending packet only exists for an acquisition that carried content, so
    // this holds; failing closed is still cheaper than assuming it.
    let AcquireOutcome::Acquired(content) = &acquired else {
        bail!("verified source for {} without content", path.as_str());
    };

    // The packet exists but cannot be read yet: only a fresh final check
    // releases its content.
    let final_state = revalidate_source(workspace.repo.as_ref(), &workspace.root, path, content)
        .with_context(|| format!("revalidate source for {}", path.as_str()))?;

    *source = Some(finish_source(pending, final_state));
    Ok(())
}
