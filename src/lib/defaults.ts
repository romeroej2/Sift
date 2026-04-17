import type { UserSettings } from "./types";

export const DEFAULT_MODEL = "google/gemma-4-26b-a4b";

export const DEFAULT_SETTINGS: UserSettings = {
  schedule: {
    enabled: true,
    timeOfDay: "07:30",
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"
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
  }
};
