//! What Claude Code's headless lines say, for the daemon.

use serde_json::Value;
use tern_bridge::context::Meter;

use crate::driver::{ChatFollow, Said};

/// Follows a Claude chat: its turns and requests, and how full its context is.
#[derive(Debug, Default)]
pub struct ClaudeFollow {
    meter: Meter,
}

impl ChatFollow for ClaudeFollow {
    fn follow(&mut self, v: &Value, out: &mut Vec<Said>) {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_owned);
        self.meter.read(v);
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        // Only these read anew: another line may come after a fresh
        // conversation's `SessionStart` with the old one's reading.
        if matches!(kind, "assistant" | "result")
            && let Some(r) = self.meter.reading()
        {
            out.push(Said::Reading(r));
        }
        match kind {
            "user" => {
                out.push(Said::TurnStarted);
                out.extend(user_text(v).map(Said::Title));
            },
            "stream_event" => out.push(Said::Streamed),
            "result" => out.push(Said::TurnEnded),
            "system" if s("/subtype") == "init" => out.push(Said::Session(s("/session_id"))),
            // A permission, a question, or an MCP server's form: the user's turn.
            "control_request" => {
                let subtype = s("/request/subtype");
                if matches!(subtype.as_str(), "can_use_tool" | "elicitation") {
                    out.push(Said::Ask(s("/request_id")));
                }
                if subtype == "elicitation" {
                    out.extend(id("/request_id").map(Said::Form));
                }
            },
            "control_cancel_request" => {
                out.push(Said::Settled(s("/request_id")));
                out.extend(id("/request_id").map(Said::FormSettled));
            },
            "control_response" => {
                out.push(Said::Settled(s("/response/request_id")));
                out.extend(id("/response/request_id").map(Said::FormSettled));
            },
            _ => {},
        }
    }

    fn notice(&self, line: &Value, agent: &str) -> Option<String> {
        notice_of(line, agent)
    }
}

/// The text of a user message the program echoed, cut to a title.
fn user_text(v: &Value) -> Option<String> {
    let content = v.pointer("/message/content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            parts.iter().find_map(|p| (p.get("type")?.as_str()? == "text").then(|| p.get("text")?.as_str().map(str::to_owned))?)?
        },
        _ => return None,
    };
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(60).collect())
}

/// What a line tells the user while no UI is connected; `agent` names the
/// agent that wrote it.
fn notice_of(v: &Value, agent: &str) -> Option<String> {
    match v.get("type")?.as_str()? {
        "control_request" if v.pointer("/request/subtype")?.as_str()? == "can_use_tool" => {
            Some(match v.pointer("/request/tool_name").and_then(Value::as_str) {
                Some("AskUserQuestion") => format!("{agent} has a question"),
                Some("ExitPlanMode") => format!("{agent} has a plan to approve"),
                Some(tool) => format!("{agent} asks to use {tool}"),
                None => format!("{agent} asks for permission"),
            })
        },
        "control_request" if v.pointer("/request/subtype")?.as_str()? == "elicitation" => {
            Some(match v.pointer("/request/mcp_server_name").and_then(Value::as_str) {
                Some(server) => format!("{server} asks for your input"),
                None => "An MCP server asks for your input".into(),
            })
        },
        "result" => {
            // Stopped by the user: no news (older CLIs say so only by an
            // error during execution without errors).
            let stopped = match v.get("terminal_reason").and_then(Value::as_str) {
                Some(r) => r.starts_with("aborted"),
                None => {
                    v.get("subtype").and_then(Value::as_str) == Some("error_during_execution")
                        && v.get("errors").and_then(Value::as_array).is_none_or(|e| e.is_empty())
                },
            };
            Some(match v.get("subtype").and_then(Value::as_str) {
                _ if stopped => return None,
                Some("success") => format!("{agent} finished"),
                _ => format!("{agent} stopped with an error"),
            })
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::driver::chat_follower;

    fn said(lines: &[Value]) -> Vec<Said> {
        let mut follower = chat_follower("claude").unwrap();
        let mut out = Vec::new();
        for line in lines {
            follower.follow(line, &mut out);
        }
        out
    }

    #[test]
    fn claude_follower_says_what_state_followed() {
        let lines = [
            json!({"type":"user"}),
            json!({"type":"system","subtype":"init","session_id":"s1"}),
            json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool"}}),
            json!({"type":"control_response","response":{"request_id":"r1"}}),
            json!({"type":"control_request","request_id":"r2","request":{"subtype":"hook_callback"}}),
            json!({"type":"result"}),
        ];
        assert_eq!(
            said(&lines),
            [
                Said::TurnStarted,
                Said::Session("s1".into()),
                Said::Ask("r1".into()),
                Said::Settled("r1".into()),
                Said::FormSettled("r1".into()),
                Said::TurnEnded,
            ]
        );
    }

    #[test]
    fn tells_forms_and_failures_but_not_a_stop() {
        let form = json!({"type":"control_request","request_id":"e","request":{"subtype":"elicitation","mcp_server_name":"deploy"}});
        assert_eq!(notice_of(&form, "Claude").as_deref(), Some("deploy asks for your input"));
        let stopped = json!({"type":"result","subtype":"error_during_execution","terminal_reason":"aborted_streaming"});
        assert_eq!(notice_of(&stopped, "Claude"), None);
        let failed = json!({"type":"result","subtype":"error_during_execution","terminal_reason":"model_error","errors":["500"]});
        assert_eq!(notice_of(&failed, "Claude").as_deref(), Some("Claude stopped with an error"));
        assert_eq!(notice_of(&failed, "Codex").as_deref(), Some("Codex stopped with an error"));
    }

    #[test]
    fn claude_follower_says_a_form_is_an_ask_too() {
        let lines = [
            json!({"type":"control_request","request_id":"e1","request":{"subtype":"elicitation"}}),
            json!({"type":"control_cancel_request","request_id":"e1"}),
        ];
        assert_eq!(
            said(&lines),
            [Said::Ask("e1".into()), Said::Form("e1".into()), Said::Settled("e1".into()), Said::FormSettled("e1".into())]
        );
    }

    #[test]
    fn claude_follower_reads_context_when_a_turn_ends() {
        let lines = [
            json!({"type":"assistant","message":{"model":"m","usage":{"input_tokens":40}}}),
            json!({"type":"result","modelUsage":{"m":{"contextWindow":100}}}),
        ];
        let reading = tern_bridge::context::Reading { percent: 40., size: 100 };
        assert_eq!(said(&lines), [Said::Reading(reading), Said::TurnEnded]);
    }
}
