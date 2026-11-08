//! Mailbox export to `.mbox` (mboxrd): one new file per folder, from the local cache plus bodies fetched through the
//! normal fetch path (`commands::raw_message`, which also caches them). It reads mail and writes NEW files only:
//! never overwrites (create-new with a numbered name), never deletes anything (a cancelled export leaves a valid,
//! shorter mbox that the user can delete), never changes the mailbox. Only messages MiraMail has synced are exported.
//!
//! Format (mboxrd): every message starts with a `From <sender> <asctime>` line; inside the message every line that is
//! `>*From ` gets one more `>` so a reader can undo it exactly. Messages keep their own line endings (CRLF from the
//! server); the separator lines use the message's style, and a blank line ends each message.

use crate::state::{err, AppState};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// Reconnect (releasing the account lock for a moment) after this many fetched bodies, so a long export does not
/// starve sync.
const FETCH_BATCH: u32 = 40;

// ----------------------------------------------------------------------------------------------- pure mbox

/// `>*From ` at the start of a line.
fn is_from_line(line: &[u8]) -> bool {
    let n = line.iter().take_while(|b| **b == b'>').count();
    line[n..].starts_with(b"From ")
}

/// mboxrd escaping: one extra `>` in front of every line that is `>*From ` (lines split on `\n`; a trailing `\r` is
/// part of the line and stays).
pub fn escape_mboxrd(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 16);
    for line in raw.split_inclusive(|b| *b == b'\n') {
        // No allocation and no length limit: the whole `>` run before `From ` is scanned.
        if is_from_line(line) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    out
}

/// The address part of a stored From header ("Name <a@b>" or "a@b"), made safe for the envelope line.
pub fn envelope_sender(from: &str) -> String {
    let addr = match (from.rfind('<'), from.rfind('>')) {
        (Some(a), Some(b)) if a < b => &from[a + 1..b],
        _ => from.trim(),
    };
    let clean: String = addr.chars().filter(|c| !c.is_whitespace() && !c.is_control() && *c != '<' && *c != '>').collect();
    if clean.is_empty() || !clean.contains('@') { "MAILER-DAEMON".to_string() } else { clean }
}

/// asctime in UTC: `Tue Nov  5 12:00:00 2026`.
pub fn asctime(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs.max(0), 0).unwrap_or_default().format("%a %b %e %H:%M:%S %Y").to_string()
}

/// Append one message: envelope line, escaped message, and the blank line that ends it.
pub fn write_message<W: Write>(w: &mut W, from_header: &str, date: i64, raw: &[u8]) -> std::io::Result<()> {
    let crlf = raw.windows(2).any(|p| p == b"\r\n");
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    w.write_all(format!("From {} {}", envelope_sender(from_header), asctime(date)).as_bytes())?;
    w.write_all(eol)?;
    let body = escape_mboxrd(raw);
    w.write_all(&body)?;
    if !body.ends_with(b"\n") {
        w.write_all(eol)?;
    }
    w.write_all(eol)
}

/// Create a NEW file `dir/name` (`name-1.ext`, `name-2.ext`, ... when taken). Never opens an existing file for writing.
pub fn create_unique(dir: &Path, name: &str) -> std::io::Result<(std::fs::File, PathBuf)> {
    let safe = crate::files::sanitize_filename(name);
    let (stem, ext) = match safe.rfind('.') {
        Some(i) if i > 0 => (safe[..i].to_string(), safe[i..].to_string()),
        _ => (safe.clone(), String::new()),
    };
    for n in 0..10_000u32 {
        let cand = if n == 0 { safe.clone() } else { format!("{stem}-{n}{ext}") };
        let path = dir.join(&cand);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many files with that name"))
}

/// Create a NEW directory (numbered when taken).
fn create_unique_dir(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let safe = crate::files::sanitize_filename(name);
    for n in 0..10_000u32 {
        let cand = if n == 0 { safe.clone() } else { format!("{safe}-{n}") };
        let path = parent.join(cand);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many folders with that name"))
}

fn folder_file_name(folder: &str) -> String {
    let f: String = folder.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '.') || c.is_control() { '_' } else { c }).collect();
    format!("{}.mbox", if f.trim().is_empty() { "folder".to_string() } else { f })
}

// ----------------------------------------------------------------------------------------------- runtime

#[derive(Default)]
pub struct MboxState {
    running: Mutex<bool>,
    cancel: AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `running` | `done` | `cancelled` | `error`
    pub state: &'static str,
    pub done: u32,
    pub total: u32,
    /// Messages whose body could not be fetched (skipped).
    pub skipped: u32,
    /// Folder or directory the files were written to.
    pub path: Option<String>,
    pub error: Option<String>,
}

fn emit(app: &AppHandle, p: Progress) {
    let _ = app.emit("export://progress", p);
}

