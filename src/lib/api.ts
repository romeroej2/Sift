import { invoke } from "@tauri-apps/api/core";
import type { BootstrapState, LmStudioHealth, UserSettings, XSessionState } from "./types";

export function getBootstrapState() {
  return invoke<BootstrapState>("get_bootstrap_state");
}

export function saveSettings(settings: UserSettings) {
  return invoke<UserSettings>("save_settings", { settings });
}

export function getXSessionState() {
  return invoke<XSessionState>("get_x_session_state");
}

export function openXSessionWindow() {
  return invoke<XSessionState>("open_x_session_window");
}

export function hideXSessionWindow() {
  return invoke<XSessionState>("hide_x_session_window");
}

export function logoutXSessionWindow() {
  return invoke<XSessionState>("logout_x_session_window");
}

export function verifyLmStudio(baseUrl: string, authToken: string | null) {
  return invoke<LmStudioHealth>("verify_lm_studio", { baseUrl, authToken });
}

export function runSync(reason: "manual" | "scheduled" = "manual") {
  return invoke<BootstrapState>("run_sync", { reason });
}

export function disconnectX() {
  return invoke<BootstrapState>("disconnect_x");
}

export function openExternalUrl(url: string) {
  return invoke<void>("open_external_url", { url });
}
