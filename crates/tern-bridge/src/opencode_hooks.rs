//! Tern's plugin in an OpenCode tab. A tab runs its own server
//! (`--standalone`), so the server's plugins see the tab's environment; the
//! plugin comes in by `OPENCODE_CONFIG_CONTENT`, which adds to the user's
//! config where `OPENCODE_CONFIG_DIR` would replace it. Nothing is written to
//! OpenCode's own folders.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::version::Version;

/// The oldest OpenCode Tern's plugin was tried with.
const MIN_VERSION: Version = (2, 0, 30);

/// What a tab's environment names its hook file by.
pub const OPENCODE_HOOK_FILE_ENV: &str = "TERN_OPENCODE_HOOK_FILE";

/// OpenCode takes a plugin only as a directory with a `package.json`.
const PLUGIN_DIR: &str = "opencode-plugin";
const PLUGIN_SOURCE: &str = include_str!("opencode_plugin.js");
const PLUGIN_PACKAGE: &str = r#"{"name":"tern-opencode","version":"1.0.0","type":"module","main":"index.js","exports":{".":"./index.js"}}"#;

/// Tern's plugin can go in an OpenCode of `version`.
pub fn opencode_wired(version: Option<Version>) -> bool {
    version.is_some_and(|v| v >= MIN_VERSION)
}

/// `path` holds `text`, replaced whole: a tab starting meanwhile reads the
/// old file or the new one.
fn write_if_changed(path: &Path, text: &str) -> io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Write Tern's plugin under `dir`, the daemon's run dir, for every tab it
/// starts; its directory.
pub fn write_opencode_plugin(dir: &Path) -> io::Result<PathBuf> {
    let plugin = dir.join(PLUGIN_DIR);
    std::fs::create_dir_all(&plugin)?;
    write_if_changed(&plugin.join("package.json"), PLUGIN_PACKAGE)?;
    write_if_changed(&plugin.join("index.js"), PLUGIN_SOURCE)?;
    Ok(plugin)
}

/// Write the tab's hook endpoint to a private file and return its path: a
/// tab's environment reaches everything it runs, so it names this file (itself
/// named for the token's hash) and not the endpoint's token.
pub fn write_opencode_hook_file(dir: &Path, token: &str, hook_url: &str) -> io::Result<PathBuf> {
    let path = crate::hooks::token_file(dir, "opencode-hooks", token);
    let mut f = crate::hooks::create_private(dir, &path)?;
    f.write_all(json!({ "hook_url": hook_url }).to_string().as_bytes())?;
    Ok(path)
}

/// The file the sidebar's preview of the tab with `token` is read from.
pub fn opencode_preview_path(dir: &Path, token: &str) -> PathBuf {
    crate::hooks::token_file(dir, "opencode-preview", token)
}

pub fn remove_opencode_hook_file(dir: &Path, token: &str) {
    let _ = std::fs::remove_file(crate::hooks::token_file(dir, "opencode-hooks", token));
    let _ = std::fs::remove_file(opencode_preview_path(dir, token));
}

/// What `OPENCODE_CONFIG_CONTENT` is for a tab: `existing`, the user's own,
/// with Tern's plugin added. `None` when theirs can't take it: it stays as it
/// is and the tab runs without Tern.
pub fn opencode_config_content(plugin_dir: &Path, existing: Option<&str>) -> Option<String> {
    let mut config = match existing {
        Some(text) => serde_json::from_str::<Value>(text).ok()?.as_object()?.clone(),
        None => Map::new(),
    };
    let plugins = config.entry("plugins").or_insert_with(|| json!([])).as_array_mut()?;
    plugins.push(json!(plugin_dir.to_string_lossy()));
    Some(Value::Object(config).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version;

    #[test]
    fn wired_from_the_verified_version() {
        assert!(opencode_wired(version::parse("2.0.30")));
        assert!(!opencode_wired(version::parse("2.0.29")));
        assert!(!opencode_wired(version::parse("1.14.0")));
        assert!(!opencode_wired(None));
    }

    #[test]
    fn config_content_names_the_plugin() {
        let c: Value = serde_json::from_str(&opencode_config_content(Path::new("/run/p"), None).unwrap()).unwrap();
        assert_eq!(c, json!({"plugins": ["/run/p"]}));
    }

    #[test]
    fn the_users_config_content_is_kept() {
        let user = r#"{"model":"a/b","plugins":["/mine"]}"#;
        let c: Value = serde_json::from_str(&opencode_config_content(Path::new("/run/p"), Some(user)).unwrap()).unwrap();
        assert_eq!(c, json!({"model": "a/b", "plugins": ["/mine", "/run/p"]}));
    }

    #[test]
    fn config_content_that_is_not_an_object_is_left_alone() {
        assert_eq!(opencode_config_content(Path::new("/run/p"), Some("{oops")), None);
        assert_eq!(opencode_config_content(Path::new("/run/p"), Some("[1]")), None);
    }

    #[test]
    fn plugins_that_are_not_a_list_are_left_alone() {
        assert_eq!(opencode_config_content(Path::new("/run/p"), Some(r#"{"plugins":"/mine"}"#)), None);
    }

    #[test]
    fn the_plugin_is_a_package_directory() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_opencode_plugin(dir.path()).unwrap();
        assert!(plugin.is_dir());
        let pkg: Value = serde_json::from_str(&std::fs::read_to_string(plugin.join("package.json")).unwrap()).unwrap();
        assert_eq!((&pkg["type"], &pkg["main"], &pkg["exports"]["."]), (&json!("module"), &json!("index.js"), &json!("./index.js")));
        assert!(std::fs::read_to_string(plugin.join("index.js")).unwrap().contains("export default"));
    }

    #[test]
    fn the_hook_file_holds_the_endpoint_privately_and_goes_with_its_preview() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let path = write_opencode_hook_file(&run, "tok", "http://127.0.0.1:1/hook/tok").unwrap();
        let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file["hook_url"], "http://127.0.0.1:1/hook/tok");
        assert!(!path.to_string_lossy().contains("tok"), "the path mustn't name the token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let preview = opencode_preview_path(&run, "tok");
        std::fs::write(&preview, "x").unwrap();
        remove_opencode_hook_file(&run, "tok");
        assert!(!path.exists() && !preview.exists());
    }

    /// The plugin's own behaviour, under node when there is one: CI's runners
    /// have it, and OpenCode's runtime isn't needed for what is tested.
    #[test]
    fn the_plugin_behaves_under_node() {
        let dir = tempfile::tempdir().unwrap();
        let (plugin, harness) = (dir.path().join("plugin.mjs"), dir.path().join("harness.mjs"));
        std::fs::write(&plugin, PLUGIN_SOURCE).unwrap();
        std::fs::write(&harness, include_str!("opencode_plugin_test.mjs")).unwrap();
        let run = std::process::Command::new("node").arg(&harness).arg(&plugin).arg(dir.path().join("hook.json")).output();
        let Ok(run) = run else {
            eprintln!("no node here: the plugin's scenarios didn't run");
            return;
        };
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    }
}
