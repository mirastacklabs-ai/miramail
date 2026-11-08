//! Conversation threading, computed locally from cached headers (docs/ESSENTIALS_SPEC.md "Conversations").
//!
//! Nothing is stored: a thread key is derived on demand from `Message-ID` / `In-Reply-To` / `References`
//! plus a deliberately conservative subject fallback, so there is no schema change and a wrong guess can
//! never persist. The algorithm is a union-find over messages and the (possibly missing) ids they name,
//! so cycles, self-references, duplicates and out-of-order arrival are all harmless.
//!
//! Subject fallback (only after the header pass) joins two otherwise separate conversations when ALL hold:
//! the normalised subject is equal and not generic; at least one side starts with a reply/forward prefix
//! (two plain "Weekly update" newsletters never merge); their date ranges are within 30 days; and the sender
//! of one is a participant of the other (the account's own address never counts as shared).

use crate::error::Result;
use crate::search::normalize_subject;
use crate::storage::Storage;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

/// Subject fallback only joins conversations whose date ranges lie within this many seconds of each other.
pub const SUBJECT_WINDOW_SECS: i64 = 30 * 86_400;
/// Ids considered per message (hostile `References` headers are cut: the first one and the last ones).
const MAX_LINKS: usize = 30;
/// Conversations sharing one subject beyond this many are left alone (bounds the quadratic scan).
const MAX_GROUP: usize = 300;
/// Subjects too common to prove two conversations are the same one.
const GENERIC: &[&str] = &[
    "hi", "hello", "hey", "test", "thanks", "thank you", "question", "update", "no subject", "(no subject)", "invoice", "meeting", "follow up", "follow-up", "quick question", "fyi",
];

/// One cached message as far as threading is concerned.
#[derive(Debug, Clone, Default)]
pub struct Item {
    /// Message-ID without angle brackets; empty when absent.
    pub message_id: String,
    /// Space-separated ids.
    pub in_reply_to: String,
    /// Space-separated ids, oldest first.
    pub references: String,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
}

fn norm_id(s: &str) -> String {
    s.trim().trim_matches(['<', '>']).trim().to_ascii_lowercase()
}

/// Lower-cased addresses found in an address header (`Name <a@x>, b@y`).
///
/// Only the REAL address of each mailbox counts: a bracketed address must close its entry (be followed by the end,
/// a `,` or a `;`) and must not sit inside a quoted display name. Otherwise a sender could write
/// `From: "<boss@corp.com>" <evil@x.org>` and be taken for the boss (VIP, block list, thread joining).
pub fn addresses_of(h: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_quote = false;
    let mut escaped = false;
    let mut open: Option<usize> = None;
    for (i, c) in h.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quote => escaped = true,
            '"' if open.is_none() => in_quote = !in_quote,
            '<' if !in_quote && open.is_none() => open = Some(i),
            '>' if open.is_some() => {
                let a = open.take().unwrap_or(i);
                let inner = h[a + 1..i].trim();
                let closes_entry = matches!(h[i + 1..].trim_start().chars().next(), None | Some(',') | Some(';'));
                if closes_entry && inner.contains('@') && !inner.contains(char::is_whitespace) && !inner.contains(['<', '"']) {
                    out.push(inner.to_ascii_lowercase());
                }
            }
            _ => {}
        }
    }
    // A header with no angle brackets at all: bare addresses.
    if out.is_empty() && !h.contains('<') {
        for t in h.split([',', ';', ' ']) {
            let t = t.trim().trim_matches('"');
            if t.contains('@') && t.len() <= 320 {
                out.push(t.to_ascii_lowercase());
            }
        }
    }
    out
}

struct Dsu {
    p: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu { p: (0..n).collect() }
    }
    fn push(&mut self) -> usize {
        self.p.push(self.p.len());
        self.p.len() - 1
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.p[x] != x {
            self.p[x] = self.p[self.p[x]];
            x = self.p[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // The smaller index stays the root: deterministic regardless of the order of the links.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.p[hi] = lo;
        }
    }
}

