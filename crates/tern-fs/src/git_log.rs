//! Git history: the commits of a repository (`git log`), the lanes of its
//! graph, one commit's message and files, a file's diff in a commit. Read
//! with the same hardened `git` as the status ([`crate::git`]).

use std::path::{Path, PathBuf};

use crate::git::{GitStatus, git, rel_slash};

/// Field and record separators in the `log` format: bytes no name or
/// subject holds.
const FIELD: char = '\x1f';
const RECORD: char = '\x1e';

/// [`git`] for reading history: repo config can turn on `log.showSignature`
/// and name the program that checks signatures (`gpg.program`), which `log`
/// and `show` would then run on every signed commit. Never check them.
fn read(repo: &Path) -> std::process::Command {
    let mut c = git(repo);
    c.args(["-c", "log.showSignature=false"]);
    c
}

/// What a ref decorating a commit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// `HEAD`: the commit checked out (a detached one has no branch ref).
    Head,
    /// A local branch; `head` marks the one checked out.
    Branch {
        head: bool,
    },
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub kind: RefKind,
    /// Short name: `main`, `origin/main`, `v1.0`.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    /// Author time in the local time zone, `2026-10-20 14:03`.
    pub date: String,
    pub refs: Vec<Ref>,
    pub subject: String,
}

/// Which commits to list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogQuery {
    /// One branch (or any rev); `None` lists every ref's history.
    pub branch: Option<String>,
    /// Only commits whose author matches (`git log --author`).
    pub author: Option<String>,
    pub limit: usize,
}

/// The newest commits `query` asks for, children before parents.
pub fn log(repo: &Path, query: &LogQuery) -> Result<Vec<Commit>, String> {
    let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%ae{FIELD}%at{FIELD}%ad{FIELD}%D{FIELD}%s{RECORD}");
    let mut cmd = read(repo);
    cmd.args(["log", "--date-order", "--decorate=full", "--no-color", "--date=format-local:%Y-%m-%d %H:%M"])
        .arg(format)
        .arg(format!("-n{}", query.limit.max(1)));
    if let Some(a) = &query.author {
        cmd.arg(format!("--author={a}"));
    }
    match &query.branch {
        // `--end-of-options`: a rev never reads as an option.
        Some(b) => cmd.args(["--end-of-options", b]),
        None => cmd.args(["--branches", "--remotes", "--tags", "HEAD"]),
    };
    cmd.arg("--");
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // A repo without commits has no history to show.
        if err.contains("does not have any commits") || err.contains("unknown revision") && query.branch.is_none() {
            return Ok(vec![]);
        }
        return Err(err.lines().next().unwrap_or("git log failed").to_owned());
    }
    Ok(parse_log(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_log(raw: &str) -> Vec<Commit> {
    raw.split(RECORD)
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            let mut f = rec.split(FIELD);
            let hash = f.next().filter(|h| !h.is_empty())?.to_owned();
            let parents = f.next()?.split_whitespace().map(str::to_owned).collect();
            let author = f.next()?.to_owned();
            let email = f.next()?.to_owned();
            let time = f.next()?.parse().unwrap_or(0);
            let date = f.next()?.to_owned();
            let refs = parse_refs(f.next()?);
            let subject = f.next().unwrap_or_default().to_owned();
            Some(Commit { hash, parents, author, email, time, date, refs, subject })
        })
        .collect()
}

