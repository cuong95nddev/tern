//! What an agent asks the window through `tern --mcp` (`mcp.rs`), by way of
//! the daemon (`ClientMsg::AskUi`): a request and its answer are JSON
//! objects. `{"op": "status"}` reads the settings and themes again and says
//! which theme shows and what is wrong; `{"op": "set_theme", "name"}`
//! chooses a theme; `{"op": "shot", "folder"}` saves a PNG of the window
//! that shows `folder` (or a folder in it, or one it is in), as the window
//! drew it last.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// How long a shot's file is kept: the agent reads it right away.
const SHOT_KEPT: Duration = Duration::from_secs(60 * 60);

/// Draw the window showing `folder` into a picture and save it as a PNG in
/// the daemon's private folder (on a background thread).
fn shot(folder: &Path, cx: &mut App) -> Result<Task<Value>, String> {
    let related = |root: &Path| root.starts_with(folder) || folder.starts_with(root);
    let Some((handle, ws)) = main_window::shown_workspaces(cx).into_iter().find(|(_, ws)| related(ws.read(cx).root())) else {
        return Err(format!("no Tern window shows {}", folder.display()));
    };
    let root = ws.read(cx).root().to_owned();
    let image = handle.update(cx, |_, window, _| window.render_to_image()).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    Ok(cx.background_spawn(async move {
        let (width, height) = image.dimensions();
        let saved = shot_file().and_then(|(path, file)| {
            let mut out = std::io::BufWriter::new(file);
            image.write_to(&mut out, image::ImageFormat::Png).map_err(std::io::Error::other)?;
            Ok(path)
        });
        match saved {
            Ok(path) => json!({ "path": path, "width": width, "height": height, "folder": root }),
            Err(e) => failed(&format!("can't save the picture: {e}")),
        }
    }))
}

/// A new file for a shot, only the user's: in `shots/` of the daemon's
/// private folder (not a shared temp folder, where another user could read
/// it or put a link at its name), made anew and readable by the user alone.
/// Shots older than `SHOT_KEPT` go.
fn shot_file() -> std::io::Result<(PathBuf, File)> {
    let dir = tern_mux::paths::mux_dir().join("shots");
    tern_mux::paths::ensure_private_dir(&dir)?;
    let now = SystemTime::now();
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > SHOT_KEPT));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let ms = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // Two shots in the same millisecond get their own files.
    for n in 0.. {
        let path = dir.join(if n == 0 { format!("window-{ms}.png") } else { format!("window-{ms}-{n}.png") });
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => {},
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns")
}
