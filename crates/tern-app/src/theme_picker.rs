//! The theme picker (⌘K ⌘T): the themes there are, each shown on the
//! window as the selection moves over it. Enter saves it in
//! `settings.json`; Escape goes back to the theme from before.

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{IconName, IndexPath, WindowExt as _};
use gpui_kit::*;

use crate::platform::bind;
use crate::settings;

actions!(tern, [SelectTheme, OpenSettings]);

pub fn bind_keys(cx: &mut App) {
    // After the terminal's ⌘K (Clear), which then waits for a second key
    // as in VS Code.
    cx.bind_keys([bind("cmd-k cmd-t", SelectTheme, None), bind("cmd-,", OpenSettings, None)]);
}

pub fn open(window: &mut Window, cx: &mut App) {
    let (themes, current) = settings::themes(cx);
    let state = cx.new(|cx| CommandState::new(window, cx));
    let start = themes.iter().position(|t| t.name == current);
    window.open_dialog(cx, {
        let state = state.clone();
        move |dialog, _, cx| {
            let (state, themes, current) = (state.clone(), themes.clone(), current.clone());
            dialog.w(crate::style::dialog_width(false, cx)).title("Theme").on_close(|_, _, cx| settings::preview_theme(None, cx)).content(
                move |c, _, _| {
                    let items = themes.iter().map(|t| {
                        CommandItem::new()
                            .label(t.name.clone())
                            .icon(if t.dark { IconName::Moon } else { IconName::Sun })
                            .checked(t.name == current)
                    });
                    let (on_select, on_confirm) = (themes.clone(), themes.clone());
                    c.child(
                        Command::new(&state)
                            .bordered(false)
                            .placeholder("Search themes")
                            .items(items)
                            .on_select(move |ix, _, cx| {
                                if let Some(t) = on_select.get(ix.row) {
                                    settings::preview_theme(Some(t.name.clone()), cx);
                                }
                            })
                            .on_confirm(move |ix, window, cx| {
                                if let Some(t) = on_confirm.get(ix.row) {
                                    settings::choose_theme(&t.name, cx);
                                }
                                window.close_dialog(cx);
                            }),
                    )
                },
            )
        }
    });
    state.update(cx, |s, cx| s.focus(window, cx));
    select_when_drawn(state, start.map(IndexPath::new), 30, window);
}

/// Highlights `ix` once the palette has drawn its list (the dialog may take
/// a few frames to show it), giving up after `frames`.
fn select_when_drawn(state: Entity<CommandState>, ix: Option<IndexPath>, frames: u32, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        if state.read(cx).matched_count() == 0 {
            if frames > 0 {
                select_when_drawn(state, ix, frames - 1, window);
            }
            return;
        }
        state.update(cx, |s, cx| s.set_selected_index(ix, window, cx));
    });
}
