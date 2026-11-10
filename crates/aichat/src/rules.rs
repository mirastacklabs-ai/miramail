//! Local mail rules (AI chat P4). **No model, no network.**
//!
//! A rule is a [`RuleSpec`] (version 1): a list of conditions over header fields and a short allow-list of
//! actions. The model may only *propose* a rule (tool `propose_rule`); what it proposes is validated here,
//! stored DISABLED, and shown to the user as an app-written description plus a dry run. Users can also write
//! rules by hand without any model.
//!
//! * Matching is pure ([`matches`]): `equals`, `contains`, `glob` (`*` and `?` only, never regex) and
//!   `domainIs`. Every input is clipped before matching and every glob is bounded, so a hostile header cannot
//!   make evaluation slow ([`glob_match`] is O(text * pattern)).
//! * Actions: `moveTo`, `archive`, `trash`, `markRead`, `flag`. Nothing else exists: no delete-forever, no
//!   forward, no reply, no send.
//! * [`RuleRunner`] walks NEW mail (per rule/folder cursor tied to UIDVALIDITY, so each message is looked at
//!   once), and applies the actions through the same set-safe executor as approved suggestions. Every run is
//!   logged (`action_log` via a hidden system conversation, id 0) and can be undone as a whole.
//!
//! This file must not depend on the AI HTTP client or the chat engine (a test scans the source).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::envelope::{clean_line, strip_hidden};
use crate::proposals::{self, Executor, FlagOp, Kind, NewItem, NewProposal, Params, ProposalError};
use crate::store::{Result as StoreResult, Store};
use crate::tools::Scope;

pub const SPEC_VERSION: u32 = 1;
/// A serialized spec larger than this is refused before it is parsed.
pub const MAX_SPEC_BYTES: usize = 16 * 1024;
pub const MAX_CONDITIONS: usize = 10;
pub const MAX_ACTIONS: usize = 3;
pub const MAX_VALUE_CHARS: usize = 200;
pub const MAX_NAME_CHARS: usize = 80;
/// Largest size threshold a rule may use (kilobytes; 1 GB).
pub const MAX_SIZE_KB: u64 = 1_048_576;
pub const MAX_COLOUR: u8 = 7;
pub const MAX_RULES_PER_ACCOUNT: usize = 100;
/// Most rules the model can propose in one user turn.
pub const MAX_PROPOSED_PER_TURN: usize = 3;
/// Header text is clipped to this many chars before any matching.
pub const MAX_FIELD_CHARS: usize = 2000;
/// Text a glob is matched against is clipped to this many chars (globs are the only super-linear matcher).
pub const MAX_GLOB_TEXT_CHARS: usize = 300;
/// Glob steps one folder pass may spend in total; when exhausted the rest of the new mail waits for the next pass.
pub const PASS_STEP_BUDGET: u64 = 20_000_000;
/// Most `*` in one glob.
pub const MAX_GLOB_STARS: usize = 4;
/// Rows read per cursor step and steps per folder pass (so one pass looks at <= 10,000 new messages).
const BATCH: u32 = 500;
const MAX_BATCHES: usize = 20;
/// A message a rule moved is left alone by every rule for this long (stops two rules ping-ponging it).
pub const MOVE_GUARD_SECS: i64 = 7 * 86_400;
/// Most existing messages a dry run looks at per folder.
pub const DRY_RUN_SCAN: u32 = 500;
pub const DRY_RUN_SAMPLE: usize = 10;
/// Most existing messages "Apply to existing" looks at (and changes) per folder: the newest cached ones.
pub const APPLY_SCAN: u32 = 2000;

// ------------------------------------------------------------------------------------------
// Spec
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    From,
    To,
    Cc,
    Subject,
    ListId,
    HasListUnsubscribe,
    /// The message has a real (non-inline) attachment; value `true`/`false`.
    HasAttachment,
    /// Message size; op `greaterThan`, value = whole kilobytes.
    Size,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Equals,
    Contains,
    Glob,
    DomainIs,
    /// Only for [`Field::Size`].
    GreaterThan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    MoveTo { folder: String },
    Archive,
    Trash,
    MarkRead,
    Flag,
    /// Local colour label 1..=7 (stored on this Mac only, like the colour menu in the list).
    Colour { colour: u8 },
}

impl Action {
    fn is_destination(&self) -> bool {
        matches!(self, Action::MoveTo { .. } | Action::Archive | Action::Trash)
    }
    fn tag(&self) -> &'static str {
        match self {
            Action::MoveTo { .. } => "moveTo",
            Action::Archive => "archive",
            Action::Trash => "trash",
            Action::MarkRead => "markRead",
            Action::Flag => "flag",
            Action::Colour { .. } => "colour",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub v: u32,
    #[serde(rename = "match", default)]
    pub mode: MatchMode,
    pub conditions: Vec<Condition>,
    pub actions: Vec<Action>,
}

fn field_text(f: Field) -> &'static str {
    match f {
        Field::From => "sender",
        Field::To => "recipient (To)",
        Field::Cc => "Cc",
        Field::Subject => "subject",
        Field::ListId => "mailing-list ID",
        Field::HasListUnsubscribe => "unsubscribe link",
        Field::HasAttachment => "attachment",
        Field::Size => "size",
        Field::Folder => "folder",
    }
}

fn op_allowed(f: Field, o: Op) -> bool {
    match f {
        Field::From | Field::To | Field::Cc => true,
        Field::Subject | Field::ListId => o != Op::DomainIs,
        Field::HasListUnsubscribe | Field::HasAttachment => o == Op::Equals,
        Field::Size => o == Op::GreaterThan,
        Field::Folder => matches!(o, Op::Equals | Op::Glob),
    }
}

fn plain_address(v: &str) -> bool {
    let mut parts = v.split('@');
    let (Some(l), Some(d), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    !l.is_empty() && !d.is_empty() && !v.chars().any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | ',' | ';' | '(' | ')'))
}

fn plain_domain(v: &str) -> bool {
    !v.is_empty() && v.len() <= 253 && !v.starts_with('.') && !v.ends_with('.') && !v.contains("..") && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn glob_ok(v: &str) -> Result<(), String> {
    let stars = v.chars().filter(|c| *c == '*').count();
    if stars > MAX_GLOB_STARS {
        return Err(format!("A pattern may contain at most {MAX_GLOB_STARS} '*'."));
    }
    if v.chars().all(|c| c == '*' || c == '?') {
        return Err("A pattern made only of wildcards would match every message.".into());
    }
    Ok(())
}

fn clean_text(v: &str, what: &str) -> Result<String, String> {
    let t = v.trim();
    if t.is_empty() {
        return Err(format!("The {what} is empty."));
    }
    if t.chars().count() > MAX_VALUE_CHARS {
        return Err(format!("The {what} is longer than {MAX_VALUE_CHARS} characters."));
    }
    if strip_hidden(t) != t || t.contains('\n') || t.contains('\t') {
        return Err(format!("The {what} contains control or hidden characters."));
    }
    Ok(t.to_string())
}

/// Check a spec and return its normalised form. All limits live here; the UI, the model tool and the
/// runner all go through it.
pub fn validate(spec: &RuleSpec) -> Result<RuleSpec, String> {
    if spec.v != SPEC_VERSION {
        return Err(format!("Unsupported rule version {}.", spec.v));
    }
    if spec.conditions.is_empty() {
        return Err("Add at least one condition.".into());
    }
    if spec.conditions.len() > MAX_CONDITIONS {
        return Err(format!("A rule may have at most {MAX_CONDITIONS} conditions."));
    }
    if spec.actions.is_empty() {
        return Err("Add at least one action.".into());
    }
    if spec.actions.len() > MAX_ACTIONS {
        return Err(format!("A rule may have at most {MAX_ACTIONS} actions."));
    }
    let mut out = spec.clone();
    for c in &mut out.conditions {
        if !op_allowed(c.field, c.op) {
            return Err(format!("\"{}\" cannot be tested that way.", field_text(c.field)));
        }
        let v = clean_text(&c.value, "condition value")?;
        c.value = match (c.field, c.op) {
            (Field::HasListUnsubscribe, _) => match v.to_ascii_lowercase().as_str() {
                x @ ("true" | "false") => x.to_string(),
                _ => return Err("The unsubscribe-link condition must be true or false.".into()),
            },
            (Field::HasAttachment, _) => match v.to_ascii_lowercase().as_str() {
                x @ ("true" | "false") => x.to_string(),
                _ => return Err("The attachment condition must be true or false.".into()),
            },
            (Field::Size, _) => match v.parse::<u64>() {
                Ok(n) if (1..=MAX_SIZE_KB).contains(&n) && v.chars().all(|c| c.is_ascii_digit()) => n.to_string(),
                _ => return Err(format!("The size must be a whole number of kilobytes between 1 and {MAX_SIZE_KB}.")),
            },
            (Field::From | Field::To | Field::Cc, Op::Equals) => {
                if !plain_address(&v) {
                    return Err("\"is exactly\" needs a plain address such as name@example.com.".into());
                }
                v.to_ascii_lowercase()
            }
            (_, Op::DomainIs) => {
                let d = v.to_ascii_lowercase();
                if !plain_domain(&d) {
                    return Err("The domain must look like example.com (letters, digits, '-' and '.').".into());
                }
                d
            }
            (_, Op::Glob) => {
                glob_ok(&v)?;
                v
            }
            _ => v,
        };
    }
    let has_folder = out.conditions.iter().any(|c| c.field == Field::Folder);
    if has_folder && out.mode == MatchMode::Any {
        return Err("A folder condition can only be used when ALL conditions must match.".into());
    }
    if out.conditions.iter().all(|c| c.field == Field::Folder) {
        return Err("A rule needs at least one condition that is not just a folder.".into());
    }
    let mut seen = HashSet::new();
    for a in &mut out.actions {
        if !seen.insert(a.tag()) {
            return Err("An action is listed twice.".into());
        }
        if let Action::Colour { colour } = a {
            if !(1..=MAX_COLOUR).contains(colour) {
                return Err(format!("The colour must be a number from 1 to {MAX_COLOUR}."));
            }
        }
        if let Action::MoveTo { folder } = a {
            let f = clean_text(folder, "folder name")?;
            if f.eq_ignore_ascii_case("INBOX") {
                return Err("A rule cannot move mail to the Inbox.".into());
            }
            *folder = f;
        }
    }
    for a in &out.actions {
        if let Action::MoveTo { folder } = a {
            let probe = MsgFacts { folder: folder.clone(), ..MsgFacts::default() };
            let watches = out.conditions.iter().filter(|c| c.field == Field::Folder).any(|c| match c.op {
                Op::Equals => c.value.eq_ignore_ascii_case(folder),
                _ => condition_matches(c, &probe),
            });
            if watches {
                return Err("A rule cannot move mail into a folder it also watches (it would keep moving the same mail).".into());
            }
        }
    }
    if out.actions.iter().filter(|a| a.is_destination()).count() > 1 {
        return Err("Choose only one of move, archive and trash.".into());
    }
    Ok(out)
}

/// Parse and validate the JSON form. Refuses oversized input before parsing.
pub fn parse_spec(json: &str) -> Result<RuleSpec, String> {
    if json.len() > MAX_SPEC_BYTES {
        return Err("The rule is too large.".into());
    }
    let spec: RuleSpec = serde_json::from_str(json).map_err(|e| format!("The rule is not valid: {}", clean_line(&e.to_string(), 160)))?;
    // serde ignores extra keys on unit variants of an internally tagged enum; refuse them explicitly.
    let raw: serde_json::Value = serde_json::from_str(json).map_err(|_| "The rule is not valid.".to_string())?;
    for a in raw.get("actions").and_then(|a| a.as_array()).into_iter().flatten() {
        let obj = a.as_object().ok_or("The rule is not valid.")?;
        let ty = obj.get("type").and_then(|t| t.as_str());
        let (moves, colours) = (ty == Some("moveTo"), ty == Some("colour"));
        if obj.keys().any(|k| k != "type" && !(moves && k == "folder") && !(colours && k == "colour")) {
            return Err("An action has a setting it does not support.".into());
        }
    }
    validate(&spec)
}

// ------------------------------------------------------------------------------------------
// Matching (pure)
// ------------------------------------------------------------------------------------------

/// What a rule can see of a message. Header text is as received (display names included).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsgFacts {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub list_id: String,
    pub has_list_unsubscribe: bool,
    pub has_attachment: bool,
    /// Bytes.
    pub size: u32,
    pub folder: String,
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_FIELD_CHARS).collect()
}

