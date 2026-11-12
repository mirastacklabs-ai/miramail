//! MiraMail iOS spike: proves the shared crates (autodetect, mailcore) and the Keychain run on iOS,
//! and that a BGAppRefreshTask can call back into Rust. Not a product.
//! Never logs passwords, tokens or message bodies.

use mailcore::{AccountConfig, ImapClient, KeychainSecretStore, SecretStore, Storage, Tls};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::OnceLock;
use tauri::Manager;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
const INBOX: &str = "INBOX";

#[derive(Serialize)]
struct Row {
    uid: u32,
    from: String,
    subject: String,
    unread: bool,
}

#[derive(Serialize)]
struct LoginReport {
    provider: Option<String>,
    imap_host: String,
    total: u32,
    rows: Vec<Row>,
}

fn data_dir() -> Result<&'static PathBuf, String> {
    DATA_DIR.get().ok_or_else(|| "data dir not ready".to_string())
}

fn storage() -> Result<Storage, String> {
    Storage::open(data_dir()?.join("mail.db")).map_err(|e| e.to_string())
}

fn log_line(s: &str) {
    use std::io::Write;
    if let Ok(d) = data_dir() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("spike.log")) {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "{secs} {s}");
        }
    }
}

/// detect -> verify -> keychain store -> sync INBOX headers -> list. The password is never logged.
#[tauri::command]
async fn login_and_list(email: String, password: String) -> Result<LoginReport, String> {
    let email = email.trim().to_lowercase();
    let det = autodetect::detect(&email).await.map_err(|e| e.user_message())?;
    let v = autodetect::verify(&det, &email, &password).await.map_err(|e| e.user_message())?;
    let to_srv = |s: &autodetect::ServerSettings| mailcore::ServerConfig {
        host: s.host.clone(),
        port: s.port,
        tls: match s.tls {
            autodetect::TlsMode::Implicit => Tls::Implicit,
            _ => Tls::StartTls,
        },
    };
    let cfg = AccountConfig { email: email.clone(), imap: to_srv(&v.imap), smtp: to_srv(&v.smtp), username: v.imap.username.clone() };
    let st = storage()?;
    st.upsert_account(&cfg).map_err(|e| e.to_string())?;
    KeychainSecretStore.set(&email, &password).map_err(|e| e.to_string())?;
    let mut c = ImapClient::connect(&cfg.imap, &cfg.username, &password).await.map_err(|e| e.to_string())?;
    let rep = c.sync_folder(&st, &email, INBOX, 25).await.map_err(|e| e.to_string())?;
    let _ = c.logout().await;
    let rows = st
        .list_messages(&email, INBOX, 25, 0)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|m| Row { uid: m.uid, from: m.from, subject: m.subject, unread: !m.flags.seen })
        .collect();
    Ok(LoginReport { provider: v.provider, imap_host: cfg.imap.host, total: rep.server_total, rows })
}

/// Offline check 1: autodetect on a domain that cannot resolve (".invalid" is reserved, RFC 6761).
#[tauri::command]
async fn offline_detect() -> String {
    match autodetect::detect("nobody@spike.invalid").await {
        Ok(d) => format!("unexpected success: {} imap candidates", d.imap.len()),
        Err(e) => format!("autodetect ran, failed as expected: {}", e.user_message()),
    }
}

/// Offline check 2: IMAP connect to a closed local port goes through mailcore's real connect path.
#[tauri::command]
async fn offline_imap() -> String {
    let cfg = mailcore::ServerConfig { host: "127.0.0.1".into(), port: 1, tls: Tls::Implicit };
    match ImapClient::connect(&cfg, "u", "p").await {
        Ok(_) => "unexpected success".into(),
        Err(e) => format!("mailcore connect ran, failed as expected: {e}"),
    }
}

/// Offline check 3: Keychain round trip with a throwaway fake secret (iOS Protected Data store).
#[tauri::command]
fn keychain_selftest() -> String {
    let acct = "spike-selftest@example.invalid";
    let s = KeychainSecretStore;
    let steps = (|| -> Result<String, String> {
        s.set(acct, "fake-secret-1").map_err(|e| format!("set: {e}"))?;
        let got = s.get(acct).map_err(|e| format!("get: {e}"))?;
        let ok = got.as_deref() == Some("fake-secret-1");
        s.delete(acct).map_err(|e| format!("delete: {e}"))?;
        let gone = s.get(acct).map_err(|e| format!("get2: {e}"))?.is_none();
        Ok(format!("set/get/delete ok; roundtrip match={ok}; gone after delete={gone}"))
    })();
    match steps {
        Ok(m) => format!("keychain PROVEN: {m}"),
        Err(e) => format!("keychain FAILED at {e}"),
    }
}

#[tauri::command]
fn read_log() -> String {
    data_dir().ok().and_then(|d| std::fs::read_to_string(d.join("spike.log")).ok()).unwrap_or_else(|| "(empty)".into())
}

/// Background check: sync INBOX of the first saved account and report the unread count only.
fn background_check() -> Result<String, String> {
    log_line("bg check: start");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let out = rt.block_on(async {
        let st = storage()?;
        let Some(cfg) = st.list_accounts().map_err(|e| e.to_string())?.into_iter().next() else {
            return Ok("Background check ran (no account yet)".to_string());
        };
        let pw = KeychainSecretStore.get(&cfg.email).map_err(|e| e.to_string())?.ok_or("no password in Keychain")?;
        let mut c = tokio::time::timeout(std::time::Duration::from_secs(20), ImapClient::connect(&cfg.imap, &cfg.username, &pw))
            .await
            .map_err(|_| "connect timeout".to_string())?
            .map_err(|e| e.to_string())?;
        c.sync_folder(&st, &cfg.email, INBOX, 25).await.map_err(|e| e.to_string())?;
        let _ = c.logout().await;
        let n = st.recent_unseen(&cfg.email, INBOX, 50).map_err(|e| e.to_string())?.len();
        Ok::<String, String>(format!("{n} unread in Inbox"))
    });
    match &out {
        Ok(m) => log_line(&format!("bg check: ok ({m})")),
        Err(e) => log_line(&format!("bg check: error ({e})")),
    }
    out
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_bgrefresh::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let _ = DATA_DIR.set(dir);
            tauri_plugin_bgrefresh::set_check_handler(Box::new(background_check));
            log_line("app setup done");
            // Headless verification (simulator): `simctl launch <udid> <id> --autotest` runs the offline checks
            // and writes the results to spike.log; no real accounts or credentials are involved.
            if std::env::args().any(|a| a == "--autotest") {
                tauri::async_runtime::spawn(async {
                    log_line(&format!("autotest keychain: {}", keychain_selftest()));
                    log_line(&format!("autotest detect: {}", offline_detect().await));
                    log_line(&format!("autotest imap: {}", offline_imap().await));
                    let r = tauri::async_runtime::spawn_blocking(background_check).await;
                    log_line(&format!("autotest direct bg handler: {r:?}"));
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![login_and_list, offline_detect, offline_imap, keychain_selftest, read_log])
        .run(tauri::generate_context!())
        .expect("error while running MiraMail spike");
}
