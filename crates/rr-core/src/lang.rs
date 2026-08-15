use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Programming languages recognized by `repo-router`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Go,
    Java,
    CSharp,
    C,
    Cpp,
    Ruby,
    Php,
    Swift,
    Kotlin,
    Scala,
    Zig,
    Lua,
    Shell,
    Toml,
    Json,
    Yaml,
    Markdown,
    Html,
    Css,
    Sql,
    Proto,
}

impl Lang {
    /// Detects language from a file extension.
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext_lower = ext.to_ascii_lowercase();
        match ext_lower.as_str() {
            "rs" => Some(Self::Rust),
            "py" | "pyw" | "pyi" => Some(Self::Python),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "cs" => Some(Self::CSharp),
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "rb" => Some(Self::Ruby),
            "php" => Some(Self::Php),
            "swift" => Some(Self::Swift),
            "kt" | "kts" => Some(Self::Kotlin),
            "scala" | "sc" => Some(Self::Scala),
            "zig" => Some(Self::Zig),
            "lua" => Some(Self::Lua),
            "sh" | "bash" | "zsh" => Some(Self::Shell),
            "toml" => Some(Self::Toml),
            "json" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "md" | "markdown" => Some(Self::Markdown),
            "html" | "htm" => Some(Self::Html),
            "css" | "scss" | "sass" | "less" => Some(Self::Css),
            "sql" => Some(Self::Sql),
            "proto" => Some(Self::Proto),
            _ => None,
        }
    }

    /// Detects language from a file path.
    #[must_use]
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            Self::from_extension(ext)
        } else {
            None
        }
    }

    /// Returns the lowercase language identifier.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Swift => "swift",
            Self::Kotlin => "kotlin",
            Self::Scala => "scala",
            Self::Zig => "zig",
            Self::Lua => "lua",
            Self::Shell => "shell",
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Css => "css",
            Self::Sql => "sql",
            Self::Proto => "proto",
        }
    }

    /// Whether this repository-relative path is a test file by convention.
    ///
    /// This is a naming convention, not a compilation fact: nothing here opens
    /// the file. It exists so that a projection of the index and the index
    /// itself cannot disagree about which files are tests, and so that the
    /// answer is language-aware rather than a single hardcoded Rust habit.
    ///
    /// A directory named `tests` counts wherever it appears, because that is
    /// the one convention every language in this list shares. Everything else
    /// is a per-language file-name rule.
    ///
    /// The path must be canonical: repository-relative, `/`-separated. A
    /// `Path`-based signature would invite a caller to pass a platform path
    /// whose separators this function cannot see.
    #[must_use]
    pub fn path_indicates_test(self, canonical_path: &str) -> bool {
        let (directories, file_name) = match canonical_path.rsplit_once('/') {
            Some((directories, file_name)) => (directories, file_name),
            None => ("", canonical_path),
        };
        if directories.split('/').any(|segment| segment == "tests") {
            return true;
        }
        let stem = file_name
            .rsplit_once('.')
            .map_or(file_name, |(stem, _)| stem);
        match self {
            // `foo_test.rs`, `foo_tests.rs`, and the `tests` submodule.
            Self::Rust => {
                stem == "test" || stem == "tests" || has_suffix(stem, &["_test", "_tests"])
            }
            // `pytest` collects `test_*.py` and `*_test.py`; shell suites
            // follow the same two shapes.
            Self::Python | Self::Shell => stem.starts_with("test_") || has_suffix(stem, &["_test"]),
            // `foo.test.ts` and `foo.spec.ts`, the two names every JS runner knows.
            Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Jsx => {
                has_suffix(stem, &[".test", ".spec"])
            }
            // The Go toolchain compiles exactly `*_test.go` into the test
            // binary, and Zig's convention borrows the same name.
            Self::Go | Self::Zig => has_suffix(stem, &["_test"]),
            // These languages name the test type, and the file takes the
            // type's name; the suffix is therefore capitalized.
            Self::Java | Self::CSharp | Self::Kotlin | Self::Scala | Self::Swift => {
                has_suffix(stem, &["Test", "Tests", "TestCase", "Spec"])
            }
            // Minitest and RSpec, whose names Lua's busted mirrors.
            Self::Ruby | Self::Lua => {
                stem.starts_with("test_") || has_suffix(stem, &["_test", "_spec"])
            }
            // PHPUnit requires the `Test` suffix on the class and its file.
            Self::Php => has_suffix(stem, &["Test"]),
            Self::C | Self::Cpp => {
                stem.starts_with("test_") || has_suffix(stem, &["_test", "_tests"])
            }
            // Data and markup languages have no test-file convention worth
            // guessing at; a wrong guess here would hide real definitions.
            Self::Toml
            | Self::Json
            | Self::Yaml
            | Self::Markdown
            | Self::Html
            | Self::Css
            | Self::Sql
            | Self::Proto => false,
        }
    }

    /// Returns the display name of the language.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript",
            Self::Jsx => "JSX",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::CSharp => "C#",
            Self::C => "C",
            Self::Cpp => "C++",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Swift => "Swift",
            Self::Kotlin => "Kotlin",
            Self::Scala => "Scala",
            Self::Zig => "Zig",
            Self::Lua => "Lua",
            Self::Shell => "Shell",
            Self::Toml => "TOML",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Markdown => "Markdown",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Sql => "SQL",
            Self::Proto => "Protocol Buffers",
        }
    }
}

/// Whether the stem ends with any of these markers.
///
/// Case-sensitive on purpose. `FooTest.java` and `footest.java` are different
/// claims, and lowering the case would make the second one a test file in a
/// repository that never said so.
fn has_suffix(stem: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| stem.ends_with(marker))
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(Lang::from_extension("rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_extension("RS"), Some(Lang::Rust));
        assert_eq!(Lang::from_extension("py"), Some(Lang::Python));
        assert_eq!(Lang::from_extension("ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_extension("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_extension("js"), Some(Lang::JavaScript));
        assert_eq!(Lang::from_extension("unknown"), None);

        assert_eq!(Lang::from_path(Path::new("src/main.rs")), Some(Lang::Rust));
        assert_eq!(Lang::from_path(Path::new("app/index.tsx")), Some(Lang::Tsx));
    }

    #[test]
    fn test_serde_serialization_matches_as_str() {
        let all_languages = [
            Lang::Rust,
            Lang::Python,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::JavaScript,
            Lang::Jsx,
            Lang::Go,
            Lang::Java,
            Lang::CSharp,
            Lang::C,
            Lang::Cpp,
            Lang::Ruby,
            Lang::Php,
            Lang::Swift,
            Lang::Kotlin,
            Lang::Scala,
            Lang::Zig,
            Lang::Lua,
            Lang::Shell,
            Lang::Toml,
            Lang::Json,
            Lang::Yaml,
            Lang::Markdown,
            Lang::Html,
            Lang::Css,
            Lang::Sql,
            Lang::Proto,
        ];

        for lang in all_languages {
            let serialized = serde_json::to_string(&lang).unwrap();
            assert_eq!(
                serialized,
                format!("\"{}\"", lang.as_str()),
                "serialized string must match as_str for {:?}",
                lang
            );

            let deserialized: Lang = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, lang);
        }

        assert_eq!(
            serde_json::to_string(&Lang::TypeScript).unwrap(),
            "\"typescript\""
        );
        assert_eq!(serde_json::to_string(&Lang::CSharp).unwrap(), "\"csharp\"");
    }
}
