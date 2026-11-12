//! Wire-format (RFC 1035 / 2782) rdata parsers for MX and SRV. Platform-independent and pure so they are
//! unit-tested on every host; only the iOS DNS-SD backend (dnssd.rs) feeds them.
//!
//! DNS-SD hands over bare rdata (no message context), so compression pointers cannot be followed and are
//! rejected. Every read is bounds-checked; malformed input returns `None`, never panics.

// Only the iOS backend calls these outside tests.
#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use crate::srv::SrvRecord;

/// Read an uncompressed name starting at `pos`; returns (name with trailing dot, position after the name).
pub(crate) fn read_name(buf: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    loop {
        let len = *buf.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            if out.is_empty() {
                out.push('.');
            }
            return Some((out, pos));
        }
        if len > 63 {
            return None; // compression pointer (0xC0..) or reserved label type
        }
        let label = buf.get(pos..pos.checked_add(len)?)?;
        pos += len;
        if out.len() + len + 1 > 254 {
            return None;
        }
        // Host names only (letters, digits, hyphen, plus underscore for service labels); anything else, including
        // an embedded dot or non-ASCII bytes, is malformed rather than silently re-interpreted.
        if !label.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_') {
            return None;
        }
        out.push_str(std::str::from_utf8(label).ok()?);
        out.push('.');
    }
}

/// MX rdata: u16 preference, then the exchange name. The name must end exactly at the end of rdata.
pub(crate) fn parse_mx(rdata: &[u8]) -> Option<(u16, String)> {
    let pref = u16::from_be_bytes([*rdata.first()?, *rdata.get(1)?]);
    let (name, end) = read_name(rdata, 2)?;
    (end == rdata.len()).then_some((pref, name))
}

/// SRV rdata: u16 priority, u16 weight, u16 port, then the target name.
pub(crate) fn parse_srv(rdata: &[u8]) -> Option<SrvRecord> {
    let word = |i: usize| Some(u16::from_be_bytes([*rdata.get(i)?, *rdata.get(i + 1)?]));
    let (priority, weight, port) = (word(0)?, word(2)?, word(4)?);
    let (target, end) = read_name(rdata, 6)?;
    (end == rdata.len()).then_some(SrvRecord { priority, weight, port, target })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(labels: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for l in labels {
            v.push(l.len() as u8);
            v.extend_from_slice(l.as_bytes());
        }
        v.push(0);
        v
    }

    #[test]
    fn mx_ok() {
        let mut r = vec![0, 10];
        r.extend(name(&["mx", "zoho", "com"]));
        assert_eq!(parse_mx(&r), Some((10, "mx.zoho.com.".into())));
    }

    #[test]
    fn mx_malformed() {
        assert_eq!(parse_mx(&[]), None);
        assert_eq!(parse_mx(&[0]), None);
        assert_eq!(parse_mx(&[0, 10]), None); // no name
        let mut r = vec![0, 10];
        r.extend(name(&["mx", "zoho", "com"]));
        for cut in 2..r.len() {
            assert_eq!(parse_mx(&r[..cut]), None, "cut at {cut}");
        }
        r.push(0); // trailing garbage
        assert_eq!(parse_mx(&r), None);
        assert_eq!(parse_mx(&[0, 10, 0xC0, 0x0C]), None); // compression pointer
        assert_eq!(parse_mx(&[0, 10, 70, b'a']), None); // label > 63
    }

    #[test]
    fn mx_root_exchange_is_null_mx() {
        assert_eq!(parse_mx(&[0, 0, 0]), Some((0, ".".into())));
    }

    #[test]
    fn srv_ok() {
        let mut r = vec![0, 1, 0, 5, 0x03, 0xE1];
        r.extend(name(&["imap", "fastmail", "com"]));
        assert_eq!(
            parse_srv(&r),
            Some(SrvRecord { priority: 1, weight: 5, port: 993, target: "imap.fastmail.com.".into() })
        );
    }

    #[test]
    fn srv_unavailable_target_dot() {
        let r = vec![0, 0, 0, 0, 0, 0, 0];
        let s = parse_srv(&r).unwrap();
        assert_eq!((s.port, s.target.as_str()), (0, "."));
    }

    #[test]
    fn srv_malformed() {
        assert_eq!(parse_srv(&[]), None);
        assert_eq!(parse_srv(&[0, 1, 0, 5, 3]), None);
        let mut r = vec![0, 1, 0, 5, 0x03, 0xE1];
        r.extend(name(&["imap", "x"]));
        for cut in 0..r.len() {
            assert_eq!(parse_srv(&r[..cut]), None, "cut at {cut}");
        }
        r.extend([1, 2, 3]);
        assert_eq!(parse_srv(&r), None);
        // name overflows 255 bytes
        let mut big = vec![0, 1, 0, 5, 0, 80];
        for _ in 0..5 {
            big.push(60);
            big.extend(std::iter::repeat_n(b'a', 60));
        }
        big.push(0);
        assert_eq!(parse_srv(&big), None);
    }

    fn raw_labels(labels: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for l in labels {
            v.push(l.len() as u8);
            v.extend_from_slice(l);
        }
        v.push(0);
        v
    }

    #[test]
    fn label_length_limits() {
        let l63 = "a".repeat(63);
        assert_eq!(read_name(&name(&[&l63]), 0), Some((format!("{l63}."), 65)));
        let mut over = vec![64u8];
        over.extend(std::iter::repeat_n(b'a', 64));
        over.push(0);
        assert_eq!(read_name(&over, 0), None);
    }

    #[test]
    fn name_length_limits() {
        // 63 + 63 + 63 + 61 label bytes + 3 separating dots = 253 characters: the maximum
        let ok = name(&[&"a".repeat(63), &"b".repeat(63), &"c".repeat(63), &"d".repeat(61)]);
        let (n, _) = read_name(&ok, 0).expect("253-char name");
        assert_eq!(n.len(), 254); // with trailing dot
        // one more character = 254: rejected
        let bad = name(&[&"a".repeat(63), &"b".repeat(63), &"c".repeat(63), &"d".repeat(62)]);
        assert_eq!(read_name(&bad, 0), None);
    }

    #[test]
    fn reserved_label_types_rejected() {
        assert_eq!(read_name(&[0x40, 0], 0), None); // extended label type 01
        assert_eq!(read_name(&[0x80, 0], 0), None); // reserved type 10
        assert_eq!(read_name(&[0xC0, 0x00], 0), None); // compression pointer
    }

    #[test]
    fn non_ldh_bytes_rejected() {
        assert_eq!(read_name(&raw_labels(&[b"a.b"]), 0), None); // embedded dot
        assert_eq!(read_name(&raw_labels(&[b"a b"]), 0), None);
        assert_eq!(read_name(&raw_labels(&[&[b'a', 0x80]]), 0), None);
        assert_eq!(read_name(&raw_labels(&[&[0xC3, 0xBC]]), 0), None); // raw UTF-8 (IDN must be A-labels)
        assert_eq!(read_name(&raw_labels(&[b"a\\x"]), 0), None);
        assert_eq!(read_name(&raw_labels(&[b"xn--mnchen-3ya", b"de"]), 0).unwrap().0, "xn--mnchen-3ya.de.");
        assert_eq!(read_name(&raw_labels(&[b"_imaps", b"_tcp", b"a-1"]), 0).unwrap().0, "_imaps._tcp.a-1.");
    }
}
