//! Ported from Orca's tests (`agent-hook-listener-claude-task-notification.test.ts`,
//! `server-claude-cancel-captures.test.ts`, …). The captures in `fixtures/`
//! are Orca's (MIT, see `fixtures/LICENSE-orca`): Claude Code driven over a
//! real PTY with every hook logged.

use serde_json::{Value, json};

use super::*;

const SESSION: &str = "00000000-0000-4000-8000-000000000000";
const LEASE: u64 = OWED_NOTIFICATION_LEASE_MS;

fn records(name: &str) -> Vec<Value> {
    let text = match name {
        "task-notification" => include_str!("fixtures/claude-task-notification-hooks.jsonl"),
        "cancel-shell" => include_str!("fixtures/claude-cancel-shell-hooks.jsonl"),
        "permission" => include_str!("fixtures/claude-permission-approve-deny-hooks.jsonl"),
        _ => unreachable!(),
    };
    text.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn is_task_notification(p: &Value) -> bool {
    p["hook_event_name"] == "UserPromptSubmit" && p["prompt"].as_str().is_some_and(|s| s.starts_with("<task-notification>"))
}

/// A tab's tracker, fed payloads at a clock the test moves.
struct Tab {
    t: ClaudeTracker,
    now: u64,
}

impl Tab {
    fn new() -> Self {
        Self { t: ClaudeTracker::default(), now: 1_000_000 }
    }

    /// A tab whose main agent is in a turn.
    fn started() -> Self {
        let mut tab = Self::new();
        tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "run both"}));
        tab
    }

    fn post(&mut self, mut payload: Value) -> Option<ClaudeStatus> {
        if payload.get("session_id").is_none() {
            payload["session_id"] = SESSION.into();
        }
        self.now += 1;
        self.t.on_event(&payload, self.now, false)
    }

    fn state(&mut self, payload: Value) -> TurnState {
        self.post(payload).expect("a status").state
    }

    fn advance(&mut self, ms: u64) {
        self.now += ms;
    }

    fn launch_agent(&mut self, id: &str) {
        self.post(json!({"hook_event_name": "SubagentStart", "agent_id": id, "agent_type": "general-purpose"}));
        self.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_response": {"isAsync": true, "status": "async_launched", "agentId": id}}));
    }

    fn launch_shell(&mut self, id: &str) {
        self.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_response": {"backgroundTaskId": id}}));
    }
}

fn shell(id: &str) -> Value {
    json!({"id": id, "type": "shell", "status": "running"})
}

fn notification(id: &str, status: Option<&str>) -> Value {
    let status = status.map(|s| format!("\n<status>{s}</status>")).unwrap_or_default();
    json!({"hook_event_name": "UserPromptSubmit", "prompt": format!("<task-notification>\n<task-id>{id}</task-id>{status}")})
}

// ---- the captures ----

/// The status after each hook that told one, as (index, state), in order.
fn replay(scenario: &str) -> Vec<(usize, TurnState)> {
    let mut t = ClaudeTracker::default();
    let mut out = vec![];
    for (i, r) in records("task-notification").iter().enumerate() {
        if r["scenario"] != scenario || r.get("payload").is_none() {
            continue;
        }
        let now = (r["t"].as_f64().unwrap() * 1000.) as u64 + 1_000_000;
        if let Some(s) = t.on_event(&r["payload"], now, false)
            && !s.session_boundary
        {
            out.push((i, s.state));
        }
    }
    out
}

#[test]
fn captures_never_read_done_while_a_notification_is_to_come() {
    let all = records("task-notification");
    let mut scenarios: Vec<&str> = all.iter().filter_map(|r| r["scenario"].as_str()).collect();
    scenarios.dedup();
    let mut checked = 0;
    for scenario in &scenarios {
        let last = all.iter().rposition(|r| r["scenario"] == *scenario && r.get("payload").is_some_and(is_task_notification));
        let Some(last) = last else { continue };
        let early: Vec<usize> = replay(scenario).into_iter().filter(|&(i, s)| s == TurnState::Done && i < last).map(|(i, _)| i).collect();
        assert_eq!(early, Vec::<usize>::new(), "{scenario}");
        checked += 1;
    }
    assert!(checked >= 8, "{checked}");
}