/// Addresses in a header value, lowercased. Quoted display names are ignored (so `"a@b.c" <real@x.y>`
/// yields only `real@x.y`). Bounded by [`MAX_FIELD_CHARS`].
pub fn addresses_of(text: &str) -> Vec<String> {
    let mut plain = String::new();
    let mut in_q = false;
    for c in clip(text).chars() {
        match c {
            '"' => in_q = !in_q,
            _ if in_q => {}
            c => plain.push(c),
        }
    }
    let mut out = Vec::new();
    for tok in plain.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '(' | ')' | '[' | ']' | ':')) {
        if plain_address(tok) {
            out.push(tok.to_ascii_lowercase());
        }
    }
    out
}

/// `*` any run (including empty), `?` any one char, everything else literal. Case-insensitive callers
/// lowercase both sides. Iterative with one backtrack point: at most `text.len() * (pattern.len() + 1)`
/// steps, never exponential. `steps` counts loop iterations (tests assert the bound).
pub fn glob_match(pat: &[char], text: &[char], steps: &mut u64) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while t < text.len() {
        *steps += 1;
        if p < pat.len() && (pat[p] == '?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

fn glob_ci(pattern: &str, text: &str, steps: &mut u64) -> bool {
    let pat: Vec<char> = pattern.to_lowercase().chars().take(MAX_VALUE_CHARS).collect();
    let txt: Vec<char> = text.chars().take(MAX_GLOB_TEXT_CHARS).collect::<String>().to_lowercase().chars().collect();
    glob_match(&pat, &txt, steps)
}

fn list_id_core(raw: &str) -> String {
    let r = clip(raw);
    match (r.find('<'), r.rfind('>')) {
        (Some(a), Some(b)) if b > a => r[a + 1..b].trim().to_lowercase(),
        _ => r.trim().to_lowercase(),
    }
}

/// Does this single condition hold for `f`? Unknown combinations never match.
pub fn condition_matches(c: &Condition, f: &MsgFacts) -> bool {
    condition_matches_b(c, f, &mut 0)
}

/// Like [`condition_matches`], adding the glob steps spent to `steps` (the runner's per-pass budget).
pub fn condition_matches_b(c: &Condition, f: &MsgFacts, steps: &mut u64) -> bool {
    if c.field == Field::HasListUnsubscribe {
        return c.op == Op::Equals && (c.value == "true") == f.has_list_unsubscribe;
    }
    if c.field == Field::HasAttachment {
        return c.op == Op::Equals && (c.value == "true") == f.has_attachment;
    }
    if c.field == Field::Size {
        return c.op == Op::GreaterThan && c.value.parse::<u64>().is_ok_and(|kb| u64::from(f.size) > kb.saturating_mul(1024));
    }
    let text = match c.field {
        Field::From => &f.from,
        Field::To => &f.to,
        Field::Cc => &f.cc,
        Field::Subject => &f.subject,
        Field::ListId => &f.list_id,
        Field::Folder => &f.folder,
        Field::HasListUnsubscribe | Field::HasAttachment | Field::Size => return false,
    };
    let want = c.value.to_lowercase();
    match c.op {
        Op::Equals => match c.field {
            Field::From | Field::To | Field::Cc => addresses_of(text).contains(&want),
            Field::ListId => list_id_core(text) == want,
            Field::Folder => {
                if want == "inbox" {
                    text.eq_ignore_ascii_case("INBOX")
                } else {
                    *text == c.value
                }
            }
            _ => clip(text).trim().to_lowercase() == want,
        },
        Op::Contains => clip(text).to_lowercase().contains(&want),
        Op::GreaterThan => false,
        Op::Glob => glob_ci(&c.value, text, steps),
        Op::DomainIs => addresses_of(text).iter().any(|a| {
            let dom = a.rsplit('@').next().unwrap_or("");
            dom == want || dom.ends_with(&format!(".{want}"))
        }),
    }
}

/// Can this rule act on mail in `folder` at all? (No folder condition = INBOX only.)
pub fn could_apply(spec: &RuleSpec, folder: &str) -> bool {
    let folder_conds: Vec<&Condition> = spec.conditions.iter().filter(|c| c.field == Field::Folder).collect();
    if folder_conds.is_empty() {
        return folder.eq_ignore_ascii_case("INBOX");
    }
    let f = MsgFacts { folder: folder.to_string(), ..MsgFacts::default() };
    folder_conds.iter().all(|c| condition_matches(c, &f))
}

/// Does the rule match this message? Mail outside the rule's folders never matches.
pub fn matches(spec: &RuleSpec, f: &MsgFacts) -> bool {
    matches_b(spec, f, &mut 0)
}

pub fn matches_b(spec: &RuleSpec, f: &MsgFacts, steps: &mut u64) -> bool {
    if !could_apply(spec, &f.folder) {
        return false;
    }
    let mut it = spec.conditions.iter().filter(|c| c.field != Field::Folder);
    match spec.mode {
        MatchMode::All => it.all(|c| condition_matches_b(c, f, steps)),
        MatchMode::Any => it.any(|c| condition_matches_b(c, f, steps)),
    }
}

/// Plain-English description written by the app (never by the model).
pub fn describe(spec: &RuleSpec) -> String {
    let q = |s: &str| format!("\"{}\"", clean_line(s, 80));
    let conds: Vec<String> = spec
        .conditions
        .iter()
        .filter(|c| c.field != Field::Folder)
        .map(|c| match (c.field, c.op) {
            (Field::HasListUnsubscribe, _) => {
                if c.value == "true" { "the message has an unsubscribe link".to_string() } else { "the message has no unsubscribe link".to_string() }
            }
            (Field::HasAttachment, _) => {
                if c.value == "true" { "the message has an attachment".to_string() } else { "the message has no attachment".to_string() }
            }
            (Field::Size, _) => format!("the message is larger than {} KB", c.value),
            (_, Op::GreaterThan) => "(invalid condition)".to_string(),
            (f, Op::Equals) => format!("the {} is exactly {}", field_text(f), q(&c.value)),
            (f, Op::Contains) => format!("the {} contains {}", field_text(f), q(&c.value)),
            (f, Op::Glob) => format!("the {} matches the pattern {}", field_text(f), q(&c.value)),
            (f, Op::DomainIs) => format!("the {} is from the domain {} (or a subdomain)", field_text(f), q(&c.value)),
        })
        .collect();
    let joiner = if spec.mode == MatchMode::Any { " OR " } else { " AND " };
    let folders: Vec<String> = spec.conditions.iter().filter(|c| c.field == Field::Folder).map(|c| q(&c.value)).collect();
    let place = if folders.is_empty() { "the Inbox".to_string() } else { format!("the folder {}", folders.join(" and ")) };
    let acts: Vec<String> = spec
        .actions
        .iter()
        .map(|a| match a {
            Action::MoveTo { folder } => format!("move it to {}", q(folder)),
            Action::Archive => "archive it".to_string(),
            Action::Trash => "move it to Trash (not deleted for good)".to_string(),
            Action::MarkRead => "mark it as read".to_string(),
            Action::Flag => "flag it".to_string(),
            Action::Colour { colour } => format!("give it colour label {colour}"),
        })
        .collect();
    format!("When a new message arrives in {place} and {}, {}.", conds.join(joiner), acts.join(" and "))
}

// ------------------------------------------------------------------------------------------
// Stored rules
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: i64,
    pub account: String,
    pub name: String,
    pub enabled: bool,
    pub spec_json: String,
    /// `None` when the stored JSON no longer validates (such a rule never runs).
    pub spec: Option<RuleSpec>,
    /// `user` or `ai`
    pub origin: String,
    pub conv_id: Option<i64>,
    pub reason: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_run_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewRule {
    pub account: String,
    pub name: String,
    pub spec: RuleSpec,
    pub origin: &'static str,
    pub conv_id: Option<i64>,
    pub reason: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRun {
    pub id: i64,
    pub rule_id: i64,
    pub rule_name: String,
    pub account: String,
    pub folder: String,
    pub ts: i64,
    pub matched: u32,
    pub summary: String,
    pub proposal_ids: Vec<i64>,
}

/// `(row id, account, message id, previous colour, new colour)`
pub type ColourLogRow = (i64, String, String, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub uidvalidity: u32,
    pub last_uid: u32,
}

/// A short, stable fingerprint (FNV-1a) of everything about a stored rule that a rule-change suggestion
/// depends on: name, conditions/actions, and on/off. Used to refuse an approval when the rule changed since.
pub fn rule_fingerprint(r: &Rule) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in r.name.bytes().chain([0u8]).chain(r.spec_json.bytes()).chain([0u8, u8::from(r.enabled)]).chain(r.account.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn clean_rule_name(n: &str) -> Result<String, String> {
    let t = clean_line(n, MAX_NAME_CHARS + 1);
    if t.is_empty() {
        return Err("Give the rule a name.".into());
    }
    if t.chars().count() > MAX_NAME_CHARS {
        return Err(format!("The rule name is longer than {MAX_NAME_CHARS} characters."));
    }
    Ok(t)
}

const RULE_COLS: &str = "id, account, name, enabled, spec_json, origin, conv_id, reason, created_at, updated_at, last_run_at";

fn row_rule(r: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    let spec_json: String = r.get(4)?;
    Ok(Rule {
        id: r.get(0)?,
        account: r.get(1)?,
        name: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        spec: parse_spec(&spec_json).ok(),
        spec_json,
        origin: r.get(5)?,
        conv_id: r.get(6)?,
        reason: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        last_run_at: r.get(10)?,
    })
}

fn row_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RuleRun> {
    let ids: String = r.get(8)?;
    Ok(RuleRun {
        id: r.get(0)?,
        rule_id: r.get(1)?,
        rule_name: r.get(2)?,
        account: r.get(3)?,
        folder: r.get(4)?,
        ts: r.get(5)?,
        matched: r.get(6)?,
        summary: r.get(7)?,
        proposal_ids: serde_json::from_str(&ids).unwrap_or_default(),
    })
}
const RUN_COLS: &str = "id, rule_id, rule_name, account, folder, ts, matched, summary, proposal_ids";

impl Store {
    /// Store a validated rule. Refuses a 101st rule for the account.
    pub fn create_rule(&mut self, n: &NewRule, now: i64) -> Result<i64, String> {
        let spec = validate(&n.spec)?;
        let name = clean_rule_name(&n.name)?;
        let db = |_: rusqlite::Error| "MiraMail couldn't save the rule.".to_string();
        let c = self.conn_mut();
        let count: i64 = c.query_row("SELECT COUNT(*) FROM rules WHERE account=?1", params![n.account], |r| r.get(0)).map_err(db)?;
        if count as usize >= MAX_RULES_PER_ACCOUNT {
            return Err(format!("An account can have at most {MAX_RULES_PER_ACCOUNT} rules."));
        }
        c.execute(
            "INSERT INTO rules (account, name, enabled, spec_json, origin, conv_id, reason, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![n.account, name, n.enabled, serde_json::to_string(&spec).map_err(|_| "MiraMail couldn't save the rule.".to_string())?, n.origin, n.conv_id, n.reason, now],
        )
        .map_err(db)?;
        Ok(c.last_insert_rowid())
    }

    pub fn get_rule(&mut self, id: i64) -> StoreResult<Option<Rule>> {
        Ok(self.conn_mut().query_row(&format!("SELECT {RULE_COLS} FROM rules WHERE id=?1"), params![id], row_rule).optional()?)
    }

    /// Oldest first (= evaluation order). `account: None` = all accounts.
    pub fn list_rules(&mut self, account: Option<&str>) -> StoreResult<Vec<Rule>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {RULE_COLS} FROM rules WHERE (?1 IS NULL OR account=?1) ORDER BY id"))?;
        let rows = st.query_map(params![account], row_rule)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replace name and spec (validated). Keeps origin. An edited rule is switched OFF: the user must look at the
    /// preview again and enable it (the cursors are re-based then).
    pub fn update_rule(&mut self, id: i64, name: &str, spec: &RuleSpec, now: i64) -> Result<(), String> {
        let spec = validate(spec)?;
        let name = clean_rule_name(name)?;
        let json = serde_json::to_string(&spec).map_err(|_| "MiraMail couldn't save the rule.".to_string())?;
        let n = self
            .conn_mut()
            .execute("UPDATE rules SET name=?2, spec_json=?3, updated_at=?4, enabled=0 WHERE id=?1", params![id, name, json, now])
            .map_err(|_| "MiraMail couldn't save the rule.".to_string())?;
        if n == 0 {
            return Err("That rule no longer exists.".into());
        }
        Ok(())
    }

    pub fn set_rule_enabled(&mut self, id: i64, enabled: bool, now: i64) -> StoreResult<bool> {
        Ok(self.conn_mut().execute("UPDATE rules SET enabled=?2, updated_at=?3 WHERE id=?1", params![id, enabled, now])? > 0)
    }

    /// Set on/off only if the stored rule still is exactly `expect` (name, spec, on/off, account). `false` = it changed.
    pub fn set_rule_enabled_matching(&mut self, expect: &Rule, enabled: bool, now: i64) -> StoreResult<bool> {
        Ok(self.conn_mut().execute(
            "UPDATE rules SET enabled=?2, updated_at=?3 WHERE id=?1 AND name=?4 AND spec_json=?5 AND enabled=?6 AND account=?7",
            params![expect.id, enabled, now, expect.name, expect.spec_json, expect.enabled, expect.account],
        )? > 0)
    }

    /// Delete only if the stored rule still is exactly `expect`. `false` = it changed or is gone.
    pub fn delete_rule_matching(&mut self, expect: &Rule) -> StoreResult<bool> {
        Ok(self.conn_mut().execute(
            "DELETE FROM rules WHERE id=?1 AND name=?2 AND spec_json=?3 AND enabled=?4 AND account=?5",
            params![expect.id, expect.name, expect.spec_json, expect.enabled, expect.account],
        )? > 0)
    }

    pub fn delete_rule(&mut self, id: i64) -> StoreResult<bool> {
        Ok(self.conn_mut().execute("DELETE FROM rules WHERE id=?1", params![id])? > 0)
    }

    pub fn get_cursor(&mut self, rule_id: i64, account: &str, folder: &str) -> StoreResult<Option<Cursor>> {
        Ok(self
            .conn_mut()
            .query_row("SELECT uidvalidity, last_uid FROM rule_cursor WHERE rule_id=?1 AND account=?2 AND folder=?3", params![rule_id, account, folder], |r| {
                Ok(Cursor { uidvalidity: r.get(0)?, last_uid: r.get(1)? })
            })
            .optional()?)
    }

    pub fn set_cursor(&mut self, rule_id: i64, account: &str, folder: &str, c: Cursor) -> StoreResult<()> {
        self.conn_mut().execute(
            "INSERT INTO rule_cursor (rule_id, account, folder, uidvalidity, last_uid) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(rule_id, account, folder) DO UPDATE SET uidvalidity=?4, last_uid=?5",
            params![rule_id, account, folder, c.uidvalidity, c.last_uid],
        )?;
        Ok(())
    }

    pub fn rules_paused(&self) -> StoreResult<bool> {
        Ok(self.get_setting("rules_paused")?.as_deref() == Some("1"))
    }

    pub fn set_rules_paused(&mut self, paused: bool) -> StoreResult<()> {
        self.set_setting("rules_paused", if paused { "1" } else { "0" })
    }

    pub fn add_rule_run(&mut self, r: &RuleRun) -> StoreResult<i64> {
        let c = self.conn_mut();
        c.execute(
            "INSERT INTO rule_runs (rule_id, rule_name, account, folder, ts, matched, summary, proposal_ids) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![r.rule_id, r.rule_name, r.account, r.folder, r.ts, r.matched, r.summary, serde_json::to_string(&r.proposal_ids)?],
        )?;
        let id = c.last_insert_rowid();
        c.execute("UPDATE rules SET last_run_at=?2 WHERE id=?1", params![r.rule_id, r.ts])?;
        Ok(id)
    }

    pub fn get_rule_run(&mut self, id: i64) -> StoreResult<Option<RuleRun>> {
        Ok(self.conn_mut().query_row(&format!("SELECT {RUN_COLS} FROM rule_runs WHERE id=?1"), params![id], row_run).optional()?)
    }

    /// Newest first.
    pub fn list_rule_runs(&mut self, limit: u32) -> StoreResult<Vec<RuleRun>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {RUN_COLS} FROM rule_runs ORDER BY ts DESC, id DESC LIMIT ?1"))?;
        let rows = st.query_map(params![limit], row_run)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Did a rule move this message (by Message-ID) since `since` and has that not been undone?
    pub fn rule_moved_recently(&mut self, account: &str, message_id: &str, since: i64) -> StoreResult<bool> {
        let n: i64 = self.conn_mut().query_row(
            "SELECT COUNT(*) FROM action_log l JOIN proposals p ON p.id = l.proposal_id
             WHERE p.conv_id = 0 AND l.op = 'move' AND l.account = ?1 AND l.message_id = ?2 AND l.ts >= ?3
               AND l.outcome IN ('ok','ok-unverified') AND l.undone = 0",
            params![account, message_id, since],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Fallback to [`Store::rule_moved_recently`] for mail without a Message-ID.
    pub fn rule_moved_fp_recently(&mut self, account: &str, fp: &str, since: i64) -> StoreResult<bool> {
        let n: i64 = self.conn_mut().query_row("SELECT COUNT(*) FROM rule_moved WHERE account=?1 AND fp=?2 AND ts>=?3", params![account, fp, since], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn add_rule_moved_fp(&mut self, account: &str, fp: &str, run_id: i64, ts: i64) -> StoreResult<()> {
        self.conn_mut().execute("INSERT INTO rule_moved (account, fp, run_id, ts) VALUES (?1,?2,?3,?4)", params![account, fp, run_id, ts])?;
        Ok(())
    }

    /// Undoing a run frees its messages for rules again.
    pub fn clear_rule_moved_run(&mut self, run_id: i64) -> StoreResult<()> {
        self.conn_mut().execute("DELETE FROM rule_moved WHERE run_id=?1", params![run_id])?;
        Ok(())
    }

    /// Replace a run's summary (finished after its local colour step).
    pub fn update_rule_run_summary(&mut self, run_id: i64, summary: &str) -> StoreResult<()> {
        self.conn_mut().execute("UPDATE rule_runs SET summary=?2 WHERE id=?1", params![run_id, summary])?;
        Ok(())
    }

    pub fn add_colour_log(&mut self, run_id: i64, account: &str, message_id: &str, prev: u8, new: u8) -> StoreResult<()> {
        self.conn_mut().execute("INSERT INTO rule_colour_log (run_id, account, message_id, prev, new) VALUES (?1,?2,?3,?4,?5)", params![run_id, account, message_id, prev, new])?;
        Ok(())
    }

    /// Colour changes of a run that have not been undone: `(row id, account, message id, previous, new)`.
    pub fn pending_colour_log(&mut self, run_id: i64) -> StoreResult<Vec<ColourLogRow>> {
        let c = self.conn_mut();
        let mut st = c.prepare("SELECT id, account, message_id, prev, new FROM rule_colour_log WHERE run_id=?1 AND undone=0 ORDER BY id DESC")?;
        let rows = st.query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(all, not yet undone)` colour changes of a run.
    pub fn colour_log_counts(&mut self, run_id: i64) -> StoreResult<(u32, u32)> {
        Ok(self.conn_mut().query_row("SELECT COUNT(*), COALESCE(SUM(undone=0),0) FROM rule_colour_log WHERE run_id=?1", params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))?)
    }

    pub fn mark_colour_undone(&mut self, id: i64) -> StoreResult<()> {
        self.conn_mut().execute("UPDATE rule_colour_log SET undone=1 WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Drop rules, run history and rule-run proposals of a removed mail account.
    pub fn purge_rules_account(&mut self, account: &str) -> StoreResult<()> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("DELETE FROM rules WHERE account=?1", params![account])?;
        tx.execute("DELETE FROM rule_runs WHERE account=?1", params![account])?;
        tx.execute("DELETE FROM rule_moved WHERE account=?1", params![account])?;
        tx.execute("DELETE FROM rule_colour_log WHERE account=?1", params![account])?;
        tx.execute("DELETE FROM proposals WHERE conv_id=0 AND id IN (SELECT proposal_id FROM proposal_items WHERE account=?1)", params![account])?;
        tx.commit()?;
        Ok(())
    }

    /// Keep the run history short: rows older than `before` go.
    pub fn prune_rule_runs(&mut self, before: i64) -> StoreResult<usize> {
        self.conn_mut().execute("DELETE FROM rule_colour_log WHERE run_id IN (SELECT id FROM rule_runs WHERE ts < ?1)", params![before])?;
        self.conn_mut().execute("DELETE FROM rule_moved WHERE ts < ?1", params![before])?;
        Ok(self.conn_mut().execute("DELETE FROM rule_runs WHERE ts < ?1", params![before])?)
    }
}

// ------------------------------------------------------------------------------------------
// Mail access for the runner (implemented by the app; faked in tests)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMsg {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub message_id: Option<String>,
    pub facts: MsgFacts,
    pub seen: bool,
    pub flagged: bool,
    /// Unix seconds and size: with From and Subject they make the fallback fingerprint of a message without Message-ID.
    pub date: i64,
    pub size: u32,
}

/// Stable (FNV-1a) fingerprint of a message that has no Message-ID.
pub fn fingerprint(m: &RuleMsg) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in [m.facts.from.as_str(), &m.date.to_string(), m.facts.subject.as_str(), &m.size.to_string()] {
        for b in part.bytes().chain(std::iter::once(0x1f)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

/// Read-only access to the local cache. Never touches the network.
pub trait RuleMail: Send + Sync {
    fn folders(&self, account: &str) -> Result<Vec<String>, String>;
    /// Lowercase role name (`junk`, `trash`, `inbox`, ...).
    fn folder_role(&self, account: &str, folder: &str) -> Result<Option<String>, String>;
    fn validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    fn max_uid(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    fn folder_exists(&self, account: &str, folder: &str) -> Result<bool, String>;
    /// Cached messages with UID greater than `after_uid`, ascending UID, at most `limit`.
    fn after(&self, account: &str, folder: &str, after_uid: u32, limit: u32) -> Result<Vec<RuleMsg>, String>;
    /// The newest cached messages (dry runs).
    fn recent(&self, account: &str, folder: &str, limit: u32) -> Result<Vec<RuleMsg>, String>;
    /// Local colour label of a message (0 = none). Default: labels unavailable.
    fn colour(&self, _account: &str, _message_id: &str) -> Result<u8, String> {
        Ok(0)
    }
    /// Set (1..=7) or clear (0) the local colour label. Local only; never touches a server.
    fn set_colour(&self, _account: &str, _message_id: &str, _colour: u8) -> Result<(), String> {
        Err("Colour labels are not available.".into())
    }
}

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn db<E>(_: E) -> String {
    "MiraMail couldn't read or write its rules database.".to_string()
}

fn skip_role(role: &Option<String>) -> bool {
    matches!(role.as_deref(), Some("junk") | Some("trash"))
}

// ------------------------------------------------------------------------------------------
// Dry run
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DryRow {
    pub folder: String,
    pub from: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DryRun {
    /// Messages looked at (the newest cached ones in the rule's folders).
    pub scanned: u32,
    pub matched: u32,
    pub sample: Vec<DryRow>,
    /// "Apply to existing" preview only: the exact `(folder, uid, uidvalidity)` set that matched.
    pub targets: Vec<Target>,
    /// Identifies `targets` (derived from them); pass both back to [`RuleRunner::apply_targets`].
    pub token: Option<String>,
}

/// One message a preview matched: `(folder, uid, uidvalidity)`.
pub type Target = (String, u32, u32);

/// Token naming a preview's target set (changes whenever the set does).
pub fn targets_token(rule_id: i64, targets: &[Target]) -> String {
    use std::hash::{Hash, Hasher};
    let mut sorted: Vec<&Target> = targets.iter().collect();
    sorted.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rule_id.hash(&mut h);
    sorted.hash(&mut h);
    format!("{:016x}-{}", h.finish(), targets.len())
}

struct Found {
    folder: String,
    v: u32,
    msgs: Vec<RuleMsg>,
}

// ------------------------------------------------------------------------------------------
// Runner
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunReport {
    pub run_id: i64,
    pub rule_id: i64,
    pub rule_name: String,
    pub folder: String,
    pub matched: usize,
    pub summary: String,
    pub proposal_ids: Vec<i64>,
    /// `(account, folder)` pairs whose contents changed.
    pub affected: Vec<(String, String)>,
    /// UIDs (in `folder`) of the messages this run moved away.
    pub moved_uids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunUndo {
    pub undone: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub affected: Vec<(String, String)>,
}

pub struct RuleRunner {
    store: Arc<Mutex<Store>>,
    mail: Arc<dyn RuleMail>,
    exec: Executor,
}

impl RuleRunner {
    pub fn new(store: Arc<Mutex<Store>>, mail: Arc<dyn RuleMail>, exec: Executor) -> Self {
        RuleRunner { store, mail, exec }
    }

    /// Preview: how many of the newest cached messages in the rule's folders match, plus a sample.
    pub fn dry_run(&self, account: &str, spec: &RuleSpec) -> Result<DryRun, String> {
        self.scan_existing(account, spec, DRY_RUN_SCAN)
    }

    /// Preview for "Apply to existing messages": like [`Self::dry_run`] but over the same (larger) window
    /// [`Self::apply_existing`] acts on, so the count the user confirms is the count that gets changed.
    /// The matched set comes back as `targets` with a `token`; [`Self::apply_targets`] then changes exactly those
    /// messages (re-checking each) and nothing else. Both use the same filters (UIDVALIDITY, recently-moved guard,
    /// step budget) through [`Self::collect_existing`].
    pub fn preview_existing(&self, account: &str, spec: &RuleSpec, now: i64) -> Result<DryRun, String> {
        let (scanned, found) = self.collect_existing(account, spec, now, None)?;
        let mut out = DryRun { scanned, ..Default::default() };
        for f in &found {
            for m in &f.msgs {
                out.matched += 1;
                out.targets.push((f.folder.clone(), m.uid, m.uidvalidity));
                if out.sample.len() < DRY_RUN_SAMPLE {
                    out.sample.push(DryRow { folder: f.folder.clone(), from: clean_line(&m.facts.from, 80), subject: clean_line(&m.facts.subject, 100) });
                }
            }
        }
        Ok(out)
    }

    /// The single scan behind both the "Apply to existing" preview and its execution. With `allow`, only messages
    /// of that set can come out (each still has to match, be in the same UIDVALIDITY and not have been moved by a
    /// rule lately).
    fn collect_existing(&self, account: &str, spec: &RuleSpec, now: i64, allow: Option<&HashSet<Target>>) -> Result<(u32, Vec<Found>), String> {
        let (mut scanned, mut steps) = (0u32, 0u64);
        let mut out = Vec::new();
        for folder in self.mail.folders(account)? {
            if !could_apply(spec, &folder) || skip_role(&self.mail.folder_role(account, &folder)?) {
                continue;
            }
            let Some(v) = self.mail.validity(account, &folder)? else { continue };
            let mut msgs: Vec<RuleMsg> = Vec::new();
            for m in self.mail.recent(account, &folder, APPLY_SCAN)? {
                scanned += 1;
                if steps > PASS_STEP_BUDGET {
                    break;
                }
                if m.uidvalidity != v || !matches_b(spec, &m.facts, &mut steps) || self.moved_recently(account, &m, now)? {
                    continue;
                }
                if allow.is_some_and(|a| !a.contains(&(folder.clone(), m.uid, m.uidvalidity))) {
                    continue;
                }
                msgs.push(m);
            }
            if !msgs.is_empty() {
                msgs.sort_by_key(|m| m.uid);
                out.push(Found { folder, v, msgs });
            }
        }
        Ok((scanned, out))
    }

    fn scan_existing(&self, account: &str, spec: &RuleSpec, scan: u32) -> Result<DryRun, String> {
        let mut out = DryRun::default();
        for folder in self.mail.folders(account)? {
            if !could_apply(spec, &folder) || skip_role(&self.mail.folder_role(account, &folder)?) {
                continue;
            }
            for m in self.mail.recent(account, &folder, scan)? {
                out.scanned += 1;
                if matches(spec, &m.facts) {
                    out.matched += 1;
                    if out.sample.len() < DRY_RUN_SAMPLE {
                        out.sample.push(DryRow { folder: folder.clone(), from: clean_line(&m.facts.from, 80), subject: clean_line(&m.facts.subject, 100) });
                    }
                }
            }
        }
        Ok(out)
    }

    /// Start a rule's cursors at "now" in every folder it covers, so only mail arriving from here on is
    /// processed. Call when a rule is enabled.
    pub fn baseline(&self, rule_id: i64) -> Result<(), String> {
        let Some(rule) = lock(&self.store).get_rule(rule_id).map_err(db)? else { return Ok(()) };
        let Some(spec) = rule.spec else { return Ok(()) };
        for folder in self.mail.folders(&rule.account)? {
            if !could_apply(&spec, &folder) || skip_role(&self.mail.folder_role(&rule.account, &folder)?) {
                continue;
            }
            let Some(v) = self.mail.validity(&rule.account, &folder)? else { continue };
            let max = self.mail.max_uid(&rule.account, &folder)?.unwrap_or(0);
            lock(&self.store).set_cursor(rule.id, &rule.account, &folder, Cursor { uidvalidity: v, last_uid: max }).map_err(db)?;
        }
        Ok(())
    }

    /// Look at the new mail of one folder (after it was synced) and run every enabled rule over it.
    /// Never touches Junk or Trash. While rules are paused nothing runs and the cursors skip ahead (mail that
    /// arrives during a pause is not processed later).
    pub async fn run_folder(&self, account: &str, folder: &str, now: i64) -> Result<Vec<RunReport>, String> {
        if skip_role(&self.mail.folder_role(account, folder)?) {
            return Ok(vec![]);
        }
        let Some(v) = self.mail.validity(account, folder)? else { return Ok(vec![]) };
        let (paused, rules) = {
            let mut st = lock(&self.store);
            (st.rules_paused().map_err(db)?, st.list_rules(Some(account)).map_err(db)?)
        };
        let max = self.mail.max_uid(account, folder)?.unwrap_or(0);
        let mut claimed: HashSet<u32> = HashSet::new();
        let mut reports = Vec::new();
        let mut pending: Vec<(Rule, RuleSpec, Vec<RuleMsg>, bool, u32)> = Vec::new();
        let mut steps: u64 = 0;
        for rule in rules.into_iter().filter(|r| r.enabled) {
            let Some(spec) = rule.spec.clone() else { continue };
            if !could_apply(&spec, folder) {
                continue;
            }
            let cur = lock(&self.store).get_cursor(rule.id, account, folder).map_err(db)?;
            let last = match cur {
                Some(c) if c.uidvalidity == v => c.last_uid,
                // Never seen, or the folder was reset on the server (UIDVALIDITY changed): start from "now".
                _ => {
                    lock(&self.store).set_cursor(rule.id, account, folder, Cursor { uidvalidity: v, last_uid: max }).map_err(db)?;
                    continue;
                }
            };
            if paused {
                if max > last {
                    lock(&self.store).set_cursor(rule.id, account, folder, Cursor { uidvalidity: v, last_uid: max }).map_err(db)?;
                }
                continue;
            }
            let dest_rule = spec.actions.iter().any(Action::is_destination);
            let mut matched: Vec<RuleMsg> = Vec::new();
            let mut cursor = last;
            'batches: for _ in 0..MAX_BATCHES {
                let batch = self.mail.after(account, folder, cursor, BATCH)?;
                let full = batch.len() as u32 >= BATCH;
                for m in &batch {
                    if steps > PASS_STEP_BUDGET {
                        break 'batches; // the rest waits for the next pass (the cursor stops before it)
                    }
                    cursor = cursor.max(m.uid);
                    if m.uidvalidity != v || (dest_rule && claimed.contains(&m.uid)) || !matches_b(&spec, &m.facts, &mut steps) {
                        continue;
                    }
                    if self.moved_recently(account, m, now)? {
                        continue;
                    }
                    matched.push(m.clone());
                }
                if !full {
                    break;
                }
            }
            // Advance BEFORE acting: a crash in the middle never makes a message run twice.
            if cursor > last {
                lock(&self.store).set_cursor(rule.id, account, folder, Cursor { uidvalidity: v, last_uid: cursor }).map_err(db)?;
            }
            if matched.is_empty() {
                continue;
            }
            if dest_rule {
                claimed.extend(matched.iter().map(|m| m.uid));
            }
            pending.push((rule, spec, matched, dest_rule, last));
        }
        // Rules that only change flags run first: once another rule has moved a message it is no longer here.
        pending.sort_by_key(|p| p.3);
        let mut queue: std::collections::VecDeque<_> = pending.into();
        while let Some((rule, spec, matched, _, before)) = queue.pop_front() {
            match self.execute(&rule, &spec, account, folder, v, matched, now).await {
                Ok((rep, retry)) => {
                    if let Some(from_uid) = retry {
                        // Nothing was done (the executor was busy): look at these messages again next pass.
                        self.rewind(&rule, account, folder, v, from_uid.max(before));
                    }
                    reports.push(rep);
                }
                Err(e) => {
                    // A failure before anything ran: do not lose this rule's (or the queued rules') mail.
                    self.rewind(&rule, account, folder, v, before);
                    for (r, _, _, _, b) in &queue {
                        self.rewind(r, account, folder, v, *b);
                    }
                    return Err(e);
                }
            }
        }
        Ok(reports)
    }

    /// Run one rule over the messages ALREADY in the local cache (newest [`APPLY_SCAN`] per folder it covers).
    /// Explicit user action: works for a rule that is switched off, refuses while rules are paused, leaves the
    /// rule's cursors alone. Every folder with matches becomes one logged run that can be undone.
    pub async fn apply_existing(&self, rule_id: i64, now: i64) -> Result<Vec<RunReport>, String> {
        self.apply_set(rule_id, None, now).await
    }

    /// Apply the rule to exactly the messages a preview returned (`targets`), after checking `token` belongs to them.
    /// Each message is re-checked; one that no longer matches, moved, or whose folder was reset is left alone.
    pub async fn apply_targets(&self, rule_id: i64, targets: &[Target], token: &str, now: i64) -> Result<Vec<RunReport>, String> {
        if token != targets_token(rule_id, targets) {
            return Err("The preview is out of date. Preview again, then apply.".into());
        }
        let allow: HashSet<Target> = targets.iter().cloned().collect();
        self.apply_set(rule_id, Some(allow), now).await
    }

    async fn apply_set(&self, rule_id: i64, allow: Option<HashSet<Target>>, now: i64) -> Result<Vec<RunReport>, String> {
        let (rule, paused) = {
            let mut st = lock(&self.store);
            (st.get_rule(rule_id).map_err(db)?.ok_or_else(|| "That rule no longer exists.".to_string())?, st.rules_paused().map_err(db)?)
        };
        if paused {
            return Err("Rules are paused. Turn \"Pause all rules\" off first.".into());
        }
        let spec = rule.spec.clone().ok_or_else(|| "This rule is damaged. Edit and save it first.".to_string())?;
        let account = rule.account.clone();
        let (_, found) = self.collect_existing(&account, &spec, now, allow.as_ref())?;
        let mut reports = Vec::new();
        for f in found {
            let (rep, _) = self.execute(&rule, &spec, &account, &f.folder, f.v, f.msgs, now).await?;
            reports.push(rep);
        }
        Ok(reports)
    }

    fn rewind(&self, rule: &Rule, account: &str, folder: &str, v: u32, to: u32) {
        let mut st = lock(&self.store);
        if let Ok(Some(c)) = st.get_cursor(rule.id, account, folder) {
            if c.uidvalidity == v && c.last_uid > to {
                let _ = st.set_cursor(rule.id, account, folder, Cursor { uidvalidity: v, last_uid: to });
            }
        }
    }

    /// Did a rule move this message lately? By Message-ID, else by fingerprint.
    fn moved_recently(&self, account: &str, m: &RuleMsg, now: i64) -> Result<bool, String> {
        let since = now - MOVE_GUARD_SECS;
        match m.message_id.as_deref().filter(|s| !s.is_empty()) {
            Some(mid) => lock(&self.store).rule_moved_recently(account, mid, since).map_err(db),
            None => lock(&self.store).rule_moved_fp_recently(account, &fingerprint(m), since).map_err(db),
        }
    }

    /// Create and approve one logged step (a proposal under the hidden system conversation) and run it.
    async fn step(&self, np: NewProposal, scopes: &[Scope], now: i64) -> Result<(i64, Result<proposals::ExecReport, ProposalError>), String> {
        let idxs: Vec<i64> = (0..np.items.len() as i64).collect();
        let pid = lock(&self.store).create_proposal(&np, now).map_err(db)?;
        Ok((pid, self.exec.approve(pid, &idxs, scopes, &|_| {}).await))
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute(&self, rule: &Rule, spec: &RuleSpec, account: &str, folder: &str, _v: u32, matched: Vec<RuleMsg>, now: i64) -> Result<(RunReport, Option<u32>), String> {
        let reason = format!("Rule: {}", clean_line(&rule.name, MAX_NAME_CHARS));
        let src_scope = vec![Scope { account: account.to_string(), folder: folder.to_string() }];
        let mut pieces: Vec<String> = Vec::new();
        let mut proposal_ids = Vec::new();
        let mut affected: Vec<(String, String)> = Vec::new();
        let item = |m: &RuleMsg, f: &str, uid: u32, validity: u32, to: &str| NewItem {
            account: account.to_string(),
            folder: f.to_string(),
            uid,
            uidvalidity: validity,
            sender: clean_line(&m.facts.from, 120),
            subject: clean_line(&m.facts.subject, 200),
            seen: m.seen,
            flagged: m.flagged,
            to_folder: to.to_string(),
        };
        // Where each matched message is now: (message, folder, uid, uidvalidity). Starts at the source.
        let mut now_at: Vec<(RuleMsg, String, u32, u32)> = matched.iter().map(|m| (m.clone(), folder.to_string(), m.uid, m.uidvalidity)).collect();
        let mut flag_scopes = src_scope.clone();
        // Busy / did anything succeed: a run where the executor was busy and nothing else happened is retried.
        let (mut busy, mut any_ok) = (false, false);
        let mut moved_uids: Vec<u32> = Vec::new();
        let mut moved_msgs: Vec<RuleMsg> = Vec::new();

        // 1. The move first: the flag steps then address the messages in their new place, so undoing the
        //    steps newest-first is consistent (flags are reverted while the message is still there).
        if let Some(a) = spec.actions.iter().find(|a| a.is_destination()) {
            let (kind, params, label) = match a {
                Action::Archive => (Kind::Archive, Params::default(), String::new()),
                Action::Trash => (Kind::Trash, Params::default(), String::new()),
                Action::MoveTo { folder: f } => {
                    if rule.origin == "ai" && self.mail.folder_role(account, f)?.as_deref() == Some("inbox") {
                        pieces.push("a rule written by the assistant may not move mail to the Inbox, nothing moved".into());
                        now_at.clear();
                        (Kind::Move, Params::default(), String::new())
                    } else if !self.mail.folder_exists(account, f)? {
                        pieces.push(format!("folder \"{}\" does not exist, nothing moved", clean_line(f, 60)));
                        now_at.clear();
                        (Kind::Move, Params::default(), String::new())
                    } else {
                        (Kind::Move, Params { dest_account: Some(account.to_string()), dest_folder: Some(f.clone()), ..Params::default() }, f.clone())
                    }
                }
                _ => unreachable!("is_destination"),
            };
            if !now_at.is_empty() {
                let items: Vec<NewItem> = matched.iter().map(|m| item(m, folder, m.uid, m.uidvalidity, &label)).collect();
                let title = proposals::title_for(kind, &params, items.len(), &clean_line(&label, 80));
                let np = NewProposal { conv_id: 0, kind, params, title, reason: reason.clone(), warnings: vec![], items };
                // The rule itself (the user's own automation, not a chat) names its destination, so it is allowed
                // next to the source folder; the executor now checks the destination against the scope it is given.
                let mut move_scope = src_scope.clone();
                if let Some(d) = np.params.dest_folder.clone().filter(|_| kind == Kind::Move) {
                    move_scope.push(Scope { account: account.to_string(), folder: d });
                }
                let (pid, res) = self.step(np, &move_scope, now).await?;
                proposal_ids.push(pid);
                now_at.clear();
                match res {
                    Ok(rep) => {
                        any_ok = true;
                        affected.extend(rep.affected.iter().cloned());
                        let n = rep.ok + rep.unverified;
                        let what = match a {
                            Action::Archive => format!("{n} archived"),
                            Action::Trash => format!("{n} moved to Trash"),
                            Action::MoveTo { folder: f } => format!("{n} moved to {}", clean_line(f, 60)),
                            _ => String::new(),
                        };
                        let bad = rep.failed + rep.skipped;
                        pieces.push(if bad > 0 { format!("{what}, {bad} could not be changed") } else { what });
                        // New coordinates of the messages that really moved (from the undo log).
                        let log = lock(&self.store).list_log(pid).map_err(db)?;
                        for row in log.iter().filter(|r| r.op == "move" && r.outcome == "ok") {
                            let (Some(src_uid), Some(dest_uid)) = (row.src_uid, row.dest_uid) else { continue };
                            let Some(m) = matched.iter().find(|m| m.uid == src_uid) else { continue };
                            moved_uids.push(src_uid);
                            moved_msgs.push(m.clone());
                            if !flag_scopes.iter().any(|s| s.folder == row.dest_folder) {
                                flag_scopes.push(Scope { account: account.to_string(), folder: row.dest_folder.clone() });
                            }
                            now_at.push((m.clone(), row.dest_folder.clone(), dest_uid, row.dest_validity.unwrap_or(0)));
                        }
                    }
                    Err(ProposalError::Busy) => {
                        busy = true;
                        pieces.push("skipped: another change was running, will retry".into());
                    }
                    Err(e) => pieces.push(format!("could not run: {e}")),
                }
            }
        }

        // 2. Flag steps, on the messages where they are now.
        for a in spec.actions.iter().filter(|a| matches!(a, Action::MarkRead | Action::Flag)) {
            let (op, label) = match a {
                Action::MarkRead => (FlagOp::Read, "marked read"),
                _ => (FlagOp::Flagged, "flagged"),
            };
            let items: Vec<NewItem> = now_at
                .iter()
                .filter(|(m, ..)| if op == FlagOp::Read { !m.seen } else { !m.flagged })
                .map(|(m, f, uid, v)| item(m, f, *uid, *v, ""))
                .collect();
            if items.is_empty() {
                if !now_at.is_empty() {
                    pieces.push(format!("0 {label}"));
                }
                continue;
            }
            let params = Params { flag: Some(op), ..Params::default() };
            let title = proposals::title_for(Kind::Flag, &params, items.len(), "");
            let np = NewProposal { conv_id: 0, kind: Kind::Flag, params, title, reason: reason.clone(), warnings: vec![], items };
            let (pid, res) = self.step(np, &flag_scopes, now).await?;
            proposal_ids.push(pid);
            match res {
                Ok(rep) => {
                    any_ok = true;
                    affected.extend(rep.affected.iter().cloned());
                    let n = rep.ok + rep.unverified;
                    let bad = rep.failed + rep.skipped;
                    pieces.push(if bad > 0 { format!("{n} {label}, {bad} could not be changed") } else { format!("{n} {label}") });
                }
                Err(ProposalError::Busy) => {
                    busy = true;
                    pieces.push("skipped: another change was running, will retry".into());
                }
                Err(e) => pieces.push(format!("could not run: {e}")),
            }
        }
        // 3. Colour label (local only, no server call). The run row and one log row per planned change are written
        // BEFORE any colour is changed, so a crash in the middle can still be undone; the summary is finished after.
        let mut colour_plan: Vec<(String, u8, u8)> = Vec::new();
        let mut colour_skipped = 0usize;
        let colour_action = spec.actions.iter().find_map(|a| if let Action::Colour { colour } = a { Some(*colour) } else { None });
        if let Some(colour) = colour_action {
            for (m, ..) in &now_at {
                let Some(mid) = m.message_id.as_deref().filter(|s| !s.is_empty()) else {
                    colour_skipped += 1;
                    continue;
                };
                match self.mail.colour(account, mid) {
                    Ok(prev) if prev == colour => {}
                    Ok(prev) => colour_plan.push((mid.to_string(), prev, colour)),
                    Err(_) => colour_skipped += 1,
                }
            }
        }
        affected.sort();
        affected.dedup();
        let summary_so_far = if pieces.is_empty() { "No change was needed.".to_string() } else { pieces.join("; ") };
        let run = RuleRun { id: 0, rule_id: rule.id, rule_name: rule.name.clone(), account: account.to_string(), folder: folder.to_string(), ts: now, matched: matched.len() as u32, summary: summary_so_far, proposal_ids: proposal_ids.clone() };
        let run_id = lock(&self.store).add_rule_run(&run).map_err(db)?;
        for (mid, prev, new) in &colour_plan {
            lock(&self.store).add_colour_log(run_id, account, mid, *prev, *new).map_err(db)?;
        }
        if let Some(colour) = colour_action {
            let logged = lock(&self.store).pending_colour_log(run_id).map_err(db)?;
            let (mut done, mut skipped) = (0usize, colour_skipped);
            for (mid, _, _) in &colour_plan {
                match self.mail.set_colour(account, mid, colour) {
                    Ok(()) => done += 1,
                    Err(_) => {
                        skipped += 1;
                        // Nothing changed for this one: its log row must not be "undone" later.
                        if let Some((row, ..)) = logged.iter().find(|(_, _, m, _, _)| m == mid) {
                            let _ = lock(&self.store).mark_colour_undone(*row);
                        }
                    }
                }
            }
            if done > 0 {
                any_ok = true;
            }
            pieces.push(if skipped > 0 { format!("{done} labelled, {skipped} could not be labelled") } else { format!("{done} labelled") });
        }
        let summary = if pieces.is_empty() { "No change was needed.".to_string() } else { pieces.join("; ") };
        lock(&self.store).update_rule_run_summary(run_id, &summary).map_err(db)?;
        for m in moved_msgs.iter().filter(|m| m.message_id.as_deref().is_none_or(str::is_empty)) {
            lock(&self.store).add_rule_moved_fp(account, &fingerprint(m), run_id, now).map_err(db)?;
        }
        let retry = (busy && !any_ok).then_some(0);
        Ok((RunReport { run_id, rule_id: rule.id, rule_name: rule.name.clone(), folder: folder.to_string(), matched: matched.len(), summary, proposal_ids, affected, moved_uids }, retry))
    }

    /// Undo one whole run: its steps are reversed newest first. Safe to call again (retries what failed).
    pub async fn undo_run(&self, run_id: i64) -> Result<RunUndo, String> {
        let run = lock(&self.store).get_rule_run(run_id).map_err(db)?.ok_or_else(|| "That run is no longer in the log.".to_string())?;
        let mut out = RunUndo::default();
        let mut any = false;
        for pid in run.proposal_ids.iter().rev() {
            match self.exec.undo(*pid, &|_| {}).await {
                Ok(rep) => {
                    any = true;
                    out.undone += rep.undone;
                    out.failed += rep.failed;
                    for e in rep.errors {
                        if out.errors.len() < 5 && !out.errors.contains(&e) {
                            out.errors.push(e);
                        }
                    }
                    out.affected.extend(rep.affected);
                }
                Err(ProposalError::NothingToUndo) | Err(ProposalError::Illegal { .. }) => {}
                Err(e) => {
                    out.failed += 1;
                    let e = e.to_string();
                    if out.errors.len() < 5 && !out.errors.contains(&e) {
                        out.errors.push(e);
                    }
                }
            }
        }
        let colours = lock(&self.store).pending_colour_log(run_id).map_err(db)?;
        for (row, account, mid, prev, new) in colours {
            any = true;
            // A colour the user changed by hand since the run is theirs: leave it.
            if self.mail.colour(&account, &mid).is_ok_and(|now| now == new) {
                match self.mail.set_colour(&account, &mid, prev) {
                    Ok(()) => out.undone += 1,
                    Err(e) => {
                        out.failed += 1;
                        if out.errors.len() < 5 && !out.errors.contains(&e) {
                            out.errors.push(e);
                        }
                        continue;
                    }
                }
            }
            let _ = lock(&self.store).mark_colour_undone(row);
        }
        if any {
            let _ = lock(&self.store).clear_rule_moved_run(run_id);
        }
        if !any && out.failed == 0 {
            return Err("Nothing in this run can be undone (it was already undone, or it changed nothing).".into());
        }
        out.affected.sort();
        out.affected.dedup();
        Ok(out)
    }
}

// ------------------------------------------------------------------------------------------
// Unit tests (pure parts; runner tests are in tests/rules_runner.rs)
// ------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(field: Field, op: Op, value: &str) -> Condition {
        Condition { field, op, value: value.into() }
    }
    fn spec(conds: Vec<Condition>, actions: Vec<Action>) -> RuleSpec {
        RuleSpec { v: 1, mode: MatchMode::All, conditions: conds, actions }
    }
    fn facts() -> MsgFacts {
        MsgFacts {
            from: "Shop <orders@mail.shop.example>".into(),
            to: "me@example.org".into(),
            cc: "Al <al@x.test>, bo@y.test".into(),
            subject: "Your receipt #42".into(),
            list_id: "Shop news <news.shop.example>".into(),
            has_list_unsubscribe: true,
            has_attachment: false,
            size: 0,
            folder: "INBOX".into(),
        }
    }

    #[test]
    fn ops_and_fields() {
        let f = facts();
        let t = |c: Condition| condition_matches(&c, &f);
        assert!(t(cond(Field::From, Op::Equals, "orders@mail.shop.example")));
        assert!(t(cond(Field::From, Op::Equals, "ORDERS@MAIL.SHOP.EXAMPLE")));
        assert!(!t(cond(Field::From, Op::Equals, "mail.shop.example")));
        assert!(t(cond(Field::From, Op::DomainIs, "shop.example")), "subdomain counts");
        assert!(t(cond(Field::From, Op::DomainIs, "mail.shop.example")));
        assert!(!t(cond(Field::From, Op::DomainIs, "hop.example")), "dot boundary");
        assert!(!t(cond(Field::From, Op::DomainIs, "example.org")));
        assert!(t(cond(Field::Cc, Op::Equals, "bo@y.test")));
        assert!(t(cond(Field::Cc, Op::DomainIs, "x.test")));
        assert!(t(cond(Field::Subject, Op::Contains, "RECEIPT")));
        assert!(t(cond(Field::Subject, Op::Glob, "your receipt #*")));
        assert!(t(cond(Field::Subject, Op::Equals, "your receipt #42")));
        assert!(t(cond(Field::ListId, Op::Equals, "news.shop.example")));
        assert!(t(cond(Field::HasListUnsubscribe, Op::Equals, "true")));
        assert!(!t(cond(Field::HasListUnsubscribe, Op::Equals, "false")));
        assert!(t(cond(Field::Folder, Op::Equals, "inbox")));
        assert!(!t(cond(Field::Folder, Op::Equals, "Archive")));
    }

    #[test]
    fn quoted_display_names_cannot_fake_an_address() {
        let f = MsgFacts { from: "\"boss@corp.example\" <evil@bad.example>".into(), ..facts() };
        assert!(!condition_matches(&cond(Field::From, Op::Equals, "boss@corp.example"), &f));
        assert!(!condition_matches(&cond(Field::From, Op::DomainIs, "corp.example"), &f));
        assert!(condition_matches(&cond(Field::From, Op::DomainIs, "bad.example"), &f));
        assert_eq!(addresses_of("a@x.y, \"Q, R\" <q@z.w>; (c) c@d.e"), vec!["a@x.y", "q@z.w", "c@d.e"]);
    }

    #[test]
    fn mode_and_folder_scoping() {
        let f = facts();
        let mut s = spec(vec![cond(Field::From, Op::DomainIs, "shop.example"), cond(Field::Subject, Op::Contains, "nope")], vec![Action::Archive]);
        assert!(!matches(&s, &f));
        s.mode = MatchMode::Any;
        assert!(matches(&s, &f));
        // no folder condition = INBOX only
        let other = MsgFacts { folder: "Archive".into(), ..f.clone() };
        assert!(!matches(&s, &other));
        s.mode = MatchMode::All;
        s.conditions = vec![cond(Field::From, Op::DomainIs, "shop.example"), cond(Field::Folder, Op::Equals, "Archive")];
        assert!(matches(&s, &other) && !matches(&s, &f));
        assert!(could_apply(&s, "Archive") && !could_apply(&s, "INBOX"));
    }

    #[test]
    fn validation_accepts_a_good_rule_and_normalises() {
        let s = spec(vec![cond(Field::From, Op::DomainIs, " Shop.Example "), cond(Field::From, Op::Equals, "A@B.C")], vec![Action::MoveTo { folder: " Receipts ".into() }, Action::MarkRead]);
        let v = validate(&s).unwrap();
        assert_eq!(v.conditions[0].value, "shop.example");
        assert_eq!(v.conditions[1].value, "a@b.c");
        assert_eq!(v.actions[0], Action::MoveTo { folder: "Receipts".into() });
    }

    #[test]
    fn validation_rejects_bad_rules() {
        let ok = || spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Archive]);
        assert!(validate(&ok()).is_ok());
        let bad: Vec<RuleSpec> = vec![
            RuleSpec { v: 2, ..ok() },
            RuleSpec { conditions: vec![], ..ok() },
            RuleSpec { actions: vec![], ..ok() },
            spec(vec![cond(Field::Subject, Op::Contains, "")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, "  ")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, "a\u{202e}b")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, "a\nb")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, &"x".repeat(201))], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::DomainIs, "x.com")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Glob, "*")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Glob, "*?*")], vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Glob, "*a*b*c*d*e*")], vec![Action::Archive]),
            spec(vec![cond(Field::From, Op::Equals, "not an address")], vec![Action::Archive]),
            spec(vec![cond(Field::From, Op::DomainIs, "bad domain")], vec![Action::Archive]),
            spec(vec![cond(Field::From, Op::DomainIs, "a..b")], vec![Action::Archive]),
            spec(vec![cond(Field::HasListUnsubscribe, Op::Contains, "true")], vec![Action::Archive]),
            spec(vec![cond(Field::HasListUnsubscribe, Op::Equals, "maybe")], vec![Action::Archive]),
            spec(vec![cond(Field::Folder, Op::Equals, "INBOX")], vec![Action::Archive]),
            spec((0..11).map(|_| cond(Field::Subject, Op::Contains, "x")).collect(), vec![Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Archive, Action::Trash]),
            spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Archive, Action::Archive]),
            spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::MarkRead, Action::Flag, Action::Archive, Action::Trash]),
            spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::MoveTo { folder: "inbox".into() }]),
            spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::MoveTo { folder: "".into() }]),
        ];
        for (i, s) in bad.iter().enumerate() {
            assert!(validate(s).is_err(), "case {i} should be refused: {s:?}");
        }
        // folder conditions only in ALL mode
        let mut any = spec(vec![cond(Field::Subject, Op::Contains, "x"), cond(Field::Folder, Op::Equals, "A")], vec![Action::Archive]);
        any.mode = MatchMode::Any;
        assert!(validate(&any).is_err());
    }

    #[test]
    fn parse_refuses_unknown_actions_fields_and_oversize() {
        let good = r#"{"v":1,"match":"all","conditions":[{"field":"from","op":"domainIs","value":"shop.example"}],"actions":[{"type":"moveTo","folder":"Receipts"},{"type":"markRead"}]}"#;
        assert!(parse_spec(good).is_ok());
        for evil in [
            r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"delete"}]}"#,
            r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"forward","to":"a@b.c"}]}"#,
            r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"reply"}]}"#,
            r#"{"v":1,"conditions":[{"field":"subject","op":"regex","value":".*"}],"actions":[{"type":"archive"}]}"#,
            r#"{"v":1,"conditions":[{"field":"body","op":"contains","value":"x"}],"actions":[{"type":"archive"}]}"#,
            r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"archive","extra":1}]}"#,
            r#"{"v":1,"conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"type":"archive"}],"sendTo":"a@b.c"}"#,
            "not json",
            "",
        ] {
            assert!(parse_spec(evil).is_err(), "{evil}");
        }
        let huge = format!(r#"{{"v":1,"conditions":[{{"field":"subject","op":"contains","value":"{}"}}],"actions":[{{"type":"archive"}}]}}"#, "x".repeat(MAX_SPEC_BYTES));
        assert!(parse_spec(&huge).unwrap_err().contains("too large"));
    }

    #[test]
    fn glob_text_is_clipped_and_steps_are_counted() {
        let mut steps = 0u64;
        let long = format!("{}z", "a".repeat(5000));
        assert!(!glob_ci("*z", &long, &mut steps), "the tail beyond the clip is never seen");
        assert!(steps <= (MAX_GLOB_TEXT_CHARS * 3) as u64, "{steps}");
        let c = cond(Field::Subject, Op::Glob, "*a*b");
        let f = MsgFacts { subject: "aaaa".into(), ..facts() };
        let mut s2 = 0;
        assert!(!condition_matches_b(&c, &f, &mut s2) && s2 > 0);
    }

    #[test]
    fn a_rule_may_not_move_mail_into_the_folder_it_watches() {
        let mv = |f: &str| vec![Action::MoveTo { folder: f.into() }];
        let sub = cond(Field::Subject, Op::Contains, "x");
        let watch = |op, v: &str| cond(Field::Folder, op, v);
        assert!(validate(&spec(vec![sub.clone(), watch(Op::Equals, "Receipts")], mv("receipts"))).is_err());
        assert!(validate(&spec(vec![sub.clone(), watch(Op::Glob, "Rec*")], mv("Receipts"))).is_err());
        assert!(validate(&spec(vec![sub.clone(), watch(Op::Equals, "INBOX")], mv("Receipts"))).is_ok());
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let m = RuleMsg { account: "a".into(), folder: "INBOX".into(), uid: 1, uidvalidity: 1, message_id: None, facts: facts(), seen: false, flagged: false, date: 5, size: 9 };
        assert_eq!(fingerprint(&m), fingerprint(&RuleMsg { uid: 77, folder: "Other".into(), ..m.clone() }), "independent of UID and folder");
        assert_ne!(fingerprint(&m), fingerprint(&RuleMsg { date: 6, ..m.clone() }));
        assert_ne!(fingerprint(&m), fingerprint(&RuleMsg { size: 10, ..m.clone() }));
    }

    #[test]
    fn glob_semantics() {
        let g = |p: &str, t: &str| glob_ci(p, t, &mut 0);
        assert!(g("a*c", "abc") && g("a*c", "ac") && g("a?c", "abc") && !g("a?c", "ac"));
        assert!(g("*news*", "Daily NEWS digest") && g("*", "") && !g("a", ""));
        assert!(g("a*b*c", "a--b--c") && !g("a*b*c", "a--b--"));
        assert!(!g("abc", "abcd") && !g("abcd", "abc"));
        assert!(g("Ünï*", "ünïcode"), "case folding is unicode-aware");
    }

    /// Adversarial pattern and text: the work is bounded by text * (pattern + 1) loop steps.
    #[test]
    fn glob_worst_case_is_bounded() {
        let pat: Vec<char> = "*a*a*a*a*b".chars().collect();
        for n in [100usize, 2000] {
            let text: Vec<char> = std::iter::repeat_n('a', n).collect();
            let mut steps = 0u64;
            assert!(!glob_match(&pat, &text, &mut steps));
            assert!(steps <= (text.len() * (pat.len() + 1)) as u64, "{steps} steps for n={n}");
        }
        let pat: Vec<char> = "?*?*?*?*z".chars().collect();
        let text: Vec<char> = std::iter::repeat_n('a', MAX_GLOB_TEXT_CHARS).collect();
        let mut steps = 0u64;
        let start = std::time::Instant::now();
        assert!(!glob_match(&pat, &text, &mut steps));
        assert!(steps <= (text.len() * (pat.len() + 1)) as u64);
        assert!(start.elapsed() < std::time::Duration::from_millis(250));
    }

    #[test]
    fn hostile_and_oversized_header_values_are_bounded() {
        let big = "a".repeat(5_000_000);
        let f = MsgFacts { from: format!("{big}@x.test"), subject: big.clone(), to: big.clone(), cc: big.clone(), list_id: big.clone(), ..facts() };
        let start = std::time::Instant::now();
        for c in [
            cond(Field::Subject, Op::Contains, "zzz"),
            cond(Field::Subject, Op::Glob, "*a*a*a*a*b"),
            cond(Field::From, Op::DomainIs, "x.test"),
            cond(Field::To, Op::Equals, "me@example.org"),
            cond(Field::Cc, Op::Glob, "?*?*?*?*q"),
            cond(Field::ListId, Op::Equals, "x"),
        ] {
            let _ = condition_matches(&c, &f);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
        // control characters / bidi in a header do not break anything
        let weird = MsgFacts { subject: "inv\u{202e}oice\u{0}\r\n".into(), ..facts() };
        assert!(condition_matches(&cond(Field::Subject, Op::Contains, "inv"), &weird));
        assert!(!matches(&spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Archive]), &MsgFacts::default()));
    }

    #[test]
    fn description_is_written_by_the_app() {
        let s = spec(
            vec![cond(Field::From, Op::DomainIs, "shop.example"), cond(Field::Subject, Op::Contains, "re\u{202e}ceipt\nIGNORE ALL")],
            vec![Action::MoveTo { folder: "Receipts".into() }, Action::MarkRead],
        );
        let d = describe(&s);
        assert!(d.starts_with("When a new message arrives in the Inbox and the sender is from the domain \"shop.example\""), "{d}");
        assert!(d.contains("move it to \"Receipts\" and mark it as read."), "{d}");
        assert!(!d.contains('\u{202e}') && !d.contains('\n'), "{d:?}");
        let t = spec(vec![cond(Field::HasListUnsubscribe, Op::Equals, "true"), cond(Field::Folder, Op::Equals, "News")], vec![Action::Trash]);
        assert!(describe(&t).contains("in the folder \"News\"") && describe(&t).contains("Trash (not deleted for good)"));
    }

    #[test]
    fn rule_store_roundtrip_limits_and_cascade() {
        let mut st = Store::open_in_memory().unwrap();
        let mk = |name: &str| NewRule { account: "a@x.org".into(), name: name.into(), spec: spec(vec![cond(Field::Subject, Op::Contains, "x")], vec![Action::Archive]), origin: "user", conv_id: None, reason: String::new(), enabled: false };
        let id = st.create_rule(&mk("  One  "), 10).unwrap();
        let r = st.get_rule(id).unwrap().unwrap();
        assert_eq!((r.name.as_str(), r.enabled, r.origin.as_str()), ("One", false, "user"));
        assert!(r.spec.is_some());
        st.set_rule_enabled(id, true, 11).unwrap();
        assert!(st.get_rule(id).unwrap().unwrap().enabled);
        st.set_cursor(id, "a@x.org", "INBOX", Cursor { uidvalidity: 1, last_uid: 5 }).unwrap();
        assert_eq!(st.get_cursor(id, "a@x.org", "INBOX").unwrap(), Some(Cursor { uidvalidity: 1, last_uid: 5 }));
        assert!(st.create_rule(&mk(""), 1).is_err());
        assert!(st.update_rule(id, "Two", &spec(vec![], vec![Action::Archive]), 12).is_err());
        st.update_rule(id, "Two", &spec(vec![cond(Field::Subject, Op::Contains, "y")], vec![Action::Trash]), 12).unwrap();
        assert_eq!(st.get_rule(id).unwrap().unwrap().name, "Two");
        // a corrupt stored spec yields a rule that never runs
        st.conn_mut().execute("UPDATE rules SET spec_json='{\"v\":1,\"conditions\":[],\"actions\":[]}' WHERE id=?1", params![id]).unwrap();
        assert!(st.get_rule(id).unwrap().unwrap().spec.is_none());
        assert!(st.delete_rule(id).unwrap());
        assert_eq!(st.get_cursor(id, "a@x.org", "INBOX").unwrap(), None, "cursors cascade");
        for i in 0..MAX_RULES_PER_ACCOUNT {
            st.create_rule(&mk(&format!("r{i}")), 1).unwrap();
        }
        assert!(st.create_rule(&mk("one too many"), 1).unwrap_err().contains("at most"));
        assert!(!st.rules_paused().unwrap());
        st.set_rules_paused(true).unwrap();
        assert!(st.rules_paused().unwrap());
    }

    #[test]
    fn system_conversation_is_hidden_and_survives_purges() {
        let mut st = Store::open_in_memory().unwrap();
        assert!(st.list_conversations().unwrap().is_empty(), "the system conversation is never listed");
        assert!(st.get_conversation(0).unwrap().is_some());
        st.purge_now(&crate::store::PurgeSettings { egress_retention_days: Some(0), conversation_retention_days: Some(0), purge_on_account_removal: true }, 1_000_000).unwrap();
        st.purge_account("a@x.org").unwrap();
        st.purge_everything().unwrap();
        assert!(st.get_conversation(0).unwrap().is_some());
        assert!(!st.delete_conversation(0).unwrap(), "cannot be deleted");
        assert!(st.get_conversation(0).unwrap().is_some());
    }
}
