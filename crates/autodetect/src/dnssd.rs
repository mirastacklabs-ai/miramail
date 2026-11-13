//! iOS DNS backend: Apple's public DNS-SD API (`DNSServiceQueryRecord`, <dns_sd.h>, in libSystem).
//! It talks to mDNSResponder, i.e. the system resolver using the user's own configured DNS servers.
//! Needed because the iOS sandbox denies reading /etc/resolv.conf (hickory's source of nameservers).
//! Hand-written FFI, no extra crate. Result rdata is parsed by dnswire.rs.

use std::ffi::{c_char, c_int, c_short, c_uint, c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TYPE_MX: u16 = 15;
pub const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;

const FLAGS_MORE_COMING: u32 = 0x1; // kDNSServiceFlagsMoreComing: more results are queued for this call batch
const FLAGS_ADD: u32 = 0x2;
const FLAGS_RETURN_INTERMEDIATES: u32 = 0x1000; // also deliver NoSuchRecord instead of waiting
/// After the first batch ends, keep listening this long: mDNSResponder may deliver the rest of an RRset in later callbacks.
const GRACE: Duration = Duration::from_millis(100);
const ERR_NO_ERROR: i32 = 0;
const ERR_NO_SUCH_NAME: i32 = -65538;
const ERR_NO_SUCH_RECORD: i32 = -65554;

type DnsServiceRef = *mut c_void;
type Callback = unsafe extern "C" fn(
    DnsServiceRef,
    u32,         // flags
    u32,         // interface index
    i32,         // error code
    *const c_char, // full name
    u16,         // rrtype
    u16,         // rrclass
    u16,         // rdlen
    *const c_void, // rdata
    u32,         // ttl
    *mut c_void, // context
);

extern "C" {
    fn DNSServiceQueryRecord(
        sd_ref: *mut DnsServiceRef,
        flags: u32,
        interface_index: u32,
        fullname: *const c_char,
        rrtype: u16,
        rrclass: u16,
        callback: Callback,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd_ref: DnsServiceRef) -> c_int;
    fn DNSServiceProcessResult(sd_ref: DnsServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
    fn poll(fds: *mut PollFd, nfds: NfdsT, timeout_ms: c_int) -> c_int;
}

/// `nfds_t` is `unsigned int` on Apple platforms (<poll.h>).
type NfdsT = c_uint;

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}
const POLLIN: c_short = 0x1;

#[derive(Default)]
struct Ctx {
    want: u16,
    records: Vec<Vec<u8>>,
    /// Some(code) once a terminal answer arrived without records (NXDOMAIN / no such record) or an error.
    error: Option<i32>,
    /// An Add answer arrived without MoreComing: the RRset delivered so far is complete.
    complete: bool,
    /// The callback panicked: the query is failed whatever was collected.
    failed: bool,
}

unsafe extern "C" fn on_reply(
    _sd: DnsServiceRef,
    flags: u32,
    _if_index: u32,
    err: i32,
    _name: *const c_char,
    rrtype: u16,
    rrclass: u16,
    rdlen: u16,
    rdata: *const c_void,
    _ttl: u32,
    context: *mut c_void,
) {
    // A panic must never unwind across the C frame: catch it and mark the query failed.
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: context is the &Mutex<Ctx> owned by `query_blocking`, alive for the whole processing loop. A Mutex
        // (interior mutability) keeps the Rust side correct: the library mutates it through this pointer, not a `&mut`.
        let Ok(mut ctx) = (*(context as *const Mutex<Ctx>)).lock() else { return };
        // MoreComing is tracked on EVERY callback (error, removal, unrelated type too), not only the first or the
        // record ones: the batch is finished only when the last delivered callback says nothing more is queued.
        let more = flags & FLAGS_MORE_COMING != 0;
        apply_reply(&mut ctx, flags, err, rrtype, rrclass, more, || {
            if rdata.is_null() {
                None
            } else {
                Some(std::slice::from_raw_parts(rdata as *const u8, rdlen as usize).to_vec())
            }
        });
    }));
    if r.is_err() {
        // SAFETY: as above; a poisoned lock is still usable for setting the flag.
        let m = &*(context as *const Mutex<Ctx>);
        m.lock().unwrap_or_else(|p| p.into_inner()).failed = true;
    }
}