#[test]
fn captures_settle_to_done_once_everything_is_delivered() {
    let all = records("task-notification");
    let mut scenarios: Vec<&str> = all.iter().filter_map(|r| r["scenario"].as_str()).collect();
    scenarios.dedup();
    assert_eq!(scenarios.len(), 12);
    for scenario in scenarios {
        assert_eq!(replay(scenario).last().map(|&(_, s)| s), Some(TurnState::Done), "{scenario}");
    }
}

#[test]
fn a_cancel_keeps_the_shell_the_turn_left_running() {
    let hooks: Vec<Value> = records("cancel-shell").into_iter().filter(|r| r["kind"] == "hook").collect();
    let mut tab = Tab::new();
    let at = |tab: &mut Tab, i: usize| {
        assert_eq!(hooks[i]["index"], i);
        tab.post(hooks[i]["payload"].clone())
    };
    for i in 0..4 {
        at(&mut tab, i);
    }
    let s = at(&mut tab, 4).unwrap();
    assert_eq!((s.state, s.monitoring), (TurnState::Working, true));
    assert_eq!(s.main_agent.unwrap().state, TurnState::Done);
    assert!(s.held.shells);

    // A foreground turn on top of it, cancelled mid-tool.
    at(&mut tab, 5);
    let s = at(&mut tab, 6).unwrap();
    assert_eq!((s.state, s.monitoring), (TurnState::Working, false));
    assert!(tab.t.main_agent_busy());
    let s = tab.t.interrupted(tab.now).unwrap();
    assert_eq!((s.state, s.monitoring), (TurnState::Working, true));
    assert_eq!(s.main_agent.unwrap().outcome, Some(Outcome::Cancellation));

    // The next turn's plain Stop carries no verdict.
    at(&mut tab, 7);
    let s = at(&mut tab, 8).unwrap();
    assert_eq!((s.state, s.monitoring), (TurnState::Working, true));
    assert_eq!(s.main_agent.unwrap().outcome, None);

    // The shell is killed from outside: the next inventory drops it, and the
    // notification Claude owes for it follows.
    for i in 9..=12 {
        at(&mut tab, i);
    }
    let s = at(&mut tab, 13).unwrap();
    assert!(!s.held.shells);
    for i in 14..hooks.len() {
        at(&mut tab, i);
    }
    tab.advance(LEASE);
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Done);
}

#[test]
fn a_permission_request_waits_until_approved() {
    let hooks: Vec<Value> = records("permission").into_iter().filter(|r| r["kind"] == "hook").collect();
    let mut tab = Tab::new();
    let states: Vec<Option<TurnState>> = hooks.iter().map(|h| tab.post(h["payload"].clone()).map(|s| s.state)).collect();
    use TurnState::*;
    // UserPromptSubmit, PreToolUse, PermissionRequest, Notification (no
    // status), PostToolUse (approved), PreToolUse, PermissionRequest,
    // Notification; the rejection fires no hook.
    assert_eq!(states, [Some(Working), Some(Working), Some(Waiting), None, Some(Working), Some(Working), Some(Waiting), None]);
    // Rejected with Esc: the turn is over.
    assert_eq!(tab.t.interrupted(tab.now).unwrap().state, Done);
}

// ---- owed notifications outside the captures ----

#[test]
fn an_owed_notification_holds_the_tab_without_listing_a_child() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    let s = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    assert_eq!(s.state, TurnState::Working);
    assert_eq!(s.wakeup, Some(Wakeup::Notification));
    assert!(s.subagents.is_empty());
    assert!(s.held.owed);
}

#[test]
fn a_turn_end_about_to_resume_is_not_announced() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    let owed_only = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    assert_eq!(owed_only.turn_completed_at, None);

    tab.launch_agent("a2");
    let child_running =
        tab.post(json!({"hook_event_name": "Stop", "background_tasks": [{"id": "a2", "type": "subagent", "status": "running"}]})).unwrap();
    assert!(child_running.turn_completed_at.is_some());
    assert!(child_running.held_open());
}

