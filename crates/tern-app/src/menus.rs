//! The application menus. macOS shows them in the system menu bar; on
//! Windows and Linux GPUI only keeps them, and windows draw no menu bar (as
//! in T3 Code: Settings sits under the agents panel). Rebuilt when the
//! menu bar icon setting may have changed (toggled here, or hidden from the
//! menu bar agent's own menu) and when the recent folders change.

use std::path::PathBuf;

use gpui_kit::*;

use crate::theme_picker::{OpenSettings, SelectTheme};
use crate::{OpenFolder, Quit, ToggleMenuBarIcon, agent_monitor, main_window, restart, terminal_view, update, window_state, workspace};

/// Open a folder from File ▸ Open Recent.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = tern, no_json)]
pub struct OpenRecent(pub PathBuf);

actions!(tern, [ClearRecent]);

/// Handle the Open Recent items.
pub fn init(cx: &mut App) {
    cx.on_action(|a: &OpenRecent, cx| main_window::open_folder(a.0.clone(), cx));
    cx.on_action(|_: &ClearRecent, cx| {
        window_state::clear_recent();
        install(cx);
        // The welcome screen lists them too.
        cx.refresh_windows();
    });
}

pub fn install(cx: &mut App) {
    cx.set_menus(menus());
}

fn menus() -> Vec<Menu> {
    let mut file = vec![
        MenuItem::action("New Claude Tab", workspace::NewClaudeTab),
        MenuItem::action("New Shell Tab", workspace::NewShellTab),
        MenuItem::action("New Scratch File", workspace::NewScratch),
        MenuItem::separator(),
        MenuItem::action("New Window", main_window::NewWindow),
        MenuItem::action("Open Folder…", OpenFolder),
        open_recent(),
        MenuItem::separator(),
        MenuItem::action("Close Tab", workspace::CloseTab),
        MenuItem::action("Close Workspace", main_window::CloseWorkspace),
    ];
    let edit = Menu::new("Edit").items([
        MenuItem::action("Copy", terminal_view::Copy),
        MenuItem::action("Paste", terminal_view::Paste),
        MenuItem::action("Select All", terminal_view::SelectAll),
        MenuItem::separator(),
        MenuItem::action("Clear Terminal", terminal_view::Clear),
    ]);
    let view = Menu::new("View").items([
        MenuItem::action("Command Palette…", workspace::CommandPalette),
        MenuItem::action("Go to File…", workspace::GoToFile),
        MenuItem::action("Search in Files", workspace::SearchInFiles),
        MenuItem::separator(),
        MenuItem::action("Explorer", workspace::ToggleExplorer),
        MenuItem::action("Changes", workspace::ToggleChanges),
        MenuItem::action("Git Log", workspace::OpenGitLog),
        MenuItem::action("Scratches", workspace::ToggleScratches),
        MenuItem::action("Usage", workspace::ToggleUsage),
        MenuItem::action("System", workspace::ToggleSystem),
        MenuItem::action("Reset Layout", workspace::ResetLayout),
        MenuItem::action("Focus Terminal", workspace::FocusTerminal),
        MenuItem::action("Agent Monitor", agent_monitor::ToggleAgentMonitor),
        MenuItem::action("Word Wrap", crate::viewer::ToggleWordWrap),
        MenuItem::separator(),
        MenuItem::action("Split Right", workspace::SplitRight),
        MenuItem::action("Split Down", workspace::SplitDown),
        MenuItem::action("Maximize Pane", workspace::ToggleZoomPane),
        MenuItem::separator(),
        MenuItem::action("Next Tab", workspace::NextTab),
        MenuItem::action("Previous Tab", workspace::PrevTab),
        MenuItem::action("Next Workspace", main_window::NextWorkspace),
        MenuItem::action("Previous Workspace", main_window::PrevWorkspace),
    ]);
    if cfg!(target_os = "macos") {
        let shown = tern_menubar::setting::enabled(&tern_menubar::setting::file());
        let mut items = vec![
            MenuItem::action("Check for Updates…", update::CheckForUpdates),
            MenuItem::action("Restart Background Terminals…", restart::RestartBackground),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Theme…", SelectTheme),
            MenuItem::action(menu_bar_toggle_label(shown), ToggleMenuBarIcon),
            MenuItem::separator(),
        ];
        #[cfg(target_os = "macos")]
        items.extend([MenuItem::action(capture_label(), crate::shot::CaptureScreenshot), MenuItem::separator()]);
        items.extend([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Tern", Quit),
        ]);
        let app = Menu::new("Tern").items(items);
        vec![app, Menu::new("File").items(file), edit, view]
    } else {
        #[cfg(windows)]
        file.extend([MenuItem::separator(), MenuItem::action(capture_label(), crate::shot::CaptureScreenshot)]);
        file.extend([
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Theme…", SelectTheme),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ]);
        let file = Menu::new("File").items(file);
        let help = Menu::new("Help").items([
            MenuItem::action("Check for Updates…", update::CheckForUpdates),
            MenuItem::action("Restart Background Terminals…", restart::RestartBackground),
        ]);
        vec![file, edit, view, help]
    }
}

/// The recent folders, most recent first, then a way to forget them.
fn open_recent() -> MenuItem {
    let recent = window_state::recent_folders();
    let mut items: Vec<MenuItem> = recent.iter().map(|p| MenuItem::action(crate::dirs::tilde(p), OpenRecent(p.clone()))).collect();
    if !items.is_empty() {
        items.push(MenuItem::separator());
    }
    items.push(MenuItem::Action {
        name: "Clear Recent".into(),
        action: Box::new(ClearRecent),
        os_action: None,
        checked: false,
        disabled: recent.is_empty(),
    });
    MenuItem::submenu(Menu::new("Open Recent").items(items))
}

#[cfg(any(target_os = "macos", windows))]
fn capture_label() -> String {
    format!("Capture Screenshot ({})", crate::shot::HOTKEY_LABEL)
}

fn menu_bar_toggle_label(shown: bool) -> &'static str {
    if shown { "Hide Menu Bar Icon" } else { "Show Menu Bar Icon" }
}

#[cfg(test)]
mod tests {
    #[test]
    fn menu_bar_toggle_label_follows_the_setting() {
        assert_eq!(super::menu_bar_toggle_label(true), "Hide Menu Bar Icon");
        assert_eq!(super::menu_bar_toggle_label(false), "Show Menu Bar Icon");
    }
}
