//! What an agent asks the window through `tern --theme` and `tern --shot`
//! (`agent_cli.rs`), by way of the daemon (`ClientMsg::AskUi`): a request and
//! its answer are JSON objects. `{"op": "status"}` reads the settings and
//! themes again and says
//! which theme shows and what is wrong; `{"op": "set_theme", "name"}`
//! chooses a theme; `{"op": "shot", "folder"}` saves a PNG of the window
//! that shows `folder` (or a folder in it, or one it is in), as the window
//! drew it last. `task_*` requests (`tern --task`) go to `agent_tasks`.
//! `{"op": "worktree_open", "path", "by"}` shows a worktree `tern
//! --worktree` made as a folder, under its repository's;
//! `{"op": "worktree_close", "path", "force"}` closes it before it is
//! removed; `{"op": "worktrees_changed"}` reads every repository's
//! worktrees again once one is gone.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use serde_json::{Value, json};

use crate::{AppServices, main_window, settings};

/// Whether worktree `path` may be closed for `request`: a linked worktree
/// (or a folder already gone) of the asking agent tab's repository; any,
/// asked from outside an agent tab.
fn closable(path: &Path, request: &Value, cx: &mut App) -> bool {
    let repo = match request.get("by").and_then(Value::as_u64) {
        Some(by) => match crate::agent_tasks::asker_repo(by, cx) {
            Some(repo) => Some(repo),
            None => return false,
        },
        None => None,
    };
    let main = if path.exists() {
        match tern_fs::worktree::linked(path) {
            Some((main, _)) => dunce::canonicalize(&main).unwrap_or(main),
            None => return false,
        }
    } else {
        // Gone: only a folder in its repository's worktrees folder.
        match path.parent().filter(|p| p.file_name().is_some_and(|n| n == ".worktrees")).and_then(Path::parent) {
            Some(main) => dunce::canonicalize(main).unwrap_or_else(|_| main.to_owned()),
            None => return false,
        }
    };
    repo.is_none_or(|repo| repo == main)
}

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
        Some("worktree_open") => {
            let answer = match str_arg("path") {
                Some(path) => {
                    let by = request.get("by").and_then(Value::as_u64);
                    main_window::open_worktree(PathBuf::from(path), by, cx);
                    json!({ "opened": true })
                },
                None => failed("`path` is required"),
            };
            reply(ask, answer, cx);
        },
        Some("worktrees_changed") => {
            main_window::refresh_worktrees(cx);
            reply(ask, json!({ "refreshed": true }), cx);
        },
        Some("worktree_close") => {
            let force = request.get("force").and_then(Value::as_bool).unwrap_or(false);
            let answer = match str_arg("path") {
                // Only a linked worktree (or one already gone, whose
                // programs are left running): never a repository's own
                // checkout or any other folder; and, asked from an agent
                // tab, only one of that tab's own repository.
                Some(path) if !closable(Path::new(&path), &request, cx) => {
                    failed(&format!("{path} is not a git worktree of your repository"))
                },
                Some(path) => match main_window::close_folder(Path::new(&path), force && Path::new(&path).exists(), cx) {
                    Ok(closed) => json!({ "closed": closed }),
                    Err(e) => failed(&e),
                },
                None => failed("`path` is required"),
            };
            reply(ask, answer, cx);
        },
        Some(op) if op.starts_with("task_") => {
            let answer = crate::agent_tasks::answer(op, &request, cx);
            reply(ask, answer, cx);
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
