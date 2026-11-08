//! Patch local edits into the ORIGINAL server ICS so a save never drops properties this crate does
//! not model (VTIMEZONE, CLASS, TRANSP, CATEGORIES, URL, ATTACH, RDATE, extra VALARMs, X- props,
//! PARTSTAT=DELEGATED, ...). Only the properties that differ between what the raw parses to and the
//! edited event are touched.

use crate::error::{Error, Result};
use crate::ics::{
    check_caps, clean, dur_string, escape, fold_into, ics_time, mailto, named_tz, now_stamp, param_val, parse_inner, person, status_str, trigger_secs, unescape, unfold, valarm_lines,
    vevent, vtimezone, zoom_lines, BuildCfg, ParseOptions, MAX_ICS_BYTES, TRUSTED_MAX_BYTES,
};
use crate::tm;
use crate::types::*;
use chrono::Datelike;
use chrono_tz::Tz;

const MAX_BLOCKS: usize = 2_000;

enum Seg {
    Raw(Vec<String>),
    Ev(Vec<String>),
}

enum Item {
    Line(String),
    Sub(Vec<String>),
}

/// `(prefix before the value colon, value)`; the colon search honours quoted parameter values.
fn split_line(line: &str) -> Option<(&str, &str)> {
    let mut in_q = false;
    for (i, b) in line.bytes().enumerate() {
        match b {
            b'"' => in_q = !in_q,
            b':' if !in_q => return Some((&line[..i], &line[i + 1..])),
            _ => {}
        }
    }
    None
}

