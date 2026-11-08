// In-memory calendar + Zoom mock (plain-browser dev only). Mirrors docs/CALENDAR_SPEC.md semantics loosely.
import type { CalEvent, CalendarInfo, Invite, Occurrence, ZoomMeeting, ZoomStatus, RespondStatus, CalPreset, CaldavSuggestion, ReusableAccount } from "./types";
import { addDays, addMonths, dayKey, localTz, sod, toRfc } from "./caldate";

const wait = <T,>(v: T, ms = 100) => new Promise<T>((r) => setTimeout(() => r(structuredClone(v)), ms));
const changed = () => window.dispatchEvent(new Event("mock-cal-changed"));
const tz = localTz();
const at = (dayOffset: number, h: number, m = 0) => { const d = addDays(sod(new Date()), dayOffset); d.setHours(h, m, 0, 0); return toRfc(d.getTime(), tz); };
const day = (dayOffset: number) => dayKey(addDays(sod(new Date()), dayOffset));

let calendars: CalendarInfo[] = [
  { id: "local-personal", name: "Personal", color: "#2bd4bf", kind: "local", url: null, username: null, visible: true, readOnly: false, lastSync: null, error: null },
  { id: "caldav-work", name: "Work (Fastmail)", color: "#ff671f", kind: "caldav", url: "https://caldav.fastmail.com/dav/calendars/user/you@example.org/work/", username: "you@example.org", visible: true, readOnly: false, lastSync: Math.floor(Date.now() / 1000) - 300, error: null },
];
const base = (uid: string, calendarId: string, summary: string, start: string, end: string, extra: Partial<CalEvent> = {}): CalEvent => ({
  uid, calendarId, summary, description: "", location: "", start, end, allDay: false, tzid: tz, rrule: null, exdates: [], attendees: [],
  organizer: null, status: "confirmed", reminderMinutes: 10, zoom: null, sequence: 0, href: null, etag: null, ...extra,
});
const zoomSample: ZoomMeeting = { meetingId: "81234567890", joinUrl: "https://zoom.us/j/81234567890?pwd=mock", password: "424242", startUrl: null };
let events: CalEvent[] = [
  base("ev-standup", "caldav-work", "Team stand-up", at(0, 9, 30), at(0, 9, 45), { rrule: "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR", reminderMinutes: 5 }),
  base("ev-review", "caldav-work", "Design review", at(0, 10), at(0, 11, 30), {
    location: zoomSample.joinUrl, description: `Review the calendar UI.\n\nJoin Zoom Meeting\n${zoomSample.joinUrl}\nPasscode: ${zoomSample.password}`, zoom: zoomSample,
    attendees: [{ email: "ada@analytical.example", name: "Ada Lovelace", role: "required", status: "accepted" }, { email: "grace@navy.example", name: "Grace Hopper", role: "optional", status: "needs-action" }],
  }),
  base("ev-sync", "caldav-work", "Roadmap sync", at(0, 10, 30), at(0, 12), { location: "Room 4" }),
  base("ev-lunch", "local-personal", "Lunch with Linus", at(1, 12, 30), at(1, 13, 30), { location: "Cafe Alpha" }),
  base("ev-trip", "local-personal", "Offsite", day(2), day(4), { allDay: true, tzid: null, location: "Lisbon" }),
  base("ev-bday", "local-personal", "Birthday: Ada", day(5), day(6), { allDay: true, tzid: null, rrule: "FREQ=YEARLY" }),
  base("ev-gym", "local-personal", "Gym", at(-1, 18), at(-1, 19), { rrule: "FREQ=WEEKLY;BYDAY=MO,WE,FR" }),
  base("ev-month", "local-personal", "Pay rent", at(3, 8), at(3, 8, 30), { rrule: "FREQ=MONTHLY;BYMONTHDAY=" + addDays(sod(new Date()), 3).getDate() }),
];
const calColor = (id: string) => calendars.find((c) => c.id === id)?.color ?? "#2bd4bf";

