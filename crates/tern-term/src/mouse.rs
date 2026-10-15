//! Mouse reporting encoders (X10 / SGR), used when the program enables mouse
//! tracking — Claude Code's fullscreen renderer does.

use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Should this event be reported at all, given the current mode?
pub fn wants(mode: TermMode, action: MouseAction, button: MouseButton) -> bool {
    match action {
        MouseAction::Motion if button == MouseButton::None => mode.contains(TermMode::MOUSE_MOTION),
        MouseAction::Motion => mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG),
        _ => mode.intersects(TermMode::MOUSE_MODE),
    }
}

/// Encode a mouse event at 0-based `(col, row)`.
pub fn encode(mode: TermMode, action: MouseAction, button: MouseButton, mods: MouseMods, col: usize, row: usize) -> Option<Vec<u8>> {
    if !wants(mode, action, button) {
        return None;
    }
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    code += (mods.shift as u32) * 4 + (mods.alt as u32) * 8 + (mods.ctrl as u32) * 16;

    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }

    // Legacy X10/normal: release is reported as button 3; coords limited to 223.
    if action == MouseAction::Release {
        code = 3 + (code & !3);
    }
    if col > 222 || row > 222 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', 32 + code as u8, 33 + col as u8, 33 + row as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_press_release_wheel() {
        let m = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let none = MouseMods::default();
        assert_eq!(encode(m, MouseAction::Press, MouseButton::Left, none, 0, 0).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(encode(m, MouseAction::Release, MouseButton::Left, none, 4, 2).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(encode(m, MouseAction::Press, MouseButton::WheelDown, none, 1, 1).unwrap(), b"\x1b[<65;2;2M");
    }

    #[test]
    fn motion_needs_mode() {
        let m = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert!(encode(m, MouseAction::Motion, MouseButton::Left, Default::default(), 0, 0).is_none());
        let m = m | TermMode::MOUSE_DRAG;
        assert_eq!(encode(m, MouseAction::Motion, MouseButton::Left, Default::default(), 0, 0).unwrap(), b"\x1b[<32;1;1M");
    }

    #[test]
    fn legacy_encoding() {
        let m = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode(m, MouseAction::Press, MouseButton::Left, Default::default(), 0, 0).unwrap(), vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn disabled_without_mode() {
        assert!(encode(TermMode::empty(), MouseAction::Press, MouseButton::Left, Default::default(), 0, 0).is_none());
    }
}
