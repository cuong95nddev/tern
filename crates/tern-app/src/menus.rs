//! The application menus. macOS shows them in the system menu bar; on
//! Windows and Linux GPUI only keeps them, and every window draws them in
//! its own menu bar (`AppMenuBar`, see `main_window`).

use gpui_kit::component::GlobalState;
use gpui_kit::*;

use crate::{OpenFolder, Quit, main_window, terminal_view, update, workspace};

pub fn install(cx: &mut App) {
    cx.set_menus(menus());
    if !cfg!(target_os = "macos") {
        let menus = cx.get_menus().unwrap_or_default();
        GlobalState::global_mut(cx).set_app_menus(menus);
    }
}

fn menus() -> Vec<Menu> {
    let mut file = vec![
        MenuItem::action("New Claude Tab", workspace::NewClaudeTab),
        MenuItem::action("New Shell Tab", workspace::NewShellTab),
        MenuItem::action("New Scratch File", workspace::NewScratch),
        MenuItem::separator(),
        MenuItem::action("New Window", main_window::NewWindow),
        MenuItem::action("Open Folder…", OpenFolder),
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
        MenuItem::action("Explorer", workspace::ToggleExplorer),
        MenuItem::action("Changes", workspace::ToggleChanges),
        MenuItem::action("Scratches", workspace::ToggleScratches),
        MenuItem::action("Focus Terminal", workspace::FocusTerminal),
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
        let app = Menu::new("Tern").items([
            MenuItem::action("Check for Updates…", update::CheckForUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Tern", Quit),
        ]);
        vec![app, Menu::new("File").items(file), edit, view]
    } else {
        file.extend([MenuItem::separator(), MenuItem::action("Quit", Quit)]);
        let file = Menu::new("File").items(file);
        let help = Menu::new("Help").items([MenuItem::action("Check for Updates…", update::CheckForUpdates)]);
        vec![file, edit, view, help]
    }
}
