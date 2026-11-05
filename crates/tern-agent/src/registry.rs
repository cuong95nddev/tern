//! The agents Tern knows: the built-in descriptors, with the user's files
//! laid over them.

use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::descriptor::Partial;
use crate::{CLAUDE, CODEX, Capability, Descriptor};

/// The built-in agents, in the order menus list them.
const BUILTIN: &[(&str, &str)] = &[
    ("claude", include_str!("../agents/claude.json")),
    ("codex", include_str!("../agents/codex.json")),
    ("opencode", include_str!("../agents/opencode.json")),
    ("gemini", include_str!("../agents/gemini.json")),
    ("cursor", include_str!("../agents/cursor.json")),
    ("grok", include_str!("../agents/grok.json")),
    ("pi", include_str!("../agents/pi.json")),
];

/// The capabilities Tern has code for, per built-in agent.
fn builtin_capabilities(id: &str) -> &'static [Capability] {
    use Capability::*;
    match id {
        CLAUDE => &[Launch, Status, Session, Transcript, Context, Steer, Usage, Chat, Skills],
        CODEX => &[Launch, Status, Session, Transcript, Context, Steer, Usage, Skills],
        _ => &[],
    }
}

/// The agents Tern knows.
#[derive(Debug, Clone)]
pub struct Registry {
    /// Built-ins first, in [`BUILTIN`]'s order, then the user's by id.
    agents: Vec<Descriptor>,
}

impl Registry {
    /// The built-in agents alone.
    pub fn builtin() -> Self {
        let agents = BUILTIN
            .iter()
            .map(|(id, text)| {
                let partial: Partial = serde_json::from_str(text).unwrap_or_else(|e| panic!("built-in agent {id}: {e}"));
                assert_eq!(partial.id, *id, "built-in agent file names another id");
                partial.new_agent().unwrap_or_else(|e| panic!("built-in agent {id}: {e}"))
            })
            .collect();
        Self { agents }
    }

    /// The built-ins with the files in `user_dir` (`<id>.json`) laid over
    /// them, and what is wrong in those files. A file that can't be read
    /// is skipped whole: a built-in stays as it was.
    pub fn load(user_dir: Option<&Path>) -> (Self, Vec<String>) {
        let mut registry = Self::builtin();
        let mut problems = Vec::new();
        let Some(dir) = user_dir else { return (registry, problems) };
        let mut files: Vec<_> = match std::fs::read_dir(dir) {
            Ok(entries) => {
                entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (registry, problems),
            Err(e) => {
                problems.push(format!("{}: {e}", dir.display()));
                return (registry, problems);
            },
        };
        files.sort();
        let builtins = registry.agents.len();
        for path in files {
            if let Err(e) = registry.lay(&path) {
                problems.push(format!("{}: {e}", path.display()));
            }
        }
        registry.agents[builtins..].sort_by(|a, b| a.id.cmp(&b.id));
        (registry, problems)
    }

    /// Lay the file at `path` over the agents.
    fn lay(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let partial: Partial = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if partial.id != stem {
            return Err(format!("its id is \"{}\": name it {}.json", partial.id, partial.id));
        }
        match self.agents.iter_mut().find(|d| d.id.is(&partial.id)) {
            Some(d) => *d = partial.over(d),
            None => self.agents.push(partial.new_agent()?),
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Descriptor> {
        self.agents.iter().find(|d| d.id.is(id))
    }

    /// Every agent: built-ins first, then the user's by id.
    pub fn all(&self) -> impl Iterator<Item = &Descriptor> {
        self.agents.iter()
    }

    /// The agent among a terminal's foreground programs (see
    /// `tern_term::procs::foreground_programs`), by exact name.
    pub fn detect(&self, programs: &[String]) -> Option<&Descriptor> {
        programs.iter().find_map(|p| self.agents.iter().find(|d| d.processes.iter().any(|q| q == p)))
    }

    /// Tern has capability `cap` for agent `id`.
    pub fn has(&self, id: &str, cap: Capability) -> bool {
        self.get(id).is_some() && builtin_capabilities(id).contains(&cap) && BUILTIN.iter().any(|(b, _)| *b == id)
    }

    /// The built-in agents' ids.
    pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
        BUILTIN.iter().map(|(id, _)| *id)
    }
}

static REGISTRY: RwLock<Option<Arc<Registry>>> = RwLock::new(None);

/// Load the agents (see [`Registry::load`]) for [`registry`] to give, and
/// return what is wrong in the user's files.
pub fn init(user_dir: Option<&Path>) -> Vec<String> {
    let (registry, problems) = Registry::load(user_dir);
    *REGISTRY.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(registry));
    problems
}

/// The agents [`init`] loaded; the built-ins alone before it ran.
pub fn registry() -> Arc<Registry> {
    if let Some(r) = &*REGISTRY.read().unwrap_or_else(|e| e.into_inner()) {
        return r.clone();
    }
    let mut slot = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| Arc::new(Registry::builtin())).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;