const DOW = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
function expand(ev: CalEvent, from: Date, to: Date): { s: Date; e: Date }[] {
  const s0 = ev.allDay ? new Date(ev.start + "T00:00:00") : new Date(ev.start);
  const e0 = ev.allDay ? new Date(ev.end + "T00:00:00") : new Date(ev.end);
  const dur = e0.getTime() - s0.getTime();
  const out: { s: Date; e: Date }[] = [];
  const push = (s: Date) => {
    const e = new Date(s.getTime() + dur);
    if (e > from && s < to && !ev.exdates.includes(ev.allDay ? dayKey(s) : toRfc(s.getTime(), ev.tzid ?? tz))) out.push({ s, e });
  };
  if (!ev.rrule) { push(s0); return out; }
  const r = Object.fromEntries(ev.rrule.replace(/^RRULE:/, "").split(";").map((p) => p.split("=") as [string, string]));
  const interval = Number(r.INTERVAL ?? 1);
  const count = r.COUNT ? Number(r.COUNT) : Infinity;
  const until = r.UNTIL ? new Date(`${r.UNTIL.slice(0, 4)}-${r.UNTIL.slice(4, 6)}-${r.UNTIL.slice(6, 8)}T23:59:59`) : null;
  const byday = r.BYDAY ? r.BYDAY.split(",").map((x) => DOW.indexOf(x.slice(-2))) : null;
  const withTime = (d: Date) => { const x = new Date(d); x.setHours(s0.getHours(), s0.getMinutes(), 0, 0); return x; };
  let n = 0;
  for (let i = 0; i < 5000; i++) {
    let cands: Date[] = [];
    if (r.FREQ === "DAILY") cands = [withTime(addDays(sod(s0), i * interval))];
    else if (r.FREQ === "WEEKLY") {
      const wk = addDays(sod(s0), -s0.getDay() + i * 7 * interval);
      cands = (byday ?? [s0.getDay()]).map((d) => withTime(addDays(wk, d)));
    } else if (r.FREQ === "MONTHLY") {
      const m = addMonths(new Date(s0.getFullYear(), s0.getMonth(), 1), i * interval);
      const dom = r.BYMONTHDAY ? Number(r.BYMONTHDAY) : s0.getDate();
      const x = withTime(new Date(m.getFullYear(), m.getMonth(), dom)); if (x.getMonth() === m.getMonth()) cands = [x];
    } else if (r.FREQ === "YEARLY") {
      const x = withTime(new Date(s0.getFullYear() + i * interval, s0.getMonth(), s0.getDate())); cands = [x];
    } else return out;
    for (const c of cands.sort((a, b) => a.getTime() - b.getTime())) {
      if (c < s0) continue;
      if (until && c > until) return out;
      if (++n > count) return out;
      push(c);
    }
    if (cands.length && cands[0] > to) break;
  }
  return out;
}

const zoomStatus: ZoomStatus = { configured: false, connected: false, clientId: null, account: null, redirectUri: "http://127.0.0.1:53682/callback" };
let meetingN = 0;
let inviteStatus: Invite["myStatus"] = "needs-action";
let inviteImported = false;
export const mockInviteEvent = (): CalEvent => base("invite-1", "", "Compiler timeline review", at(3, 15), at(3, 16), {
  location: zoomSample.joinUrl, zoom: zoomSample, description: "Review the A-0 timeline.\n\nBring the punch cards.",
  organizer: { email: "grace@navy.example", name: "Grace Hopper", role: "chair", status: "accepted" },
  attendees: [{ email: "grace@navy.example", name: "Grace Hopper", role: "chair", status: "accepted" }, { email: "you@example.org", name: null, role: "required", status: inviteStatus ?? "needs-action" }, { email: "ada@analytical.example", name: "Ada", role: "required", status: "needs-action" }],
});
export const mockInvite = (): Invite => ({ method: "REQUEST", event: mockInviteEvent(), myStatus: inviteStatus, alreadyInCalendar: inviteImported, attachmentId: "inv1" });

