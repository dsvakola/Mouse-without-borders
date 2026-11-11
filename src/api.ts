// The only place that talks to Rust.
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { AppInfo, AppState, ApplyRequest } from "./types";

const inTauri = "__TAURI_INTERNALS__" in window;

export async function getState(): Promise<AppState> {
  if (!inTauri && import.meta.env.DEV) return (await import("./mock")).mockState();
  return invoke<AppState>("get_state");
}

/** Returns true when the app is restarting. */
export async function applySettings(req: ApplyRequest): Promise<boolean> {
  if (!inTauri && import.meta.env.DEV) return (await import("./mock")).mockApply(req);
  return invoke<boolean>("apply_settings", { req });
}

export async function getAppInfo(): Promise<AppInfo> {
  if (!inTauri && import.meta.env.DEV) return (await import("./mock")).mockInfo();
  return invoke<AppInfo>("get_app_info");
}

export async function hideWindow(): Promise<void> {
  if (inTauri) await invoke("hide_window");
}

export async function openLink(url: string): Promise<void> {
  if (inTauri) await openUrl(url);
}
