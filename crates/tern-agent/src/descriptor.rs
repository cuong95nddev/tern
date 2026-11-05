use serde::Deserialize;

use crate::AgentId;
use crate::screen::ScreenRules;

/// What Tern can do for an agent beyond detecting it and reading its
/// screen. Only built-in agents have these: each is code in the crate that
/// owns it (see the crate docs), which a user file can't supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Start it in a tab with Tern's integration wired in.
    Launch,
    /// Its state from its own events (hooks, a plugin).
    Status,
    /// Its session id and how to resume it.
    Session,
    /// Read its conversation: the latest answer, what it does now.
    Transcript,
    /// How full its context window is.
    Context,
    /// Tell it something between turns (a handoff, Tern's guide).
    Steer,
    /// Its account's quota.
    Usage,
    /// Run it headless as a chat.
    Chat,
    /// Where its skill files go.
    Skills,
}

/// An agent's logo: asset stems (`agents/<stem>.svg`) for light and dark
/// themes; a logo in brand colors has the same stem twice.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logo {
    pub light: String,
    pub dark: String,
}

/// An agent, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    pub id: AgentId,
    /// Its product name: `Claude Code`.
    pub name: String,
    /// What menus call it: `New Claude Tab`.
    pub short_name: String,
    /// What a new tab runs.
    pub command: String,
    /// Flags added to `command`.
    pub args: Vec<String>,
    /// Foreground program names that identify it (see
    /// `tern_term::procs::foreground_programs`).
    pub processes: Vec<String>,
    pub logo: Option<Logo>,
    /// The command that resumes a session, `{session}` standing for its id;
    /// empty when it can't.
    pub resume: Vec<String>,
    /// Where its skills go, under the home folder.
    pub skills_dir: Option<String>,
    pub screen: ScreenRules,
}

impl Descriptor {
    /// The command that resumes `session`, when it can.
    pub fn resume_argv(&self, session: &str) -> Option<Vec<String>> {
        (!self.resume.is_empty()).then(|| self.resume.iter().map(|a| a.replace("{session}", session)).collect())
    }
}

/// A descriptor as a file has it: a built-in's has every field; a user's
/// has those it changes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Partial {
    pub id: String,
    pub name: Option<String>,
    pub short_name: Option<String>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub processes: Option<Vec<String>>,
    pub logo: Option<Logo>,
    pub resume: Option<Vec<String>>,
    pub skills_dir: Option<String>,
    pub screen: Option<ScreenRules>,
}

impl Partial {
    /// `base` with this file's fields laid over it.
    pub fn over(self, base: &Descriptor) -> Descriptor {
        let name = self.name.unwrap_or_else(|| base.name.clone());
        Descriptor {
            id: base.id.clone(),
            short_name: self.short_name.unwrap_or_else(|| base.short_name.clone()),
            name,
            command: self.command.unwrap_or_else(|| base.command.clone()),
            args: self.args.unwrap_or_else(|| base.args.clone()),
            processes: self.processes.unwrap_or_else(|| base.processes.clone()),
            logo: self.logo.or_else(|| base.logo.clone()),
            resume: self.resume.unwrap_or_else(|| base.resume.clone()),
            skills_dir: self.skills_dir.or_else(|| base.skills_dir.clone()),
            screen: self.screen.unwrap_or_else(|| base.screen.clone()),
        }
    }

    /// A new agent: it needs a name and a command.
    pub fn new_agent(self) -> Result<Descriptor, String> {
        let name = self.name.ok_or("a new agent needs a \"name\"")?;
        let command = self.command.ok_or("a new agent needs a \"command\"")?;
        Ok(Descriptor {
            id: AgentId::new(&self.id),
            short_name: self.short_name.unwrap_or_else(|| name.clone()),
            name,
            command,
            args: self.args.unwrap_or_default(),
            processes: self.processes.unwrap_or_default(),
            logo: self.logo,
            resume: self.resume.unwrap_or_default(),
            skills_dir: self.skills_dir,
            screen: self.screen.unwrap_or_default(),
        })
    }
}