/// Callback logic, separated from the FFI so it can be tested.
fn apply_reply(
    ctx: &mut Ctx,
    flags: u32,
    err: i32,
    rrtype: u16,
    rrclass: u16,
    more: bool,
    rdata: impl FnOnce() -> Option<Vec<u8>>,
) {
    if err != ERR_NO_ERROR {
        ctx.error = Some(err);
    } else if rrclass == CLASS_IN && rrtype == ctx.want {
        if let Some(b) = rdata() {
            if flags & FLAGS_ADD != 0 {
                // The same answer arrives once per interface: keep one copy.
                if !ctx.records.contains(&b) {
                    ctx.records.push(b);
                }
            } else if let Some(i) = ctx.records.iter().position(|r| *r == b) {
                ctx.records.remove(i); // the record went away (no Add flag): drop the matching rdata
            }
        }
    }
    ctx.complete = !more && !ctx.records.is_empty();
}

/// Run one query; returns the raw rdata of every answer (empty = no records).
pub async fn query(name: String, rrtype: u16, budget: Duration) -> Result<Vec<Vec<u8>>, String> {
    // The deadline is fixed here, before the task waits for a blocking-pool thread, so queueing cannot extend it.
    let deadline = Instant::now() + budget;
    tokio::task::spawn_blocking(move || query_blocking(&name, rrtype, deadline))
        .await
        .map_err(|e| format!("dns task: {e}"))?
}

fn query_blocking(name: &str, rrtype: u16, deadline: Instant) -> Result<Vec<Vec<u8>>, String> {
    let cname = CString::new(name).map_err(|_| "bad DNS name".to_string())?;
    let ctx = Mutex::new(Ctx { want: rrtype, ..Default::default() });
    let mut sd: DnsServiceRef = std::ptr::null_mut();
    // SAFETY: valid out pointer, NUL-terminated name, callback has the C signature; ctx outlives the ref
    // (deallocated below before ctx is dropped).
    let rc = unsafe {
        DNSServiceQueryRecord(
            &mut sd,
            FLAGS_RETURN_INTERMEDIATES,
            0, // kDNSServiceInterfaceIndexAny
            cname.as_ptr(),
            rrtype,
            CLASS_IN,
            on_reply,
            &ctx as *const Mutex<Ctx> as *mut c_void,
        )
    };
    if rc != ERR_NO_ERROR || sd.is_null() {
        return Err(format!("DNS-SD query failed ({rc})"));
    }
    let result = drive(sd, &ctx, deadline);
    // SAFETY: sd came from a successful DNSServiceQueryRecord and is deallocated exactly once.
    unsafe { DNSServiceRefDeallocate(sd) };
    result?;
    let ctx = ctx.into_inner().map_err(|_| "DNS state poisoned".to_string())?;
    if ctx.failed {
        return Err("DNS-SD callback failed".into());
    }
    match ctx.error {
        Some(ERR_NO_SUCH_NAME) | Some(ERR_NO_SUCH_RECORD) if ctx.records.is_empty() => Ok(Vec::new()),
        Some(e) if ctx.records.is_empty() => Err(format!("DNS-SD error {e}")),
        _ => Ok(ctx.records),
    }
}

