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

    /// Every language, in declaration order.
    ///
    /// Exists so the walk allowlist and the tier table can prove they cover
    /// all of them rather than the handful their fixtures happen to contain.
    pub const ALL: [Self; 27] = [
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::Tsx,
        Self::JavaScript,
        Self::Jsx,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::C,
        Self::Cpp,
        Self::Ruby,
        Self::Php,
        Self::Swift,
        Self::Kotlin,
        Self::Scala,
        Self::Zig,
        Self::Lua,
        Self::Shell,
        Self::Toml,
        Self::Json,
        Self::Yaml,
        Self::Markdown,
        Self::Html,
        Self::Css,
        Self::Sql,
        Self::Proto,
    ];

    /// Whether this language has definitions rr could route to.
    ///
    /// The six data formats answer `false`: a key in a YAML file is not a
    /// function with a signature and a visibility, and no extraction tier —
    /// not even the lexical one — turns it into one. `Lang` recognises them so
    /// the walk can classify and skip them, which is what it should keep doing.
    ///
    /// Written as an exhaustive match with no wildcard arm, so a new variant
    /// fails to compile until someone decides which side it falls on. A `_`
    /// arm here would answer `true` for a format nobody classified, and the
    /// walk would index it before anyone noticed.
    #[must_use]
    pub const fn is_code(self) -> bool {
        match self {
            Self::Rust
            | Self::Python
            | Self::TypeScript
            | Self::Tsx
            | Self::JavaScript
            | Self::Jsx
            | Self::Go
            | Self::Java
            | Self::CSharp
            | Self::C
            | Self::Cpp
            | Self::Ruby
            | Self::Php
            | Self::Swift
            | Self::Kotlin
            | Self::Scala
            | Self::Zig
            | Self::Lua
            | Self::Shell
            | Self::Sql
            | Self::Proto => true,
            Self::Toml | Self::Json | Self::Yaml | Self::Markdown | Self::Html | Self::Css => false,
        }
    }

    /// The separator this language writes between qualified-name segments.
    ///
    /// This is the spelling `rr query` has to accept back, so the extractor
    /// that builds `local_qualified` and the index that prefixes the module
    /// path both read it from here rather than each hardcoding one.
    #[must_use]
    pub const fn qualified_separator(self) -> &'static str {
        match self {
            Self::Rust => "::",
            _ => ".",
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
            Self::Rust => {
                stem == "test" || stem == "tests" || has_suffix(stem, &["_test", "_tests"])
            }
            Self::Python | Self::Shell => stem.starts_with("test_") || has_suffix(stem, &["_test"]),
            Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Jsx => {
                has_suffix(stem, &[".test", ".spec"])
            }
            Self::Go | Self::Zig => has_suffix(stem, &["_test"]),
            Self::Java | Self::CSharp | Self::Kotlin | Self::Scala | Self::Swift => {
                has_suffix(stem, &["Test", "Tests", "TestCase", "Spec"])
            }
            Self::Ruby | Self::Lua => {
                stem.starts_with("test_") || has_suffix(stem, &["_test", "_spec"])
            }
            Self::Php => has_suffix(stem, &["Test"]),
            Self::C | Self::Cpp => {
                stem.starts_with("test_") || has_suffix(stem, &["_test", "_tests"])
            }
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
    use crate::test_support::assert_variant_count;

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

    #[test]
    fn lang_all_lists_every_variant() {
        assert_variant_count::<Lang>(Lang::ALL.len(), "Lang::ALL");
        let mut names: Vec<&str> = Lang::ALL.iter().map(Lang::as_str).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "a language is listed in ALL twice");
    }

    #[test]
    fn every_lang_is_code_or_a_format() {
        let code: Vec<Lang> = Lang::ALL
            .into_iter()
            .filter(|lang| lang.is_code())
            .collect();
        assert_eq!(code.len(), 21);
        assert!(!Lang::Toml.is_code());
        assert!(!Lang::Json.is_code());
        assert!(!Lang::Yaml.is_code());
        assert!(!Lang::Markdown.is_code());
        assert!(!Lang::Html.is_code());
        assert!(!Lang::Css.is_code());
        assert!(Lang::CSharp.is_code());
        assert!(Lang::Go.is_code());
    }
}
