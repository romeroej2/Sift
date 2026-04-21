import { invoke } from "@tauri-apps/api/core";
import type { BootstrapState, BrowserSessionState, LmStudioHealth, UserSettings } from "./types";

export function getBootstrapState() {
  return invoke<BootstrapState>("get_bootstrap_state");
}

export function saveSettings(settings: UserSettings) {
  return invoke<UserSettings>("save_settings", { settings });
}

export function getXSessionState() {
  return invoke<BrowserSessionState>("get_x_session_state");
}

export function getLinkedInSessionState() {
  return invoke<BrowserSessionState>("get_linkedin_session_state");
}

export function getRedditSessionState() {
  return invoke<BrowserSessionState>("get_reddit_session_state");
}

export function openXSessionWindow() {
  return invoke<BrowserSessionState>("open_x_session_window");
}

export function openLinkedInSessionWindow() {
  return invoke<BrowserSessionState>("open_linkedin_session_window");
}

export function openRedditSessionWindow() {
  return invoke<BrowserSessionState>("open_reddit_session_window");
}

export function hideXSessionWindow() {
  return invoke<BrowserSessionState>("hide_x_session_window");
}

export function hideLinkedInSessionWindow() {
  return invoke<BrowserSessionState>("hide_linkedin_session_window");
}

export function hideRedditSessionWindow() {
  return invoke<BrowserSessionState>("hide_reddit_session_window");
}

export function logoutXSessionWindow() {
  return invoke<BrowserSessionState>("logout_x_session_window");
}

export function logoutLinkedInSessionWindow() {
  return invoke<BrowserSessionState>("logout_linkedin_session_window");
}

export function logoutRedditSessionWindow() {
  return invoke<BrowserSessionState>("logout_reddit_session_window");
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

export function deleteRun(runId: string) {
  return invoke<BootstrapState>("delete_run", { runId });
}

export function deleteAllEditions() {
  return invoke<BootstrapState>("delete_all_editions");
}
