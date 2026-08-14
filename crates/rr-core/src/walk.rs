use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ignore::overrides::OverrideBuilder;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

use crate::lang::Lang;
use crate::path::RelPath;
use crate::{Error, Result};

/// Default excluded directory patterns.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".rr",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "vendor",
];

/// Configuration for repository traversal.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct WalkCfg {
    /// Additional exclusion globs following gitignore semantics.
    ///
    /// Patterns without `!` are treated as ignore rules (e.g. `*.gen.rs` or `logs/`).
    /// Patterns prefixed with `!` are treated as un-ignore / whitelist rules (e.g. `!important.gen.rs`).
    pub custom_excludes: Vec<String>,
    /// Whether to apply [`DEFAULT_EXCLUDES`].
    pub use_default_excludes: bool,
    /// Whether to respect `.gitignore`, `.ignore`, and global gitignore rules.
    pub standard_filters: bool,
    /// Language whitelist filter; `None` means all recognized languages.
    pub languages: Option<Vec<Lang>>,
    /// Whether to follow symbolic links.
    pub follow_symlinks: bool,
    /// Maximum number of files to return (`None` for unlimited).
    pub max_files: Option<usize>,
    /// Number of worker threads (`None` or `0` for auto).
    pub threads: Option<usize>,
    /// Whether to sniff file content for generated headers (`@generated` / `DO NOT EDIT`).
    /// Path-based heuristics are always checked. Defaults to `true`.
    pub detect_generated: bool,
}

impl Default for WalkCfg {
    fn default() -> Self {
        Self {
            custom_excludes: Vec::new(),
            use_default_excludes: true,
            standard_filters: true,
            languages: None,
            follow_symlinks: false,
            max_files: None,
            threads: None,
            detect_generated: true,
        }
    }
}

/// A classified source file discovered during repository traversal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceFile {
    /// Path relative to the repository root, normalized with `/`.
    pub path: RelPath,
    /// Detected language.
    pub lang: Lang,
    /// Whether the file is detected as generated code.
    pub generated: bool,
}

impl SourceFile {
    /// Creates a new [`SourceFile`].
    #[must_use]
    pub fn new(path: RelPath, lang: Lang, generated: bool) -> Self {
        Self {
            path,
            lang,
            generated,
        }
    }
}