/// `%D` with `--decorate=full`: `HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1`.
/// The branch checked out comes first, then local branches, remotes, tags.
fn parse_refs(raw: &str) -> Vec<Ref> {
    let mut refs = vec![];
    for part in raw.split(", ").filter(|p| !p.is_empty()) {
        let (head, name) = match part.strip_prefix("HEAD -> ") {
            Some(rest) => (true, rest),
            None => (false, part),
        };
        let r = if name == "HEAD" {
            Ref { kind: RefKind::Head, name: "HEAD".into() }
        } else if let Some(t) = name.strip_prefix("tag: ") {
            Ref { kind: RefKind::Tag, name: t.strip_prefix("refs/tags/").unwrap_or(t).to_owned() }
        } else if let Some(b) = name.strip_prefix("refs/heads/") {
            Ref { kind: RefKind::Branch { head }, name: b.to_owned() }
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            // `origin/HEAD` only repeats the remote's default branch.
            if r.ends_with("/HEAD") {
                continue;
            }
            Ref { kind: RefKind::Remote, name: r.to_owned() }
        } else {
            continue;
        };
        refs.push(r);
    }
    let rank = |r: &Ref| match r.kind {
        RefKind::Branch { head: true } | RefKind::Head => 0,
        RefKind::Branch { head: false } => 1,
        RefKind::Remote => 2,
        RefKind::Tag => 3,
    };
    refs.sort_by_key(rank);
    refs
}

/// Branch names to filter the log by: local ones, then remote ones.
pub fn branches(repo: &Path) -> Vec<String> {
    let Ok(out) = git(repo).args(["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"]).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut local, mut remote) = (vec![], vec![]);
    for line in text.lines() {
        if let Some(b) = line.strip_prefix("refs/heads/") {
            local.push(b.to_owned());
        } else if let Some(r) = line.strip_prefix("refs/remotes/").filter(|r| !r.ends_with("/HEAD")) {
            remote.push(r.to_owned());
        }
    }
    local.into_iter().chain(remote).collect()
}

/// A file a commit changed, relative to the repo, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub status: GitStatus,
    /// A renamed file's old path.
    pub from: Option<String>,
}

/// One commit in full: its message and the files it changed (against its
/// first parent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetails {
    pub hash: String,
    pub author: String,
    pub email: String,
    pub date: String,
    pub committer: String,
    pub commit_date: String,
    pub message: String,
    pub files: Vec<CommitFile>,
}

pub fn details(repo: &Path, hash: &str) -> Result<CommitDetails, String> {
    let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%ae{FIELD}%ad{FIELD}%cn{FIELD}%cd{FIELD}%B");
    let out = read(repo)
        .args(["show", "-s", "--no-color", "--date=format-local:%Y-%m-%d %H:%M"])
        .arg(format)
        .args(["--end-of-options", hash, "--"])
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("git show failed").to_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let f: Vec<&str> = text.splitn(8, FIELD).collect();
    let [hash, parents, author, email, date, committer, commit_date, message] = f[..] else {
        return Err("unexpected git show output".into());
    };
    let parent = parents.split_whitespace().next();
    let mut cmd = git(repo);
    cmd.args(["diff-tree", "-r", "-M", "--no-commit-id", "--name-status", "-z", "--no-ext-diff", "--ignore-submodules=all"]);
    match parent {
        Some(p) => cmd.args(["--end-of-options", p, hash]),
        None => cmd.args(["--root", "--end-of-options", hash]),
    };
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    Ok(CommitDetails {
        hash: hash.to_owned(),
        author: author.to_owned(),
        email: email.to_owned(),
        date: date.to_owned(),
        committer: committer.to_owned(),
        commit_date: commit_date.to_owned(),
        message: message.trim_end().to_owned(),
        files: parse_name_status(&out.stdout),
    })
}

/// `diff-tree --name-status -z`: `M\0path\0`, `R100\0old\0new\0`.
fn parse_name_status(raw: &[u8]) -> Vec<CommitFile> {
    let mut files = vec![];
    let mut parts = raw.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned());
    while let Some(code) = parts.next() {
        let status = match code.as_bytes().first() {
            Some(b'A') => GitStatus::Added,
            Some(b'D') => GitStatus::Deleted,
            Some(b'R' | b'C') => GitStatus::Renamed,
            Some(b'U') => GitStatus::Conflicted,
            _ => GitStatus::Modified,
        };
        let Some(first) = parts.next() else { break };
        let (path, from) = if matches!(code.as_bytes().first(), Some(b'R' | b'C')) {
            match parts.next() {
                Some(to) => (to, Some(first)),
                None => break,
            }
        } else {
            (first, None)
        };
        files.push(CommitFile { path, status, from });
    }
    files
}