/// Clears the `running` flag when dropped, so every exit path (early return, error, panic) frees the slot.
struct RunGuard(AppHandle);

impl Drop for RunGuard {
    fn drop(&mut self) {
        *self.0.state::<MboxState>().running.lock().unwrap_or_else(|e| e.into_inner()) = false;
    }
}

struct Plan {
    folder: String,
    items: Vec<(u32, i64)>,
}

/// Start exporting `folders` (None = every selectable folder) of an account. Returns at once; progress arrives as
/// `export://progress`. One export at a time.
#[tauri::command]
pub async fn mbox_export(app: AppHandle, state: State<'_, AppState>, mbox: State<'_, MboxState>, account: String, folders: Option<Vec<String>>) -> Result<(), String> {
    {
        let mut r = mbox.running.lock().unwrap_or_else(|e| e.into_inner());
        if *r {
            return Err("An export is already running.".into());
        }
        *r = true;
    }
    let guard = RunGuard(app.clone());
    mbox.cancel.store(false, Ordering::SeqCst);
    let plan = (|| -> Result<Vec<Plan>, String> {
        let known = state.storage.list_folders(&account).map_err(err)?;
        let names: Vec<String> = match folders {
            Some(f) => {
                for n in &f {
                    if !known.iter().any(|k| &k.name == n && k.selectable) {
                        return Err("That folder no longer exists.".into());
                    }
                }
                f
            }
            None => known.iter().filter(|f| f.selectable).map(|f| f.name.clone()).collect(),
        };
        let mut out = Vec::new();
        for n in names {
            let items = state.storage.uids_with_dates(&account, &n).map_err(err)?;
            if !items.is_empty() {
                out.push(Plan { folder: n, items });
            }
        }
        Ok(out)
    })();
    let plan = match plan {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            return Err("There are no synced messages to export in that selection.".into());
        }
        Err(e) => return Err(e),
    };
    let dir = crate::files::downloads_dir(&app)?;
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let out = run(&app, &account, &dir, plan).await;
        drop(_guard);
        emit(&app, out);
    });
    Ok(())
}

#[tauri::command]
pub fn mbox_cancel(mbox: State<'_, MboxState>) {
    mbox.cancel.store(true, Ordering::SeqCst);
}

