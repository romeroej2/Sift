import { DEFAULT_SETTINGS } from "./defaults";
import type {
  BootstrapState,
  Edition,
  LmStudioHealth,
  SyncProgressEvent,
  XSessionState
} from "./types";

export const SYNC_PROGRESS_EVENT = "sync-progress";

export const EMPTY_BOOTSTRAP: BootstrapState = {
  settings: DEFAULT_SETTINGS,
  editions: [],
  latestRun: null,
  xConnection: null
};

export const EMPTY_X_SESSION: XSessionState = {
  isOpen: false,
  isVisible: false,
  isAuthenticated: false,
  lastKnownUrl: null,
  mode: "native-webview"
};

export function pickInitialEdition(editions: Edition[]) {
  return editions[0] ?? null;
}

export function pickFreshEdition(state: BootstrapState) {
  if (!state.editions.length) {
    return null;
  }

  return (
    state.editions.find((edition) => edition.id === state.latestRun?.editionId) ??
    state.editions[0]
  );
}

export function getLmStudioSummary(health: LmStudioHealth) {
  if (!health.models.length) {
    return "Connected, but no models available yet.";
  }

  return `${health.models.length} model${health.models.length === 1 ? "" : "s"} available.`;
}

export function getModelDeskSummary(
  selectedModelId: string | null,
  health: LmStudioHealth | null
) {
  if (selectedModelId) {
    return selectedModelId;
  }

  if (health) {
    return getLmStudioSummary(health);
  }

  return "Connect LM Studio";
}

export function getModelDeskStatusLabel(
  selectedModelId: string | null,
  health: LmStudioHealth | null
) {
  if (health) {
    return "Ready";
  }

  if (selectedModelId) {
    return "Saved";
  }

  return "Setup";
}

function getSyncStageLabel(stage: SyncProgressEvent["stage"]) {
  switch (stage) {
    case "starting":
      return "Starting";
    case "navigating-home":
      return "Opening Home";
    case "capturing-feed":
      return "Capturing feed";
    case "ranking-items":
      return "Ranking posts";
    case "building-edition":
      return "Writing edition";
    case "saving-edition":
      return "Saving";
    case "complete":
      return "Complete";
    case "error":
      return "Failed";
  }
}

export function getSyncProgressMeta(progress: SyncProgressEvent) {
  const parts = [getSyncStageLabel(progress.stage)];

  if (progress.itemCount !== null) {
    parts.push(`${progress.itemCount} captured`);
  }

  if (progress.newItemCount !== null) {
    parts.push(`${progress.newItemCount} new`);
  }

  if (progress.keptCount !== null) {
    parts.push(`${progress.keptCount} kept`);
  }

  return parts.join(" · ");
}

export function getErrorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }

  if (typeof error === "string" && error.trim()) {
    return error;
  }

  if (
    typeof error === "object" &&
    error &&
    "message" in error &&
    typeof error.message === "string" &&
    error.message.trim()
  ) {
    return error.message;
  }

  return fallback;
}

export function getXSessionToggleLabel(session: XSessionState) {
  if (session.isOpen && session.isVisible) {
    return "Hide X session";
  }

  if (session.isOpen) {
    return "Show X session";
  }

  return "Open X session";
}

export async function withTimeout<T>(
  promise: Promise<T>,
  label: string,
  ms = 10000
): Promise<T> {
  return await Promise.race([
    promise,
    new Promise<T>((_, reject) => {
      window.setTimeout(() => {
        reject(new Error(`${label} timed out. Check the local app logs and try again.`));
      }, ms);
    })
  ]);
}
