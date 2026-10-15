//! The environment terminal children inherit from Tern.

use std::path::Path;

/// Variables other terminals (or a debug run of Tern) set for their own
/// children; a shell in Tern must not see them.
const FOREIGN: &[&str] = &["TERM_SESSION_ID", "TERN_CONTROL", "RUST_LOG", "ALACRITTY_WINDOW_ID", "WINDOWID"];
const FOREIGN_PREFIXES: &[&str] = &["ITERM_", "GHOSTTY_", "KITTY_", "VSCODE_", "WEZTERM_", "ALACRITTY_"];

pub fn is_foreign(key: &str) -> bool {
    FOREIGN.contains(&key) || FOREIGN_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// `<lang>_<REGION>.UTF-8` for a system locale identifier such as `vi_VN`,
/// `en_GB@rg=vnzzzz` or `zh-Hans_CN`, when `exists` knows it; else
/// `en_US.UTF-8`.
pub fn utf8_locale(identifier: &str, exists: impl Fn(&str) -> bool) -> String {
    let id = identifier.split('@').next().unwrap_or_default();
    let mut parts = id.split(['_', '-']);
    let lang = parts.next().unwrap_or_default();
    let region = parts.rfind(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_uppercase()));
    if let Some(region) = region
        && !lang.is_empty()
    {
        let name = format!("{lang}_{region}.UTF-8");
        if exists(&name) {
            return name;
        }
    }
    "en_US.UTF-8".to_owned()
}

#[cfg(target_os = "macos")]
fn system_locale() -> Option<String> {
    Some(objc2_foundation::NSLocale::currentLocale().localeIdentifier().to_string())
}

#[cfg(not(target_os = "macos"))]
fn system_locale() -> Option<String> {
    None
}

/// Make Tern's own environment the one its terminals should inherit: drop
/// other terminals' variables and, when started from Finder or the Dock
/// (launchd sets no locale), use a UTF-8 locale so non-ASCII input and
/// output work.
///
/// # Safety
/// Changes the process environment: call before any other thread starts.
pub unsafe fn prepare_process_env() {
    let foreign: Vec<_> = std::env::vars_os().filter_map(|(k, _)| k.to_str().filter(|k| is_foreign(k)).map(str::to_owned)).collect();
    for key in foreign {
        unsafe { std::env::remove_var(key) };
    }
    let unset = |k: &str| std::env::var_os(k).is_none_or(|v| v.is_empty());
    if unset("LANG") && unset("LC_ALL") && unset("LC_CTYPE") {
        let exists = |name: &str| Path::new("/usr/share/locale").join(name).exists();
        let locale = utf8_locale(&system_locale().unwrap_or_default(), exists);
        unsafe {
            std::env::set_var("LANG", &locale);
            std::env::set_var("LC_CTYPE", &locale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_from_system_identifier() {
        let known = |n: &str| ["vi_VN.UTF-8", "en_GB.UTF-8", "zh_CN.UTF-8"].contains(&n);
        assert_eq!(utf8_locale("vi_VN", known), "vi_VN.UTF-8");
        assert_eq!(utf8_locale("en_GB@rg=vnzzzz", known), "en_GB.UTF-8");
        assert_eq!(utf8_locale("zh-Hans_CN", known), "zh_CN.UTF-8");
        // en_VN has no locale files.
        assert_eq!(utf8_locale("en_VN", known), "en_US.UTF-8");
        assert_eq!(utf8_locale("", known), "en_US.UTF-8");
    }

    #[test]
    fn foreign_variables() {
        assert!(is_foreign("ITERM_SESSION_ID"));
        assert!(is_foreign("TERN_CONTROL"));
        assert!(is_foreign("VSCODE_INJECTION"));
        assert!(!is_foreign("PATH"));
        assert!(!is_foreign("TERM_PROGRAM"));
    }
}