/// Unified diff of `path` (relative to `repo`) in commit `hash`, against its
/// first parent (against nothing for a root commit). External diff drivers
/// and textconv are off, as in [`crate::git::diff_vs_head`].
pub fn commit_diff(repo: &Path, hash: &str, path: &str, from: Option<&str>) -> Option<String> {
    let out = read(repo)
        .args(["--literal-pathspecs", "show", "--format=", "-M", "--no-ext-diff", "--no-textconv", "--no-color", "-U3"])
        .args(["--end-of-options", hash, "--"])
        .args(from)
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (!text.is_empty()).then_some(text)
}

/// Commit `paths` (absolute, inside `repo`) as they are on disk with
/// `message`: new files are added, deleted ones removed, and changes to
/// other files (staged or not) are left out. `amend` rewrites the last
/// commit instead; with no paths, only its message. The repo's hooks run,
/// as they do for `git commit`. Returns git's summary line.
pub fn commit(repo: &Path, paths: &[PathBuf], message: &str, amend: bool) -> Result<String, String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let rels: Vec<String> = paths.iter().filter_map(|p| rel_slash(repo, p)).collect();
    let fail = |out: &std::process::Output| {
        let text = String::from_utf8_lossy(&out.stderr).trim().to_owned() + "\n" + String::from_utf8_lossy(&out.stdout).trim();
        let text = text.trim();
        if text.is_empty() { "git commit failed".to_owned() } else { text.to_owned() }
    };
    if !rels.is_empty() {
        let out = git(repo).args(["--literal-pathspecs", "add", "-A", "--"]).args(&rels).output().map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(fail(&out));
        }
    }
    let mut cmd = git(repo);
    cmd.args(["--literal-pathspecs", "commit", "-F", "-"]);
    if amend {
        cmd.arg("--amend");
    }
    if rels.is_empty() {
        // Amending the message alone: leave staged changes out too.
        cmd.arg("--only");
    }
    cmd.arg("--").args(&rels);
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("git: {e}"))?;
    let mut stdin = child.stdin.take();
    let input = message.as_bytes().to_vec();
    // Feed stdin from another thread: hooks may write a lot before git reads it all.
    let out = std::thread::scope(|s| {
        s.spawn(move || stdin.as_mut().map(|w| w.write_all(&input)));
        child.wait_with_output()
    })
    .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(fail(&out));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_owned())
}

/// The last commit's message (to edit when amending).
pub fn last_message(repo: &Path) -> Option<String> {
    let out = read(repo).args(["log", "-1", "--format=%B", "--no-color"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

/// Where an edge of the graph starts or ends in a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Top,
    Middle,
    Bottom,
}

/// A line in one row of the graph, from one lane to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: (usize, At),
    pub to: (usize, At),
    /// Which color: lines of one branch share it.
    pub color: usize,
}

/// One commit's row of the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    /// The lane the commit's dot is in.
    pub lane: usize,
    pub color: usize,
    pub edges: Vec<Edge>,
    /// Lanes the row uses.
    pub width: usize,
}

