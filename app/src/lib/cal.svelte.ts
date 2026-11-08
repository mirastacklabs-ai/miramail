import { api, onCalChanged } from "./api";
import { app } from "./state.svelte";
import { workspace } from "./workspace.svelte";
import { rangeFor, sod, stepAnchor, titleFor, localTz, toRfc, weekStartDay, type CalMode } from "./caldate";
import type { CalEvent, CalendarInfo, Occurrence, ZoomStatus } from "./types";

export interface EditorInit {
  uid?: string; calendarId?: string; start?: Date; end?: Date; allDay?: boolean;
}
export const CAL_COLORS = ["#2bd4bf", "#ff671f", "#a78bfa", "#60a5fa", "#f472b6", "#facc15", "#4ade80"];

class CalState {
  calendars = $state<CalendarInfo[]>([]);
  occ = $state<Occurrence[]>([]);
  mode = $state<CalMode>("week");
  anchor = $state(sod(new Date()));
  loading = $state(false);
  loaded = $state(false);
  syncing = $state(false);
  popover = $state<{ occ: Occurrence; x: number; y: number } | null>(null);
  zoom = $state<ZoomStatus | null>(null);
  zoomOpen = $state(false);
  addOpen = $state(false);
  ws = weekStartDay();
  private token = 0;
  private started = false;

  get range(): [Date, Date] { return rangeFor(this.mode, this.anchor, this.ws); }
  get title() { return titleFor(this.mode, this.anchor, this.ws); }
  get defaultCalendar(): CalendarInfo | undefined { return this.calendars.find((c) => !c.readOnly && c.visible) ?? this.calendars.find((c) => !c.readOnly); }
  /** True while any modal/popover owned by the calendar is open. */
  get modalOpen() { return !!(this.zoomOpen || this.addOpen); }

  async start() {
    if (this.started) return;
    this.started = true;
    void onCalChanged(() => { void this.refresh(); });
    await this.refreshCalendars();
    void this.refreshZoom();
    await this.load();
  }

  async refreshCalendars() {
    try { this.calendars = await api.calListCalendars(); } catch (e) { app.fail(e); }
  }
  async refreshZoom() { try { this.zoom = await api.zoomStatus(); } catch (e) { app.fail(e); } }
  async refresh() { await Promise.all([this.refreshCalendars(), this.load()]); }

  async load() {
    const t = ++this.token;
    this.loading = !this.loaded;
    const [s, e] = this.range;
    try {
      const rows = await api.calListEvents(toRfc(s.getTime(), localTz()), toRfc(e.getTime(), localTz()));
      if (t === this.token) { this.occ = rows; this.loaded = true; }
    } catch (err) { if (t === this.token) app.fail(err); }
    if (t === this.token) this.loading = false;
  }

  go(date: Date) { this.anchor = sod(date); this.popover = null; void this.load(); }
  today() { this.go(new Date()); }
  step(dir: 1 | -1) { this.anchor = stepAnchor(this.mode, this.anchor, dir); this.popover = null; void this.load(); }
  setMode(m: CalMode) { this.mode = m; this.popover = null; void this.load(); }
  newEvent(init: EditorInit = {}) {
    this.popover = null;
    if (!this.defaultCalendar) { app.notify("Add or enable a writable calendar first.", "error"); return; }
    const d = new Date(); d.setMinutes(0, 0, 0); d.setHours(d.getHours() + 1);
    const base = sameAnchorDay(this.anchor) ? d : (() => { const x = new Date(this.anchor); x.setHours(9, 0, 0, 0); return x; })();
    const o = { calendarId: this.defaultCalendar.id, start: base, end: new Date(base.getTime() + 3600000), ...init };
    workspace.openEvent({ uid: o.uid, calendarId: o.calendarId, start: o.start?.getTime(), end: o.end?.getTime(), allDay: o.allDay });
  }
  edit(o: Occurrence) { this.popover = null; workspace.openEvent({ uid: o.uid, calendarId: o.calendarId }); }

  async sync(calendarId?: string) {
    if (this.syncing) return;
    this.syncing = true;
    try {
      const r = await api.calSync(calendarId);
      app.notify(`Synced ${r.calendars} calendar${r.calendars === 1 ? "" : "s"}`);
    } catch (e) { app.fail(e); }
    this.syncing = false;
    await this.refresh();
  }

  async setVisible(c: CalendarInfo, v: boolean) {
    c.visible = v;
    try { await api.calUpdateCalendar(c.id, { visible: v }); await this.load(); } catch (e) { c.visible = !v; app.fail(e); }
  }
  async rename(c: CalendarInfo, name: string) {
    const n = name.trim(); if (!n || n === c.name) return;
    try { await api.calUpdateCalendar(c.id, { name: n }); c.name = n; } catch (e) { app.fail(e); }
  }
  async remove(c: CalendarInfo) {
    const ok = await app.ask({ title: "Remove calendar?", body: c.kind === "local" ? `“${c.name}” and its events will be deleted from this Mac.` : `“${c.name}” and its saved password will be removed from this Mac. Events on the server are not touched.`, ok: "Remove", danger: true });
    if (!ok) return;
    try { await api.calRemoveCalendar(c.id); await this.refresh(); } catch (e) { app.fail(e); }
  }

  async deleteEvent(uid: string, calendarId: string, notify: boolean) {
    try { await api.calDeleteEvent(uid, calendarId, notify); this.popover = null; await this.load(); app.notify("Event deleted"); return true; }
    catch (e) { app.fail(e); return false; }
  }
  async save(ev: CalEvent, sendInvites: boolean): Promise<boolean> {
    try { await api.calSaveEvent(ev, sendInvites); await this.load(); app.notify(sendInvites ? "Saved and invitations sent" : "Event saved"); return true; }
    catch (e) { app.fail(e); return false; }
  }
}
function sameAnchorDay(a: Date) { return sod(new Date()).getTime() === a.getTime(); }
export const cal = new CalState();
