//! Golden stream tests (task A1): every `tests/golden/stream/*.sse` fixture is run through
//! `SseParser` + `StreamParser` and compared with its `.json` expectation. Each fixture is also
//! re-run split at every byte offset, byte by byte, and in pseudo-random chunkings: the output
//! must not depend on chunk boundaries.
//!
//! Expectation format: `{"events": [StreamEvent...], "error"?: {"kind", "contains",
//! "not_contains"?}, "tool_args"?: {"<idx>": <parsed args object>}}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aichat::provider::{StreamParser, UserFacingError};
use aichat::sse::SseParser;
use aichat::types::{Dialect, StreamEvent};
use serde_json::Value;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/stream")
}

fn fixtures() -> Vec<(String, Vec<u8>, Value)> {
    let mut v = Vec::new();
    for entry in std::fs::read_dir(golden_dir()).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("sse") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        let input = std::fs::read(&p).unwrap();
        let want: Value = serde_json::from_slice(&std::fs::read(p.with_extension("json")).unwrap())
            .unwrap_or_else(|e| panic!("{name}.json: {e}"));
        v.push((name, input, want));
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(v.len() >= 18, "fixtures missing: found {}", v.len());
    v
}

fn dialect_of(name: &str) -> Dialect {
    if name.starts_with("anthropic_") {
        Dialect::Anthropic
    } else {
        Dialect::OpenAiChat
    }
}

type Outcome = (Vec<StreamEvent>, Option<UserFacingError>);

fn run<'a>(dialect: Dialect, chunks: impl IntoIterator<Item = &'a [u8]>) -> Outcome {
    let mut sse = SseParser::new();
    let mut p = StreamParser::new(dialect);
    let mut out = Vec::new();
    for chunk in chunks {
        for ev in sse.feed(chunk).expect("sse") {
            match p.feed(ev) {
                Ok(v) => out.extend(v),
                Err(e) => return (out, Some(e)),
            }
        }
    }
    for ev in sse.finish().expect("sse finish") {
        match p.feed(ev) {
            Ok(v) => out.extend(v),
            Err(e) => return (out, Some(e)),
        }
    }
    match p.finish() {
        Ok(v) => out.extend(v),
        Err(e) => return (out, Some(e)),
    }
    (out, None)
}

fn check(name: &str, (events, error): &Outcome, want: &Value) {
    let got = serde_json::to_value(events).unwrap();
    assert_eq!(&got, &want["events"], "{name}: events\n got: {got:#}");
    match (error, want.get("error")) {
        (None, None) => {
            assert!(matches!(events.last(), Some(StreamEvent::Done(_))), "{name}: Done must be last");
            assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::Done(_))).count(), 1, "{name}");
        }
        (Some(e), Some(w)) => {
            assert_eq!(format!("{:?}", e.kind), w["kind"].as_str().unwrap(), "{name}: kind ({})", e.message);
            let c = w["contains"].as_str().unwrap();
            assert!(e.message.contains(c), "{name}: message {:?} lacks {c:?}", e.message);
            if let Some(nc) = w.get("not_contains").and_then(Value::as_str) {
                assert!(!e.message.contains(nc), "{name}: message leaks {nc:?}");
            }
            assert!(e.message.chars().count() < 600, "{name}: message too long");
        }
        (e, w) => panic!("{name}: error mismatch: got {e:?}, want {w:?}"),
    }
    if let Some(want_args) = want.get("tool_args") {
        let mut args: BTreeMap<usize, String> = BTreeMap::new();
        for ev in events {
            match ev {
                StreamEvent::ToolCallStart { idx, .. } => {
                    args.insert(*idx, String::new());
                }
                StreamEvent::ToolCallArgsDelta { idx, frag } => args.get_mut(idx).expect("delta before start").push_str(frag),
                _ => {}
            }
        }
        let parsed: serde_json::Map<String, Value> = args
            .into_iter()
            .map(|(i, s)| {
                let v = if s.is_empty() { Value::Object(Default::default()) } else { serde_json::from_str(&s).unwrap() };
                (i.to_string(), v)
            })
            .collect();
        assert_eq!(&Value::Object(parsed), want_args, "{name}: tool args");
    }
}

