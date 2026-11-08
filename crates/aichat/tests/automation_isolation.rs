//! Rules and template auto-reply never use the AI endpoint: their source has no dependency on the HTTP
//! client, the chat engine or the providers, and the chat side has no way to reach the auto-reply or the
//! rule runner. Source-level scans, in the style of the SMTP scan in tools_engine.rs.
use std::path::Path;

fn src(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name)).unwrap()
}

/// Non-comment lines containing `needle` (case-sensitive).
fn hits(text: &str, needle: &str) -> Vec<String> {
    text.lines().filter(|l| !l.trim_start().starts_with("//") && l.contains(needle)).map(|l| l.trim().to_string()).collect()
}

#[test]
fn rules_and_autoreply_do_not_depend_on_the_ai_client() {
    let banned = ["reqwest", "crate::http", "crate::engine", "crate::provider", "crate::sse", "crate::budget", "crate::context", "crate::types", "tokio::net", "hyper", "Dialect", "api_key", "ProfileRuntime"];
    for file in ["rules.rs", "autoreply.rs"] {
        let text = src(file);
        // strip the #[cfg(test)] module: tests may use anything, production code may not
        let prod = text.split("#[cfg(test)]").next().unwrap();
        for b in banned {
            assert!(hits(prod, b).is_empty(), "{file} mentions `{b}`: {:?}", hits(prod, b));
        }
        // and the whole file never names the network layer either
        for b in ["reqwest", "crate::http", "crate::provider", "crate::sse"] {
            assert!(hits(&text, b).is_empty(), "{file}: {b}");
        }
    }
    // The only chat-engine item the executor (used to run rules) imports is the clock type alias.
    let engine_uses = hits(&src("proposals.rs"), "crate::engine");
    assert_eq!(engine_uses, vec!["use crate::engine::Clock;".to_string()], "{engine_uses:?}");
    for file in ["proposals.rs", "envelope.rs"] {
        for b in ["reqwest", "crate::http", "crate::provider", "crate::sse"] {
            assert!(hits(&src(file), b).is_empty(), "{file}: {b}");
        }
    }
}

#[test]
fn the_chat_side_cannot_reach_autoreply_or_the_rule_runner() {
    for file in ["engine.rs", "http.rs", "provider.rs", "context.rs", "budget.rs", "registry.rs", "redact.rs"] {
        let Ok(text) = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)) else { continue };
        for b in ["autoreply", "RuleRunner", "rules::"] {
            assert!(hits(&text, b).is_empty(), "{file} mentions `{b}`: {:?}", hits(&text, b));
        }
    }
    // tools.rs may only use the rule GRAMMAR to store a disabled proposal, never the runner or auto-reply.
    let tools = src("tools.rs");
    for b in ["autoreply", "RuleRunner", "undo_run", "run_folder", "baseline("] {
        assert!(hits(&tools, b).is_empty(), "tools.rs mentions `{b}`");
    }
    assert!(hits(&tools, "enabled: false").len() == 1, "propose_rule must create the rule disabled");
    assert!(hits(&tools, "set_rule_enabled").is_empty() && hits(&tools, "enabled: true").is_empty());
    // no model-facing tool name mentions auto-reply
    assert!(hits(&src("registry.rs"), "autoreply").is_empty());
}