fn drive(sd: DnsServiceRef, ctx: &Mutex<Ctx>, deadline: Instant) -> Result<(), String> {
    // SAFETY: sd is a live ref.
    let fd = unsafe { DNSServiceRefSockFD(sd) };
    if fd < 0 {
        return Err("DNS-SD socket unavailable".into());
    }
    // Set once a batch ended with records: the RRset may still grow, so wait GRACE more (never past the deadline).
    let mut grace_end: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if grace_end.is_some_and(|g| now >= g) {
            return Ok(());
        }
        if now >= deadline {
            let c = ctx.lock().unwrap_or_else(|p| p.into_inner());
            return if c.records.is_empty() && c.error.is_none() { Err("DNS lookup timed out".into()) } else { Ok(()) };
        }
        let mut p = PollFd { fd, events: POLLIN, revents: 0 };
        let until = grace_end.map_or(deadline, |g| g.min(deadline));
        let ms = (until - now).as_millis().clamp(1, i32::MAX as u128) as c_int;
        // SAFETY: one valid pollfd.
        let n = unsafe { poll(&mut p, 1 as NfdsT, ms) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err("DNS-SD poll failed".into());
        }
        if n == 0 {
            continue; // loop re-checks the deadline
        }
        // SAFETY: sd is live; this invokes on_reply on this thread.
        let rc = unsafe { DNSServiceProcessResult(sd) };
        // A poisoned lock (callback panicked) is recovered so `failed` is still seen below.
        let c = ctx.lock().unwrap_or_else(|p| p.into_inner());
        if rc != ERR_NO_ERROR {
            // Keep what was already collected; only a failure with nothing to show is an error.
            return if c.records.is_empty() { Err(format!("DNS-SD process error ({rc})")) } else { Ok(()) };
        }
        if c.failed || c.error.is_some() {
            return Ok(());
        }
        grace_end = if c.complete { Some((Instant::now() + GRACE).min(deadline)) } else { None };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(ctx: &mut Ctx, rrtype: u16, more: bool, data: &[u8]) {
        let flags = FLAGS_ADD | if more { FLAGS_MORE_COMING } else { 0 };
        apply_reply(ctx, flags, ERR_NO_ERROR, rrtype, CLASS_IN, more, || Some(data.to_vec()));
    }

    #[test]
    fn complete_only_when_the_last_callback_has_no_more_coming() {
        let mut c = Ctx { want: TYPE_MX, ..Default::default() };
        add(&mut c, TYPE_MX, true, b"a");
        assert!(!c.complete);
        add(&mut c, TYPE_MX, false, b"b");
        assert!(c.complete);
        // a later callback that has MoreComing reopens the batch, even if it carries no wanted record
        apply_reply(&mut c, FLAGS_ADD, ERR_NO_ERROR, TYPE_SRV, CLASS_IN, true, || None);
        assert!(!c.complete);
        assert_eq!(c.records.len(), 2);
    }

    #[test]
    fn callback_without_add_removes_the_matching_record() {
        let mut c = Ctx { want: TYPE_MX, ..Default::default() };
        add(&mut c, TYPE_MX, true, b"a");
        add(&mut c, TYPE_MX, false, b"b");
        apply_reply(&mut c, 0, ERR_NO_ERROR, TYPE_MX, CLASS_IN, false, || Some(b"a".to_vec()));
        assert_eq!(c.records, vec![b"b".to_vec()]);
        // removal of an unknown rdata, or of another type, changes nothing
        apply_reply(&mut c, 0, ERR_NO_ERROR, TYPE_MX, CLASS_IN, false, || Some(b"zz".to_vec()));
        apply_reply(&mut c, 0, ERR_NO_ERROR, TYPE_SRV, CLASS_IN, false, || Some(b"b".to_vec()));
        assert_eq!(c.records.len(), 1);
        // removing the last one leaves nothing, so the batch is not "complete"
        apply_reply(&mut c, 0, ERR_NO_ERROR, TYPE_MX, CLASS_IN, false, || Some(b"b".to_vec()));
        assert!(c.records.is_empty() && !c.complete);
    }

    #[test]
    fn identical_answers_from_several_interfaces_are_kept_once() {
        let mut c = Ctx { want: TYPE_MX, ..Default::default() };
        add(&mut c, TYPE_MX, true, b"a");
        add(&mut c, TYPE_MX, false, b"a");
        assert_eq!(c.records, vec![b"a".to_vec()]);
        assert!(c.complete);
    }

    #[test]
    fn rrset_members_in_separate_batches_are_all_kept() {
        let mut c = Ctx { want: TYPE_MX, ..Default::default() };
        add(&mut c, TYPE_MX, false, b"a");
        assert!(c.complete); // drive() now starts the grace window instead of returning
        add(&mut c, TYPE_MX, false, b"b");
        assert_eq!(c.records.len(), 2);
    }

    #[test]
    fn nfds_is_c_uint() {
        assert_eq!(std::mem::size_of::<NfdsT>(), std::mem::size_of::<u32>());
    }
}
