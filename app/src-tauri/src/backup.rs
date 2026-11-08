//! Settings backup / restore (docs/ESSENTIALS_SPEC.md, Tier 2).
//!
//! The Preferences UI gathers the sections it can read (preferences, platform prefs, saved searches, signatures,
//! identities, rules); this module is the gate they pass through, on the way out AND on the way in:
//!
//! * only whitelisted sections are kept;
//! * a field whose NAME looks like a secret (password, token, API key, ...) or whose VALUE looks like one (`sk-...`,
//!   `-----BEGIN`, `Bearer ...`) makes the whole export refuse: nothing is written, and the message names the field path;
//! * a file to restore that carries such fields is refused too (a backup from MiraMail never has them);
//! * passwords, tokens and API keys live in the Keychain and are never part of this file.
//!
//! The file is a NEW file in Downloads (never overwritten). Restore is two steps: `backup_inspect` (validate + preview,
//! changes nothing) and the UI applying the sections the user ticks through the ordinary save commands.

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::AppHandle;

pub const FORMAT: &str = "miramail-settings";
pub const VERSION: u32 = 1;
const MAX_BYTES: usize = 5 * 1024 * 1024;

/// `(key, label, max items)`; sections that are arrays are counted, objects count as one.
pub const SECTIONS: [(&str, &str, usize); 8] = [
    ("prefs", "Preferences", 1),
    ("platform", "Dock, menu bar, widget and notifications", 1),
    ("sync", "Sync settings", 1),
    ("savedSearches", "Saved searches", 50),
    ("signatures", "Signatures", 100),
    ("signatureDefaults", "Default signatures", 200),
    ("identities", "Sender identities", 200),
    ("rules", "Rules", 500),
];

/// Key names (lower-cased, letters and digits only) that must never appear in a backup.
const SECRET_KEY_PARTS: [&str; 12] = [
    "password", "passwd", "passphrase", "secret", "token", "apikey", "privatekey", "authorization", "credential", "keychain", "bearer", "cookie",
];

/// Whether a JSON key name looks like it holds a secret.
pub fn secret_key(name: &str) -> bool {
    let n: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    n == "pw" || n == "pwd" || SECRET_KEY_PARTS.iter().any(|p| n.contains(p))
}

/// Whether a string value looks like a credential.
pub fn secret_value(v: &str) -> bool {
    if v.contains("-----BEGIN ") {
        return true;
    }
    let tok = |prefix: &str, min: usize| {
        v.match_indices(prefix).any(|(i, _)| {
            let rest = &v[i + prefix.len()..];
            rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).count() >= min
        })
    };
    tok("sk-", 20) || tok("Bearer ", 16) || tok("xoxb-", 10) || tok("xoxp-", 10) || tok("ghp_", 20) || tok("AKIA", 16) || tok("eyJhbGci", 10)
}

