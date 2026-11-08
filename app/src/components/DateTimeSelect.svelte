<script lang="ts">
  // Branded date / time / date+time picker (no native macOS popup): day, hour, minute dropdowns built on Select.
  // `value` and `onchange` use the same strings as the native inputs: date "YYYY-MM-DD", time "HH:mm",
  // datetime "YYYY-MM-DDTHH:mm" (local time). The current value is always offered, even outside the range.
  import Select from "./Select.svelte";

  let {
    value, onchange, idPrefix = "dts", mode = "datetime", days = 90, from = 0, past = false, min, max,
    allowEmpty = false, emptyLabel = "None", step = 5, ariaPrefix = "", disabled = false,
  }: {
    value: string; onchange: (v: string) => void; idPrefix?: string;
    mode?: "datetime" | "date" | "time";
    /** How many days to offer. */
    days?: number;
    /** Offset in days from today of the first day offered (forward ranges). */
    from?: number;
    /** Offer today and the days before it (newest first) instead of days ahead. */
    past?: boolean;
    /** Inclusive limits, "YYYY-MM-DD". */
    min?: string; max?: string;
    /** Offer an empty choice (optional dates). */
    allowEmpty?: boolean; emptyLabel?: string;
    /** Minute step. */
    step?: number;
    /** Prefix for the accessible names, e.g. "Start". */
    ariaPrefix?: string;
    disabled?: boolean;
  } = $props();

  const p2 = (n: number) => String(n).padStart(2, "0");
  const ymd = (d: Date) => `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
  const nm = (s: string) => (ariaPrefix ? `${ariaPrefix} ${s}` : s[0].toUpperCase() + s.slice(1));
  const parts = $derived.by(() => {
    if (mode === "date") return { d: /^\d{4}-\d{2}-\d{2}$/.test(value) ? value : "", h: "09", mi: "00" };
    if (mode === "time") { const m = /^(\d{2}):(\d{2})$/.exec(value); return { d: "", h: m ? m[1] : "09", mi: m ? m[2] : "00" }; }
    const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2})$/.exec(value);
    return m ? { d: m[1], h: m[2], mi: m[3] } : { d: "", h: "09", mi: "00" };
  });
  const dayOpts = $derived.by(() => {
    const t0 = new Date(); t0.setHours(12, 0, 0, 0);
    const yr = t0.getFullYear();
    const label = (key: string) => {
      const [y, mo, da] = key.split("-").map(Number);
      const d = new Date(y, mo - 1, da, 12);
      const diff = Math.round((d.getTime() - t0.getTime()) / 86_400_000);
      const base = d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short", ...(y !== yr ? { year: "numeric" } : {}) });
      const rel = diff === 0 ? "Today" : diff === 1 ? "Tomorrow" : diff === -1 ? "Yesterday" : "";
      return rel ? `${rel} · ${d.toLocaleDateString(undefined, { day: "numeric", month: "short" })}` : base;
    };
    const keys = new Set<string>();
    for (let i = 0; i < days; i++) {
      const d = new Date(t0); d.setDate(t0.getDate() + (past ? -i : from + i));
      const k = ymd(d);
      if ((min && k < min) || (max && k > max)) continue;
      keys.add(k);
    }
    if (parts.d) keys.add(parts.d);
    const sorted = [...keys].sort();
    if (past) sorted.reverse();
    const out: { value: string; label: string }[] = sorted.map((k) => ({ value: k, label: label(k) }));
    if (allowEmpty) out.unshift({ value: "", label: emptyLabel });
    return out;
  });
  const hours = Array.from({ length: 24 }, (_, i) => ({ value: p2(i), label: p2(i) }));
  const mins = $derived.by(() => {
    const s = Math.max(1, Math.floor(step));
    const o: { value: string; label: string }[] = [];
    for (let m = 0; m < 60; m += s) o.push({ value: p2(m), label: p2(m) });
    if (!o.some((x) => x.value === parts.mi)) o.push({ value: parts.mi, label: parts.mi });
    return o.sort((a, b) => a.value.localeCompare(b.value));
  });
  function emit(d: string, h: string, mi: string) {
    if (mode === "date") return onchange(d);
    if (mode === "time") return onchange(`${h}:${mi}`);
    if (!d && allowEmpty) return onchange("");
    onchange(`${d || ymd(new Date())}T${h}:${mi}`);
  }
  const timeOff = $derived(disabled || (mode === "datetime" && allowEmpty && !parts.d));
</script>

<div class="dts" class:row={mode === "date" || mode === "time"}>
  {#if mode !== "time"}
    <Select id={mode === "date" ? idPrefix : `${idPrefix}-d`} value={parts.d} options={dayOpts} ariaLabel={nm("day")} {disabled} placeholder={emptyLabel} onchange={(v) => emit(v, parts.h, parts.mi)} />
  {/if}
  {#if mode !== "date"}
    <div class="tm">
      <Select id={mode === "time" ? idPrefix : `${idPrefix}-h`} value={parts.h} options={hours} ariaLabel={nm("hour")} compact block={false} disabled={timeOff} onchange={(v) => emit(parts.d, v, parts.mi)} />
      <span aria-hidden="true">:</span>
      <Select id="{idPrefix}-m" value={parts.mi} options={mins} ariaLabel={nm("minute")} compact block={false} disabled={timeOff} onchange={(v) => emit(parts.d, parts.h, v)} />
    </div>
  {/if}
</div>

<style>
  .dts { display: grid; gap: 6px; min-width: 0; }
  .dts.row { display: block; }
  .tm { display: flex; align-items: center; gap: 6px; color: var(--muted); }
</style>
