//! What an agent asks the window through `tern --mcp` (`mcp.rs`), by way of
//! the daemon (`ClientMsg::AskUi`): a request and its answer are JSON
//! objects. `{"op": "status"}` reads the settings and themes again and says
//! which theme shows and what is wrong; `{"op": "set_theme", "name"}`
//! chooses a theme; `{"op": "shot", "folder"}` saves a PNG of the window
//! that shows `folder` (or a folder in it, or one it is in), as the window
//! drew it last.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use serde_json::{Value, json};

use crate::{AppServices, main_window, settings};

/// Answer request `ask`; a shot answers once its file is written.
pub fn answer(ask: u64, request: &str, cx: &mut App) {
    let request: Value = serde_json::from_str(request).unwrap_or(Value::Null);
    let str_arg = |k: &str| request.get(k).and_then(Value::as_str).map(str::to_owned);
    match request.get("op").and_then(Value::as_str) {
        Some("status") => reply(ask, status(cx), cx),
        Some("set_theme") => {
            let answer = match str_arg("name") {
                Some(name) => set_theme(&name, cx),
                None => failed("`name` is required"),
            };
            reply(ask, answer, cx);
        },
        Some("shot") => match str_arg("folder").ok_or_else(|| "`folder` is required".to_owned()).and_then(|f| shot(Path::new(&f), cx)) {
            Ok(task) => cx
                .spawn(async move |cx| {
                    let answer = task.await;
                    cx.update(|cx| reply(ask, answer, cx));
                })
                .detach(),
            Err(e) => reply(ask, failed(&e), cx),
        },
        _ => reply(ask, failed("unknown request"), cx),
    }
}

fn reply(ask: u64, answer: Value, cx: &App) {
    cx.global::<AppServices>().0.mux.answer_ui(ask, answer.to_string());
}

fn failed(message: &str) -> Value {
    json!({ "error": message })
}

/// The settings and themes as they apply now (read again first).
fn status(cx: &mut App) -> Value {
    settings::reload(cx);
    let (themes, current) = settings::themes(cx);
    json!({
        "theme": current.as_ref(),
        "themes": themes.iter().map(|t| json!({ "name": t.name.as_ref(), "dark": t.dark })).collect::<Vec<_>>(),
        "problems": settings::problems(cx).map(|p| p.to_string()),
        "settings": settings::settings_path(cx),
        "themes_dir": settings::themes_dir(cx),
    })
}

fn set_theme(name: &str, cx: &mut App) -> Value {
    settings::reload(cx);
    let (themes, _) = settings::themes(cx);
    if !themes.iter().any(|t| t.name.as_ref() == name) {
        let names: Vec<&str> = themes.iter().map(|t| t.name.as_ref()).collect();
        return failed(&format!("no theme \"{name}\" (there are: {})", names.join(", ")));
    }
    settings::choose_theme(name, cx);
    status(cx)
}

/// Draw the window showing `folder` into a picture and save it as a PNG in
/// the temp folder (on a background thread).
fn shot(folder: &Path, cx: &mut App) -> Result<Task<Value>, String> {
    let related = |root: &Path| root.starts_with(folder) || folder.starts_with(root);
    let Some((handle, ws)) = main_window::shown_workspaces(cx).into_iter().find(|(_, ws)| related(ws.read(cx).root())) else {
        return Err(format!("no Tern window shows {}", folder.display()));
    };
    let root = ws.read(cx).root().to_owned();
    let image = handle.update(cx, |_, window, _| window.render_to_image()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let path = shot_path();
    Ok(cx.background_spawn(async move {
        let (width, height) = image.dimensions();
        match image.save_with_format(&path, image::ImageFormat::Png) {
            Ok(()) => json!({ "path": path, "width": width, "height": height, "folder": root }),
            Err(e) => failed(&format!("can't save {}: {e}", path.display())),
        }
    }))
}

/// A new file in the temp folder.
fn shot_path() -> PathBuf {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    std::env::temp_dir().join(format!("tern-window-{ms}.png"))
}
