//! Reader timing probe (ignored by default). Synthetic data only, no store or network.
//! Run: cargo test -p mailcore --release --test perf_reader -- --ignored --nocapture
use std::time::Instant;

fn fixture(links: usize, target_kb: usize) -> Vec<u8> {
    let mut h = String::from("<html><body><table width=600><tr><td>");
    let mut i = 0;
    while h.len() < target_kb * 1024 || i < links {
        if i < links || i % 3 == 0 {
            h.push_str(&format!(
                "<p style=\"font:14px Arial\">Item {i} <a href=\"https://click.example{}.com/track?u={i}&amp;sig=abcdef0123456789abcdef0123456789\">Read more about thing {i}</a> <img src=\"https://img.example{}.com/p/{i}.gif\" width=1 height=1></p>\n",
                i % 7, i % 5
            ));
        } else {
            h.push_str("<p style=\"color:#333\">Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.</p>\n");
        }
        i += 1;
    }
    h.push_str("</td></tr></table></body></html>");
    format!(
        "From: News <news@example.com>\r\nTo: me@example.org\r\nSubject: Weekly\r\nDate: Mon, 02 Nov 2026 10:00:00 +0000\r\nMessage-ID: <a@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{h}"
    )
    .into_bytes()
}

fn ms<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("perf-test: {label:<34} {:>8.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

#[test]
#[ignore]
fn reader_timings() {
    for (links, kb) in [(60, 30), (320, 200), (800, 500)] {
        let raw = fixture(links, kb);
        println!("--- links>={links} size={} KB", raw.len() / 1024);
        let parsed = ms("parse_message", || mailcore::parse_message(&raw).unwrap());
        let extra = ms("parse_extra_headers", || mailcore::mime::parse_extra_headers(&raw));
        let own = vec!["me@example.org".to_string()];
        let input = ms("safety::build_input (sanitize)", || mailcore::safety::build_input(&parsed, &extra, &own, &[], Some(3)));
        let rep = ms("safety::analyze", || mailcore::safety::analyze(&input));
        let _ = rep;
        let html = parsed.html.as_deref().unwrap();
        let rcpt = vec!["me@example.org".to_string()];
        ms("analyze_remote_images_from", || mailcore::analyze_remote_images_from(html, &rcpt, Some("example.com")));
        let deny = ms("tracker_url_set", || mailcore::tracker_url_set(html, &rcpt));
        ms("render_html_with (blocked)", || mailcore::render_html_with(&raw, false, &Default::default()).unwrap());
        ms("render_html_with (images+deny)", || mailcore::render_html_with(&raw, true, &deny).unwrap());
    }
}
