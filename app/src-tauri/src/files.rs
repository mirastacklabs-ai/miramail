//! Safe file naming and non-overwriting writes for attachment downloads.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_NAME_BYTES: usize = 200;

/// The user's real home directory. In an App Sandbox build `$HOME` points into the app container
/// (`/Users/x/Library/Containers/<id>/Data`), which would make "Downloads" a private folder the user never sees;
/// the `files.downloads.read-write` entitlement grants the REAL `~/Downloads`, so strip the container suffix.
pub fn real_home(home: &Path) -> PathBuf {
    let s = home.to_string_lossy();
    match s.find("/Library/Containers/") {
        Some(i) if i > 0 => PathBuf::from(&s[..i]),
        _ => home.to_path_buf(),
    }
}

/// Where exports and saved attachments go: the user's real Downloads folder (sandbox-aware, see `real_home`).
pub fn downloads_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    let err = || "Couldn't find your Downloads folder.".to_string();
    let d = app.path().download_dir().map_err(|_| err())?;
    let home = app.path().home_dir().map_err(|_| err())?;
    let real = real_home(&home);
    if real == home {
        return Ok(d);
    }
    Ok(real.join("Downloads"))
}

/// True when running inside the macOS App Sandbox (the system sets this variable for sandboxed processes).
pub fn is_sandboxed() -> bool {
    std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some_and(|v| !v.is_empty())
}

/// Write the copy of an attachment that is about to be handed to another app ("open attachment").
/// Sandboxed: another app may not be able to read a file inside our container, so the copy goes into the real
/// Downloads folder (`downloads`, granted by `files.downloads.read-write`) via `save_unique` (never overwrites).
/// Unsandboxed: the per-message cache folder `cache_dir`; a byte-identical file already there is reused when
/// `allow_reuse`, otherwise a fresh unique file is written. Returns exactly the path that should be opened.
pub fn stage_for_open(sandboxed: bool, downloads: Option<&Path>, cache_dir: &Path, name: &str, bytes: &[u8], allow_reuse: bool) -> std::io::Result<PathBuf> {
    if sandboxed {
        let dl = downloads.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no Downloads folder"))?;
        return save_unique(dl, name, bytes);
    }
    std::fs::create_dir_all(cache_dir)?;
    let existing = cache_dir.join(sanitize_filename(name));
    if allow_reuse && std::fs::read(&existing).is_ok_and(|b| b == bytes) {
        return Ok(existing);
    }
    save_unique(cache_dir, name, bytes)
}

/// Reduce an attacker-controlled attachment name to a single safe file name.
pub fn sanitize_filename(name: &str) -> String {
    // Keep only the last path component (either separator style).
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '\0') { '_' } else { c })
        .collect();
    let mut s = cleaned.trim().trim_start_matches('.').trim().to_string();
    if s.len() > MAX_NAME_BYTES {
        let (stem, ext) = split_ext(&s);
        let ext: String = ext.chars().take(20).collect();
        let keep = MAX_NAME_BYTES.saturating_sub(ext.len());
        let mut stem: String = stem.to_string();
        while stem.len() > keep {
            stem.pop();
        }
        s = format!("{stem}{ext}");
    }
    if s.is_empty() {
        "attachment".into()
    } else {
        s
    }
}