#[test]
fn golden_whole() {
    for (name, input, want) in fixtures() {
        let out = run(dialect_of(&name), [input.as_slice()]);
        check(&name, &out, &want);
    }
}

#[test]
fn golden_split_at_every_byte_offset() {
    for (name, input, want) in fixtures() {
        let whole = run(dialect_of(&name), [input.as_slice()]);
        for cut in 0..=input.len() {
            let (a, b) = input.split_at(cut);
            let out = run(dialect_of(&name), [a, b]);
            assert_eq!(out, whole, "{name}: split at {cut}");
        }
        check(&name, &whole, &want);
    }
}

#[test]
fn golden_byte_by_byte_and_random_chunks() {
    for (name, input, want) in fixtures() {
        let d = dialect_of(&name);
        let out = run(d, input.chunks(1));
        check(&format!("{name} (bytewise)"), &out, &want);
        // Deterministic LCG chunkings, chunk sizes 1..=37.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15 ^ input.len() as u64;
        for round in 0..20 {
            let mut chunks = Vec::new();
            let mut rest = input.as_slice();
            while !rest.is_empty() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let n = ((seed >> 33) % 37 + 1) as usize;
                let (a, b) = rest.split_at(n.min(rest.len()));
                chunks.push(a);
                rest = b;
            }
            let out = run(d, chunks);
            check(&format!("{name} (random round {round})"), &out, &want);
        }
    }
}

#[test]
fn split_events_crlf_variants_identical() {
    // The same OpenAI fixture with LF, CRLF and CR line endings yields the same events.
    let input = std::fs::read(golden_dir().join("openai_parallel_tools.sse")).unwrap();
    let lf = run(Dialect::OpenAiChat, [input.as_slice()]);
    let text = String::from_utf8(input).unwrap();
    let crlf = text.replace('\n', "\r\n").into_bytes();
    let cr = text.replace('\n', "\r").into_bytes();
    for v in [crlf, cr] {
        for cut in 0..=v.len() {
            let (a, b) = v.split_at(cut);
            assert_eq!(run(Dialect::OpenAiChat, [a, b]), lf, "cut {cut}");
        }
    }
}

#[test]
fn bom_prefixed_stream() {
    let input = std::fs::read(golden_dir().join("anthropic_text.sse")).unwrap();
    let plain = run(Dialect::Anthropic, [input.as_slice()]);
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(&input);
    for cut in 0..8 {
        let (a, b) = bom.split_at(cut);
        assert_eq!(run(Dialect::Anthropic, [a, b]), plain, "cut {cut}");
    }
}

#[test]
fn events_after_done_are_ignored() {
    let input = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"}}]}\n\n";
    let (events, err) = run(Dialect::OpenAiChat, [&input[..]]);
    assert!(err.is_none());
    assert_eq!(events, vec![StreamEvent::TextDelta("a".into()), StreamEvent::Done(aichat::types::StopReason::EndTurn)]);
}

#[test]
fn malformed_json_is_an_error_not_a_panic() {
    for d in [Dialect::OpenAiChat, Dialect::Anthropic] {
        let (_, err) = run(d, [&b"data: {not json\n\n"[..]]);
        let e = err.expect("error");
        assert!(!e.message.contains("not json"), "must not echo the body");
    }
}

#[test]
fn garbage_never_panics() {
    // Arbitrary bytes, including fragments of real fixtures, must never panic.
    let mut seed: u64 = 42;
    let corpus: Vec<u8> = fixtures().into_iter().flat_map(|(_, i, _)| i).collect();
    for _ in 0..300 {
        let mut buf = Vec::new();
        for _ in 0..64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = (seed >> 33) as usize;
            match r % 4 {
                0 => buf.push((r >> 8) as u8),
                1 => buf.extend_from_slice(b"\n"),
                _ => {
                    let start = (r >> 4) % corpus.len();
                    let end = (start + (r >> 12) % 80).min(corpus.len());
                    buf.extend_from_slice(&corpus[start..end]);
                }
            }
        }
        for d in [Dialect::OpenAiChat, Dialect::Anthropic] {
            let _ = run(d, buf.chunks(7));
        }
    }
}
