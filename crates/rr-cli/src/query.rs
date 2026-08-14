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
use rr_core::query::{parse_query, route_query, QueryRequest};
use rr_core::ranking::{RankingScratch, DEFAULT_RANKING_PROFILE};
use rr_core::render::{render_json, render_json_explained, render_text, render_text_explained};
use rr_core::result::QueryResult;
use rr_core::snapshot::{LoadOutcome, SnapshotStore};
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
    let (mut result, evidence) = route_query(
        &workspace.snapshot,
        &parsed,
        &DEFAULT_RANKING_PROFILE,
        &mut scratch,
    )
    .map_err(anyhow::Error::new)?;

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
                bail!("index is stale; run 'rr refresh'");
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
