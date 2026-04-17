import { DEFAULT_SETTINGS } from "./defaults";
import type {
  BrowserSessionState,
  BrowserSource,
  BootstrapState,
  Edition,
  EditionView,
  LmStudioHealth,
  ScheduleSettings,
  SyncProgressEvent,
  UserSettings
} from "./types";

export const SYNC_PROGRESS_EVENT = "sync-progress";

export const EMPTY_BOOTSTRAP: BootstrapState = {
  settings: DEFAULT_SETTINGS,
  editions: [],
  latestRun: null,
  xConnection: null
};

export const EMPTY_BROWSER_SESSION: BrowserSessionState = {
  isOpen: false,
  isVisible: false,
  isAuthenticated: false,
  lastKnownUrl: null,
  mode: "native-webview"
};

export function pickInitialEdition(editions: Edition[], view?: EditionView) {
  if (!view) {
    return editions[0] ?? null;
  }

  return editions.find((edition) => edition.view === view) ?? editions[0] ?? null;
}

export function getAvailableEditionViews(editions: Edition[]): EditionView[] {
  const allViews: EditionView[] = ["consolidated", "x", "linkedin"];
  return allViews.filter((view) => editions.some((edition) => edition.view === view));
}

export function pickFreshEdition(state: BootstrapState, preferredView?: EditionView) {
  if (!state.editions.length) {
    return null;
  }

  const preferredEditions = preferredView
    ? state.editions.filter((edition) => edition.view === preferredView)
    : state.editions;

  return preferredEditions.find((edition) => edition.id === state.latestRun?.editionId)
    ?? preferredEditions[0]
    ?? state.editions.find((edition) => edition.id === state.latestRun?.editionId)
    ?? state.editions.find((edition) => edition.view === "consolidated")
    ?? state.editions[0];
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

export function getSessionToggleLabel(source: BrowserSource, session: BrowserSessionState) {
  const sourceLabel = source === "linkedin" ? "LinkedIn" : "X";

  if (session.isOpen && session.isVisible) {
    return `Hide ${sourceLabel} session`;
  }

  if (session.isOpen) {
    return `Show ${sourceLabel} session`;
  }

  return `Open ${sourceLabel} session`;
}

export function getXSessionToggleLabel(session: BrowserSessionState) {
  return getSessionToggleLabel("x", session);
}

export interface ScheduleSummary {
  title: string;
  detail: string;
}

function scheduledDate(now: Date, timeOfDay: string, dayOffset = 0) {
  const [rawHours = "7", rawMinutes = "30"] = timeOfDay.split(":");
  const hours = Number.parseInt(rawHours, 10);
  const minutes = Number.parseInt(rawMinutes, 10);
  const next = new Date(now);
  next.setHours(
    Number.isFinite(hours) ? hours : 7,
    Number.isFinite(minutes) ? minutes : 30,
    0,
    0
  );
  next.setDate(next.getDate() + dayOffset);
  return next;
}

function formatScheduledDate(value: Date) {
  return new Intl.DateTimeFormat(undefined, {
    weekday: "short",
    hour: "numeric",
    minute: "2-digit"
  }).format(value);
}

export function getScheduleSummary(
  schedule: ScheduleSettings,
  sessionsOrSession: Partial<Record<BrowserSource, BrowserSessionState>> | BrowserSessionState,
  settingsOrNow: UserSettings | Date = DEFAULT_SETTINGS,
  now = new Date()
): ScheduleSummary {
  const settings =
    settingsOrNow instanceof Date
      ? DEFAULT_SETTINGS
      : settingsOrNow;
  const resolvedNow = settingsOrNow instanceof Date ? settingsOrNow : now;
  const sessions =
    "isOpen" in sessionsOrSession
      ? { x: sessionsOrSession }
      : sessionsOrSession;
  const enabledSources = (Object.entries(settings.capture.sources) as Array<[BrowserSource, boolean]>)
    .filter(([, enabled]) => enabled)
    .map(([source]) => source);
  const blockedSourceLabel = (enabledSources.length ? enabledSources : ["x"])
    .map((source) => (source === "linkedin" ? "LinkedIn Session" : "X Session"))
    .join(" and ");
  const allOpen = enabledSources.every((source) => sessions[source]?.isOpen);
  const allAuthenticated = enabledSources.every((source) => sessions[source]?.isAuthenticated);

  if (!schedule.enabled) {
    return {
      title: "Auto-run is off",
      detail: "Turn on morning auto-run to have SIFT publish automatically."
    };
  }

  const todayRunAt = scheduledDate(resolvedNow, schedule.timeOfDay);

  if (resolvedNow < todayRunAt) {
    if (!allOpen) {
      return {
        title: `Next run ${formatScheduledDate(todayRunAt)}`,
        detail: `Open ${blockedSourceLabel} before then. Scheduled runs need each enabled SIFT-managed session to be available.`
      };
    }

    if (!allAuthenticated) {
      return {
        title: `Next run ${formatScheduledDate(todayRunAt)}`,
        detail: `Finish signing in to ${blockedSourceLabel} in SIFT before then or the auto-run will stay blocked.`
      };
    }

    return {
      title: `Next run ${formatScheduledDate(todayRunAt)}`,
      detail: "Ready. SIFT will try automatically while the app is running in the background."
    };
  }

  if (!allOpen) {
    return {
      title: "Run is due now",
      detail: `The schedule time has passed, but SIFT is waiting for you to open ${blockedSourceLabel}.`
    };
  }

  if (!allAuthenticated) {
    return {
      title: "Run is due now",
      detail: `The schedule time has passed, but SIFT is waiting for you to finish signing in to ${blockedSourceLabel}.`
    };
  }

  return {
    title: "Run is due now",
    detail: "The schedule time has passed and SIFT should pick it up on the next scheduler check while the app is running."
  };
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
