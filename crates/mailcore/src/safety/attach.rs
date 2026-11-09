//! Attachment names and types. Names and MIME types only: nothing is opened, unpacked or executed (so the contents
//! of archives are not listed).

use super::{clean, is_bidi, Collector, SafetyInput, Severity};

const EXECUTABLE: &[&str] = &[
    "exe", "scr", "com", "bat", "cmd", "msi", "pif", "cpl", "app", "command", "sh", "pkg", "js", "jse", "vbs", "vbe", "wsf", "wsh", "ps1",
    "jar", "lnk", "hta", "reg", "dll", "apk", "action", "workflow", "terminal", "url", "scpt", "applescript", "msp", "gadget",
];
const DISK_IMAGE: &[&str] = &["iso", "img", "dmg", "vhd", "vhdx"];
const HTML: &[&str] = &["html", "htm", "xhtml", "shtml", "mht", "mhtml", "svg"];
const MACRO: &[&str] = &["docm", "xlsm", "pptm", "dotm", "xltm", "xlam", "ppam", "sldm"];
const ARCHIVE: &[&str] = &["zip", "rar", "7z", "gz", "tgz", "tar", "cab", "arj", "ace", "z", "bz2", "xz", "zipx"];
/// Extensions people expect to be harmless; a risky ending after one of these is a disguise.
const DOCLIKE: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "jpg", "jpeg", "png", "gif", "csv", "mp3", "mp4", "mov", "odt", "ods", "heic", "tiff", "bmp",
];

fn ext_of(name: &str) -> Option<String> {
    let (_, e) = name.trim().rsplit_once('.')?;
    let e = e.trim().to_lowercase();
    (!e.is_empty() && e.len() <= 12).then_some(e)
}

/// MIME types that are clearly programs / scripts / web pages, whatever the file name says.
fn risky_mime(m: &str) -> bool {
    let m = m.trim().to_ascii_lowercase();
    matches!(
        m.as_str(),
        "application/x-msdownload" | "application/x-dosexec" | "application/x-msdos-program" | "application/x-sh" | "application/x-csh"
            | "application/x-executable" | "application/x-mach-binary" | "application/vnd.microsoft.portable-executable" | "text/html"
            | "application/java-archive" | "application/x-bat" | "application/hta" | "text/javascript" | "application/javascript"
    )
}

fn benign_name(ext: &str) -> bool {
    DOCLIKE.contains(&ext) && !HTML.contains(&ext)
}

/// Does the text of a message look like it hands out an archive password?
fn password_in_text(t: &str) -> bool {
    let t = t.to_lowercase();
    ["password is", "password:", "password -", "pass:", "pwd:", "the password", "password to open", "password for the"].iter().any(|p| t.contains(p))
}

pub fn check(input: &SafetyInput, c: &mut Collector) {
    let mut archive_seen = false;
    for a in input.attachments.iter().take(100) {
        let shown = clean(&a.name, 60);
        if a.name.chars().any(is_bidi) {
            c.add_floor("ATT_BIDI_NAME", Severity::Critical, 80, format!("The file name \"{shown}\" contains hidden text-direction characters that can disguise its real ending."));
        }
        // Right-to-left tricks and trailing spaces: judge the visible name.
        let visible: String = a.name.chars().filter(|ch| !is_bidi(*ch) && !ch.is_control()).collect();
        let name = visible.trim();
        let Some(ext) = ext_of(name) else {
            if risky_mime(&a.mime) {
                c.add("ATT_MIME_MISMATCH", Severity::Medium, format!("\"{shown}\" has no file ending but is declared as {}.", clean(&a.mime, 40)));
            }
            continue;
        };
        let prev_ext = name.rsplit_once('.').and_then(|(stem, _)| ext_of(stem.trim_end()));
        let exec = EXECUTABLE.contains(&ext.as_str());
        let kind_high = DISK_IMAGE.contains(&ext.as_str()) || HTML.contains(&ext.as_str()) || MACRO.contains(&ext.as_str());
        let disguised = prev_ext.as_deref().is_some_and(benign_name);

        if exec {
            c.add_floor("ATT_EXECUTABLE", Severity::Critical, 80, format!("\"{shown}\" is a program or script (.{ext}); opening it can run code on your Mac."));
        }
        if disguised && (exec || kind_high || ARCHIVE.contains(&ext.as_str())) {
            let sev = if exec { Severity::Critical } else { Severity::High };
            c.add_floor("ATT_DOUBLE_EXT", sev, if exec { 80 } else { 0 }, format!("\"{shown}\" pretends to be a .{} file but really ends in .{ext}.", prev_ext.clone().unwrap_or_default()));
        }
        if MACRO.contains(&ext.as_str()) {
            c.add_floor("ATT_MACRO", Severity::High, 60, format!("\"{shown}\" is an Office file that can contain macros (.{ext})."));
        } else if DISK_IMAGE.contains(&ext.as_str()) {
            c.add("ATT_DISK_IMAGE", Severity::High, format!("\"{shown}\" is a disk image (.{ext}), a common way to deliver malware."));
        } else if HTML.contains(&ext.as_str()) {
            c.add("ATT_HTML", if ext == "svg" { Severity::Medium } else { Severity::High }, format!("\"{shown}\" is a web page (.{ext}); fake login forms are often sent this way."));
        } else if ARCHIVE.contains(&ext.as_str()) {
            archive_seen = true;
            c.add("ATT_ARCHIVE", Severity::Low, format!("\"{shown}\" is an archive; MiraMail cannot see what is inside."));
        }
        if benign_name(&ext) && risky_mime(&a.mime) {
            c.add("ATT_MIME_MISMATCH", Severity::Medium, format!("\"{shown}\" is named like a document but is declared as {}.", clean(&a.mime, 40)));
        }
    }
    if archive_seen && password_in_text(&input.text) {
        c.add(
            "ATT_ENCRYPTED_ARCHIVE_PASSWORD",
            Severity::Medium,
            "An archive is attached and the message text mentions a password. Passwords in the message are used to stop mail scanners looking inside (heuristic).",
        );
    }
}
