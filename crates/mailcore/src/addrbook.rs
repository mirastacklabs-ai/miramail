//! Local address book source: raw address fields of cached messages. No network.
//! The app turns these rows into ranked suggestions (it owns the display-form parser).

use crate::storage::Storage;
use crate::Result;
use rusqlite::params;

/// One cached message's address fields (display form as stored: `Name <addr>, addr2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddrRow {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    /// Folder role as stored (`sent`, `inbox`, ...).
    pub role: String,
}

fn like_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

impl Storage {
    /// `(count, newest date)` of cached messages (junk and trash excluded) sent FROM `address` (case-insensitive,
    /// matched on the address inside the stored `Name <addr>` field). Local cache only.
    pub fn count_from(&self, address: &str) -> Result<(u32, i64)> {
        let a = address.trim().to_lowercase();
        if a.is_empty() || a.len() > 254 {
            return Ok((0, 0));
        }
        let pat = format!("%{}%", like_escape(&a));
        let c = self.lock()?;
        let (n, newest): (u32, Option<i64>) = c.query_row(
            "SELECT COUNT(*), MAX(m.date) FROM messages m JOIN folders f ON f.account = m.account AND f.name = m.folder
             WHERE f.role NOT IN ('junk','trash') AND lower(m.from_addr) LIKE ?1 ESCAPE '\\'",
            params![pat],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((n, newest.unwrap_or(0)))
    }

    /// Newest `limit` cached messages (junk and trash excluded) whose from/to/cc contain `needle`
    /// (case-insensitive). An empty needle returns the newest messages overall.
    pub fn address_rows(&self, needle: &str, limit: u32) -> Result<Vec<AddrRow>> {
        self.address_rows_for(None, needle, limit)
    }

    /// Like `address_rows`, restricted to one account's cached mail when `account` is given (a contact seen only
    /// through another mailbox is not offered while composing from this one).
    pub fn address_rows_for(&self, account: Option<&str>, needle: &str, limit: u32) -> Result<Vec<AddrRow>> {
        let pat = format!("%{}%", like_escape(needle.trim()));
        let c = self.lock()?;
        let mut st = c.prepare(
            "SELECT m.from_addr, m.to_addr, m.cc, m.date, f.role FROM messages m
             JOIN folders f ON f.account = m.account AND f.name = m.folder
             WHERE f.role NOT IN ('junk','trash')
               AND (?3 IS NULL OR m.account = ?3)
               AND (m.from_addr LIKE ?1 ESCAPE '\\' OR m.to_addr LIKE ?1 ESCAPE '\\' OR m.cc LIKE ?1 ESCAPE '\\')
             ORDER BY m.date DESC LIMIT ?2",
        )?;
        let rows = st
            .query_map(params![pat, limit, account], |r| Ok(AddrRow { from: r.get(0)?, to: r.get(1)?, cc: r.get(2)?, date: r.get(3)?, role: r.get(4)? }))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn store() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        let cfg = AccountConfig {
            email: "me@x.org".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
            username: "me@x.org".into(),
        };
        s.upsert_account(&cfg).unwrap();
        let f = |name: &str, role: FolderRole| Folder { name: name.into(), role, selectable: true, delimiter: None };
        s.replace_folders("me@x.org", &[f("INBOX", FolderRole::Inbox), f("Sent", FolderRole::Sent), f("Junk", FolderRole::Junk)]).unwrap();
        let m = |folder: &str, uid: u32, from: &str, to: &str, date: i64| MessageSummary {
            account: "me@x.org".into(), folder: folder.into(), uid, uidvalidity: 1, message_id: None, subject: "s".into(),
            from: from.into(), to: to.into(), date, flags: Flags::default(), snippet: String::new(), has_body: false, size: 1,
        };
        s.upsert_message(&m("INBOX", 1, "Ada <ada@a.org>", "me@x.org", 10)).unwrap();
        s.upsert_message(&m("Sent", 1, "me@x.org", "Grace Hopper <grace@n.org>", 20)).unwrap();
        s.upsert_message(&m("Junk", 1, "Spam <spam@bad.example>", "me@x.org", 30)).unwrap();
        s
    }

    #[test]
    fn finds_by_substring_newest_first_and_skips_junk() {
        let s = store();
        let r = s.address_rows("gra", 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].role.as_str(), r[0].to.as_str()), ("sent", "Grace Hopper <grace@n.org>"));
        assert!(s.address_rows("spam", 10).unwrap().is_empty());
        let all = s.address_rows("", 10).unwrap();
        assert_eq!(all.iter().map(|r| r.date).collect::<Vec<_>>(), vec![20, 10]);
    }

    #[test]
    fn account_filter_is_parameterised_and_limits_rows() {
        let s = store();
        let cfg = AccountConfig {
            email: "other@y.org".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
            username: "other@y.org".into(),
        };
        s.upsert_account(&cfg).unwrap();
        s.replace_folders("other@y.org", &[Folder { name: "INBOX".into(), role: FolderRole::Inbox, selectable: true, delimiter: None }]).unwrap();
        s.upsert_message(&MessageSummary {
            account: "other@y.org".into(), folder: "INBOX".into(), uid: 1, uidvalidity: 1, message_id: None, subject: "s".into(),
            from: "Bob <bob@b.org>".into(), to: "other@y.org".into(), date: 40, flags: Flags::default(), snippet: String::new(), has_body: false, size: 1,
        })
        .unwrap();
        assert_eq!(s.address_rows_for(Some("me@x.org"), "bob", 10).unwrap().len(), 0);
        assert_eq!(s.address_rows_for(Some("other@y.org"), "bob", 10).unwrap().len(), 1);
        assert_eq!(s.address_rows_for(None, "bob", 10).unwrap().len(), 1);
        // A quote in the account value is just a value that matches nothing.
        assert!(s.address_rows_for(Some("x' OR '1'='1"), "", 10).unwrap().is_empty());
    }

    #[test]
    fn count_from_counts_senders_only_and_skips_junk() {
        let s = store();
        assert_eq!(s.count_from("ADA@a.org").unwrap(), (1, 10));
        assert_eq!(s.count_from("grace@n.org").unwrap(), (0, 0), "recipients are not senders");
        assert_eq!(s.count_from("spam@bad.example").unwrap(), (0, 0), "junk is skipped");
        assert_eq!(s.count_from("%").unwrap(), (0, 0));
        assert_eq!(s.count_from("").unwrap(), (0, 0));
    }

    #[test]
    fn like_wildcards_in_the_needle_are_literal() {
        let s = store();
        assert!(s.address_rows("%", 10).unwrap().is_empty());
        assert!(s.address_rows("_da", 10).unwrap().is_empty());
    }
}