#[test]
fn a_replaced_conversation_owes_nothing() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "hello", "session_id": "next-session"}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": [], "session_id": "next-session"})), TurnState::Done);
}

#[test]
fn a_notified_subagent_that_resumes_owes_again() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    tab.post(notification("a1", Some("completed")));
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": []}));
    // Its own background work ending resumes it, with no launching tool call.
    tab.post(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "general-purpose"}));
    assert_eq!(tab.state(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"})), TurnState::Working);
}

#[test]
fn a_monitor_event_turn_is_not_its_end() {
    let mut tab = Tab::started();
    tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Monitor", "tool_response": {"taskId": "b1"}}));
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [shell("b1")]}));
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "<task-notification>\n<task-id>b1</task-id>\n<event>tick 1</event>"}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Working);
    tab.post(notification("b1", Some("completed")));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
}

#[test]
fn a_status_tag_in_monitor_output_is_not_its_end() {
    let mut tab = Tab::started();
    tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Monitor", "tool_response": {"taskId": "b1"}}));
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [shell("b1")]}));
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "<task-notification>\n<task-id>b1</task-id>\n<summary>Monitor event: \"health\"</summary>\n<event><status>up</status></event>"}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Working);
}

#[test]
fn a_stop_without_an_inventory_says_nothing_of_shells() {
    let mut tab = Tab::started();
    tab.launch_shell("b1");
    assert_eq!(tab.state(json!({"hook_event_name": "Stop"})), TurnState::Done);
}

#[test]
fn past_its_cap_it_forgets_only_what_nobody_waits_on() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    for i in 0..300 {
        tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_response": {"isAsync": true, "agentId": format!("a{}", i + 2)}}));
    }
    assert_eq!(tab.t.tasks.len(), 256);
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Working);
}

#[test]
fn at_its_cap_it_forgets_an_announced_subagent() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    tab.post(notification("a1", Some("completed")));
    for i in 2..=257 {
        tab.post(
            json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_response": {"isAsync": true, "agentId": format!("a{i}")}}),
        );
    }
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [{"id": "a257", "type": "subagent", "status": "running"}]}));
    assert_eq!(tab.state(json!({"hook_event_name": "SubagentStop", "agent_id": "a257"})), TurnState::Working);
}

#[test]
fn a_failed_turn_end_owes_a_vanished_shell_too() {
    let mut tab = Tab::started();
    tab.launch_shell("b1");
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [shell("b1")]}));
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "and now?"}));
    assert_eq!(tab.state(json!({"hook_event_name": "StopFailure", "background_tasks": []})), TurnState::Working);
}

#[test]
fn an_idle_teammate_owes_nothing() {
    let mut tab = Tab::started();
    tab.launch_agent("areviewer-6d3cb5b52120b7bf");
    tab.post(json!({"hook_event_name": "TeammateIdle", "teammate_name": "reviewer"}));
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "areviewer-6d3cb5b52120b7bf"}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
}

#[test]
fn a_stopped_shell_owes_nothing() {
    let mut tab = Tab::started();
    tab.launch_shell("b1");
    tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "TaskStop", "tool_response": {"task_id": "b1"}}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
}

// ---- the lease ----

/// The main agent launched shell b1, ended its turn, and b1 then vanished
/// unannounced.
fn shell_gone_unannounced() -> (Tab, ClaudeStatus) {
    let mut tab = Tab::started();
    tab.launch_shell("b1");
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [shell("b1")]}));
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "thanks"}));
    let stopped = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    (tab, stopped)
}

#[test]
fn an_idle_main_agent_is_owed_for_one_lease() {
    let (mut tab, stopped) = shell_gone_unannounced();
    assert_eq!(stopped.state, TurnState::Working);
    let deadline = tab.t.deadline().unwrap();
    assert_eq!(deadline, tab.now + LEASE);
    tab.advance(LEASE - 1);
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Working);
    tab.advance(1);
    let s = tab.t.current(tab.now).unwrap();
    assert_eq!((s.state, s.monitoring, s.held.owed), (TurnState::Done, false, false));
    assert_eq!(tab.t.deadline(), None);
}

