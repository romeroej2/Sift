import type { UserSettings } from "./types";

export const DEFAULT_MODEL = "google/gemma-4-26b-a4b";

export function getMachineTimeZone() {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

export const DEFAULT_SETTINGS: UserSettings = {
  schedule: {
    enabled: true,
    timeOfDay: "07:30",
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
    browsePageCount: {
      x: 12,
      linkedin: 8,
      reddit: 10
    }
  }
};