    fn user(files: &[(&str, &str)]) -> (Registry, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        Registry::load(Some(dir.path()))
    }

    fn programs(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn detect_matches_exact_program_names() {
        let r = Registry::builtin();
        let id = |p: &[&str]| r.detect(&programs(p)).map(|d| d.id.as_str().to_owned());
        assert_eq!(id(&["zsh", "claude"]).as_deref(), Some("claude"));
        assert_eq!(id(&["cursor-agent"]).as_deref(), Some("cursor"));
        // What `procs::foreground_programs` gives for `node …/codex.js`.
        assert_eq!(id(&["node", "codex"]).as_deref(), Some("codex"));
        assert_eq!(id(&["claudette"]), None);
    }

    #[test]
    fn user_file_overrides_fields_and_keeps_capabilities() {
        let (r, problems) = user(&[("claude.json", r#"{"id":"claude","command":"claude-beta"}"#)]);
        assert!(problems.is_empty(), "{problems:?}");
        let c = r.get("claude").unwrap();
        assert_eq!(c.command, "claude-beta");
        assert_eq!(c.name, "Claude Code");
        assert!(r.has("claude", Capability::Chat));
    }

    #[test]
    fn user_file_adds_a_basic_agent() {
        let (r, problems) = user(&[("amp.json", r#"{"id":"amp","name":"Amp","command":"amp","processes":["amp"]}"#)]);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(r.get("amp").is_some());
        assert!(!r.has("amp", Capability::Status));
        assert_eq!(r.detect(&programs(&["amp"])).map(|d| d.id.as_str()), Some("amp"));
    }

    #[test]
    fn user_file_cannot_grant_capabilities() {
        let (r, problems) = user(&[("amp.json", r#"{"id":"amp","name":"Amp","command":"amp","capabilities":["chat"]}"#)]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(!r.has("amp", Capability::Chat));
    }

    #[test]
    fn broken_user_file_is_reported_and_builtins_survive() {
        let (r, problems) = user(&[("claude.json", "{oops")]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("claude.json"), "{problems:?}");
        assert_eq!(r.get("claude").unwrap().command, "claude");
        assert!(r.has("claude", Capability::Chat));
    }

    #[test]
    fn a_new_agent_needs_a_name_and_a_command() {
        let (r, problems) = user(&[("amp.json", r#"{"id":"amp"}"#)]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(r.get("amp").is_none());
    }

    #[test]
    fn claude_and_codex_launch_with_tern_wired_in() {
        let r = Registry::builtin();
        assert!(r.has("claude", Capability::Launch));
        assert!(r.has("codex", Capability::Launch));
        for id in ["opencode", "gemini", "cursor", "grok", "pi"] {
            assert!(!r.has(id, Capability::Launch), "{id}");
        }
    }

    #[test]
    fn short_name_defaults_to_name() {
        assert_eq!(Registry::builtin().get("claude").unwrap().short_name, "Claude");
        let (r, _) = user(&[("amp.json", r#"{"id":"amp","name":"Amp","command":"amp"}"#)]);
        assert_eq!(r.get("amp").unwrap().short_name, "Amp");
    }

    #[test]
    fn resume_argv_fills_the_session() {
        let r = Registry::builtin();
        let argv = r.get("codex").unwrap().resume_argv("abc");
        assert_eq!(argv, Some(vec!["codex".to_owned(), "resume".to_owned(), "abc".to_owned()]));
        assert_eq!(r.get("gemini").unwrap().resume_argv("abc"), None);
    }

    #[test]
    fn builtins_come_first_in_order_then_user_ids_sorted() {
        let (r, _) = user(&[
            ("zed.json", r#"{"id":"zed","name":"Zed","command":"zed"}"#),
            ("amp.json", r#"{"id":"amp","name":"Amp","command":"amp"}"#),
        ]);
        let ids: Vec<&str> = r.all().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["claude", "codex", "opencode", "gemini", "cursor", "grok", "pi", "amp", "zed"]);
    }

    #[test]
    fn a_file_named_after_another_id_is_reported() {
        let (_, problems) = user(&[("amp.json", r#"{"id":"zed","name":"Zed","command":"zed"}"#)]);
        assert_eq!(problems.len(), 1, "{problems:?}");
    }
}