/// Paths (`rules[2].spec.password`) of everything in `v` that looks like a secret.
pub fn find_secrets(v: &Value) -> Vec<String> {
    fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    if secret_key(k) {
                        out.push(p.clone());
                    }
                    walk(x, &p, out);
                }
            }
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    walk(x, &format!("{path}[{i}]"), out);
                }
            }
            Value::String(s) if secret_value(s) => out.push(path.to_string()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

fn refuse(paths: &[String], what: &str) -> String {
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    format!("{what} contains a field that looks like a password or key ({}). MiraMail never puts those in a backup, so nothing was done.", shown.join(", "))
}

/// Keep the whitelisted sections of `parts`, check them, and wrap them in the backup envelope.
pub fn build(parts: &Value, now_ms: i64) -> Result<String, String> {
    let Some(obj) = parts.as_object() else { return Err("Nothing to back up.".into()) };
    let mut out = Map::new();
    out.insert("format".into(), json!(FORMAT));
    out.insert("version".into(), json!(VERSION));
    out.insert("createdAt".into(), json!(now_ms));
    out.insert("note".into(), json!("MiraMail settings backup. Contains no passwords, tokens or API keys: those stay in the macOS Keychain."));
    for (key, _, max) in SECTIONS {
        let Some(v) = obj.get(key) else { continue };
        match v {
            Value::Array(a) if a.len() > max => return Err(format!("Too many items in “{key}”.")),
            Value::Array(_) | Value::Object(_) => {
                out.insert(key.into(), v.clone());
            }
            _ => {}
        }
    }
    let doc = Value::Object(out);
    let bad = find_secrets(&doc);
    if !bad.is_empty() {
        return Err(refuse(&bad, "The backup"));
    }
    let text = serde_json::to_string_pretty(&doc).map_err(|_| "The backup could not be encoded.".to_string())?;
    if text.len() > MAX_BYTES {
        return Err("The backup is unexpectedly large; not written.".into());
    }
    Ok(text)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SectionPreview {
    pub key: &'static str,
    pub label: &'static str,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub version: u32,
    pub created_at: Option<i64>,
    pub sections: Vec<SectionPreview>,
    /// Account addresses that identities and rules refer to (restore only applies those that exist here).
    pub accounts: Vec<String>,
    /// The validated sections, for the UI to apply.
    pub data: Value,
}

/// Validate a backup file's text and say what is in it. Changes nothing.
pub fn inspect(text: &str) -> Result<Preview, String> {
    if text.len() > MAX_BYTES {
        return Err("That file is too large to be a MiraMail backup.".into());
    }
    let doc: Value = serde_json::from_str(text).map_err(|_| "That file is not a MiraMail settings backup.".to_string())?;
    let obj = doc.as_object().ok_or("That file is not a MiraMail settings backup.")?;
    if obj.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err("That file is not a MiraMail settings backup.".into());
    }
    let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;
    if version == 0 || version > VERSION {
        return Err("This backup was made by a newer MiraMail. Update the app to restore it.".into());
    }
    let bad = find_secrets(&doc);
    if !bad.is_empty() {
        return Err(refuse(&bad, "That file"));
    }
    let mut sections = Vec::new();
    let mut data = Map::new();
    let mut accounts: Vec<String> = Vec::new();
    for (key, label, max) in SECTIONS {
        let Some(v) = obj.get(key) else { continue };
        let count = match v {
            Value::Array(a) if a.len() <= max => a.len(),
            Value::Object(_) => 1,
            _ => return Err(format!("The “{label}” section of that file is not valid.")),
        };
        if let Value::Array(a) = v {
            for it in a {
                if let Some(acc) = it.get("account").and_then(Value::as_str) {
                    if !accounts.iter().any(|x| x == acc) {
                        accounts.push(acc.to_string());
                    }
                }
            }
        }
        sections.push(SectionPreview { key, label, count });
        data.insert(key.into(), v.clone());
    }
    accounts.sort();
    Ok(Preview { version, created_at: obj.get("createdAt").and_then(Value::as_i64), sections, accounts, data: Value::Object(data) })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Write the backup as a NEW file in Downloads; returns its path.
#[tauri::command]
pub fn backup_export(app: AppHandle, sections: Value) -> Result<String, String> {
    let text = build(&sections, now_ms())?;
    let dir = crate::files::downloads_dir(&app)?;
    let name = format!("MiraMail-settings-{}.json", chrono::Local::now().format("%Y-%m-%d"));
    let path = crate::files::save_unique(&dir, &name, text.as_bytes()).map_err(|e| format!("Couldn't save the file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Restore step 1: validate the text of a chosen file and describe it. Nothing is changed.
#[tauri::command]
pub fn backup_inspect(text: String) -> Result<Preview, String> {
    inspect(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "prefs": {"uiFont": "mira", "density": "compact", "undoSendSecs": 5},
            "platform": {"dockBadge": true, "notify": true, "notifyOff": ["a@x.org"]},
            "sync": {"initialSyncMessages": 200},
            "savedSearches": [{"id": "s1", "name": "Invoices", "query": "from:billing", "scope": "all"}],
            "signatures": [{"id": "g1", "name": "Work", "rich": false, "text": "Best,\nMe", "html": "", "placement": "below", "accounts": []}],
            "signatureDefaults": [{"account": "a@x.org", "newId": "g1", "replyId": null}],
            "identities": [{"id": "i1", "account": "a@x.org", "name": "Me", "address": "me@x.org", "replyTo": "", "signatureId": "g1"}],
            // Text that merely MENTIONS passwords is fine: only field names and credential-shaped values count.
            "rules": [{"id": 1, "account": "a@x.org", "name": "Reset mails", "enabled": true, "spec": {"conditions": [{"field": "subject", "value": "password reset"}], "actions": [{"kind": "mark_read"}]}}],
            "somethingElse": {"dropped": true}
        })
    }

    #[test]
    fn export_has_only_known_sections_and_no_secrets() {
        let text = build(&sample(), 1_700_000_000_000).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["format"], FORMAT);
        assert!(doc.get("somethingElse").is_none(), "unknown sections are dropped");
        assert!(find_secrets(&doc).is_empty(), "scan of the finished export finds nothing");
        // Belt and braces: scan the raw text for secret-looking field names and credential shapes.
        let lower = text.to_ascii_lowercase();
        for bad in ["\"password\"", "\"apikey\"", "\"api_key\"", "\"token\"", "\"secret\"", "\"accesstoken\"", "sk-", "-----begin", "bearer "] {
            assert!(!lower.contains(bad), "export contains {bad}");
        }
        // Round trip through the importer.
        let p = inspect(&text).unwrap();
        assert_eq!(p.version, 1);
        let counts: Vec<(&str, usize)> = p.sections.iter().map(|s| (s.key, s.count)).collect();
        assert_eq!(counts, vec![("prefs", 1), ("platform", 1), ("sync", 1), ("savedSearches", 1), ("signatures", 1), ("signatureDefaults", 1), ("identities", 1), ("rules", 1)]);
        assert_eq!(p.accounts, vec!["a@x.org".to_string()]);
    }

    #[test]
    fn secret_looking_fields_make_the_export_refuse() {
        for (k, v) in [("password", json!("hunter2")), ("apiKey", json!("x")), ("api_key", json!("x")), ("accessToken", json!("x")), ("Authorization", json!("x")), ("clientSecret", json!("x"))] {
            let mut doc = sample();
            doc["identities"][0][k] = v;
            let e = build(&doc, 0).unwrap_err();
            assert!(e.contains("looks like a password or key") && e.contains(&format!("identities[0].{k}")), "{k}: {e}");
        }
        for v in ["sk-abcdefghijklmnopqrstuvwxyz", "Authorization: Bearer abcdefghijklmnopqrstuvwxyz", "-----BEGIN PRIVATE KEY-----", "ghp_abcdefghijklmnopqrstuvwxyz0123"] {
            let mut doc = sample();
            doc["signatures"][0]["text"] = json!(v);
            assert!(build(&doc, 0).is_err(), "{v}");
        }
        // Not secrets: ordinary words and short tokens.
        for v in ["Please reset your password", "ask-me", "Bearer of good news", "sk-1"] {
            let mut doc = sample();
            doc["signatures"][0]["text"] = json!(v);
            assert!(build(&doc, 0).is_ok(), "{v}");
        }
    }

    #[test]
    fn restore_refuses_files_with_secrets_or_the_wrong_shape() {
        let good = build(&sample(), 5).unwrap();
        assert!(inspect(&good).is_ok());
        let mut doc: Value = serde_json::from_str(&good).unwrap();
        doc["prefs"]["imapPassword"] = json!("x");
        assert!(inspect(&doc.to_string()).unwrap_err().contains("never puts those in a backup"));
        assert!(inspect("not json").is_err());
        assert!(inspect("[]").is_err());
        assert!(inspect(r#"{"format":"other","version":1}"#).is_err());
        assert!(inspect(&format!(r#"{{"format":"{FORMAT}","version":99}}"#)).unwrap_err().contains("newer"));
        assert!(inspect(&format!(r#"{{"format":"{FORMAT}","version":1,"rules":"nope"}}"#)).is_err());
        let many: Vec<Value> = (0..51).map(|i| json!({"id": i.to_string(), "name": "n", "query": "q", "scope": "all"})).collect();
        assert!(inspect(&json!({"format": FORMAT, "version": 1, "savedSearches": many}).to_string()).is_err());
        assert!(build(&json!({"savedSearches": (0..51).map(|_| json!({})).collect::<Vec<_>>()}), 0).is_err());
        assert!(build(&json!("x"), 0).is_err());
    }

    #[test]
    fn key_name_heuristic() {
        for k in ["password", "Password", "imap_password", "authToken", "apiKey", "API-KEY", "privateKey", "secret", "pw", "refresh_token"] {
            assert!(secret_key(k), "{k}");
        }
        for k in ["name", "account", "query", "signatureId", "tokenless_ok_not", "undoSendSecs", "notifyPreview"] {
            // `tokenless_ok_not` contains "token": the heuristic is deliberately conservative.
            assert_eq!(secret_key(k), k == "tokenless_ok_not", "{k}");
        }
    }
}