async fn run(app: &AppHandle, account: &str, dir: &Path, plan: Vec<Plan>) -> Progress {
    let state = app.state::<AppState>();
    let total: u32 = plan.iter().map(|p| p.items.len() as u32).sum();
    let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
    let single = plan.len() == 1;
    let fail = |done: u32, skipped: u32, path: Option<String>, e: String| Progress { state: "error", done, total, skipped, path, error: Some(e) };
    // One folder: a file in Downloads. Several: a new directory with one file per folder.
    let root = if single {
        dir.to_path_buf()
    } else {
        match create_unique_dir(dir, &format!("MiraMail-{account}-{stamp}")) {
            Ok(d) => d,
            Err(e) => return fail(0, 0, None, format!("Couldn't create the export folder: {e}")),
        }
    };
    let (mut done, mut skipped, mut fetched) = (0u32, 0u32, 0u32);
    let mut conn: Option<crate::state::Conn> = None;
    let mut last_path = root.to_string_lossy().into_owned();
    for p in &plan {
        let fname = if single { format!("MiraMail-{account}-{}-{stamp}.mbox", p.folder.replace(['/', '.'], "_")) } else { folder_file_name(&p.folder) };
        let (file, path) = match create_unique(&root, &fname) {
            Ok(x) => x,
            Err(e) => return fail(done, skipped, Some(last_path), format!("Couldn't create the file: {e}")),
        };
        last_path = if single { path.to_string_lossy().into_owned() } else { root.to_string_lossy().into_owned() };
        let mut w = BufWriter::new(file);
        for (uid, date) in &p.items {
            if app.state::<MboxState>().cancel.load(Ordering::SeqCst) {
                let _ = w.flush();
                if let Some(c) = conn.take() {
                    c.close().await;
                }
                return Progress { state: "cancelled", done, total, skipped, path: Some(last_path), error: None };
            }
            let had_body = state.storage.has_cached_body(account, &p.folder, *uid).unwrap_or(false);
            if !had_body {
                fetched += 1;
                if fetched % FETCH_BATCH == 0 {
                    if let Some(c) = conn.take() {
                        c.close().await;
                    }
                }
            }
            match crate::commands::raw_message(&state, &mut conn, account, &p.folder, *uid).await {
                Ok(raw) => {
                    let from = state.storage.get_message(account, &p.folder, *uid).ok().flatten().map(|m| m.from).unwrap_or_default();
                    if let Err(e) = write_message(&mut w, &from, *date, &raw) {
                        let _ = w.flush();
                        return fail(done, skipped, Some(last_path), format!("Couldn't write the file: {e}"));
                    }
                }
                Err(_) => skipped += 1,
            }
            done += 1;
            if done % 5 == 0 || done == total {
                emit(app, Progress { state: "running", done, total, skipped, path: None, error: None });
            }
        }
        if let Err(e) = w.flush() {
            return fail(done, skipped, Some(last_path), format!("Couldn't write the file: {e}"));
        }
    }
    if let Some(c) = conn.take() {
        c.close().await;
    }
    Progress { state: "done", done, total, skipped, path: Some(last_path), error: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_from_lines_mboxrd_style() {
        let raw = b"Subject: x\r\n\r\nFrom the start\r\n>From quoted\r\n>>From twice\r\nFromage is not\r\n>From: header-like\r\n From indented\r\nend";
        let out = String::from_utf8(escape_mboxrd(raw)).unwrap();
        assert_eq!(
            out,
            "Subject: x\r\n\r\n>From the start\r\n>>From quoted\r\n>>>From twice\r\nFromage is not\r\n>From: header-like\r\n From indented\r\nend"
        );
        // The first line of the message too.
        assert_eq!(escape_mboxrd(b"From me\nbody"), b">From me\nbody");
        // Reversible: strip one '>' from every `>+From ` line.
        let back: Vec<u8> = escape_mboxrd(raw)
            .split_inclusive(|b| *b == b'\n')
            .flat_map(|l| if l.starts_with(b">") && is_from_line(l) { l[1..].to_vec() } else { l.to_vec() })
            .collect();
        assert_eq!(back, raw);
    }

    #[test]
    fn long_gt_runs_before_from_are_escaped() {
        let mut line = vec![b'>'; 1000];
        line.extend_from_slice(b"From x\n");
        let out = escape_mboxrd(&line);
        assert_eq!(out.len(), line.len() + 1);
        assert!(out.starts_with(b">>>") && out[1001..].starts_with(b"From x"));
        let mut not = vec![b'>'; 1000];
        not.extend_from_slice(b"Fromage\n");
        assert_eq!(escape_mboxrd(&not), not);
    }

    #[test]
    fn envelope_line_and_crlf_framing() {
        let raw = b"From: Bob <bob@x.org>\r\nSubject: hi\r\n\r\nFrom here\r\nbye\r\n";
        let mut out = Vec::new();
        write_message(&mut out, "Bob Smith <bob@x.org>", 1_700_000_000, raw).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("From bob@x.org Tue Nov 14 22:13:20 2023\r\n"), "{s:?}");
        assert!(s.contains("\r\n>From here\r\nbye\r\n\r\n"), "{s:?}");
        assert!(s.ends_with("bye\r\n\r\n"), "blank line ends the message");
        assert!(!s.replace("\r\n", "").contains('\n'), "no bare LF in a CRLF message");
        // A message without a final newline is terminated; an LF message keeps LF.
        let mut out = Vec::new();
        write_message(&mut out, "nobody", 0, b"Subject: a\n\nlast line").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "From MAILER-DAEMON Thu Jan  1 00:00:00 1970\nSubject: a\n\nlast line\n\n");
    }

    #[test]
    fn two_messages_parse_back_apart() {
        let mut out = Vec::new();
        write_message(&mut out, "a@x.org", 0, b"Subject: 1\r\n\r\nFrom nobody\r\n").unwrap();
        write_message(&mut out, "b@x.org", 0, b"Subject: 2\r\n\r\nhi\r\n").unwrap();
        let s = String::from_utf8(out).unwrap();
        let starts: Vec<&str> = s.split("\r\n").filter(|l| l.starts_with("From ")).collect();
        assert_eq!(starts.len(), 2, "only the two envelope lines start with 'From ': {s:?}");
    }

    #[test]
    fn envelope_sender_is_sanitised() {
        assert_eq!(envelope_sender("Bob <bob@x.org>"), "bob@x.org");
        assert_eq!(envelope_sender("  c@x.org "), "c@x.org");
        assert_eq!(envelope_sender("<a b@x.org>"), "ab@x.org");
        assert_eq!(envelope_sender("no address here"), "MAILER-DAEMON");
        assert_eq!(envelope_sender(""), "MAILER-DAEMON");
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let (mut f, p1) = create_unique(dir.path(), "a.mbox").unwrap();
        f.write_all(b"first").unwrap();
        let (_f2, p2) = create_unique(dir.path(), "a.mbox").unwrap();
        assert_ne!(p1, p2);
        assert!(p2.file_name().unwrap().to_string_lossy().starts_with("a-1"));
        assert_eq!(std::fs::read(&p1).unwrap(), b"first", "the first file is untouched");
        let d1 = create_unique_dir(dir.path(), "exp").unwrap();
        let d2 = create_unique_dir(dir.path(), "exp").unwrap();
        assert_ne!(d1, d2);
        assert_eq!(folder_file_name("INBOX/Sub.Folder"), "INBOX_Sub_Folder.mbox");
    }
}
