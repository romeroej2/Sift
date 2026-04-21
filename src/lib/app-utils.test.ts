import { describe, expect, it, vi } from "vitest";
import { DEFAULT_MODEL, DEFAULT_SETTINGS } from "./defaults";
import {
  getErrorMessage,
  getModelDeskStatusLabel,
  getModelDeskSummary,
  getSessionToggleLabel,
  getScheduleSummary,
  getSyncProgressMeta,
  getXSessionToggleLabel,
  pickFreshEdition,
  withTimeout
} from "./app-utils";
import type { BootstrapState, Edition, LmStudioHealth, SyncProgressEvent, SyncRun } from "./types";

function createEdition(id: string, title: string): Edition {
  return {
    id,
    editionDate: "2026-04-16",
    title,
    frontPageSummary: `${title} summary`,
    createdAt: "2026-04-16T12:00:00Z",
    runId: "run-1",
    view: "x",
    sections: []
  };
}

function createRun(overrides: Partial<SyncRun> = {}): SyncRun {
  return {
    id: "run-1",
    reason: "manual",
    scheduleRuleId: null,
    scheduleRuleLabel: null,
    scheduleSlotKey: null,
    startedAt: "2026-04-16T12:05:00Z",
    finishedAt: "2026-04-16T12:06:00Z",
    status: "success",
    itemCount: 3,
    keptCount: 2,
    errorMessage: null,
    editionId: "edition-1",
    timings: {
      captureMs: 1000,
      rankingMs: 2000,
      frontPageMs: 500,
      savingMs: 250,
      totalMs: 3750
    },
    ...overrides
  };
}

function createBootstrapState(overrides: Partial<BootstrapState> = {}): BootstrapState {
  return {
    settings: DEFAULT_SETTINGS,
    editions: [],
    latestRun: null,
    runHistory: [],
    xConnection: null,
    ...overrides
  };
}

function createHealth(modelIds: string[]): LmStudioHealth {
  return {
    ok: true,
    serverLabel: "LM Studio",
    message: "Connected",
    models: modelIds.map((id) => ({
      id,
      displayName: id,
      loaded: id === DEFAULT_MODEL
    }))
  };
}

describe("pickFreshEdition", () => {
  it("prefers the edition from the latest successful run when present", () => {
    const editions = [createEdition("edition-1", "Morning"), createEdition("edition-2", "Noon")];

    expect(
      pickFreshEdition(
        createBootstrapState({
          editions,
          latestRun: createRun({ editionId: "edition-2" })
        })
      )
    ).toEqual(editions[1]);
  });

  it("falls back to the first available edition when the latest run has no matching edition", () => {
    const editions = [createEdition("edition-1", "Morning"), createEdition("edition-2", "Noon")];

    expect(
      pickFreshEdition(
        createBootstrapState({
          editions,
          latestRun: createRun({ editionId: "missing-edition" })
        })
      )
    ).toEqual(editions[0]);
  });
});

describe("model desk helpers", () => {
  it("shows the selected model id ahead of health details", () => {
    const health = createHealth([DEFAULT_MODEL, "mistral"]);

    expect(getModelDeskSummary(DEFAULT_MODEL, health)).toBe(DEFAULT_MODEL);
    expect(getModelDeskStatusLabel(DEFAULT_MODEL, null)).toBe("Saved");
  });

  it("uses health details when no model is selected", () => {
    expect(getModelDeskSummary(null, createHealth(["mistral", "gemma"]))).toBe("2 models available.");
    expect(getModelDeskStatusLabel(null, createHealth(["mistral"]))).toBe("Ready");
    expect(getModelDeskSummary(null, null)).toBe("Connect LM Studio");
    expect(getModelDeskStatusLabel(null, null)).toBe("Setup");
  });
});

describe("getSyncProgressMeta", () => {
  it("formats the stage and only the available counters", () => {
    const progress: SyncProgressEvent = {
      runId: "run-1",
      reason: "manual",
      status: "running",
      stage: "capturing-feed",
      message: "Capturing the live feed",
      itemCount: 18,
      newItemCount: null,
      keptCount: 7,
      editionId: null,
      timestamp: "2026-04-16T12:00:00Z"
    };

    expect(getSyncProgressMeta(progress)).toBe("Capturing feed · 18 captured · 7 kept");
  });
});