fn has_reply_prefix(subject: &str) -> bool {
    let l = subject.trim_start().to_ascii_lowercase();
    ["re:", "fwd:", "fw:", "aw:", "sv:", "wg:"].iter().any(|p| l.starts_with(p))
}

fn joinable_subject(norm: &str) -> bool {
    norm.chars().count() >= 4 && !GENERIC.contains(&norm)
}

#[derive(Clone)]
struct Comp {
    rep: usize,
    min_date: i64,
    max_date: i64,
    froms: HashSet<String>,
    parts: HashSet<String>,
    reply: bool,
}

fn senders_overlap(a: &Comp, b: &Comp) -> bool {
    a.froms.iter().any(|f| b.parts.contains(f) || b.froms.contains(f)) || b.froms.iter().any(|f| a.parts.contains(f))
}

fn within_window(a: &Comp, b: &Comp) -> bool {
    a.min_date <= b.max_date + SUBJECT_WINDOW_SECS && b.min_date <= a.max_date + SUBJECT_WINDOW_SECS
}

/// For every item, the index of the earliest message (by date, then index) of its conversation.
/// `own` is the account's own address (never counts as a shared participant).
pub fn assign_heads(items: &[Item], own: &str) -> Vec<usize> {
    let n = items.len();
    let own = own.to_ascii_lowercase();
    let mut dsu = Dsu::new(n);
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let ids: Vec<String> = items.iter().map(|i| norm_id(&i.message_id)).collect();
    for (i, id) in ids.iter().enumerate() {
        if id.is_empty() {
            continue;
        }
        match by_id.get(id) {
            Some(&e) => dsu.union(i, e),
            None => {
                by_id.insert(id.clone(), i);
            }
        }
    }
    // Per component (by root): how many real messages it holds and their normalised subjects. A header link may
    // not fuse two established conversations (2+ messages each) unless their subjects agree: one crafted
    // `References` header must not glue unrelated threads together.
    let mut real: HashMap<usize, usize> = HashMap::new();
    let mut subs: HashMap<usize, HashSet<String>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let r = dsu.find(i);
        *real.entry(r).or_default() += 1;
        let ns = normalize_subject(&it.subject);
        if !ns.is_empty() {
            subs.entry(r).or_default().insert(ns);
        }
    }
    // Header pass: link every message to the ids it names (virtual elements stand in for absent messages).
    for (i, it) in items.iter().enumerate() {
        let mut links: Vec<String> = it.in_reply_to.split_whitespace().take(MAX_LINKS).map(norm_id).collect();
        let refs: Vec<&str> = it.references.split_whitespace().collect();
        if let Some(first) = refs.first() {
            links.push(norm_id(first));
        }
        let tail_from = refs.len().saturating_sub(MAX_LINKS);
        links.extend(refs[tail_from.max(1).min(refs.len())..].iter().map(|r| norm_id(r)));
        for l in links {
            if l.is_empty() || l == ids[i] {
                continue;
            }
            let e = match by_id.get(&l) {
                Some(&e) => e,
                None => {
                    let v = dsu.push();
                    by_id.insert(l, v);
                    v
                }
            };
            let (ra, rb) = (dsu.find(i), dsu.find(e));
            if ra == rb {
                continue;
            }
            let (na, nb) = (real.get(&ra).copied().unwrap_or(0), real.get(&rb).copied().unwrap_or(0));
            if na >= 2 && nb >= 2 {
                let agree = match (subs.get(&ra), subs.get(&rb)) {
                    (Some(x), Some(y)) => x.iter().any(|t| y.contains(t)),
                    _ => true,
                };
                if !agree {
                    continue;
                }
            }
            dsu.union(ra, rb);
            let root = dsu.find(ra);
            let other = if root == ra { rb } else { ra };
            let moved = real.remove(&other).unwrap_or(0);
            *real.entry(root).or_default() += moved;
            if let Some(o) = subs.remove(&other) {
                subs.entry(root).or_default().extend(o);
            }
        }
    }
    // Conservative subject fallback over the header-linked conversations.
    let mut comps: HashMap<usize, Comp> = HashMap::new();
    let mut earliest: HashMap<usize, (i64, usize)> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let r = dsu.find(i);
        let e = earliest.entry(r).or_insert((it.date, i));
        if (it.date, i) < *e {
            *e = (it.date, i);
        }
        let c = comps.entry(r).or_insert_with(|| Comp { rep: i, min_date: it.date, max_date: it.date, froms: HashSet::new(), parts: HashSet::new(), reply: false });
        c.min_date = c.min_date.min(it.date);
        c.max_date = c.max_date.max(it.date);
        for f in addresses_of(&it.from) {
            if f != own {
                c.parts.insert(f.clone());
                c.froms.insert(f);
            }
        }
        for t in addresses_of(&it.to).into_iter().chain(addresses_of(&it.cc)) {
            if t != own {
                c.parts.insert(t);
            }
        }
    }
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (r, (_, head)) in &earliest {
        let it = &items[*head];
        let norm = normalize_subject(&it.subject);
        if let Some(c) = comps.get_mut(r) {
            c.reply = has_reply_prefix(&it.subject);
        }
        if joinable_subject(&norm) {
            groups.entry(norm).or_default().push(*r);
        }
    }
    for (_, mut roots) in groups {
        if roots.len() < 2 || roots.len() > MAX_GROUP {
            continue;
        }
        roots.sort_by_key(|r| (comps[r].min_date, *r));
        let mut merged: Vec<Comp> = Vec::new();
        for r in roots {
            let c = comps[&r].clone();
            let target = merged.iter().rposition(|t| (c.reply || t.reply) && within_window(t, &c) && senders_overlap(t, &c));
            match target {
                Some(ti) => {
                    let t = &mut merged[ti];
                    dsu.union(t.rep, c.rep);
                    t.min_date = t.min_date.min(c.min_date);
                    t.max_date = t.max_date.max(c.max_date);
                    t.froms.extend(c.froms);
                    t.parts.extend(c.parts);
                    t.reply &= c.reply;
                }
                None => merged.push(c),
            }
        }
    }
    // Head = earliest member of the final conversation.
    let mut head: HashMap<usize, (i64, usize)> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let r = dsu.find(i);
        let e = head.entry(r).or_insert((it.date, i));
        if (it.date, i) < *e {
            *e = (it.date, i);
        }
    }
    (0..n).map(|i| head[&dsu.find(i)].1).collect()
}