fn segs(prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_q = false;
    let mut start = 0;
    for (i, b) in prefix.bytes().enumerate() {
        match b {
            b'"' => in_q = !in_q,
            b';' if !in_q => {
                out.push(prefix[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(prefix[start..].to_string());
    out
}

/// `Some(line with SCHEDULE-AGENT=CLIENT)` unless the line already carries exactly that.
fn force_client(line: &str) -> Option<String> {
    let (prefix, value) = split_line(line)?;
    let mut parts = segs(prefix);
    let has = parts.iter().skip(1).any(|p| p.eq_ignore_ascii_case("SCHEDULE-AGENT=CLIENT"));
    if has {
        return None;
    }
    set_param(&mut parts, "SCHEDULE-AGENT", Some("CLIENT"));
    Some(format!("{}:{}", parts.join(";"), value))
}

fn line_name(line: &str) -> String {
    let (prefix, _) = split_line(line).unwrap_or((line, ""));
    let n = prefix.split(';').next().unwrap_or("").trim().to_ascii_uppercase();
    n.rsplit('.').next().unwrap_or("").to_string()
}

fn line_value(line: &str) -> &str {
    split_line(line).map(|x| x.1).unwrap_or("")
}

fn set_param(parts: &mut Vec<String>, key: &str, val: Option<&str>) {
    let pre = format!("{key}=");
    let at = parts.iter().skip(1).position(|p| is_ci(p, &pre)).map(|i| i + 1);
    match (at, val) {
        (Some(i), Some(v)) => parts[i] = format!("{key}={v}"),
        (Some(i), None) => {
            parts.remove(i);
        }
        (None, Some(v)) => parts.push(format!("{key}={v}")),
        (None, None) => {}
    }
}

fn is_ci(line: &str, pre: &str) -> bool {
    line.len() >= pre.len() && line.is_char_boundary(pre.len()) && line[..pre.len()].eq_ignore_ascii_case(pre)
}

struct Block {
    items: Vec<Item>,
    changed: bool,
}

impl Block {
    fn parse(lines: &[String]) -> Block {
        let mut items = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if is_ci(&lines[i], "BEGIN:") {
                let mut depth = 0usize;
                let mut sub = Vec::new();
                while i < lines.len() {
                    let t = &lines[i];
                    if is_ci(t, "BEGIN:") {
                        depth += 1;
                    } else if is_ci(t, "END:") {
                        depth = depth.saturating_sub(1);
                    }
                    sub.push(t.clone());
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                items.push(Item::Sub(sub));
            } else {
                items.push(Item::Line(lines[i].clone()));
                i += 1;
            }
        }
        Block { items, changed: false }
    }

    fn emit(&self, out: &mut Vec<String>) {
        out.push("BEGIN:VEVENT".into());
        for it in &self.items {
            match it {
                Item::Line(l) => out.push(l.clone()),
                Item::Sub(v) => out.extend(v.iter().cloned()),
            }
        }
        out.push("END:VEVENT".into());
    }

    fn pos(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|it| matches!(it, Item::Line(l) if line_name(l) == name))
    }

    fn remove(&mut self, name: &str) {
        let had = self.pos(name).is_some();
        self.items.retain(|it| !matches!(it, Item::Line(l) if line_name(l) == name));
        if had {
            self.changed = true;
        }
    }

    fn append(&mut self, line: String) {
        let at = self.items.iter().rposition(|it| matches!(it, Item::Line(_))).map(|i| i + 1).unwrap_or(0);
        self.items.insert(at, Item::Line(line));
        self.changed = true;
    }

    /// Replace the value of `name` keeping its parameters; add the line when missing.
    fn set_value(&mut self, name: &str, value: &str) {
        match self.pos(name) {
            Some(i) => {
                if let Item::Line(l) = &self.items[i] {
                    let prefix = split_line(l).map(|x| x.0).unwrap_or(name).to_string();
                    self.items[i] = Item::Line(format!("{prefix}:{value}"));
                }
                let mut seen = false;
                self.items.retain(|it| match it {
                    Item::Line(l) if line_name(l) == name => !std::mem::replace(&mut seen, true),
                    _ => true,
                });
                self.changed = true;
            }
            None => self.append(format!("{name}:{value}")),
        }
    }

    fn lines_named(&self, name: &str) -> Vec<usize> {
        self.items.iter().enumerate().filter(|(_, it)| matches!(it, Item::Line(l) if line_name(l) == name)).map(|(i, _)| i).collect()
    }
}

fn same_rid(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (tm::parse_wire_dt(a), tm::parse_wire_dt(b)) {
        (Some(x), Some(y)) => x == y,
        _ => (a.len() == 10 && b.get(..10) == Some(a)) || (b.len() == 10 && a.get(..10) == Some(b)),
    }
}

struct Ctx<'a> {
    trust: bool,
    shell: &'a [String],
    sched: bool,
    need_tz: Vec<(Tz, i32)>,
}

fn mini_parse(shell: &[String], block: &[String], trust: bool) -> Option<(CalEvent, Option<String>)> {
    let mut text = String::new();
    let end_at = shell.iter().rposition(|l| l.trim().eq_ignore_ascii_case("END:VCALENDAR")).unwrap_or(shell.len());
    for l in &shell[..end_at] {
        text.push_str(l);
        text.push_str("\r\n");
    }
    for l in block {
        text.push_str(l);
        text.push_str("\r\n");
    }
    text.push_str("END:VCALENDAR\r\n");
    let mut p = parse_inner(text.as_bytes(), ParseOptions { trust_zoom_props: trust }, TRUSTED_MAX_BYTES).ok()?;
    if !p.events.is_empty() {
        Some((p.events.remove(0), None))
    } else if !p.overrides.is_empty() {
        let o = p.overrides.remove(0);
        Some((o.event, Some(o.recurrence_id)))
    } else {
        None
    }
}

fn email_of(line: &str) -> String {
    mailto(line_value(line)).to_ascii_lowercase()
}

fn role_str(r: AttendeeRole) -> &'static str {
    match r {
        AttendeeRole::Chair => "CHAIR",
        AttendeeRole::Required => "REQ-PARTICIPANT",
        AttendeeRole::Optional => "OPT-PARTICIPANT",
    }
}

fn note_tz(ev: &CalEvent, need: &mut Vec<(Tz, i32)>) {
    if !ev.all_day {
        if let Some(tz) = named_tz(&ev.tzid) {
            let year = tm::parse_wire_dt(&ev.start).map(|d| d.year()).unwrap_or(2026);
            need.push((tz, year));
        }
    }
}

/// Apply every difference between `old` (what the raw component parses to) and `new` to `b`.
fn patch_block(b: &mut Block, old: &CalEvent, new: &CalEvent, cx: &mut Ctx<'_>) -> Result<()> {
    check_caps(new)?;
    let mut significant = false;

    // ---- text fields
    for (name, o, n, keep_empty) in [("SUMMARY", &old.summary, &new.summary, true), ("DESCRIPTION", &old.description, &new.description, false), ("LOCATION", &old.location, &new.location, false)] {
        if o != n {
            significant = true;
            if n.is_empty() && !keep_empty {
                b.remove(name);
            } else {
                b.set_value(name, &escape(n));
            }
        }
    }
    if old.status != new.status {
        significant = true;
        b.set_value("STATUS", status_str(new.status));
    }

    // ---- time
    if old.start != new.start || old.end != new.end || old.all_day != new.all_day || old.tzid != new.tzid {
        significant = true;
        let at = b.pos("DTSTART").unwrap_or(0);
        for n in ["DTSTART", "DTEND", "DUE", "DURATION"] {
            b.remove(n);
        }
        let s = ics_time(new, &new.start).ok_or_else(|| Error::Invalid("unparsable start time".into()))?;
        let mut lines = vec![format!("DTSTART{}:{}", s.0, s.1)];
        if let Some(e) = ics_time(new, &new.end) {
            lines.push(format!("DTEND{}:{}", e.0, e.1));
        }
        let at = at.min(b.items.len());
        for (k, l) in lines.into_iter().enumerate() {
            b.items.insert(at + k, Item::Line(l));
        }
        b.changed = true;
        note_tz(new, &mut cx.need_tz);
    }

    // ---- recurrence
    if old.rrule != new.rrule {
        significant = true;
        b.remove("RRULE");
        if let Some(r) = new.rrule.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            b.append(format!("RRULE:{}", clean(r.trim_start_matches("RRULE:"))));
        }
    }
    let (mut oe, mut ne) = (old.exdates.clone(), new.exdates.clone());
    oe.sort();
    ne.sort();
    if oe != ne {
        significant = true;
        b.remove("EXDATE");
        for x in &new.exdates {
            let (p, v) = ics_time(new, x).ok_or_else(|| Error::Invalid("unparsable EXDATE".into()))?;
            b.append(format!("EXDATE{p}:{v}"));
        }
    }

    // ---- people
    let same_org = match (&old.organizer, &new.organizer) {
        (None, None) => true,
        (Some(x), Some(y)) => x.email.trim().eq_ignore_ascii_case(y.email.trim()) && x.name == y.name,
        _ => false,
    };
    if !same_org {
        b.remove("ORGANIZER");
        if let Some(o) = &new.organizer {
            b.append(person("ORGANIZER", o, false, false));
        }
    }
    let raw_att = b.lines_named("ATTENDEE");
    let mut present: Vec<String> = Vec::new();
    let mut drop_idx: Vec<usize> = Vec::new();
    for &i in &raw_att {
        let Item::Line(l) = &b.items[i] else { continue };
        let email = email_of(l);
        let Some(na) = new.attendees.iter().find(|a| a.email.trim().eq_ignore_ascii_case(&email)) else {
            drop_idx.push(i);
            continue;
        };
        if present.contains(&email) {
            continue;
        }
        present.push(email.clone());
        let oa = old.attendees.iter().find(|a| a.email.trim().eq_ignore_ascii_case(&email));
        let (ost, orole, oname) = oa.map(|a| (Some(a.status), Some(a.role), a.name.clone())).unwrap_or((None, None, None));
        if ost == Some(na.status) && orole == Some(na.role) && oname == na.name {
            continue; // untouched: keeps DELEGATED, CUTYPE, X- params, ...
        }
        let prefix = split_line(l).map(|x| x.0).unwrap_or("ATTENDEE").to_string();
        let value = line_value(l).to_string();
        let mut parts = segs(&prefix);
        if ost != Some(na.status) {
            set_param(&mut parts, "PARTSTAT", Some(na.status.as_partstat()));
        }
        if orole != Some(na.role) {
            set_param(&mut parts, "ROLE", Some(role_str(na.role)));
        }
        if oname != na.name {
            let cn = na.name.as_deref().filter(|n| !n.is_empty()).map(param_val);
            set_param(&mut parts, "CN", cn.as_deref());
        }
        b.items[i] = Item::Line(format!("{}:{}", parts.join(";"), value));
        b.changed = true;
    }
    if !drop_idx.is_empty() {
        let mut k = 0;
        b.items.retain(|_| {
            let keep = !drop_idx.contains(&k);
            k += 1;
            keep
        });
        b.changed = true;
    }
    for a in &new.attendees {
        let e = a.email.trim().to_ascii_lowercase();
        if !present.contains(&e) {
            present.push(e);
            b.append(person("ATTENDEE", a, true, cx.sched));
        }
    }
    // MiraMail sends the iMIP mails itself: tell an RFC 6638 scheduling server to stay out of it, on the
    // ATTENDEE lines (organizer side) and the ORGANIZER line (attendee side, no automatic REPLY).
    if cx.sched {
        for name in ["ORGANIZER", "ATTENDEE"] {
            for i in b.lines_named(name) {
                let forced = if let Item::Line(l) = &b.items[i] { force_client(l) } else { None };
                if let Some(nl) = forced {
                    b.items[i] = Item::Line(nl);
                    b.changed = true;
                }
            }
        }
    }

    // ---- reminder
    let new_min = new.reminder_minutes.filter(|m| *m >= 0);
    if old.reminder_minutes != new_min {
        let target = b.items.iter().position(|it| match it {
            Item::Sub(v) if v.first().is_some_and(|l| is_ci(l, "BEGIN:VALARM")) => v.iter().any(|l| {
                line_name(l) == "TRIGGER" && !l.to_ascii_uppercase().contains("VALUE=DATE-TIME") && trigger_secs(line_value(l)).is_some_and(|s| s <= 0)
            }),
            _ => false,
        });
        match (target, new_min) {
            (Some(i), Some(m)) => {
                if let Item::Sub(v) = &mut b.items[i] {
                    for l in v.iter_mut() {
                        if line_name(l) == "TRIGGER" {
                            let prefix = split_line(l).map(|x| x.0).unwrap_or("TRIGGER").to_string();
                            *l = format!("{prefix}:{}", dur_string(m));
                            break;
                        }
                    }
                }
            }
            (Some(i), None) => {
                b.items.remove(i);
            }
            (None, Some(m)) => b.items.push(Item::Sub(valarm_lines(m))),
            (None, None) => {}
        }
        b.changed = true;
    }

    // ---- zoom (never write the host start_url; always strip a stale one)
    let zkey = |z: &Option<ZoomMeeting>| z.as_ref().map(|z| (z.meeting_id.clone(), z.join_url.clone(), z.password.clone()));
    let had_starturl = b.pos("X-MIRAMAIL-ZOOM-STARTURL").is_some();
    if zkey(&old.zoom) != zkey(&new.zoom) {
        b.items.retain(|it| !matches!(it, Item::Line(l) if line_name(l).starts_with("X-MIRAMAIL-ZOOM-")));
        if let Some(z) = &new.zoom {
            for l in zoom_lines(z) {
                b.append(l);
            }
        }
        b.changed = true;
    } else if had_starturl {
        b.remove("X-MIRAMAIL-ZOOM-STARTURL");
    }

    // ---- bookkeeping
    if b.changed {
        if new.sequence != old.sequence {
            b.set_value("SEQUENCE", &new.sequence.max(0).to_string());
        } else if significant {
            b.set_value("SEQUENCE", &(old.sequence.max(0) + 1).to_string());
        }
        b.set_value("DTSTAMP", &now_stamp());
    }
    Ok(())
}

fn tzids_in(lines: &[String]) -> Vec<String> {
    lines.iter().filter(|l| is_ci(l, "TZID:")).map(|l| unescape(&l[5..]).trim().to_string()).collect()
}

/// Patch `raw` (an original resource) with the edited `master` / `overrides` of `uid`.
/// `Ok(None)` when the raw has no master component to patch (caller rebuilds from the stored form).
pub(crate) fn patch_resource(raw: &str, trust: bool, uid: &str, master: Option<&CalEvent>, overrides: &[EventOverride], schedule_client: bool) -> Result<Option<String>> {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let lines = unfold(text);
    let mut segs_v: Vec<Seg> = Vec::new();
    let mut cur_raw: Vec<String> = Vec::new();
    let mut i = 0;
    let mut n_blocks = 0usize;
    while i < lines.len() {
        if lines[i].trim().eq_ignore_ascii_case("BEGIN:VEVENT") {
            let mut depth = 0usize;
            let mut blk = Vec::new();
            while i < lines.len() {
                let t = &lines[i];
                if is_ci(t, "BEGIN:") {
                    depth += 1;
                } else if is_ci(t, "END:") {
                    depth = depth.saturating_sub(1);
                }
                blk.push(t.clone());
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            n_blocks += 1;
            if n_blocks > MAX_BLOCKS {
                return Err(Error::Invalid("too many events in resource".into()));
            }
            if !cur_raw.is_empty() {
                segs_v.push(Seg::Raw(std::mem::take(&mut cur_raw)));
            }
            if blk.len() < 2 || !blk.last().is_some_and(|l| is_ci(l, "END:")) {
                blk.push("END:VEVENT".into()); // tolerate a truncated component
            }
            segs_v.push(Seg::Ev(blk));
        } else {
            let l = lines[i].clone();
            i += 1;
            if is_ci(&l, "METHOD:") {
                continue; // a stored resource never carries an iTIP method
            }
            cur_raw.push(l);
        }
    }
    if !cur_raw.is_empty() {
        segs_v.push(Seg::Raw(cur_raw));
    }
    let shell: Vec<String> = segs_v
        .iter()
        .filter_map(|s| if let Seg::Raw(v) = s { Some(v.iter().cloned()) } else { None })
        .flatten()
        .collect();

    let mut cx = Ctx { trust, shell: &shell, sched: schedule_client, need_tz: Vec::new() };
    let mut matched_override = vec![false; overrides.len()];
    let mut master_done = false;
    let mut out: Vec<String> = Vec::new();
    for seg in &segs_v {
        match seg {
            Seg::Raw(v) => out.extend(v.iter().cloned()),
            Seg::Ev(blk) => {
                let inner = &blk[1..blk.len() - 1];
                // cheap UID screen before the (costly) mini parse
                if let Some(l) = inner.iter().find(|l| line_name(l) == "UID") {
                    if unescape(line_value(l)).trim() != uid {
                        out.extend(blk.iter().cloned());
                        continue;
                    }
                }
                let Some((old, rec)) = mini_parse(cx.shell, blk, cx.trust) else {
                    out.extend(blk.iter().cloned());
                    continue;
                };
                if old.uid != uid {
                    out.extend(blk.iter().cloned());
                    continue;
                }
                let mut b = Block::parse(inner);
                match rec {
                    None => match master {
                        Some(m) if !master_done => {
                            master_done = true;
                            patch_block(&mut b, &old, m, &mut cx)?;
                            b.emit(&mut out);
                        }
                        Some(_) => {} // duplicate master component: drop
                        None => b.emit(&mut out),
                    },
                    Some(rid) => {
                        if let Some(k) = overrides.iter().enumerate().position(|(k, o)| !matched_override[k] && same_rid(&o.recurrence_id, &rid)) {
                            matched_override[k] = true;
                            patch_block(&mut b, &old, &overrides[k].event, &mut cx)?;
                            b.emit(&mut out);
                        } // else: deleted locally -> dropped
                    }
                }
            }
        }
    }
    if master.is_some() && !master_done {
        return Ok(None);
    }
    // ---- brand-new override components
    let mut extra = String::new();
    let cfg = BuildCfg { strict: true, schedule_client };
    for (k, o) in overrides.iter().enumerate() {
        if !matched_override[k] {
            vevent(&mut extra, &o.event, Some(&o.recurrence_id), cfg)?;
            note_tz(&o.event, &mut cx.need_tz);
        }
    }
    if !extra.is_empty() {
        let end_at = out.iter().rposition(|l| l.trim().eq_ignore_ascii_case("END:VCALENDAR")).unwrap_or(out.len());
        for (k, l) in unfold(&extra).into_iter().enumerate() {
            out.insert(end_at + k, l);
        }
    }
    // ---- VTIMEZONE for zones the raw does not define
    let have = tzids_in(&out);
    let first_ev = out.iter().position(|l| l.trim().eq_ignore_ascii_case("BEGIN:VEVENT")).unwrap_or(out.len());
    let mut added: Vec<Tz> = Vec::new();
    let mut ins: Vec<String> = Vec::new();
    for (tz, year) in &cx.need_tz {
        if !have.iter().any(|h| h == tz.name()) && !added.contains(tz) {
            added.push(*tz);
            ins.extend(vtimezone(*tz, *year));
        }
    }
    for (k, l) in ins.into_iter().enumerate() {
        out.insert(first_ev + k, l);
    }
    let mut s = String::new();
    for l in &out {
        fold_into(&mut s, l);
    }
    if s.len() > MAX_ICS_BYTES {
        return Err(Error::TooLarge { max: MAX_ICS_BYTES });
    }
    Ok(Some(s))
}
