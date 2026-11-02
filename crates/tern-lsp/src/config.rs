//! Which language servers there are, for which languages, and where a
//! file's project root is. The built-in servers are OpenIDEA's defaults
//! (`servers.json`); `settings.json` replaces or removes them by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BUILTIN: &str = include_str!("servers.json");

/// One language server: how to start it and what it serves.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Its name, which a `settings.json` entry replaces it by (built-in: the
    /// `name` in the key, filled in when read from settings).
    #[serde(default)]
    pub name: String,
    /// The LSP language ids it serves: `rust`, `c`, `cpp`, `go`, `java`,
    /// `javascript`, `javascriptreact`, `typescript`, `typescriptreact`,
    /// `python`, `swift`.
    pub languages: Vec<String>,
    /// The program, found on `PATH`.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables over Tern's.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Files or folders that mark a project root (`Cargo.toml`): the nearest
    /// folder above a file holding one is its server's root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Without a root marker, no server (else the workspace is the root).
    #[serde(default)]
    pub require_root_marker: bool,
    /// Used only when the servers before it for the language aren't installed.
    #[serde(default)]
    pub fallback: bool,
    /// Sent with `initialize`.
    #[serde(default)]
    pub initialization_options: Option<Value>,
    /// What the server reads with `workspace/configuration`, by section.
    #[serde(default)]
    pub settings: Option<Value>,
    /// How to install it, shown when it isn't on `PATH`.
    #[serde(default)]
    pub install_hint: Option<String>,
}

/// The built-in servers.
pub fn builtin() -> Vec<ServerConfig> {
    serde_json::from_str(BUILTIN).expect("built-in servers.json")
}

/// The built-in servers, as written (the reference file).
pub fn builtin_text() -> &'static str {
    BUILTIN
}

/// `builtin` with the user's entries by name: one replaces the built-in of
/// that name, `null` removes it, a new name is added after them.
pub fn resolve(builtin: &[ServerConfig], user: &BTreeMap<String, Option<ServerConfig>>) -> Vec<ServerConfig> {
    let mut out: Vec<ServerConfig> = builtin
        .iter()
        .filter_map(|b| match user.get(&b.name) {
            None => Some(b.clone()),
            Some(None) => None,
            Some(Some(u)) => Some(ServerConfig { name: b.name.clone(), ..u.clone() }),
        })
        .collect();
    for (name, entry) in user {
        if let Some(u) = entry
            && !builtin.iter().any(|b| &b.name == name)
        {
            out.push(ServerConfig { name: name.clone(), ..u.clone() });
        }
    }
    out
}

/// The servers for `language`: the first choices, then the fallbacks.
pub fn servers_for<'a>(configs: &'a [ServerConfig], language: &str) -> Vec<&'a ServerConfig> {
    let serves = |s: &&ServerConfig| s.languages.iter().any(|l| l == language);
    let first = configs.iter().filter(serves).filter(|s| !s.fallback);
    first.chain(configs.iter().filter(serves).filter(|s| s.fallback)).collect()
}

/// The LSP language id of `path`, by its extension.
pub fn language_id(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "c" => "c",
        // Headers are C++ to clangd unless a compile database says otherwise.
        "h" | "hh" | "hpp" | "hxx" | "h++" | "cc" | "cpp" | "cxx" | "c++" | "ipp" | "tpp" | "inl" | "ixx" | "cppm" => "cpp",
        "go" => "go",
        "java" => "java",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "py" | "pyi" | "pyw" => "python",
        "swift" | "swiftinterface" => "swift",
        _ => return None,
    })
}

/// The project root of `file` for a server with `markers`: the nearest
/// folder from the file's up to `workspace` holding one of them; else
/// `workspace`, unless `require`. `None` for a file outside `workspace`.
pub fn find_root(file: &Path, markers: &[String], workspace: &Path, require: bool) -> Option<PathBuf> {
    if !file.starts_with(workspace) {
        return None;
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        if markers.iter().any(|m| d.join(m).exists()) {
            return Some(d.to_path_buf());
        }
        if d == workspace {
            break;
        }
        dir = d.parent();
    }
    (!require).then(|| workspace.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(c: &[&ServerConfig]) -> Vec<String> {
        c.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn builtin_parses_and_names_are_unique() {
        let b = builtin();
        assert!(b.len() >= 10);
        let mut seen = std::collections::HashSet::new();
        for s in &b {
            assert!(seen.insert(&s.name), "duplicate {}", s.name);
            assert!(!s.languages.is_empty() && !s.command.is_empty(), "{}", s.name);
        }
    }

    #[test]
    fn user_entry_replaces_removes_and_adds() {
        let user: BTreeMap<String, Option<ServerConfig>> = serde_json::from_str(
            r#"{
                "gopls": { "languages": ["go"], "command": "gopls", "args": ["-remote=auto"] },
                "clangd": null,
                "zls": { "languages": ["zig"], "command": "zls" }
            }"#,
        )
        .unwrap();
        let all = resolve(&builtin(), &user);
        let gopls = all.iter().find(|s| s.name == "gopls").unwrap();
        assert_eq!(gopls.args, ["-remote=auto"]);
        assert!(gopls.root_markers.is_empty(), "a user entry replaces the whole built-in one");
        assert!(!all.iter().any(|s| s.name == "clangd"));
        assert_eq!(all.last().unwrap().name, "zls");
    }

    #[test]
    fn fallbacks_come_after() {
        let b = builtin();
        assert_eq!(names(&servers_for(&b, "python")), ["basedpyright", "pyright", "ty"]);
        assert_eq!(names(&servers_for(&b, "typescriptreact")), ["vtsls", "typescript-language-server"]);
        assert_eq!(names(&servers_for(&b, "c")), ["clangd"]);
        assert!(servers_for(&b, "zig").is_empty());
    }

    #[test]
    fn root_is_nearest_marker_inside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("a/src")).unwrap();
        std::fs::write(ws.join("a/Cargo.toml"), "").unwrap();
        let markers = vec!["Cargo.toml".to_string()];
        assert_eq!(find_root(&ws.join("a/src/x.rs"), &markers, &ws, false), Some(ws.join("a")));
        assert_eq!(find_root(&ws.join("b/x.rs"), &markers, &ws, false), Some(ws.clone()));
        assert_eq!(find_root(&ws.join("b/x.rs"), &markers, &ws, true), None);
        assert_eq!(find_root(&dir.path().join("out/x.rs"), &markers, &ws, false), None);
    }

    #[test]
    fn language_ids_by_extension() {
        let id = |p: &str| language_id(Path::new(p));
        assert_eq!(id("x.tsx"), Some("typescriptreact"));
        assert_eq!(id("x.jsx"), Some("javascriptreact"));
        assert_eq!(id("x.h"), Some("cpp"));
        assert_eq!(id("x.c"), Some("c"));
        assert_eq!(id("X.RS"), Some("rust"));
        assert_eq!(id("x.pyw"), Some("python"));
        assert_eq!(id("x.txt"), None);
    }
}