/// A stable-looking key per item: the conversation's root id (the head's first `References` id, else its own
/// Message-ID, else a positional fallback). Equal keys <=> same conversation.
pub fn thread_keys(items: &[Item], own: &str) -> Vec<String> {
    let heads = assign_heads(items, own);
    heads
        .iter()
        .map(|&h| {
            let it = &items[h];
            let root = it.references.split_whitespace().next().map(norm_id).filter(|s| !s.is_empty());
            let own_id = norm_id(&it.message_id);
            root.or(Some(own_id).filter(|s| !s.is_empty())).unwrap_or_else(|| format!("~{h}"))
        })
        .collect()
}

/// A cached message with its threading fields and where it lives.
#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub seen: bool,
    pub item: Item,
}

/// `(uid, message_id, in_reply_to, references)` of a folder's unread messages.
pub type LinkRow = (u32, String, String, String);

impl Storage {
    /// Newest `limit` cached messages of the account outside Trash, Junk and Drafts, with threading fields.
    pub fn thread_rows(&self, account: &str, limit: u32) -> Result<Vec<ThreadRow>> {
        let c = self.lock()?;
        let mut st = c.prepare(
            "SELECT m.folder, m.uid, m.uidvalidity, COALESCE(m.message_id,''), m.in_reply_to, m.refs, m.subject, m.from_addr, m.to_addr, m.cc, m.date, m.seen
             FROM messages m JOIN folders f ON f.account=m.account AND f.name=m.folder
             WHERE m.account=?1 AND f.role NOT IN ('trash','junk','drafts') AND m.deleted=0
             ORDER BY m.date DESC, m.uid DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![account, limit], |r| {
            Ok(ThreadRow {
                folder: r.get(0)?,
                uid: r.get(1)?,
                uidvalidity: r.get(2)?,
                seen: r.get(11)?,
                item: Item { message_id: r.get(3)?, in_reply_to: r.get(4)?, references: r.get(5)?, subject: r.get(6)?, from: r.get(7)?, to: r.get(8)?, cc: r.get(9)?, date: r.get(10)? },
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Link fields of the unread messages of one folder (for "muted threads do not count as unread").
    pub fn unseen_link_rows(&self, account: &str, folder: &str, limit: u32) -> Result<Vec<LinkRow>> {
        let c = self.lock()?;
        let mut st = c.prepare(
            "SELECT uid, COALESCE(message_id,''), in_reply_to, refs FROM messages WHERE account=?1 AND folder=?2 AND seen=0 AND deleted=0 ORDER BY uid DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![account, folder, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::ExtraHeaders;
    use crate::types::*;

    const ME: &str = "me@example.org";
    const DAY: i64 = 86_400;

    fn it(id: &str, irt: &str, refs: &str, subject: &str, from: &str, date: i64) -> Item {
        Item { message_id: id.into(), in_reply_to: irt.into(), references: refs.into(), subject: subject.into(), from: from.into(), to: ME.into(), cc: String::new(), date }
    }
    fn same(k: &[String], a: usize, b: usize) -> bool {
        k[a] == k[b]
    }

    #[test]
    fn a_references_chain_is_one_thread_and_unrelated_mail_is_not() {
        let items = vec![
            it("a@x", "", "", "Plan", "ann@x.org", 100),
            it("b@x", "a@x", "a@x", "Re: Plan", "bob@x.org", 200),
            it("c@x", "b@x", "a@x b@x", "Re: Plan", "ann@x.org", 300),
            it("z@x", "", "", "Something else", "zed@x.org", 150),
        ];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1) && same(&k, 1, 2));
        assert!(!same(&k, 0, 3));
        assert_eq!(k[0], "a@x", "the root id names the conversation");
    }

    #[test]
    fn replies_to_a_missing_parent_still_meet_and_order_does_not_matter() {
        let mut items = vec![
            it("c@x", "b@x", "root@x b@x", "Re: Hello there", "ann@x.org", 300),
            it("b@x", "root@x", "root@x", "Re: Hello there", "bob@x.org", 200),
            it("n@x", "", "", "Newsletter", "n@x.org", 250),
        ];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1) && !same(&k, 0, 2));
        assert_eq!(k[0], "root@x", "the head's first reference is the key even though that message is absent");
        items.reverse();
        let k2 = thread_keys(&items, ME);
        assert!(same(&k2, 1, 2) && !same(&k2, 0, 1));
    }

    #[test]
    fn a_crafted_references_header_does_not_fuse_two_established_threads() {
        let items = vec![
            it("a1@x", "", "", "Budget", "ann@x.org", 1),
            it("a2@x", "a1@x", "a1@x", "Re: Budget", "bob@x.org", 2),
            it("b1@x", "", "", "Holiday", "cy@x.org", 3),
            it("b2@x", "b1@x", "b1@x", "Re: Holiday", "di@x.org", 4),
            // names one message of each thread
            it("evil@x", "b2@x", "a1@x b2@x", "Whatever", "evil@x.org", 5),
        ];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1) && same(&k, 2, 3));
        assert!(!same(&k, 0, 2), "the two threads stay apart");
        // With agreeing subjects the link is honoured.
        let mut ok = items.clone();
        ok[2].subject = "Budget".into();
        ok[3].subject = "Re: Budget".into();
        let k = thread_keys(&ok, ME);
        assert!(same(&k, 0, 2));
        // A singleton with another subject still joins a thread by header (subject changes in replies are normal).
        let items = vec![it("a1@x", "", "", "Budget", "ann@x.org", 1), it("a2@x", "a1@x", "a1@x", "Re: Budget", "bob@x.org", 2), it("c@x", "a2@x", "a1@x a2@x", "Dinner instead", "cy@x.org", 3)];
        assert!(same(&thread_keys(&items, ME), 0, 2));
    }

    #[test]
    fn a_display_name_cannot_pose_as_another_address() {
        assert_eq!(addresses_of("<boss@corp.com> <evil@x.org>"), vec!["evil@x.org"]);
        assert_eq!(addresses_of("\"<boss@corp.com>\" <evil@x.org>"), vec!["evil@x.org"]);
        assert_eq!(addresses_of("\"boss@corp.com\" <evil@x.org>, Ann <ann@y.org>"), vec!["evil@x.org", "ann@y.org"]);
        assert!(addresses_of("<boss@corp.com> nobody").is_empty(), "no bare-address fallback once brackets are involved");
        // Subject fallback: the spoofed boss does not count as a shared participant.
        let items = vec![
            it("p1@x", "", "", "Project plan", "boss@corp.com", 100),
            it("p2@x", "", "", "Re: Project plan", "<boss@corp.com> <evil@x.org>", 100 + DAY),
        ];
        assert!(!same(&thread_keys(&items, ME), 0, 1));
    }

    #[test]
    fn ids_compare_without_brackets_and_case() {
        let items = vec![it("<A@X>", "", "", "One", "a@x", 1), it("b@x", "<a@x>", "", "Two", "b@x", 2)];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1));
    }

    #[test]
    fn cycles_self_references_and_duplicates_are_harmless() {
        let items = vec![
            it("a@x", "b@x", "b@x", "Loop", "a@x.org", 1),
            it("b@x", "a@x", "a@x", "Loop", "b@x.org", 2),
            it("s@x", "s@x", "s@x s@x", "Selfish", "s@x.org", 3),
            it("dup@x", "", "", "Copy", "d@x.org", 4),
            it("dup@x", "", "", "Copy", "d@x.org", 4),
        ];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1));
        assert!(!same(&k, 1, 2));
        assert!(same(&k, 3, 4), "the same Message-ID in two folders is one conversation");
        assert_eq!(k.len(), 5);
    }

    #[test]
    fn messages_without_ids_stay_alone() {
        let items = vec![it("", "", "", "A message", "a@x.org", 1), it("", "", "", "A message", "a@x.org", 2)];
        let k = thread_keys(&items, ME);
        assert!(!same(&k, 0, 1), "two plain mails with the same subject do not merge");
        assert_ne!(k[0], k[1]);
    }

    #[test]
    fn subject_fallback_joins_a_reply_whose_headers_lost_the_link() {
        let items = vec![
            it("p1@x", "", "", "Project plan", "ann@x.org", 100),
            it("p2@x", "", "", "Re: Project plan", ME, 100 + DAY), // my reply, no In-Reply-To (broken client)
        ];
        let k = thread_keys(&items, ME);
        // Ann is a participant of the reply only through To; here To is ME, so use a proper To.
        assert!(!same(&k, 0, 1), "no shared participant besides me: not joined");
        let mut items = items;
        items[1].to = "Ann <ann@x.org>".into();
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1));
    }

    #[test]
    fn subject_fallback_limits() {
        let base = it("p1@x", "", "", "Project plan", "ann@x.org", 1_000_000);
        // outside 30 days
        let late = it("p2@x", "", "", "Re: Project plan", "ann@x.org", 1_000_000 + 31 * DAY);
        assert!(!same(&thread_keys(&[base.clone(), late], ME), 0, 1));
        // inside 30 days
        let soon = it("p3@x", "", "", "Re: Project plan", "ann@x.org", 1_000_000 + 29 * DAY);
        assert!(same(&thread_keys(&[base.clone(), soon], ME), 0, 1));
        // nothing in common but the subject (and my own address)
        let other = it("p4@x", "", "", "Re: Project plan", "mallory@evil.org", 1_000_000 + DAY);
        assert!(!same(&thread_keys(&[base.clone(), other], ME), 0, 1));
        // neither side is a reply: two newsletters / notifications never merge
        let again = it("p5@x", "", "", "Project plan", "ann@x.org", 1_000_000 + DAY);
        assert!(!same(&thread_keys(&[base.clone(), again], ME), 0, 1));
        // generic and very short subjects prove nothing
        let g1 = it("g1@x", "", "", "Hello", "ann@x.org", 1);
        let g2 = it("g2@x", "", "", "Re: Hello", "ann@x.org", 2);
        assert!(!same(&thread_keys(&[g1, g2], ME), 0, 1));
        let s1 = it("s1@x", "", "", "ok", "ann@x.org", 1);
        let s2 = it("s2@x", "", "", "Re: ok", "ann@x.org", 2);
        assert!(!same(&thread_keys(&[s1, s2], ME), 0, 1));
        // different people writing "Re: Project plan" to me are not one conversation just because I received both
        let a = it("q1@x", "", "", "Re: Project plan", "ann@x.org", 5);
        let b = it("q2@x", "", "", "Re: Project plan", "bob@x.org", 6);
        assert!(!same(&thread_keys(&[a, b], ME), 0, 1));
    }

    #[test]
    fn a_different_header_thread_is_never_pulled_into_another_by_subject() {
        // Two header-linked conversations with the same subject and participants stay as they are when neither head is a reply.
        let items = vec![
            it("a@x", "", "", "Weekly report", "ann@x.org", 10),
            it("b@x", "a@x", "a@x", "Weekly report", "bob@x.org", 11),
            it("c@x", "", "", "Weekly report", "ann@x.org", 12),
        ];
        let k = thread_keys(&items, ME);
        assert!(same(&k, 0, 1) && !same(&k, 0, 2));
    }

    #[test]
    fn hostile_references_are_bounded() {
        let refs: Vec<String> = (0..20_000).map(|i| format!("r{i}@x")).collect();
        let items = vec![it("h@x", "", &refs.join(" "), "Hostile", "a@x.org", 1), it("r19999@x", "", "", "Real", "b@x.org", 2), it("r5@x", "", "", "Middle", "c@x.org", 3)];
        let t = std::time::Instant::now();
        let k = thread_keys(&items, ME);
        assert!(t.elapsed().as_secs() < 2);
        assert!(same(&k, 0, 1), "the newest ids are followed");
        assert!(!same(&k, 0, 2), "ids in the middle of an absurd list are not");
    }

    #[test]
    fn five_thousand_messages_are_fast_and_correct() {
        // 1000 conversations of 5 messages, plus 300 unrelated singletons with a shared reply subject.
        let mut items = Vec::new();
        for t in 0..1000 {
            let root = format!("root{t}@x");
            let mut refs = String::new();
            for m in 0..5 {
                let id = format!("m{t}-{m}@x");
                let irt = if m == 0 { String::new() } else { format!("m{t}-{}@x", m - 1) };
                items.push(it(&id, &irt, &refs, &format!("Topic {t}"), &format!("user{}@x.org", t % 50), (t * 10 + m) as i64));
                refs = if m == 0 { id.clone() } else { format!("{refs} {id}") };
            }
            let _ = root;
        }
        assert_eq!(items.len(), 5000);
        let start = std::time::Instant::now();
        let k = thread_keys(&items, ME);
        let took = start.elapsed();
        assert!(took.as_secs() < 3, "took {took:?}");
        let distinct: HashSet<&String> = k.iter().collect();
        assert_eq!(distinct.len(), 1000);
        for t in 0..1000 {
            let base = t * 5;
            assert!((1..5).all(|m| k[base] == k[base + m]));
        }
        // worst case for the subject pass: thousands of replies with the same subject
        let many: Vec<Item> = (0..5000).map(|i| it(&format!("w{i}@x"), "", "", "Re: same subject here", "ann@x.org", i as i64)).collect();
        let start = std::time::Instant::now();
        let k = thread_keys(&many, ME);
        assert!(start.elapsed().as_secs() < 3);
        assert_eq!(k.len(), 5000);
    }

    #[test]
    fn address_extraction() {
        assert_eq!(addresses_of("Ann Lee <Ann@X.org>, bob@y.org"), vec!["ann@x.org"]);
        assert_eq!(addresses_of("bob@y.org, Cy <cy@z.org>"), vec!["cy@z.org"]);
        assert_eq!(addresses_of("\"Doe, J\" <j@d.org>"), vec!["j@d.org"]);
        assert_eq!(addresses_of("bob@y.org"), vec!["bob@y.org"]);
        assert!(addresses_of("nobody").is_empty());
    }

    fn account_store() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ME.into(),
            username: "me".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        s.replace_folders(ME, &[f("INBOX", FolderRole::Inbox), f("Sent", FolderRole::Sent), f("Trash", FolderRole::Trash), f("Junk", FolderRole::Junk)]).unwrap();
        s
    }
    #[allow(clippy::too_many_arguments)]
    fn put(s: &Storage, folder: &str, uid: u32, date: i64, seen: bool, mid: &str, subject: &str, refs: &str) {
        s.upsert_message(&MessageSummary {
            account: ME.into(),
            folder: folder.into(),
            uid,
            uidvalidity: 7,
            message_id: Some(mid.into()),
            subject: subject.into(),
            from: "ann@x.org".into(),
            to: ME.into(),
            date,
            flags: Flags { seen, ..Default::default() },
            snippet: String::new(),
            has_body: false,
            size: 1,
        })
        .unwrap();
        s.set_extra_headers(ME, folder, uid, &ExtraHeaders { references: refs.into(), in_reply_to: refs.split(' ').next_back().unwrap_or("").into(), ..Default::default() }).unwrap();
    }

    #[test]
    fn rows_come_from_the_cache_without_trash_junk_or_drafts_and_group_across_folders() {
        let s = account_store();
        put(&s, "INBOX", 1, 100, true, "a@x", "Plan", "");
        put(&s, "Sent", 1, 200, true, "b@x", "Re: Plan", "a@x");
        put(&s, "INBOX", 2, 300, false, "c@x", "Re: Plan", "a@x b@x");
        put(&s, "Trash", 1, 400, false, "d@x", "Re: Plan", "a@x");
        put(&s, "Junk", 1, 500, false, "e@x", "Spam", "");
        let rows = s.thread_rows(ME, 100).unwrap();
        assert_eq!(rows.len(), 3, "Trash and Junk are not threaded");
        let items: Vec<Item> = rows.iter().map(|r| r.item.clone()).collect();
        let k = thread_keys(&items, ME);
        assert!(k.iter().all(|x| x == &k[0]));
        assert_eq!(k[0], "a@x");
        assert_eq!(rows[0].item.references, "a@x b@x", "newest first");
        let un = s.unseen_link_rows(ME, "INBOX", 10).unwrap();
        assert_eq!(un, vec![(2, "c@x".to_string(), "b@x".to_string(), "a@x b@x".to_string())]);
        assert_eq!(s.thread_rows(ME, 2).unwrap().len(), 2, "limit applies");
    }
}
