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
            "user" => out.push(Said::TurnStarted),
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
