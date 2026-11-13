//! iPhone: hand an attachment, the raw message (.eml) or a printout to the system sheets (Share / Save to Files,
//! Quick Look, Print). The bytes are written to a per-share folder inside the app's own Caches (`<Library/Caches>/share/<unique>/`),
//! the Swift side (plugins/mailcheck, ShareSheet.swift) presents the sheet and deletes the folder when the sheet is
//! dismissed; anything left over (app killed mid-sheet) is swept at the next launch. Nothing is ever written outside
//! the app container. File names, subjects and bodies are never logged.
//!
//! These are user actions: they are deliberately NOT in `presence::ALLOWED_WHILE_AWAY`, so the iPhone invoke guard
//! refuses them while the user is away. Off iOS the commands only return an error (the Mac has its own paths).

#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use crate::state::{err, AppState};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, State};

type CmdResult<T> = Result<T, String>;

/// Folder under the app cache that holds the per-share folders.
pub const SHARE_DIR: &str = "share";
/// Longest file name handed to the system (characters, extension kept).
const MAX_NAME_CHARS: usize = 100;
/// Largest printout accepted from the page.
const MAX_PRINT_BYTES: usize = 8 * 1024 * 1024;
/// A share folder older than this is leftover (the sheet is long gone) and is swept before the next share.
/// Largest attachment / message handed to the share or Quick Look sheet.
const MAX_SHARE_BYTES: usize = 100 * 1024 * 1024;
const STALE: Duration = Duration::from_secs(3600);

#[cfg_attr(target_os = "ios", allow(dead_code))]
const NOT_PHONE: &str = "This is only available on iPhone.";

/// One safe file name: last path component only, no control / bidi / zero-width characters, no leading dots, at most
/// 100 characters with the extension kept, never empty (`fallback`).
pub fn share_file_name(name: &str, fallback: &str) -> String {
    let base = crate::files::sanitize_filename(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| if matches!(c, '/' | '\\' | ':' | '\0') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    let blank = name.rsplit(['/', '\\']).next().unwrap_or("").trim().trim_start_matches('.').trim().is_empty();
    if cleaned.is_empty() || blank {
        return fallback.to_string();
    }
    if cleaned.chars().count() <= MAX_NAME_CHARS {
        return cleaned;
    }
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if i > 0 => (&cleaned[..i], &cleaned[i..]),
        _ => (cleaned.as_str(), ""),
    };
    let ext: String = ext.chars().take(16).collect();
    let keep = MAX_NAME_CHARS.saturating_sub(ext.chars().count());
    let stem: String = stem.chars().take(keep).collect();
    let out = format!("{}{ext}", stem.trim_end());
    if out.trim().is_empty() { fallback.to_string() } else { out }
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
            | '\u{FEFF}'
            | '\u{00AD}'
    )
}

/// True when `path` is `root` plus plain components only (no `..`, no root, no prefix). Lexical: the caller made both.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(rest) => rest.components().next().is_some() && rest.components().all(|c| matches!(c, Component::Normal(_))),
        Err(_) => false,
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh folder name that cannot collide with another share (time, process, counter).
fn unique_dir_name() -> String {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}-{:x}-{}", nanos, std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// Write `bytes` to `<root>/<unique>/<name>` (name already safe) and return the path. Refuses a path that would leave `root`.
pub fn stage(root: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let dir = root.join(unique_dir_name());
    let path = dir.join(name);
    if !is_inside(root, &path) || name.contains(['/', '\\']) || name == ".." || name == "." {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsafe file name"));
    }
    std::fs::create_dir_all(&dir)?;
    let r = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut f| f.write_all(bytes).and_then(|_| f.flush()));
    if let Err(e) = r {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(path)
}

/// Remove the share folder that holds `file` (its parent), only if it is a direct child of `root`.
pub fn remove_staged(root: &Path, file: &Path) {
    if let Some(dir) = file.parent() {
        if dir.parent() == Some(root) && is_inside(root, dir) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Delete share folders under `root` older than `older_than` (`Duration::ZERO` = all). Only direct children of `root`
/// are touched, and symlinks are removed as links, never followed. Returns how many were removed.
pub fn sweep(root: &Path, older_than: Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(root) else { return 0 };
    let now = SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).unwrap_or(Duration::MAX);
        if age < older_than {
            continue;
        }
        let ok = if ft.is_symlink() || ft.is_file() { std::fs::remove_file(&p).is_ok() } else { std::fs::remove_dir_all(&p).is_ok() };
        n += ok as usize;
    }
    n
}

#[cfg(target_os = "ios")]
fn share_root(app: &AppHandle) -> CmdResult<PathBuf> {
    use tauri::Manager;
    Ok(app.path().cache_dir().map_err(|_| "Couldn't find the app's cache folder.".to_string())?.join(SHARE_DIR))
}

