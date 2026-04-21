import type { BrowserSource, ScheduleRule, UserSettings } from "./types";

export const DEFAULT_MODEL = "google/gemma-4-26b-a4b";

export const DEFAULT_BROWSE_PAGE_COUNT: Record<BrowserSource, number> = {
  x: 12,
  linkedin: 8,
  reddit: 10
};

export const DEFAULT_SHORT_BROWSE_PAGE_COUNT: Record<BrowserSource, number> = {
  x: 4,
  linkedin: 3,
  reddit: 4
};

export function getMachineTimeZone() {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

function scheduleRuleId() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `schedule-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export function createScheduleRule(overrides: Partial<ScheduleRule> = {}): ScheduleRule {
  return {
    id: overrides.id ?? scheduleRuleId(),
    label: overrides.label ?? "Morning brief",
    enabled: overrides.enabled ?? true,
    cadence: overrides.cadence ?? "daily",
    timeOfDay: overrides.timeOfDay ?? "07:30",
    intervalHours: overrides.intervalHours ?? 1,
    windowStart: overrides.windowStart ?? "09:00",
    windowEnd: overrides.windowEnd ?? "17:00",
    browsePageCount: overrides.browsePageCount ?? DEFAULT_BROWSE_PAGE_COUNT
  };
}

export const DEFAULT_SETTINGS: UserSettings = {
  schedule: {
    rules: [
      createScheduleRule()
    ],
    timezone: getMachineTimeZone()
  },
  cleanup: {
    hideReplies: true,
    hideRetweets: true,
    removeBait: true,
    mutedKeywords: [],
    mutedAuthors: []
  },
  lmStudio: {
    baseUrl: "http://127.0.0.1:1234",
    authToken: null,
    selectedModel: null,
    includeImages: false
  },
  capture: {
    sources: {
      x: true,
      linkedin: false,
      reddit: false
    },
    browsePageCount: DEFAULT_BROWSE_PAGE_COUNT
  }
};
