//! Workspace persistence: compose/event drafts and the pane layout (docs/WORKSPACE_SPEC.md, docs/APP_API.md).
//! Own database file (`workspace.db`). Draft payloads are user content: they are never logged and never
//! appear in error strings.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tauri::State;

pub const MAX_DRAFT_BYTES: usize = 1024 * 1024;
/// Must match MAX_JSON / MAX_DEPTH (= MAX_TREE_DEPTH here) / MAX_PANES in app/src/lib/wstree.ts: 200 drafts are 200 registered panes.
pub const MAX_LAYOUT_BYTES: usize = 256 * 1024;
const MAX_DRAFTS: i64 = 200;
const MAX_DEPTH: usize = 40;
/// Depth limit for a layout tree, same as MAX_DEPTH in wstree.ts (serde_json itself refuses nesting beyond 128 levels).
const MAX_TREE_DEPTH: usize = 96;
const KINDS: [&str; 2] = ["compose", "event"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: String,
    pub kind: String,
    /// Owning mail account (compose); None for events.
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub title: String,
    pub payload: Value,
    /// Unix ms, set by the backend on save (any value sent by the client is ignored).
    #[serde(default)]
    pub updated_at: i64,
}

pub struct DraftStore {
    conn: Mutex<Connection>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn depth_ok(v: &Value, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match v {
        Value::Array(a) => a.iter().all(|x| depth_ok(x, depth + 1)),
        Value::Object(o) => o.values().all(|x| depth_ok(x, depth + 1)),
        _ => true,
    }
}

/// Structural check of a persisted tree node (mirrors `parse` in app/src/lib/wstree.ts).
fn node_ok(n: &Value, depth: usize) -> bool {
    if depth > MAX_TREE_DEPTH {
        return false;
    }
    match n.get("t").and_then(Value::as_str) {
        Some("leaf") => n.get("tabs").and_then(Value::as_array).is_some_and(|t| !t.is_empty() && t.iter().all(Value::is_string)) && n.get("active").is_some_and(Value::is_string),
        Some("split") => {
            matches!(n.get("dir").and_then(Value::as_str), Some("row" | "col"))
                && n.get("ratio").is_some_and(Value::is_number)
                && n.get("a").is_some_and(|a| node_ok(a, depth + 1))
                && n.get("b").is_some_and(|b| node_ok(b, depth + 1))
        }
        _ => false,
    }
}

impl DraftStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_conn(Connection::open(path).map_err(|e| e.to_string())?)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS drafts(
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, account TEXT, title TEXT NOT NULL,
                payload TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS drafts_account ON drafts(account);
             CREATE TABLE IF NOT EXISTS layout(id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL, updated_at INTEGER NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(DraftStore { conn: Mutex::new(conn) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "Draft storage is unavailable.".to_string())
    }

    pub fn save(&self, d: &Draft) -> Result<i64, String> {
        if !valid_id(&d.id) {
            return Err("Invalid draft id.".into());
        }
        if !KINDS.contains(&d.kind.as_str()) {
            return Err("Invalid draft kind.".into());
        }
        if !depth_ok(&d.payload, 0) {
            return Err("Draft is too deeply nested.".into());
        }
        let payload = serde_json::to_string(&d.payload).map_err(|_| "Draft could not be encoded.".to_string())?;
        if payload.len() > MAX_DRAFT_BYTES {
            return Err("This draft is too large to save (limit 1 MB).".into());
        }
        let title: String = d.title.chars().take(200).collect();
        let account = d.account.as_deref().filter(|a| a.len() <= 320);
        let at = now_ms();
        let c = self.lock()?;
        let exists: bool = c.query_row("SELECT 1 FROM drafts WHERE id=?1", [&d.id], |_| Ok(true)).optional().map_err(|e| e.to_string())?.unwrap_or(false);
        if !exists {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM drafts", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            if n >= MAX_DRAFTS {
                return Err("Too many saved drafts. Discard some first.".into());
            }
        }
        c.execute(
            "INSERT INTO drafts(id,kind,account,title,payload,updated_at) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, account=excluded.account, title=excluded.title, payload=excluded.payload, updated_at=excluded.updated_at",
            params![d.id, d.kind, account, title, payload, at],
        )
        .map_err(|e| e.to_string())?;
        Ok(at)
    }

    pub fn list(&self) -> Result<Vec<Draft>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT id,kind,account,title,payload,updated_at FROM drafts ORDER BY updated_at DESC, id").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?)))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, account, title, payload, updated_at) = row.map_err(|e| e.to_string())?;
            // A corrupt row is skipped rather than failing the whole restore.
            if let Ok(payload) = serde_json::from_str(&payload) {
                out.push(Draft { id, kind, account, title, payload, updated_at });
            }
        }
        Ok(out)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM drafts WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Account removal: drop that account's compose drafts.
    pub fn delete_for_account(&self, account: &str) -> Result<usize, String> {
        self.lock()?.execute("DELETE FROM drafts WHERE account=?1", [account]).map_err(|e| e.to_string())
    }