/// Launch: remove every leftover share folder (the Swift side deletes them on dismissal; this covers a kill mid-sheet).
pub fn sweep_on_launch(app: &AppHandle) {
    #[cfg(target_os = "ios")]
    if let Ok(root) = share_root(app) {
        sweep(&root, Duration::ZERO);
    }
    #[cfg(not(target_os = "ios"))]
    let _ = app;
}

#[derive(Clone, Copy)]
enum Sheet {
    Share,
    Preview,
}

#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
async fn hand_off(app: &AppHandle, name: &str, fallback: &str, bytes: Vec<u8>, sheet: Sheet) -> CmdResult<()> {
    #[cfg(target_os = "ios")]
    {
        let root = share_root(app)?;
        let r2 = root.clone();
        tauri::async_runtime::spawn_blocking(move || {
            std::fs::create_dir_all(&r2)?;
            sweep(&r2, STALE);
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(err)?
        .map_err(|_| "Couldn't prepare the file.".to_string())?;
        if bytes.len() > MAX_SHARE_BYTES {
            return Err("This file is larger than 100 MB, which is too big to share from the phone.".into());
        }
        let file_name = share_file_name(name, fallback);
        let root2 = root.clone();
        let path = tauri::async_runtime::spawn_blocking(move || stage(&root2, &file_name, &bytes))
            .await
            .map_err(err)?
            .map_err(|_| "Couldn't prepare the file.".to_string())?;
        let (a, p) = (app.clone(), path.clone());
        let r = tauri::async_runtime::spawn_blocking(move || match sheet {
            Sheet::Share => tauri_plugin_mailcheck::share_file(&a, &p),
            Sheet::Preview => tauri_plugin_mailcheck::preview_file(&a, &p),
        })
        .await
        .map_err(err)
        .and_then(|r| r);
        if r.is_err() {
            remove_staged(&root, &path); // the sheet never opened, so nobody else will delete it
        }
        r
    }
    #[cfg(not(target_os = "ios"))]
    {
        Err(NOT_PHONE.into())
    }
}

/// "Save" on iPhone: the system share sheet (Save to Files, AirDrop, Mail, ...) for one attachment.
#[tauri::command]
pub async fn share_attachment(app: AppHandle, state: State<'_, AppState>, account: String, folder: String, uid: u32, id: String) -> CmdResult<()> {
    if !cfg!(target_os = "ios") {
        return Err(NOT_PHONE.into());
    }
    let (name, _mime, bytes) = crate::commands::load_attachment(&state, &account, &folder, uid, &id, None).await?;
    hand_off(&app, &name, "attachment", bytes, Sheet::Share).await
}

/// "Open" on iPhone: Quick Look inside the app. Same type allow-list as the Mac's Open.
#[tauri::command]
pub async fn preview_attachment(app: AppHandle, state: State<'_, AppState>, account: String, folder: String, uid: u32, id: String) -> CmdResult<()> {
    if !cfg!(target_os = "ios") {
        return Err(NOT_PHONE.into());
    }
    let (name, _mime, bytes) = crate::commands::load_attachment(&state, &account, &folder, uid, &id, None).await?;
    if !crate::organise::openable(&crate::files::sanitize_filename(&name)) {
        return Err("For safety MiraMail only previews documents, pictures and similar files. Use Save for this one.".into());
    }
    hand_off(&app, &name, "attachment", bytes, Sheet::Preview).await
}

/// "Share as .eml" on iPhone: the raw message through the share sheet.
#[tauri::command]
pub async fn share_eml(app: AppHandle, state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<()> {
    if !cfg!(target_os = "ios") {
        return Err(NOT_PHONE.into());
    }
    let mut conn = None;
    let raw = crate::commands::raw_message(&state, &mut conn, &account, &folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    let raw = raw?;
    let subject = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.subject).unwrap_or_default();
    hand_off(&app, &crate::organise::eml_name(&subject), "message.eml", raw, Sheet::Share).await
}

/// "Print" on iPhone: the page sends the finished, sanitised printout document (header block + the same body the
/// Reader shows, links inert, remote images only if already allowed); Swift renders it with JavaScript off and opens the
/// system print sheet. Nothing is staged on disk.
#[tauri::command]
#[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
pub async fn print_message(app: AppHandle, html: String, title: String) -> CmdResult<()> {
    if html.len() > MAX_PRINT_BYTES {
        return Err("This message is too large to print.".into());
    }
    #[cfg(target_os = "ios")]
    {
        let title: String = title.chars().filter(|c| !c.is_control() && !is_invisible(*c)).take(80).collect();
        tauri::async_runtime::spawn_blocking(move || tauri_plugin_mailcheck::print_html(&app, &html, &title)).await.map_err(err)?
    }
    #[cfg(not(target_os = "ios"))]
    {
        Err(NOT_PHONE.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_one_safe_component() {
        assert_eq!(share_file_name("../../etc/passwd", "x"), "passwd");
        assert_eq!(share_file_name("C:\\Users\\x\\evil.exe", "x"), "evil.exe");
        assert_eq!(share_file_name("a\nb\0c.txt", "x"), "a_b_c.txt");
        assert_eq!(share_file_name("\u{202E}fdp.exe\u{200B}", "x"), "fdp.exe");
        assert_eq!(share_file_name(".hidden", "x"), "hidden");
        assert_eq!(share_file_name("", "message.eml"), "message.eml");
        assert_eq!(share_file_name("   ", "message.eml"), "message.eml");
        assert_eq!(share_file_name("..", "fb"), "fb");
        assert_eq!(share_file_name("report.pdf", "x"), "report.pdf");
    }

    #[test]
    fn long_names_are_capped_and_keep_the_extension() {
        let n = share_file_name(&format!("{}.pdf", "é".repeat(300)), "x");
        assert!(n.chars().count() <= MAX_NAME_CHARS, "{}", n.chars().count());
        assert!(n.ends_with(".pdf"));
        let n = share_file_name(&format!("{}.{}", "a".repeat(150), "b".repeat(80)), "x");
        assert!(n.chars().count() <= MAX_NAME_CHARS);
        assert!(!n.is_empty());
    }

    #[test]
    fn invisible_and_separator_characters_are_dropped() {
        for c in ['\u{2028}', '\u{2029}', '\u{2060}', '\u{2064}', '\u{206F}', '\u{061C}', '\u{180E}', '\u{FFF9}', '\u{FFFB}', '\u{E0001}', '\u{E007F}', '\u{202E}', '\u{FEFF}'] {
            assert_eq!(share_file_name(&format!("a{c}b.pdf"), "x"), "ab.pdf", "U+{:04X}", c as u32);
        }
        assert_eq!(share_file_name("na\u{EF}ve.pdf", "x"), "na\u{EF}ve.pdf", "ordinary letters stay");
    }

    #[test]
    fn inside_rejects_escapes() {
        let root = Path::new("/c/share");
        assert!(is_inside(root, Path::new("/c/share/a/b.pdf")));
        assert!(!is_inside(root, Path::new("/c/share")));
        assert!(!is_inside(root, Path::new("/c/share/../x")));
        assert!(!is_inside(root, Path::new("/c/share/a/../../x")));
        assert!(!is_inside(root, Path::new("/c/other/a")));
        assert!(!is_inside(root, Path::new("/c/share2/a")));
    }

    #[test]
    fn staged_file_stays_inside_the_root_and_is_removed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join(SHARE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        let a = stage(&root, "one.txt", b"hi").unwrap();
        let b = stage(&root, "one.txt", b"hi").unwrap();
        assert_ne!(a.parent(), b.parent(), "each share has its own folder");
        assert!(is_inside(&root, &a) && a.parent().unwrap().parent() == Some(root.as_path()));
        assert_eq!(std::fs::read(&a).unwrap(), b"hi");
        remove_staged(&root, &a);
        assert!(!a.exists() && !a.parent().unwrap().exists());
        assert!(b.exists(), "the other share is untouched");
    }

    #[test]
    fn stage_refuses_unsafe_names() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join(SHARE_DIR);
        for bad in ["../x", "a/b", "..", ".", "a\\b"] {
            assert!(stage(&root, bad, b"x").is_err(), "{bad}");
        }
        assert!(!t.path().join("x").exists());
    }

    #[test]
    fn remove_staged_only_removes_direct_children_of_root() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join(SHARE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        let outside = t.path().join("keep");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.txt"), b"x").unwrap();
        remove_staged(&root, &outside.join("f.txt"));
        assert!(outside.join("f.txt").exists(), "outside the root: refused");
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("f"), b"x").unwrap();
        remove_staged(&root, &deep.join("f"));
        assert!(deep.join("f").exists(), "not a direct child folder: refused");
    }

    #[test]
    fn sweep_removes_leftovers_but_respects_age() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join(SHARE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        let a = stage(&root, "a.txt", b"1").unwrap();
        let b = stage(&root, "b.txt", b"2").unwrap();
        assert_eq!(sweep(&root, STALE), 0, "fresh shares are kept (a sheet may still be open)");
        assert!(a.exists() && b.exists());
        assert_eq!(sweep(&root, Duration::ZERO), 2, "launch sweep removes everything");
        assert!(!a.exists() && !b.exists() && root.exists());
        assert_eq!(sweep(&t.path().join("missing"), Duration::ZERO), 0);
    }

    #[test]
    fn these_commands_wait_for_the_user() {
        for c in ["share_attachment", "preview_attachment", "share_eml", "print_message"] {
            assert!(!crate::presence::ALLOWED_WHILE_AWAY.contains(&c), "{c} must be refused while the user is away");
            assert!(!crate::presence::command_allowed(false, c));
        }
    }
}
