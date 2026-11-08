// Typed gateway to the vacation commands (src-tauri/src/vacation.rs). In a plain browser an in-memory fake answers.
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./api";
import type { DisableOutcome, EnableOutcome, SieveOverride, VacationSettings, VacationStatus } from "./vacation";

let fake: { settings: VacationSettings | null; active: boolean; other: string | null } = { settings: null, active: false, other: "main" };

async function mockCall(cmd: string, a: Record<string, unknown>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 150));
  if (cmd === "vacation_status") return { kind: "ready", active: fake.active, settings: fake.settings, otherActive: fake.active ? null : fake.other, datesSupported: true } satisfies VacationStatus;
  if (cmd === "vacation_enable") {
    if (!fake.active && fake.other && !a.confirmReplace) return { kind: "needs_confirm", active: fake.other } satisfies EnableOutcome;
    const replaced = fake.active ? null : fake.other;
    fake = { ...fake, settings: a.settings as VacationSettings, active: true };
    return { kind: "done", replaced } satisfies EnableOutcome;
  }
  fake.active = false;
  return { restored: a.restorePrevious ? fake.other : null, wasActive: true } satisfies DisableOutcome;
}

async function call<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  return (await mockCall(cmd, JSON.parse(JSON.stringify(args)) as Record<string, unknown>)) as T;
}

export const vacationApi = {
  status: (account: string, server?: SieveOverride) => call<VacationStatus>("vacation_status", { account, server: server ?? null }),
  enable: (account: string, settings: VacationSettings, confirmReplace: boolean, server?: SieveOverride) =>
    call<EnableOutcome>("vacation_enable", { account, settings, confirmReplace, server: server ?? null }),
  disable: (account: string, restorePrevious: boolean, server?: SieveOverride) =>
    call<DisableOutcome>("vacation_disable", { account, restorePrevious, server: server ?? null }),
};