    pub fn save_layout(&self, json: &str) -> Result<(), String> {
        if json.len() > MAX_LAYOUT_BYTES {
            return Err("Layout is too large to save.".into());
        }
        let v: Value = serde_json::from_str(json).map_err(|_| "Layout is not valid JSON.".to_string())?;
        let ok = v.get("v").and_then(Value::as_i64) == Some(1)
            && v.get("panes").is_some_and(Value::is_object)
            && v.get("parked").is_some_and(Value::is_array)
            && match v.get("root") {
                Some(Value::Null) => true,
                Some(n) => node_ok(n, 0),
                None => false,
            };
        if !ok {
            return Err("Layout is not valid.".into());
        }
        self.lock()?
            .execute(
                "INSERT INTO layout(id,json,updated_at) VALUES (1,?1,?2) ON CONFLICT(id) DO UPDATE SET json=excluded.json, updated_at=excluded.updated_at",
                params![json, now_ms()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load_layout(&self) -> Result<Option<String>, String> {
        self.lock()?.query_row("SELECT json FROM layout WHERE id=1", [], |r| r.get(0)).optional().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn save_draft(store: State<'_, DraftStore>, draft: Draft) -> Result<i64, String> {
    store.save(&draft)
}
#[tauri::command]
pub async fn list_drafts(store: State<'_, DraftStore>) -> Result<Vec<Draft>, String> {
    store.list()
}
#[tauri::command]
pub async fn delete_draft(store: State<'_, DraftStore>, id: String) -> Result<(), String> {
    store.delete(&id)
}
#[tauri::command]
pub async fn save_layout(store: State<'_, DraftStore>, json: String) -> Result<(), String> {
    store.save_layout(&json)
}
#[tauri::command]
pub async fn load_layout(store: State<'_, DraftStore>) -> Result<Option<String>, String> {
    store.load_layout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(id: &str, account: Option<&str>) -> Draft {
        Draft { id: id.into(), kind: "compose".into(), account: account.map(String::from), title: "Hi".into(), payload: json!({"text": "hello"}), updated_at: 0 }
    }

    #[test]
    fn save_list_update_delete() {
        let s = DraftStore::open_in_memory().unwrap();
        s.save(&d("a1", Some("me@x.org"))).unwrap();
        let mut b = d("a1", Some("me@x.org"));
        b.payload = json!({"text": "changed"});
        s.save(&b).unwrap();
        let l = s.list().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].payload["text"], "changed");
        assert!(l[0].updated_at > 0);
        s.delete("a1").unwrap();
        assert!(s.list().unwrap().is_empty());
        s.delete("a1").unwrap(); // idempotent
    }

    #[test]
    fn rejects_bad_ids_kinds_and_oversize() {
        let s = DraftStore::open_in_memory().unwrap();
        assert!(s.save(&d("", None)).is_err());
        assert!(s.save(&d("../x", None)).is_err());
        assert!(s.save(&d(&"a".repeat(65), None)).is_err());
        let mut k = d("k1", None);
        k.kind = "mailbox".into();
        assert!(s.save(&k).is_err());
        let mut big = d("big", None);
        big.payload = json!({"text": "x".repeat(MAX_DRAFT_BYTES)});
        let e = s.save(&big).unwrap_err();
        assert!(!e.contains("xxxx"), "errors never echo content");
        let mut ok = d("ok", None);
        ok.payload = json!({"text": "x".repeat(MAX_DRAFT_BYTES - 64)});
        s.save(&ok).unwrap();
        let mut deep = json!(1);
        for _ in 0..60 {
            deep = json!([deep]);
        }
        let mut dd = d("deep", None);
        dd.payload = deep;
        assert!(s.save(&dd).is_err());
    }

    #[test]
    fn draft_count_is_capped() {
        let s = DraftStore::open_in_memory().unwrap();
        for i in 0..MAX_DRAFTS {
            s.save(&d(&format!("d{i}"), None)).unwrap();
        }
        assert!(s.save(&d("extra", None)).is_err());
        s.save(&d("d0", None)).unwrap(); // updating an existing one still works
    }

    #[test]
    fn account_removal_deletes_only_that_accounts_drafts() {
        let s = DraftStore::open_in_memory().unwrap();
        s.save(&d("a", Some("one@x.org"))).unwrap();
        s.save(&d("b", Some("two@x.org"))).unwrap();
        let mut ev = d("e", None);
        ev.kind = "event".into();
        s.save(&ev).unwrap();
        assert_eq!(s.delete_for_account("one@x.org").unwrap(), 1);
        let ids: Vec<String> = s.list().unwrap().into_iter().map(|x| x.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(!ids.contains(&"a".to_string()));
    }

    #[test]
    fn corrupt_row_is_skipped() {
        let s = DraftStore::open_in_memory().unwrap();
        s.save(&d("good", None)).unwrap();
        s.lock().unwrap().execute("INSERT INTO drafts VALUES ('bad','compose',NULL,'t','{not json',1)", []).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn layout_roundtrip_and_validation() {
        let s = DraftStore::open_in_memory().unwrap();
        assert_eq!(s.load_layout().unwrap(), None);
        let good = r#"{"v":1,"root":{"t":"split","dir":"row","ratio":0.4,"a":{"t":"leaf","tabs":["mailbox"],"active":"mailbox"},"b":{"t":"leaf","tabs":["reader"],"active":"reader"}},"panes":{},"parked":[],"focus":null,"zoom":null}"#;
        s.save_layout(good).unwrap();
        assert_eq!(s.load_layout().unwrap().as_deref(), Some(good));
        s.save_layout(r#"{"v":1,"root":null,"panes":{},"parked":[]}"#).unwrap(); // single row: overwritten
        assert!(s.load_layout().unwrap().unwrap().contains("\"root\":null"));
        for bad in [
            "nope",
            "[]",
            r#"{"v":2,"root":null,"panes":{},"parked":[]}"#,
            r#"{"v":1,"panes":{},"parked":[]}"#,
            r#"{"v":1,"root":{"t":"leaf","tabs":[],"active":"x"},"panes":{},"parked":[]}"#,
            r#"{"v":1,"root":{"t":"split","dir":"diag","ratio":0.5,"a":null,"b":null},"panes":{},"parked":[]}"#,
            r#"{"v":1,"root":null,"panes":[],"parked":[]}"#,
        ] {
            assert!(s.save_layout(bad).is_err(), "{bad}");
        }
        // B3: a layout with the maximum number of drafts (200 registered panes) and a deep split tree must be storable.
        let mut panes = String::new();
        let mut parked = String::new();
        for i in 0..200 {
            let sep = if i == 0 { "" } else { "," };
            let i = format!("c{i}{}", "x".repeat(40));
            panes.push_str(&format!(r#"{sep}"{i}":{{"id":"{i}","kind":"compose","title":"{}","payload":null}}"#, "t".repeat(200)));
            parked.push_str(&format!(r#"{sep}"{i}""#));
        }
        let mut tree = String::from(r#"{"t":"leaf","tabs":["mailbox"],"active":"mailbox"}"#);
        for i in 0..90 {
            tree = format!(r#"{{"t":"split","dir":"row","ratio":0.5,"a":{tree},"b":{{"t":"leaf","tabs":["x{i}"],"active":"x{i}"}}}}"#);
        }
        let many = format!(r#"{{"v":1,"root":{tree},"panes":{{{panes}}},"parked":[{parked}],"focus":null,"zoom":null}}"#);
        assert!(many.len() > 64 * 1024 && many.len() <= MAX_LAYOUT_BYTES, "{}", many.len());
        s.save_layout(&many).unwrap();
        let mut deep = String::from(r#"{"t":"leaf","tabs":["mailbox"],"active":"mailbox"}"#);
        for i in 0..100 {
            deep = format!(r#"{{"t":"split","dir":"row","ratio":0.5,"a":{deep},"b":{{"t":"leaf","tabs":["x{i}"],"active":"x{i}"}}}}"#);
        }
        assert!(s.save_layout(&format!(r#"{{"v":1,"root":{deep},"panes":{{}},"parked":[]}}"#)).is_err());
        let big = format!(r#"{{"v":1,"root":null,"panes":{{}},"parked":[],"pad":"{}"}}"#, "x".repeat(MAX_LAYOUT_BYTES));
        assert!(s.save_layout(&big).is_err());
    }

    #[test]
    fn persists_in_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        DraftStore::open(&p).unwrap().save(&d("p", None)).unwrap();
        assert_eq!(DraftStore::open(&p).unwrap().list().unwrap().len(), 1);
    }
}
