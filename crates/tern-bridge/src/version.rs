//! An agent CLI's version, from what `<cli> --version` prints.

/// A version, `major.minor.patch`.
pub type Version = (u32, u32, u32);

/// The version in `--version`'s output (`2.1.312 (Claude Code)`,
/// `codex-cli 0.166.0`).
pub fn parse(output: &str) -> Option<Version> {
    output.split(|c: char| !(c.is_ascii_digit() || c == '.')).find_map(|word| {
        let mut parts = word.split('.');
        let v = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        parts.next().is_none().then_some(v)
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_codex_and_claude() {
        assert_eq!(super::parse("codex-cli 0.166.0\n"), Some((0, 166, 0)));
        assert_eq!(super::parse("2.1.312 (Claude Code)"), Some((2, 1, 312)));
        assert_eq!(super::parse("command not found"), None);
    }
}