/// The graph of `commits` (children before parents, as [`log`] lists them):
/// each commit sits in a lane, a line runs down from it to each parent, and
/// lines expecting the same parent meet at it. A parent not listed (past the
/// limit) leaves its line running off the bottom.
pub fn graph(commits: &[Commit]) -> Vec<GraphRow> {
    // Per lane: the commit it runs down to, and its color.
    let mut lanes: Vec<Option<(&str, usize)>> = vec![];
    let mut next_color = 0;
    let mut fresh = || {
        next_color += 1;
        next_color - 1
    };
    let free = |lanes: &mut Vec<Option<(&str, usize)>>| match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        },
    };
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let before = lanes.len();
        let lane = match lanes.iter().position(|l| l.is_some_and(|(h, _)| h == c.hash)) {
            Some(i) => i,
            None => {
                let i = free(&mut lanes);
                lanes[i] = Some((c.hash.as_str(), fresh()));
                i
            },
        };
        let color = lanes[lane].map_or(0, |(_, k)| k);
        let mut edges = vec![];
        for (i, slot) in lanes.iter_mut().enumerate().take(before) {
            let Some((h, k)) = *slot else { continue };
            if h == c.hash {
                edges.push(Edge { from: (i, At::Top), to: (lane, At::Middle), color: k });
                if i != lane {
                    *slot = None;
                }
            } else {
                edges.push(Edge { from: (i, At::Top), to: (i, At::Bottom), color: k });
            }
        }
        match c.parents.split_first() {
            None => lanes[lane] = None,
            Some((first, rest)) => {
                lanes[lane] = Some((first.as_str(), color));
                edges.push(Edge { from: (lane, At::Middle), to: (lane, At::Bottom), color });
                for p in rest {
                    let j = match lanes.iter().position(|l| l.is_some_and(|(h, _)| h == p.as_str())) {
                        Some(j) => j,
                        None => {
                            let j = free(&mut lanes);
                            lanes[j] = Some((p.as_str(), fresh()));
                            j
                        },
                    };
                    if j != lane {
                        let k = lanes[j].map_or(0, |(_, k)| k);
                        edges.push(Edge { from: (lane, At::Middle), to: (j, At::Bottom), color: k });
                    }
                }
            },
        }
        let width = before.max(lanes.len()).max(lane + 1);
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(GraphRow { lane, color, edges, width });
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn commit(hash: &str, parents: &[&str]) -> Commit {
        Commit {
            hash: hash.into(),
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            date: String::new(),
            refs: vec![],
            subject: String::new(),
        }
    }

    #[test]
    fn parses_refs_checked_out_branch_first() {
        let refs =
            parse_refs("HEAD -> refs/heads/main, tag: refs/tags/v1, refs/remotes/origin/main, refs/remotes/origin/HEAD, refs/heads/dev");
        let names: Vec<_> = refs.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (RefKind::Branch { head: true }, "main"),
                (RefKind::Branch { head: false }, "dev"),
                (RefKind::Remote, "origin/main"),
                (RefKind::Tag, "v1"),
            ]
        );
        assert_eq!(parse_refs("HEAD")[0].kind, RefKind::Head);
    }

    #[test]
    fn parses_name_status_with_renames() {
        let files = parse_name_status(b"M\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b c.txt\0");
        assert_eq!(
            files,
            [
                CommitFile { path: "src/a.rs".into(), status: GitStatus::Modified, from: None },
                CommitFile { path: "new.rs".into(), status: GitStatus::Renamed, from: Some("old.rs".into()) },
                CommitFile { path: "b c.txt".into(), status: GitStatus::Added, from: None },
            ]
        );
    }

    #[test]
    fn graph_of_a_branch_and_merge() {
        // m merges b into a; both come from r.
        let commits = [commit("m", &["a", "b"]), commit("b", &["r"]), commit("a", &["r"]), commit("r", &[])];
        let rows = graph(&commits);
        assert_eq!(rows.iter().map(|r| r.lane).collect::<Vec<_>>(), [0, 1, 0, 0]);
        // The merge sends a line down its own lane and one to lane 1.
        assert_eq!(
            rows[0].edges,
            [Edge { from: (0, At::Middle), to: (0, At::Bottom), color: 0 }, Edge { from: (0, At::Middle), to: (1, At::Bottom), color: 1 },]
        );
        // At r both lanes meet; lane 1 ends there.
        assert!(rows[3].edges.contains(&Edge { from: (1, At::Top), to: (0, At::Middle), color: 1 }));
        assert_eq!(rows[3].width, 2);
        assert!(rows[3].edges.iter().all(|e| e.to.1 != At::Bottom));
    }

    #[test]
    fn graph_reuses_freed_lanes() {
        let commits = [commit("c", &["b"]), commit("x", &["y"]), commit("y", &[]), commit("b", &[]), commit("z", &[])];
        let rows = graph(&commits);
        assert_eq!(rows.iter().map(|r| r.lane).collect::<Vec<_>>(), [0, 1, 1, 0, 0]);
    }

    /// A repo whose config asks every `log` to check signatures with a
    /// program of its own: listing its history must not run it.
    #[cfg(unix)]
    #[test]
    fn never_runs_the_repos_signature_program() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(r).output().unwrap().status.success(), "{args:?}");
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@x"]);
        // Signs anything (so the commit is signed) and, asked to verify, leaves a mark.
        let program = r.join("gpg.sh");
        let mark = r.join("pwned");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncase \"$*\" in *--verify*) touch '{}';; esac\n\
                 echo '-----BEGIN PGP SIGNATURE-----'; echo x; echo '-----END PGP SIGNATURE-----'\n\
                 echo '[GNUPG:] SIG_CREATED ' >&2\n",
                mark.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        run(&["config", "gpg.program", program.to_str().unwrap()]);
        std::fs::write(r.join("a.txt"), "1\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-S", "-m", "signed"]);
        run(&["config", "log.showSignature", "true"]);
        // The trap works: plain `git log` runs the program.
        run(&["log", "-1"]);
        assert!(mark.exists(), "the repo's program should run for a plain git log");
        std::fs::remove_file(&mark).unwrap();
        let log = log(r, &LogQuery { limit: 10, ..Default::default() }).unwrap();
        details(r, &log[0].hash).unwrap();
        commit_diff(r, &log[0].hash, "a.txt", None).unwrap();
        last_message(r).unwrap();
        assert!(!mark.exists(), "reading the log ran the repo's gpg.program");
    }

    #[test]
    fn logs_commits_and_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(r).output().unwrap().status.success(), "{args:?}");
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "user.email", "t@x"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(r.join("a.txt"), "1\n").unwrap();
        std::fs::write(r.join("b.txt"), "1\n").unwrap();
        // Only the paths named are committed, new files included.
        let line = super::commit(r, &[r.join("a.txt")], "first\n\nbody", false).unwrap();
        assert!(line.contains("first"), "{line}");
        std::fs::write(r.join("a.txt"), "2\n").unwrap();
        std::fs::remove_file(r.join("b.txt")).unwrap();
        std::fs::write(r.join("c.txt"), "1\n").unwrap();
        super::commit(r, &[r.join("a.txt"), r.join("c.txt")], "second", false).unwrap();
        let log = log(r, &LogQuery { limit: 10, ..Default::default() }).unwrap();
        assert_eq!(log.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["second", "first"]);
        assert_eq!(log[0].refs[0], Ref { kind: RefKind::Branch { head: true }, name: "main".into() });
        assert_eq!(log[0].parents, [log[1].hash.clone()]);
        let d = details(r, &log[0].hash).unwrap();
        let files: Vec<_> = d.files.iter().map(|f| (f.path.as_str(), f.status)).collect();
        assert_eq!(files, [("a.txt", GitStatus::Modified), ("c.txt", GitStatus::Added)]);
        assert_eq!(details(r, &log[1].hash).unwrap().message, "first\n\nbody");
        let diff = commit_diff(r, &log[0].hash, "a.txt", None).unwrap();
        assert!(diff.contains("-1\n+2"), "{diff}");
        // b.txt's removal was never committed.
        assert!(!d.files.iter().any(|f| f.path == "b.txt"));
        // Amending with no paths changes the message alone.
        super::commit(r, &[], "second, reworded", true).unwrap();
        assert_eq!(last_message(r).unwrap(), "second, reworded");
        assert_eq!(branches(r), ["main"]);
    }
}