#[test]
fn a_settled_cancel_keeps_its_verdict() {
    let mut tab = Tab::started();
    tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_response": {"isAsync": true, "agentId": "a1"}}));
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    let cancelled = tab.t.interrupted(tab.now).unwrap();
    assert_eq!(cancelled.state, TurnState::Working);
    tab.advance(LEASE);
    let s = tab.t.current(tab.now).unwrap();
    assert_eq!(s.state, TurnState::Done);
    assert_eq!(s.main_agent.unwrap().outcome, Some(Outcome::Cancellation));
}

#[test]
fn the_lease_does_not_run_during_a_turn() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"}));
    assert_eq!(tab.t.deadline(), None);
    // Queued behind a long foreground tool.
    tab.advance(LEASE * 5);
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Working);
    tab.advance(LEASE);
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Done);
}

#[test]
fn a_task_ending_long_after_the_turn_gets_its_own_lease() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [{"id": "a1", "type": "subagent", "status": "running"}]}));
    tab.advance(LEASE * 5);
    assert_eq!(tab.state(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"})), TurnState::Working);
    tab.advance(LEASE - 1);
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Working);
    tab.advance(1);
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Done);
}

#[test]
fn an_expired_lease_is_noticed_on_the_next_hook_too() {
    let (mut tab, _) = shell_gone_unannounced();
    tab.advance(LEASE);
    // Claude's prompt-suggestion helper: an unannounced SubagentStop after a Stop.
    assert_eq!(tab.state(json!({"hook_event_name": "SubagentStop", "agent_id": "ahelper"})), TurnState::Done);
}

#[test]
fn a_notification_given_up_on_is_not_waited_for_again() {
    let (mut tab, _) = shell_gone_unannounced();
    tab.advance(LEASE);
    tab.t.current(tab.now);
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "one more thing"}));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
}

#[test]
fn a_notification_in_time_opens_a_wakeup_turn() {
    let (mut tab, _) = shell_gone_unannounced();
    let s = tab.post(notification("b1", Some("completed"))).unwrap();
    assert_eq!(s.wakeup, Some(Wakeup::FinishingTurn));
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
    assert_eq!(tab.t.deadline(), None);
}

// ---- the main agent's states ----

#[test]
fn ask_user_question_waits_and_clears_when_answered() {
    let mut tab = Tab::started();
    let s = tab.post(json!({"hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_use_id": "q1"})).unwrap();
    assert_eq!(s.state, TurnState::Waiting);
    assert!(tab.t.asking());
    // A parallel sibling finishing does not clear it.
    tab.post(json!({"hook_event_name": "PostToolUse", "tool_name": "Read", "tool_use_id": "r1"}));
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Waiting);
    assert_eq!(tab.t.question_answered(tab.now).unwrap().state, TurnState::Working);
    assert!(!tab.t.asking());
}

#[test]
fn a_childs_permission_wait_goes_back_to_what_it_displaced() {
    let mut tab = Tab::started();
    tab.launch_agent("a1");
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [{"id": "a1", "type": "subagent", "status": "running"}]}));
    let s = tab.post(json!({"hook_event_name": "PermissionRequest", "agent_id": "a1", "tool_name": "Bash"})).unwrap();
    assert_eq!(s.state, TurnState::Waiting);
    // The main agent is still done underneath.
    assert_eq!(s.main_agent.unwrap().state, TurnState::Done);
    // Another child's activity doesn't clear it.
    tab.post(json!({"hook_event_name": "PostToolUse", "agent_id": "a2", "tool_name": "Read"}));
    assert_eq!(tab.t.current(tab.now).unwrap().state, TurnState::Waiting);
    // Its own next tool event does: the main agent is done, the child works.
    let s = tab.post(json!({"hook_event_name": "PostToolUse", "agent_id": "a1", "tool_name": "Bash"})).unwrap();
    assert_eq!(s.state, TurnState::Working);
    assert_eq!(s.main_agent.unwrap().state, TurnState::Done);
    assert!(s.held_open());
}

