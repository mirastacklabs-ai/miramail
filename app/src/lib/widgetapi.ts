// Home-screen widgets (iPhone): the privacy setting and widget taps. Backend: src-tauri/src/widget_snapshot.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./api";

export interface WidgetPrefs { showDetails: boolean }
export type WidgetTab = "mail" | "calendar";

let mock: WidgetPrefs = { showDetails: false };

export const widgetApi = {
  getPrefs: (): Promise<WidgetPrefs> => (isTauri ? invoke("get_widget_prefs") : Promise.resolve({ ...mock })),
  /** Sender names, subjects and event titles in widgets (default off: widgets then show counts, times and "Busy"). */
  setShowDetails: (showDetails: boolean): Promise<WidgetPrefs> => {
    if (isTauri) return invoke("set_widget_prefs", { showDetails });
    mock = { showDetails };
    return Promise.resolve({ ...mock });
  },
  /** The tab a widget tap asked for (returned once), else null. */
  takePendingTab: (): Promise<WidgetTab | null> => (isTauri ? invoke<WidgetTab | null>("take_pending_tab") : Promise.resolve(null)),
};

export const onOpenTab = async (cb: () => void): Promise<() => void> => (isTauri ? listen("app://open-tab", () => cb()) : () => {});
