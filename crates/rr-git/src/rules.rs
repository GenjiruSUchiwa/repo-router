//! Which files decide what discovery sees, and how a change to one is noticed.
//!
//! Discovery rules live in two places and neither can be watched by a delta.
//! Rules **outside** the working tree — `.git/info/exclude`, the user's global
//! ignore file — never appear in any status at all. Rules **inside** it do
//! appear, but only as "differs from `HEAD`", which an uncommitted rule file
//! says for ever; treating that as "the rules just changed" would rebuild the
//! whole repository on every run for as long as the file stayed uncommitted.
//!
//! So both go into one content digest, stored in the snapshot and compared on
//! the next run. A rule change is then a *difference between two contents*
//! rather than a *mention in a delta*, which is the only form of the question
//! that settles once the change has been absorbed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rr_core::path::RelPath;
use rr_core::refresh::DiscoveryIdentity;
use rr_core::walk::WalkCfg;

use crate::repo::RepoState;
use crate::GitRepo;

/// File names that change what discovery collects, wherever they appear.
///
/// `.gitignore` and `.ignore` change which paths are visible. `.gitmodules`
/// changes which paths are submodules, and therefore which subtrees belong to
/// another corpus. `.gitattributes` changes the clean filter, and therefore the
/// bytes a path resolves to — a different question with the same answer, since
/// both mean the previous run's conclusions no longer hold.
const RULE_FILE_NAMES: &[&str] = &[".gitignore", ".ignore", ".gitattributes", ".gitmodules"];

/// Whether a change to this path invalidates the previous run's discovery.
///
/// Matches on the final component, because these files apply per-directory and
/// a nested one is exactly as authoritative as the root one.
#[must_use]
pub fn is_rule_path(path: &RelPath) -> bool {
    let name = path
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or_else(|| path.as_str());
    RULE_FILE_NAMES.contains(&name)
}

/// Computes the identity of everything that decides what discovery collects.
///
/// Two runs that agree on this value would have walked the same paths. Two runs
/// that disagree might not have, so the caller must rebuild in full rather than
/// trust a delta that was computed under different rules.
///
/// `observed` supplies the rule files inside the working tree that currently
/// differ from `HEAD`. Only those are read: a rule file that matches `HEAD` is
/// already covered by the commit the snapshot records, and reading every rule
/// file in the repository would cost the walk this digest exists to avoid.
#[must_use]
pub fn discovery_digest(
    repo: Option<&GitRepo>,
    cfg: &WalkCfg,
    observed: Option<&RepoState>,
) -> [u8; 32] {
    let mut identity = DiscoveryIdentity::new(cfg);
    for (label, path) in external_rule_files(repo) {
        // A rule file that is absent is a fact about the rules, not a failure:
        // it must hash differently from the same file existing but empty, which
        // `mix_rule_file` distinguishes by taking an `Option` rather than a
        // possibly-empty slice. An unreadable file is treated as absent, which
        // is the same conclusion the walker itself reaches.
        let contents = path.as_deref().and_then(|path| std::fs::read(path).ok());
        identity.mix_rule_file(label, contents.as_deref());
    }

    if let (Some(repo), Some(observed)) = (repo, observed) {
        for path in dirty_rule_paths(observed) {
            let contents = std::fs::read(repo.workdir().join(path.as_str())).ok();
            // A deleted rule file reads as absent, which is what it now is.
            identity.mix_rule_file(&format!("worktree:{path}"), contents.as_deref());
        }
    }
    identity.finish()
}

/// The in-tree rule files an observation reported, in one sorted order.
///
/// A `BTreeSet` rather than the observation's own order: rename sources are
/// folded in alongside targets, and the digest must not depend on which of the
/// two a status happened to list first.
fn dirty_rule_paths(observed: &RepoState) -> BTreeSet<&RelPath> {
    observed
        .changes
        .iter()
        .flat_map(|change| std::iter::once(&change.path).chain(change.source.as_ref()))
        .filter(|path| is_rule_path(path))
        .collect()
}

/// The rule files that no status can report, in a fixed order.
///
/// The order is part of the digest, so it is spelled out here once rather than
/// left to whatever order the sources happen to be discovered in.
fn external_rule_files(repo: Option<&GitRepo>) -> [(&'static str, Option<PathBuf>); 3] {
    let git_dir = repo.map(|repo| repo.gix_repo().git_dir().to_path_buf());
    let info = |name: &str| git_dir.as_ref().map(|dir| dir.join("info").join(name));
    [
        ("git-info-exclude", info("exclude")),
        ("git-info-attributes", info("attributes")),
        ("core-excludes-file", global_excludes_file(repo)),
    ]
}

/// Resolves the user's global ignore file the way the walker's own resolution
/// does: `core.excludesFile` when configured, and the XDG default otherwise.
fn global_excludes_file(repo: Option<&GitRepo>) -> Option<PathBuf> {
    let configured = repo.and_then(|repo| {
        let config = repo.gix_repo().config_snapshot();
        config.trusted_path("core.excludesFile").ok().flatten()
    });
    configured.or_else(xdg_git_ignore)
}

/// `$XDG_CONFIG_HOME/git/ignore`, falling back to `$HOME/.config/git/ignore`.
fn xdg_git_ignore() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(base.join("git").join("ignore"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(path).unwrap()
    }

    #[test]
    fn nested_rule_files_are_recognized_like_root_ones() {
        assert!(is_rule_path(&rel(".gitignore")));
        assert!(is_rule_path(&rel("crates/rr-core/.gitignore")));
        assert!(is_rule_path(&rel("deep/nested/.gitattributes")));
    }

    #[test]
    fn ordinary_source_files_are_not_rule_files() {
        assert!(!is_rule_path(&rel("src/lib.rs")));
        assert!(!is_rule_path(&rel("gitignore")));
        // A file whose name merely ends with a rule name is a different file.
        assert!(!is_rule_path(&rel("not.gitignore")));
        assert!(!is_rule_path(&rel("src/.gitignore.bak")));
    }

    #[test]
    fn a_directory_named_like_a_rule_file_does_not_match_its_children() {
        assert!(!is_rule_path(&rel(".gitignore/child.rs")));
    }

    #[test]
    fn the_digest_is_stable_across_repeated_computation() {
        let cfg = WalkCfg::default();
        assert_eq!(
            discovery_digest(None, &cfg, None),
            discovery_digest(None, &cfg, None)
        );
    }
}