/// Checks if a file is generated based on path/file-name heuristics or content sniffing.
///
/// Path heuristics match:
/// - Directory component named `generated` (e.g. `src/generated/foo.rs`)
/// - File name starting with `generated.` (e.g. `generated.rs`)
/// - File name with stem ending in `_generated` or `.generated` (e.g. `models_generated.rs`, `schema.generated.ts`)
/// - File name containing `.pb.` (e.g. `service.pb.rs`, `api.pb.go`)
/// - File name containing `_pb2` (e.g. `user_pb2.py`, `user_pb2_grpc.py`)
///
/// Content heuristics scan up to the first 5 non-empty lines (within the first 2048 bytes):
/// - Line contains `@generated` or `DO NOT EDIT` (case-insensitive).
#[must_use]
pub fn is_generated(rel_path: &str, full_path: Option<&Path>) -> bool {
    let path_obj = Path::new(rel_path);

    // 1. Check directory components for a segment named "generated"
    if let Some(parent) = path_obj.parent() {
        for comp in parent.components() {
            if let Component::Normal(c) = comp {
                if c.to_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("generated"))
                {
                    return true;
                }
            }
        }
    }

    // 2. Check file name markers
    if let Some(file_name) = path_obj.file_name().and_then(|n| n.to_str()) {
        let name_lower = file_name.to_ascii_lowercase();

        if name_lower.contains(".pb.")
            || name_lower.contains("_pb2")
            || name_lower.contains(".generated.")
            || name_lower.starts_with("generated.")
        {
            return true;
        }

        let stem = match name_lower.rfind('.') {
            Some(idx) => &name_lower[..idx],
            None => &name_lower,
        };

        if stem == "generated" || stem.ends_with("_generated") || stem.ends_with(".generated") {
            return true;
        }
    }

    // 3. Content heuristics: scan up to first 5 non-empty lines within 2048 bytes
    if let Some(path) = full_path {
        if let Ok(file) = File::open(path) {
            let mut reader = BufReader::new(file.take(2048));
            let mut line = String::new();
            let mut non_empty_count = 0;
            while non_empty_count < 5 {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                non_empty_count += 1;
                if trimmed.contains("@generated")
                    || trimmed.to_ascii_uppercase().contains("DO NOT EDIT")
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Classifies a directory entry into a [`SourceFile`], if it represents a candidate source file.
#[must_use]
pub fn classify_entry(root: &Path, entry: &DirEntry, cfg: &WalkCfg) -> Option<SourceFile> {
    let file_type = entry.file_type()?;
    if !file_type.is_file() {
        return None;
    }

    let full_path = entry.path();
    let rel_path = RelPath::from_path(root, full_path).ok()?;

    // Detect language from file path/extension
    let lang = Lang::from_path(full_path)?;

    // Filter by allowed languages if specified
    if let Some(allowed) = &cfg.languages {
        if !allowed.contains(&lang) {
            return None;
        }
    }

    // Fast path check first (no file I/O)
    let generated = if is_generated(rel_path.as_str(), None) {
        true
    } else if cfg.detect_generated {
        is_generated(rel_path.as_str(), Some(full_path))
    } else {
        false
    };

    Some(SourceFile {
        path: rel_path,
        lang,
        generated,
    })
}

fn is_loop_error(err: &ignore::Error) -> bool {
    match err {
        ignore::Error::Loop { .. } => true,
        ignore::Error::Io(io_err) => {
            if io_err.raw_os_error() == Some(62) || io_err.raw_os_error() == Some(40) {
                return true;
            }
            let msg = io_err.to_string().to_ascii_lowercase();
            msg.contains("symbolic link") || msg.contains("loop")
        }
        ignore::Error::WithDepth { err, .. }
        | ignore::Error::WithPath { err, .. }
        | ignore::Error::WithLineNumber { err, .. } => is_loop_error(err),
        _ => false,
    }
}

/// Checks if a directory entry matches default exclusion rules.
fn is_excluded_dir(entry: &DirEntry, cfg: &WalkCfg) -> bool {
    if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }

    let file_name = entry.file_name().to_str().unwrap_or("");

    if cfg.use_default_excludes && DEFAULT_EXCLUDES.contains(&file_name) {
        return true;
    }

    false
}

struct VisitorCollector {
    root: PathBuf,
    cfg: WalkCfg,
    tx: crossbeam_channel::Sender<SourceFile>,
    err_tx: crossbeam_channel::Sender<ignore::Error>,
    collected_count: Arc<AtomicUsize>,
}

impl ParallelVisitor for VisitorCollector {
    fn visit(&mut self, entry: std::result::Result<DirEntry, ignore::Error>) -> WalkState {
        if let Some(max) = self.cfg.max_files {
            if self.collected_count.load(Ordering::Relaxed) >= max {
                return WalkState::Quit;
            }
        }

        match entry {
            Ok(dir_entry) => {
                if is_excluded_dir(&dir_entry, &self.cfg) {
                    return WalkState::Skip;
                }

                if let Some(source_file) = classify_entry(&self.root, &dir_entry, &self.cfg) {
                    let _ = self.tx.send(source_file);
                    if let Some(max) = self.cfg.max_files {
                        if self.collected_count.fetch_add(1, Ordering::Relaxed) + 1 >= max {
                            return WalkState::Quit;
                        }
                    }
                }
                WalkState::Continue
            }
            Err(err) => {
                if !is_loop_error(&err) {
                    let _ = self.err_tx.send(err);
                }
                WalkState::Continue
            }
        }
    }
}

struct VisitorBuilderImpl {
    root: PathBuf,
    cfg: WalkCfg,
    tx: crossbeam_channel::Sender<SourceFile>,
    err_tx: crossbeam_channel::Sender<ignore::Error>,
    collected_count: Arc<AtomicUsize>,
}

impl<'s> ParallelVisitorBuilder<'s> for VisitorBuilderImpl {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(VisitorCollector {
            root: self.root.clone(),
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
            err_tx: self.err_tx.clone(),
            collected_count: Arc::clone(&self.collected_count),
        })
    }
}

/// Discovers candidate source files in `root` according to `cfg`.
///
/// Output is sorted deterministically by relative path.
///
/// # Errors
/// Returns [`Error::Ignore`] if custom ignore rules contain invalid globs, if override
/// building fails, or if a critical traversal error occurs.
pub fn discover(root: impl AsRef<Path>, cfg: &WalkCfg) -> Result<Vec<SourceFile>> {
    let root = root.as_ref();
    let mut builder = WalkBuilder::new(root);

    // Decouple hidden file filtering: dotfiles are traversed unless ignored by gitignore/overrides
    builder
        .hidden(false)
        .git_ignore(cfg.standard_filters)
        .git_global(cfg.standard_filters)
        .git_exclude(cfg.standard_filters)
        .ignore(cfg.standard_filters)
        .parents(cfg.standard_filters)
        .follow_links(cfg.follow_symlinks);

    if let Some(threads) = cfg.threads {
        builder.threads(threads);
    }

    let mut override_builder = OverrideBuilder::new(root);

    // If any whitelist pattern is used (e.g. `!important.gen.rs`), include `*` first so positive overrides don't filter out everything else
    let has_whitelist = cfg.custom_excludes.iter().any(|c| c.starts_with('!'));
    if has_whitelist {
        override_builder.add("*").map_err(Error::Ignore)?;
    }

    if cfg.use_default_excludes {
        for exclude in DEFAULT_EXCLUDES {
            override_builder
                .add(&format!("!{exclude}"))
                .map_err(Error::Ignore)?;
            override_builder
                .add(&format!("!{exclude}/**"))
                .map_err(Error::Ignore)?;
        }
    }

    for custom in &cfg.custom_excludes {
        // Gitignore semantics:
        // - "pattern" -> ignore rule -> in OverrideBuilder this is "!pattern"
        // - "!pattern" -> whitelist rule -> in OverrideBuilder this is "pattern"
        if let Some(whitelist) = custom.strip_prefix('!') {
            override_builder.add(whitelist).map_err(Error::Ignore)?;
        } else {
            override_builder
                .add(&format!("!{custom}"))
                .map_err(Error::Ignore)?;
            if !custom.contains('/') && !custom.contains('*') {
                override_builder
                    .add(&format!("!**/{custom}/**"))
                    .map_err(Error::Ignore)?;
            }
        }
    }

    let overrides = override_builder.build().map_err(Error::Ignore)?;
    builder.overrides(overrides);

    let (tx, rx) = crossbeam_channel::unbounded();
    let (err_tx, err_rx) = crossbeam_channel::unbounded();
    let collected_count = Arc::new(AtomicUsize::new(0));

    let parallel_walker = builder.build_parallel();
    let mut visitor_builder = VisitorBuilderImpl {
        root: root.to_path_buf(),
        cfg: cfg.clone(),
        tx,
        err_tx,
        collected_count,
    };

    parallel_walker.visit(&mut visitor_builder);

    // Drop our senders so iterators finish
    drop(visitor_builder);

    // Check for critical traversal errors
    if let Ok(err) = err_rx.try_recv() {
        return Err(Error::Ignore(err));
    }

    let mut files: Vec<SourceFile> = rx.into_iter().collect();

    // Deterministic sort by relative path
    files.sort_by(|a, b| a.path.cmp(&b.path));

    if let Some(max) = cfg.max_files {
        files.truncate(max);
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_is_generated_heuristics() {
        assert!(is_generated("src/generated/proto.rs", None));
        assert!(is_generated("src/models_generated.rs", None));
        assert!(is_generated("src/generated.rs", None));
        assert!(is_generated("src/schema.generated.ts", None));
        assert!(is_generated("api/foo.pb.go", None));
        assert!(is_generated("models/user_pb2.py", None));
        assert!(is_generated("models/user_pb2_grpc.py", None));

        // False positives avoided
        assert!(!is_generated("src/regenerated_cache.rs", None));
        assert!(!is_generated("src/auth/token.rs", None));

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("normal_name.rs");
        fs::write(&file_path, "\n\n// @generated\npub fn foo() {}\n").unwrap();
        assert!(is_generated("src/normal_name.rs", Some(&file_path)));

        let file_path2 = tmp.path().join("normal_name2.rs");
        fs::write(
            &file_path2,
            "// Code generated by protoc. DO NOT EDIT.\npub fn bar() {}\n",
        )
        .unwrap();
        assert!(is_generated("src/normal_name2.rs", Some(&file_path2)));

        let file_path3 = tmp.path().join("regular.rs");
        fs::write(&file_path3, "// Normal file\npub fn baz() {}\n").unwrap();
        assert!(!is_generated("src/regular.rs", Some(&file_path3)));
    }

    #[test]
    fn test_discover_basic() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src/auth")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();

        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/auth/token.rs"), "fn token() {}\n").unwrap();
        fs::write(
            root.join("node_modules/pkg/index.js"),
            "module.exports = {};\n",
        )
        .unwrap();
        fs::write(root.join("target/debug/app.rs"), "fn dummy() {}\n").unwrap();

        let cfg = WalkCfg::default();
        let files = discover(root, &cfg).unwrap();

        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/auth/token.rs", "src/main.rs"]);
    }

    #[test]
    fn test_gitignore_respected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap(); // mark as git repo
        fs::write(root.join(".gitignore"), "ignored.rs\nbuild_output/\n").unwrap();

        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("ignored.rs"), "fn ignored() {}\n").unwrap();

        fs::create_dir_all(root.join("build_output")).unwrap();
        fs::write(root.join("build_output/gen.rs"), "fn gen() {}\n").unwrap();

        let cfg = WalkCfg::default();
        let files = discover(root, &cfg).unwrap();

        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/main.rs"]);
    }

    #[test]
    fn test_cyclic_symlink_no_infinite_loop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let _ = symlink(root, root.join("src/loop_link"));
        }

        let cfg = WalkCfg {
            follow_symlinks: true,
            ..WalkCfg::default()
        };

        let files = discover(root, &cfg).unwrap();
        assert!(!files.is_empty());
    }
}
