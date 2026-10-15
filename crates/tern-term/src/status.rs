//! What a terminal's program is doing, from its OSC 7501 status reports.
//! Shared by the UI (tab badges) and the daemon (notifications while no UI
//! is attached).

use serde::{Deserialize, Serialize};

use crate::osc::OscSignal;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityStatus {
    #[default]
    None,
    Working,
    Blocked,
    Done,
}

/// The status after `sig`, or `None` when `sig` doesn't change it.
pub fn next_status(prev: ActivityStatus, sig: &OscSignal) -> Option<ActivityStatus> {
    let OscSignal::ProgramStatus(st) = sig else { return None };
    let status = match st.state.as_str() {
        "working" => ActivityStatus::Working,
        "blocked" => ActivityStatus::Blocked,
        "clear" => ActivityStatus::None,
        _ => ActivityStatus::Done,
    };
    // A finished "working" turn with no explicit idle state reads as done.
    let status = if status == ActivityStatus::None && prev == ActivityStatus::Working { ActivityStatus::Done } else { status };
    (status != prev).then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::ProgramStatus;

    fn st(state: &str) -> OscSignal {
        OscSignal::ProgramStatus(ProgramStatus { state: state.into(), ..Default::default() })
    }

    #[test]
    fn maps_states() {
        use ActivityStatus::*;
        assert_eq!(next_status(None, &st("working")), Some(Working));
        assert_eq!(next_status(Working, &st("blocked")), Some(Blocked));
        assert_eq!(next_status(Working, &st("idle")), Some(Done));
        assert_eq!(next_status(Working, &st("clear")), Some(Done));
        assert_eq!(next_status(Blocked, &st("clear")), Some(None));
        assert_eq!(next_status(Working, &st("working")), Option::None);
    }

    #[test]
    fn other_signals_change_nothing() {
        let notify = OscSignal::Notify { title: None, body: "x".into() };
        assert_eq!(next_status(ActivityStatus::Working, &notify), None);
        assert_eq!(next_status(ActivityStatus::Working, &OscSignal::ProgramStatusProbe), None);
    }
}