#[test]
fn a_waiting_child_that_stops_releases_its_wait() {
    let mut tab = Tab::started();
    tab.post(json!({"hook_event_name": "SubagentStart", "agent_id": "a1"}));
    tab.post(json!({"hook_event_name": "PermissionRequest", "agent_id": "a1", "tool_name": "Bash"}));
    assert_eq!(tab.state(json!({"hook_event_name": "SubagentStop", "agent_id": "a1"})), TurnState::Working);
}

#[test]
fn a_session_cron_monitors() {
    let mut tab = Tab::started();
    let s = tab.post(json!({"hook_event_name": "Stop", "background_tasks": [], "session_crons": [{"id": "c1"}]})).unwrap();
    assert_eq!((s.state, s.monitoring, s.held.cron), (TurnState::Working, true, true));
    assert!(s.turn_completed_at.is_some());
    let s = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    assert_eq!(s.state, TurnState::Done);
}

#[test]
fn a_manual_compact_clears_the_tab() {
    let mut tab = Tab::started();
    let s = tab.post(json!({"hook_event_name": "PostCompact", "trigger": "manual"})).unwrap();
    assert_eq!(s.state, TurnState::Done);
    assert!(s.session_boundary);
    // An auto compact is inside a turn: nothing.
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    assert!(tab.post(json!({"hook_event_name": "PostCompact", "trigger": "auto"})).is_none());
    // Nor does the continuation turn after it start one.
    assert!(tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "This session is being continued from a previous conversation that ran out"})).is_none());
}

#[test]
fn a_session_start_is_a_boundary_and_forgets_the_old_process() {
    let mut tab = Tab::started();
    tab.launch_shell("b1");
    tab.post(json!({"hook_event_name": "Stop", "background_tasks": [shell("b1")]}));
    let s = tab.post(json!({"hook_event_name": "SessionStart", "source": "clear"})).unwrap();
    assert_eq!(s.state, TurnState::Done);
    assert!(s.session_boundary);
    assert!(tab.post(json!({"hook_event_name": "SessionStart", "source": "compact"})).is_none());
}

#[test]
fn a_stop_tern_blocked_goes_on() {
    let mut tab = Tab::started();
    tab.now += 1;
    let s = tab.t.on_event(&json!({"hook_event_name": "Stop", "session_id": SESSION, "background_tasks": []}), tab.now, true).unwrap();
    assert_eq!(s.state, TurnState::Working);
    assert_eq!(tab.state(json!({"hook_event_name": "Stop", "background_tasks": []})), TurnState::Done);
}

#[test]
fn a_cancel_carries_to_the_turns_end() {
    let mut tab = Tab::started();
    assert_eq!(tab.t.interrupted(tab.now).unwrap().state, TurnState::Done);
    let s = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    assert_eq!(s.main_agent.unwrap().outcome, Some(Outcome::Cancellation));
    // A new turn drops it.
    tab.post(json!({"hook_event_name": "UserPromptSubmit", "prompt": "again"}));
    let s = tab.post(json!({"hook_event_name": "Stop", "background_tasks": []})).unwrap();
    assert_eq!(s.main_agent.unwrap().outcome, None);
}

#[test]
fn the_tracker_survives_a_handoff() {
    let (tab, _) = shell_gone_unannounced();
    let json = serde_json::to_string(&tab.t).unwrap();
    let back: ClaudeTracker = serde_json::from_str(&json).unwrap();
    assert_eq!(back, tab.t);
}

#[test]
fn teammate_ids() {
    assert!(is_teammate_id("areviewer-6d3cb5b52120b7bf"));
    assert!(!is_teammate_id("a9de070bc47af3cd6"));
    assert!(teammate_id_matches("arev-1a2b", "rev"));
    assert!(!teammate_id_matches("arev-two-1a2b", "rev"));
    assert!(teammate_id_matches("arev-two-1a2b", "rev-two"));
}
