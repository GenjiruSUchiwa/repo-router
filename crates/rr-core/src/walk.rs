use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use ignore::overrides::OverrideBuilder;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};

use crate::lang::Lang;
use crate::path::RelPath;
use crate::Result;

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
pub struct WalkCfg {
    /// Additional exclusion globs.
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

/// Checks if a file is generated based on path heuristics or first-line content.
#[must_use]
pub fn is_generated(rel_path: &str, full_path: Option<&Path>) -> bool {
    let path_lower = rel_path.to_ascii_lowercase();

    // 1. Path heuristics: contains 'generated', '.pb.', '_pb2.py', or '_pb2_grpc.py'
    if path_lower.contains("generated")
        || path_lower.contains(".pb.")
        || path_lower.contains("_pb2.py")
        || path_lower.contains("_pb2_grpc.py")
    {
        return true;
    }

    // 2. Content heuristics: first non-empty line contains '@generated' or 'DO NOT EDIT'
    if let Some(path) = full_path {
        if let Ok(file) = File::open(path) {
            let mut reader = BufReader::new(file.take(2048));
            let mut line = String::new();
            // Check up to first 5 non-empty lines (or first line)
            for _ in 0..5 {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
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
    // Only regular files (or symlinks pointing to regular files) are candidate source files
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

    let generated = is_generated(rel_path.as_str(), Some(full_path));

    Some(SourceFile {
        path: rel_path,
        lang,
        generated,
    })
}

/// Checks if a directory entry matches default or custom exclusion rules.
fn is_excluded_dir(entry: &DirEntry, cfg: &WalkCfg) -> bool {
    if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
        return false;
    }

    let file_name = entry.file_name().to_str().unwrap_or("");

    if cfg.use_default_excludes && DEFAULT_EXCLUDES.contains(&file_name) {
        return true;
    }

    for custom in &cfg.custom_excludes {
        let pattern = custom.trim_start_matches('!').trim_matches('/');
        if file_name == pattern {
            return true;
        }
    }

    false
}

struct VisitorCollector {
    root: PathBuf,
    cfg: WalkCfg,
    tx: crossbeam_channel::Sender<SourceFile>,
}

impl ParallelVisitor for VisitorCollector {
    fn visit(&mut self, entry: std::result::Result<DirEntry, ignore::Error>) -> WalkState {
        match entry {
            Ok(dir_entry) => {
                // Skip excluded directories early
                if is_excluded_dir(&dir_entry, &self.cfg) {
                    return WalkState::Skip;
                }

                if let Some(source_file) = classify_entry(&self.root, &dir_entry, &self.cfg) {
                    let _ = self.tx.send(source_file);
                }
                WalkState::Continue
            }
            Err(_) => WalkState::Continue,
        }
    }
}

struct VisitorBuilderImpl {
    root: PathBuf,
    cfg: WalkCfg,
    tx: crossbeam_channel::Sender<SourceFile>,
}

impl<'s> ParallelVisitorBuilder<'s> for VisitorBuilderImpl {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(VisitorCollector {
            root: self.root.clone(),
            cfg: self.cfg.clone(),
            tx: self.tx.clone(),
        })
    }
}

/// Discovers candidate source files in `root` according to `cfg`.
///
/// Output is sorted deterministically by relative path.
///
/// # Errors
/// Returns an error if directory traversal fails or ignore rules fail to build.
pub fn discover(root: impl AsRef<Path>, cfg: &WalkCfg) -> Result<Vec<SourceFile>> {
    let root = root.as_ref();
    let mut builder = WalkBuilder::new(root);

    builder
        .standard_filters(cfg.standard_filters)
        .follow_links(cfg.follow_symlinks);

    if let Some(threads) = cfg.threads {
        builder.threads(threads);
    }

    // Build custom ignore/override rules
    let mut override_builder = OverrideBuilder::new(root);
    if cfg.use_default_excludes {
        for exclude in DEFAULT_EXCLUDES {
            let _ = override_builder.add(&format!("!{exclude}"));
            let _ = override_builder.add(&format!("!{exclude}/**"));
        }
    }

    for custom in &cfg.custom_excludes {
        let rule = if custom.starts_with('!') {
            custom.clone()
        } else {
            format!("!{custom}")
        };
        let _ = override_builder.add(&rule);
        let _ = override_builder.add(&format!("{rule}/**"));
    }

    if let Ok(overrides) = override_builder.build() {
        builder.overrides(overrides);
    }

    let (tx, rx) = crossbeam_channel::unbounded();

    let parallel_walker = builder.build_parallel();
    let mut visitor_builder = VisitorBuilderImpl {
        root: root.to_path_buf(),
        cfg: cfg.clone(),
        tx,
    };

    parallel_walker.visit(&mut visitor_builder);

    // Drop our sender so iterator finishes
    drop(visitor_builder);

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
        assert!(is_generated("api/foo.pb.go", None));
        assert!(is_generated("models/user_pb2.py", None));
        assert!(is_generated("models/user_pb2_grpc.py", None));
        assert!(!is_generated("src/auth/token.rs", None));

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("normal_name.rs");
        fs::write(&file_path, "// @generated\npub fn foo() {}\n").unwrap();
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
