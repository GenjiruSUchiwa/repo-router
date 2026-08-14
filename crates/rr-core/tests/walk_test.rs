use std::fs;
use tempfile::TempDir;

use rr_core::{discover, Lang, SourceFile, WalkCfg, DEFAULT_EXCLUDES};

#[test]
fn test_deterministic_ordering_across_runs() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Create a reasonably complex file tree with various depths and names
    let files_to_create = [
        "src/main.rs",
        "src/auth/token.rs",
        "src/auth/mod.rs",
        "src/db/users.rs",
        "src/db/models.rs",
        "src/api/routes.ts",
        "src/api/handlers.tsx",
        "scripts/build.py",
        "tests/integration_test.rs",
        "README.md",
        "package.json",
    ];

    for path_str in &files_to_create {
        let full = root.join(path_str);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, "// test content\n").unwrap();
    }

    let cfg = WalkCfg::default();

    let first_run = discover(root, &cfg).unwrap();
    assert!(!first_run.is_empty());

    // Run 50 times and ensure the output is 100% identical on every run
    for _ in 0..50 {
        let run = discover(root, &cfg).unwrap();
        assert_eq!(
            run, first_run,
            "discovered files must match exactly in order and content"
        );
    }

    // Verify paths are strictly in ascending order
    for window in first_run.windows(2) {
        assert!(
            window[0].path < window[1].path,
            "paths must be strictly sorted: {} vs {}",
            window[0].path,
            window[1].path
        );
    }
}

#[test]
fn test_gitignore_dynamic_exclusion() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();

    let main_rs = root.join("src/main.rs");
    let secret_rs = root.join("src/secret.rs");
    let temp_py = root.join("src/temp.py");

    fs::write(&main_rs, "fn main() {}\n").unwrap();
    fs::write(&secret_rs, "fn secret() {}\n").unwrap();
    fs::write(&temp_py, "print('temp')\n").unwrap();

    let cfg = WalkCfg::default();

    // Initially secret.rs and temp.py are discovered
    let initial = discover(root, &cfg).unwrap();
    let initial_paths: Vec<&str> = initial.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        initial_paths,
        vec!["src/main.rs", "src/secret.rs", "src/temp.py"]
    );

    // Add secret.rs to .gitignore
    fs::write(root.join(".gitignore"), "src/secret.rs\n*.py\n").unwrap();

    // Now secret.rs and temp.py must immediately be excluded
    let after_ignore = discover(root, &cfg).unwrap();
    let after_paths: Vec<&str> = after_ignore.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(after_paths, vec!["src/main.rs"]);
}

#[test]
fn test_default_exclusions() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    for dir in DEFAULT_EXCLUDES {
        let dir_path = root.join(dir).join("sub");
        fs::create_dir_all(&dir_path).unwrap();
        fs::write(dir_path.join("file.rs"), "fn inside() {}\n").unwrap();
    }

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/app.rs"), "fn app() {}\n").unwrap();

    let cfg = WalkCfg::default();
    let files = discover(root, &cfg).unwrap();

    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["src/app.rs"]);
}

#[test]
fn test_custom_excludes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src/legacy")).unwrap();
    fs::create_dir_all(root.join("src/current")).unwrap();

    fs::write(root.join("src/legacy/old.rs"), "fn old() {}\n").unwrap();
    fs::write(root.join("src/current/new.rs"), "fn new() {}\n").unwrap();

    let cfg = WalkCfg {
        custom_excludes: vec!["legacy".to_string()],
        ..WalkCfg::default()
    };

    let files = discover(root, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["src/current/new.rs"]);
}

#[test]
fn test_language_filtering() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/script.py"), "print(1)\n").unwrap();
    fs::write(root.join("src/app.ts"), "const x = 1;\n").unwrap();
    fs::write(root.join("src/view.tsx"), "export const V = () => null;\n").unwrap();

    let cfg = WalkCfg {
        languages: Some(vec![Lang::Rust, Lang::TypeScript]),
        ..WalkCfg::default()
    };

    let files = discover(root, &cfg).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["src/app.ts", "src/main.rs"]);
}

#[test]
fn test_max_files_limit() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..10 {
        fs::write(root.join(format!("src/file_{i:02}.rs")), "fn f() {}\n").unwrap();
    }

    let cfg = WalkCfg {
        max_files: Some(3),
        ..WalkCfg::default()
    };

    let files = discover(root, &cfg).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path.as_str(), "src/file_00.rs");
    assert_eq!(files[1].path.as_str(), "src/file_01.rs");
    assert_eq!(files[2].path.as_str(), "src/file_02.rs");
}

#[test]
fn test_generated_file_flagging() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src/generated")).unwrap();
    fs::create_dir_all(root.join("src/proto")).unwrap();
    fs::create_dir_all(root.join("src/normal")).unwrap();

    // By path
    fs::write(root.join("src/generated/code.rs"), "pub fn gen() {}\n").unwrap();
    fs::write(root.join("src/proto/service.pb.rs"), "pub fn pb() {}\n").unwrap();
    fs::write(root.join("src/proto/user_pb2.py"), "def user(): pass\n").unwrap();

    // By content
    fs::write(
        root.join("src/normal/annotated.rs"),
        "// @generated\npub fn annotated() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/normal/do_not_edit.rs"),
        "/* Code generated by tooling. DO NOT EDIT. */\npub fn dne() {}\n",
    )
    .unwrap();

    // Regular file
    fs::write(
        root.join("src/normal/handwritten.rs"),
        "pub fn normal() {}\n",
    )
    .unwrap();

    let cfg = WalkCfg::default();
    let files = discover(root, &cfg).unwrap();

    let find = |name: &str| -> &SourceFile {
        files
            .iter()
            .find(|f| f.path.as_str().ends_with(name))
            .unwrap_or_else(|| panic!("file {name} not found"))
    };

    assert!(find("code.rs").generated);
    assert!(find("service.pb.rs").generated);
    assert!(find("user_pb2.py").generated);
    assert!(find("annotated.rs").generated);
    assert!(find("do_not_edit.rs").generated);
    assert!(!find("handwritten.rs").generated);
}

#[test]
fn test_cyclic_symlink_safety() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    fs::create_dir_all(root.join("src/nested")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/nested/mod.rs"), "pub mod sub;\n").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        // Create cyclic symlink: nested/cycle -> root
        let _ = symlink(root, root.join("src/nested/cycle"));
        // Create mutually cyclic symlinks: a -> b, b -> a
        let _ = symlink(root.join("src/nested/b"), root.join("src/nested/a"));
        let _ = symlink(root.join("src/nested/a"), root.join("src/nested/b"));
    }

    let cfg = WalkCfg {
        follow_symlinks: true,
        ..WalkCfg::default()
    };

    // Must terminate promptly and not hang
    let files = discover(root, &cfg).unwrap();
    assert!(!files.is_empty());
}

#[test]
fn test_discover_rust_basic_fixture() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = manifest_dir.join("../../fixtures/rust-basic");

    let cfg = WalkCfg::default();
    let files = discover(&fixture_root, &cfg).unwrap();

    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "Cargo.toml",
            "src/auth/mod.rs",
            "src/auth/token.rs",
            "src/db/mod.rs",
            "src/db/users.rs",
            "src/main.rs",
            "tests/token_test.rs",
        ]
    );

    // None of these are generated
    for f in &files {
        assert!(!f.generated);
    }
}
