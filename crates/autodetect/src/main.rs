//! CLI harness: `autodetect <email> [--verify]`.

use autodetect::{detect, verify, DetectionResult, SourceOutcome};
use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

fn read_password() -> std::io::Result<String> {
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password("Password (not echoed): ")
    } else {
        // Non-interactive: first line of stdin.
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    }
}

fn print_detection(d: &DetectionResult) {
    println!("email: {}  domain: {}  provider: {}", d.email, d.domain, d.provider.as_deref().unwrap_or("-"));
    println!("\nsources:");
    for s in &d.sources {
        let o = match s.outcome {
            SourceOutcome::Hit { candidates } => format!("HIT ({candidates})"),
            SourceOutcome::Miss => "miss".into(),
            SourceOutcome::Error => format!("error: {}", s.detail.as_deref().unwrap_or("?")),
            SourceOutcome::TimedOut => "timed out".into(),
        };
        println!("  {:<42} {:>5} ms  {}", s.name, s.elapsed_ms, o);
    }
    for (label, list) in [("IMAP", &d.imap), ("SMTP", &d.smtp)] {
        println!("\n{label} candidates (ranked):");
        for (i, c) in list.iter().enumerate() {
            println!("  {}. {}:{} {:?}  [{:?}]  user={:?}", i + 1, c.host, c.port, c.tls, c.source, c.username_hint);
        }
    }
    println!("\ndetect total: {} ms", d.elapsed_ms);
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let do_verify = args.iter().any(|a| a == "--verify");
    let Some(email) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: autodetect <email> [--verify]");
        return ExitCode::from(2);
    };
    let det = match detect(email).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("detect failed: {e}\n  -> {}", e.user_message());
            return ExitCode::from(1);
        }
    };
    print_detection(&det);
    if !do_verify {
        return ExitCode::SUCCESS;
    }
    let pw = match read_password() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("could not read password: {e}");
            return ExitCode::from(2);
        }
    };
    let t = std::time::Instant::now();
    let r = verify(&det, &det.email, &pw).await;
    drop(pw);
    let _ = std::io::stdout().flush();
    match r {
        Ok(v) => {
            println!(
                "\nVERIFIED in {} ms\n  IMAP {}:{} {:?} user={}\n  SMTP {}:{} {:?} user={}",
                t.elapsed().as_millis(),
                v.imap.host, v.imap.port, v.imap.tls, v.imap.username,
                v.smtp.host, v.smtp.port, v.smtp.tls, v.smtp.username
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\nverify failed after {} ms: {}", t.elapsed().as_millis(), e.user_message());
            ExitCode::from(1)
        }
    }
}