/// Splits "a.tar.gz" into ("a.tar", ".gz"); a name without extension gives ("name", "").
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Write `bytes` to a new file in `dir`, never overwriting: `name`, `name-1.ext`, `name-2.ext`...
/// Uses `create_new`, so it is race-free. Returns the final path.
pub fn save_unique(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let safe = sanitize_filename(name);
    let (stem, ext) = split_ext(&safe);
    for n in 0..10_000u32 {
        let candidate = if n == 0 { safe.clone() } else { format!("{stem}-{n}{ext}") };
        let path = dir.join(&candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).and_then(|_| f.flush()) {
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many files with the same name"))
}

/// Sub-folder of the app cache that holds files handed to other apps ("open attachment").
pub const OPENED_DIR: &str = "opened";
/// How long an opened attachment copy is kept.
#[allow(dead_code)]
pub const OPENED_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Remove copies older than `max_age` from `root` (the `opened` cache folder). Touches only what that
/// feature creates: regular files inside direct sub-folders named by a 16-digit hex key, and those
/// sub-folders once empty. Symlinks are never followed or removed, anything else is left alone. Best effort.
#[allow(dead_code)]
pub fn cleanup_opened(root: &Path, max_age: std::time::Duration) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let now = std::time::SystemTime::now();
    for e in rd.flatten() {
        let name = e.file_name();
        let key_ok = name.to_str().is_some_and(|n| n.len() == 16 && n.bytes().all(|b| b.is_ascii_hexdigit()));
        let dir = e.path();
        if !key_ok || !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let Ok(m) = std::fs::symlink_metadata(f.path()) else { continue };
            let old = m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= max_age);
            if m.is_file() && old {
                let _ = std::fs::remove_file(f.path());
            }
        }
        let _ = std::fs::remove_dir(&dir); // only succeeds when empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_home_strips_the_sandbox_container() {
        assert_eq!(real_home(Path::new("/Users/a/Library/Containers/ai.mirastacklabs.miramail/Data")), PathBuf::from("/Users/a"));
        assert_eq!(real_home(Path::new("/Users/a")), PathBuf::from("/Users/a"));
        assert_eq!(real_home(Path::new("/Library/Containers/x")), PathBuf::from("/Library/Containers/x"), "no empty prefix");
    }

    #[test]
    fn stage_for_open_sandboxed_goes_to_downloads_and_never_overwrites() {
        let dl = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let c = cache.path().join("k");
        let a = stage_for_open(true, Some(dl.path()), &c, "../x/report.pdf", b"one", true).unwrap();
        assert_eq!(a, dl.path().join("report.pdf"));
        let b = stage_for_open(true, Some(dl.path()), &c, "report.pdf", b"two", true).unwrap();
        assert_eq!(b, dl.path().join("report-1.pdf"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert!(!c.exists(), "nothing written to the cache when sandboxed");
        assert!(stage_for_open(true, None, &c, "a.pdf", b"", true).is_err());
    }

    #[test]
    fn stage_for_open_unsandboxed_uses_cache_and_reuses_identical() {
        let dl = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let c = cache.path().join("k");
        let a = stage_for_open(false, Some(dl.path()), &c, "a.txt", b"same", true).unwrap();
        assert_eq!(a, c.join("a.txt"));
        assert_eq!(stage_for_open(false, Some(dl.path()), &c, "a.txt", b"same", true).unwrap(), a, "identical reused");
        let b = stage_for_open(false, Some(dl.path()), &c, "a.txt", b"other", true).unwrap();
        assert_eq!(b, c.join("a-1.txt"));
        let d = stage_for_open(false, None, &c, "a.txt", b"same", false).unwrap();
        assert_eq!(d, c.join("a-2.txt"), "no reuse when not allowed");
        assert_eq!(std::fs::read_dir(dl.path()).unwrap().count(), 0);
    }

    #[test]
    fn sanitises_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        assert_eq!(sanitize_filename("a\nb\0c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("../"), "attachment");
        assert_eq!(sanitize_filename("..."), "attachment");
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        let long = format!("{}.pdf", "x".repeat(500));
        let s = sanitize_filename(&long);
        assert!(s.len() <= MAX_NAME_BYTES && s.ends_with(".pdf"));
    }

    #[test]
    fn never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let p0 = save_unique(d.path(), "a.txt", b"one").unwrap();
        let p1 = save_unique(d.path(), "a.txt", b"two").unwrap();
        let p2 = save_unique(d.path(), "../a.txt", b"three").unwrap();
        assert_eq!(p0.file_name().unwrap(), "a.txt");
        assert_eq!(p1.file_name().unwrap(), "a-1.txt");
        assert_eq!(p2.file_name().unwrap(), "a-2.txt");
        assert_eq!(std::fs::read(&p0).unwrap(), b"one");
        assert_eq!(std::fs::read(&p1).unwrap(), b"two");
        assert_eq!(p2.parent().unwrap(), d.path());
    }

    #[test]
    fn no_extension_and_dotfile_dedupe() {
        let d = tempfile::tempdir().unwrap();
        let a = save_unique(d.path(), "README", b"x").unwrap();
        let b = save_unique(d.path(), "README", b"y").unwrap();
        assert_eq!(a.file_name().unwrap(), "README");
        assert_eq!(b.file_name().unwrap(), "README-1");
    }

    #[test]
    fn cleanup_removes_only_old_files_in_our_key_folders() {
        use std::time::Duration;
        let root = tempfile::tempdir().unwrap();
        let ours = root.path().join("0123456789abcdef");
        let foreign = root.path().join("not-ours");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(ours.join("a.pdf"), b"x").unwrap();
        std::fs::write(foreign.join("keep.txt"), b"x").unwrap();
        std::fs::write(root.path().join("loose.txt"), b"x").unwrap();
        cleanup_opened(root.path(), Duration::from_secs(3600));
        assert!(ours.join("a.pdf").exists(), "too young to remove");
        cleanup_opened(root.path(), Duration::ZERO);
        assert!(!ours.exists(), "old file and the emptied key folder are gone");
        assert!(foreign.join("keep.txt").exists() && root.path().join("loose.txt").exists(), "nothing else is touched");
        cleanup_opened(&root.path().join("missing"), Duration::ZERO); // no panic
    }
}