describe("getErrorMessage", () => {
  it("prefers useful messages and falls back when necessary", () => {
    expect(getErrorMessage(new Error("LM Studio offline"), "fallback")).toBe("LM Studio offline");
    expect(getErrorMessage("plain string", "fallback")).toBe("plain string");
    expect(getErrorMessage({ message: "from object" }, "fallback")).toBe("from object");
    expect(getErrorMessage({ message: "   " }, "fallback")).toBe("fallback");
  });
});

describe("getXSessionToggleLabel", () => {
  it("reflects whether the native X session should be opened, shown, or hidden", () => {
    expect(
      getXSessionToggleLabel({
        isOpen: false,
        isVisible: false,
        isAuthenticated: false,
        lastKnownUrl: null,
        mode: "native-webview"
      })
    ).toBe("Open X session");

    expect(
      getXSessionToggleLabel({
        isOpen: true,
        isVisible: false,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home",
        mode: "native-webview"
      })
    ).toBe("Show X session");

    expect(
      getXSessionToggleLabel({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home",
        mode: "native-webview"
      })
    ).toBe("Hide X session");
  });
});

describe("getSessionToggleLabel", () => {
  it("uses Reddit-specific labels for Reddit sessions", () => {
    expect(
      getSessionToggleLabel("reddit", {
        isOpen: false,
        isVisible: false,
        isAuthenticated: false,
        lastKnownUrl: null,
        mode: "native-webview"
      })
    ).toBe("Open Reddit session");
  });
});

describe("getScheduleSummary", () => {
  it("shows the upcoming scheduled time when the session is ready", () => {
    expect(
      getScheduleSummary(
        DEFAULT_SETTINGS.schedule,
        {
          isOpen: true,
          isVisible: false,
          isAuthenticated: true,
          lastKnownUrl: "https://x.com/home",
          mode: "native-webview"
        },
        new Date("2026-04-16T06:30:00")
      )
    ).toMatchObject({
      title: expect.stringContaining("Next Morning brief"),
      detail: "1 schedule armed. SIFT will try automatically while the app is running in the background."
    });
  });

  it("explains when a due run is blocked by the X session", () => {
    expect(
      getScheduleSummary(
        DEFAULT_SETTINGS.schedule,
        {
          isOpen: false,
          isVisible: false,
          isAuthenticated: false,
          lastKnownUrl: null,
          mode: "native-webview"
        },
        new Date("2026-04-16T08:30:00")
      )
    ).toEqual({
      title: "1 run due now",
      detail: "A scheduled run window is open, but SIFT is waiting for you to open X Session."
    });
  });

  it("describes interval auto-runs with the short-run label", () => {
    expect(
      getScheduleSummary(
        {
          ...DEFAULT_SETTINGS.schedule,
          rules: [
            {
              ...DEFAULT_SETTINGS.schedule.rules[0],
              cadence: "interval",
              intervalHours: 2
            }
          ]
        },
        {
          isOpen: true,
          isVisible: false,
          isAuthenticated: true,
          lastKnownUrl: "https://x.com/home",
          mode: "native-webview"
        },
        new Date("2026-04-16T05:30:00")
      )
    ).toMatchObject({
      title: expect.stringContaining("Next Morning brief"),
      detail: "1 schedule armed. SIFT will try automatically while the app is running in the background."
    });
  });
});

describe("withTimeout", () => {
  it("resolves when the wrapped promise finishes in time", async () => {
    await expect(withTimeout(Promise.resolve("ready"), "Loading", 25)).resolves.toBe("ready");
  });

  it("rejects with a helpful timeout message", async () => {
    vi.useFakeTimers();

    const pending = new Promise<string>(() => undefined);
    const result = withTimeout(pending, "Loading the newsroom", 25);
    const expectation = expect(result).rejects.toThrow(
      "Loading the newsroom timed out. Check the local app logs and try again."
    );

    await vi.advanceTimersByTimeAsync(25);
    await expectation;

    vi.useRealTimers();
  });
});