export const calMock = {
  cal_list_calendars: () => wait(calendars),
  async cal_create_local(a: { name: string; color: string }) {
    const c: CalendarInfo = { id: "local-" + Math.random().toString(36).slice(2, 8), name: a.name, color: a.color, kind: "local", url: null, username: null, visible: true, readOnly: false, lastSync: null, error: null };
    calendars.push(c); changed(); return wait(c);
  },
  async cal_suggest_caldav(): Promise<CaldavSuggestion> {
    return wait({ preset: "zoho", region: "in", url: "https://calendar.zoho.in/", username: "you@example.org" }, 80);
  },
  async cal_reusable_accounts(a: { preset: CalPreset }): Promise<ReusableAccount[]> {
    return wait(a.preset === "zoho" ? [{ email: "you@example.org", provider: "zoho" }] : [], 60);
  },
  async cal_add_caldav(a: { preset: CalPreset; url?: string; region?: string; username?: string; password?: string; reuseMailAccount?: string }) {
    if (a.reuseMailAccount) {
      if (a.reuseMailAccount !== "you@example.org") throw { message: "That mail account is not one of your saved accounts.", diagnostics: [] };
      a = { ...a, username: a.reuseMailAccount, password: "mail" };
    }
    a = { ...a, username: a.username ?? "", password: a.password ?? "" };
    const step = (s: string) => window.dispatchEvent(new CustomEvent("mock-cal-progress", { detail: s }));
    step("Looking up your calendar server"); await wait(null, 400);
    step("Signing in"); await wait(null, 400);
    const trail = [`step: Looking up your calendar server`, `PROPFIND calendar.zoho.${a.region || "com"}/.well-known/caldav -> HTTP 207`];
    if (a.password === "wrong") {
      throw { message: "Signing in failed: the server rejected your username or password (401). Check the password. If you use two-factor authentication, create an app-specific password and use that instead.", diagnostics: [...trail, "PROPFIND /.well-known/caldav -> HTTP 401"] };
    }
    if (a.preset === "custom" && !/^https:\/\//i.test(a.url ?? "")) throw { message: "CalDAV URLs must start with https://", diagnostics: [] };
    const c: CalendarInfo = { id: "caldav-" + Math.random().toString(36).slice(2, 8), name: `${a.preset} calendar`, color: "#a78bfa", kind: "caldav", url: a.url ?? `https://${a.preset}.example/dav/`, username: a.username ?? null, visible: true, readOnly: false, lastSync: Math.floor(Date.now() / 1000), error: null };
    step("Found 1 calendar"); await wait(null, 200);
    calendars.push(c); changed(); return wait([c]);
  },
  async cal_update_calendar(a: { id: string; name?: string; color?: string; visible?: boolean }) {
    const c = calendars.find((x) => x.id === a.id); if (c) { if (a.name != null) c.name = a.name; if (a.color != null) c.color = a.color; if (a.visible != null) c.visible = a.visible; }
    changed(); return wait(undefined, 30);
  },
  async cal_remove_calendar(a: { id: string }) { calendars = calendars.filter((c) => c.id !== a.id); events = events.filter((e) => e.calendarId !== a.id); changed(); return wait(undefined, 50); },
  async cal_sync() { await wait(null, 700); for (const c of calendars) if (c.kind === "caldav") c.lastSync = Math.floor(Date.now() / 1000); changed(); return { calendars: calendars.length, events: events.length }; },
  cal_list_events(a: { start: string; end: string }): Promise<Occurrence[]> {
    const from = new Date(a.start), to = new Date(a.end);
    const out: Occurrence[] = [];
    for (const ev of events) {
      if (!calendars.find((c) => c.id === ev.calendarId)?.visible) continue;
      for (const { s, e } of expand(ev, from, to)) {
        out.push({ uid: ev.uid, calendarId: ev.calendarId, recurrenceStart: ev.allDay ? dayKey(s) : toRfc(s.getTime(), ev.tzid ?? tz),
          start: ev.allDay ? dayKey(s) : toRfc(s.getTime(), ev.tzid ?? tz), end: ev.allDay ? dayKey(e) : toRfc(e.getTime(), ev.tzid ?? tz),
          allDay: ev.allDay, summary: ev.summary, location: ev.location, color: calColor(ev.calendarId), hasZoom: !!ev.zoom, isRecurring: !!ev.rrule });
      }
    }
    return wait(out, 80);
  },
  async cal_get_event(a: { uid: string; calendarId: string }) {
    const e = events.find((x) => x.uid === a.uid && x.calendarId === a.calendarId); if (!e) throw "That event no longer exists."; return wait(e, 40);
  },
  async cal_save_event(a: { event: CalEvent; sendInvites: boolean }) {
    await wait(null, 250);
    const ev = structuredClone(a.event);
    if (!ev.uid) ev.uid = "ev-" + Math.random().toString(36).slice(2, 10) + "@miramail";
    const i = events.findIndex((x) => x.uid === ev.uid);
    ev.sequence = i >= 0 ? events[i].sequence + 1 : 0;
    if (i >= 0) events[i] = ev; else events.push(ev);
    changed(); return structuredClone(ev);
  },
  async cal_delete_event(a: { uid: string; calendarId: string }) { events = events.filter((e) => !(e.uid === a.uid && e.calendarId === a.calendarId)); changed(); return wait(undefined, 80); },
  async cal_respond_invite(a: { status: RespondStatus }) {
    await wait(null, 400); inviteStatus = a.status; inviteImported = true;
    if (!events.find((e) => e.uid === "invite-1")) events.push({ ...mockInviteEvent(), calendarId: "local-personal" });
    changed();
  },
  async cal_import_invite() {
    await wait(null, 300); inviteImported = true;
    const ev = { ...mockInviteEvent(), calendarId: "local-personal" };
    if (!events.find((e) => e.uid === ev.uid)) events.push(ev);
    changed(); return structuredClone(ev);
  },
  zoom_status: () => wait(zoomStatus),
  async zoom_set_client(a: { clientId: string }) { zoomStatus.clientId = a.clientId || null; zoomStatus.configured = !!a.clientId; return wait(undefined, 50); },
  async zoom_connect() {
    if (!zoomStatus.configured) throw "Save a Client ID first.";
    await wait(null, 900); zoomStatus.connected = true; zoomStatus.account = "you@example.org"; return wait(zoomStatus, 10);
  },
  async zoom_disconnect() { zoomStatus.connected = false; zoomStatus.account = null; return wait(undefined, 100); },
  async zoom_create_meeting(a: { topic: string }): Promise<ZoomMeeting> {
    if (!zoomStatus.connected) throw "Zoom is not connected.";
    await wait(null, 600);
    const id = String(81000000000 + ++meetingN * 137);
    return { meetingId: id, joinUrl: `https://zoom.us/j/${id}?pwd=mock${meetingN}`, password: String(100000 + meetingN * 7919).slice(0, 6), startUrl: null };
  },
  zoom_delete_meeting: () => wait(undefined, 100),
};
