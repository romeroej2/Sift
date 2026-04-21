import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_MODEL, DEFAULT_SETTINGS } from "./lib/defaults";
import type {
  BrowserSessionState,
  BootstrapState,
  Edition,
  LmStudioHealth,
  SyncRun,
  SyncProgressEvent,
  UserSettings
} from "./lib/types";

const {
  getBootstrapStateMock,
  getXSessionStateMock,
  getLinkedInSessionStateMock,
  getRedditSessionStateMock,
  verifyLmStudioMock,
  saveSettingsMock,
  runSyncMock,
  openXSessionWindowMock,
  openLinkedInSessionWindowMock,
  openRedditSessionWindowMock,
  hideXSessionWindowMock,
  hideLinkedInSessionWindowMock,
  hideRedditSessionWindowMock,
  logoutXSessionWindowMock,
  logoutLinkedInSessionWindowMock,
  logoutRedditSessionWindowMock,
  disconnectXMock,
  deleteRunMock,
  deleteAllEditionsMock,
  openExternalUrlMock,
  listenMock,
  enableAutostartMock,
  isAutostartEnabledMock,
  isNotificationPermissionGrantedMock,
  requestNotificationPermissionMock
} = vi.hoisted(() => ({
  getBootstrapStateMock: vi.fn<() => Promise<BootstrapState>>(),
  getXSessionStateMock: vi.fn<() => Promise<BrowserSessionState>>(),
  getLinkedInSessionStateMock: vi.fn<() => Promise<BrowserSessionState>>(),
  getRedditSessionStateMock: vi.fn<() => Promise<BrowserSessionState>>(),
  verifyLmStudioMock: vi.fn<
    (baseUrl: string, authToken: string | null) => Promise<LmStudioHealth>
  >(),
  saveSettingsMock: vi.fn<(settings: UserSettings) => Promise<UserSettings>>(),
  runSyncMock: vi.fn(),
  openXSessionWindowMock: vi.fn(),
  openLinkedInSessionWindowMock: vi.fn(),
  openRedditSessionWindowMock: vi.fn(),
  hideXSessionWindowMock: vi.fn(),
  hideLinkedInSessionWindowMock: vi.fn(),
  hideRedditSessionWindowMock: vi.fn(),
  logoutXSessionWindowMock: vi.fn(),
  logoutLinkedInSessionWindowMock: vi.fn(),
  logoutRedditSessionWindowMock: vi.fn(),
  disconnectXMock: vi.fn(),
  deleteRunMock: vi.fn(),
  deleteAllEditionsMock: vi.fn(),
  openExternalUrlMock: vi.fn(),
  listenMock: vi.fn(),
  enableAutostartMock: vi.fn(),
  isAutostartEnabledMock: vi.fn(),
  isNotificationPermissionGrantedMock: vi.fn(),
  requestNotificationPermissionMock: vi.fn()
}));

vi.mock("./lib/api", () => ({
  disconnectX: disconnectXMock,
  deleteAllEditions: deleteAllEditionsMock,
  deleteRun: deleteRunMock,
  getBootstrapState: getBootstrapStateMock,
  getLinkedInSessionState: getLinkedInSessionStateMock,
  getRedditSessionState: getRedditSessionStateMock,
  getXSessionState: getXSessionStateMock,
  hideLinkedInSessionWindow: hideLinkedInSessionWindowMock,
  hideRedditSessionWindow: hideRedditSessionWindowMock,
  hideXSessionWindow: hideXSessionWindowMock,
  logoutLinkedInSessionWindow: logoutLinkedInSessionWindowMock,
  logoutRedditSessionWindow: logoutRedditSessionWindowMock,
  logoutXSessionWindow: logoutXSessionWindowMock,
  openExternalUrl: openExternalUrlMock,
  openLinkedInSessionWindow: openLinkedInSessionWindowMock,
  openRedditSessionWindow: openRedditSessionWindowMock,
  openXSessionWindow: openXSessionWindowMock,
  runSync: runSyncMock,
  saveSettings: saveSettingsMock,
  verifyLmStudio: verifyLmStudioMock
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: listenMock
}));

vi.mock("@tauri-apps/plugin-autostart", () => ({
  enable: enableAutostartMock,
  isEnabled: isAutostartEnabledMock
}));

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: isNotificationPermissionGrantedMock,
  requestPermission: requestNotificationPermissionMock
}));

import App from "./App";

function createEdition(overrides: Partial<Edition> = {}): Edition {
  return {
    id: "edition-1",
    editionDate: "2026-04-16",
    title: "Your SIFT for 2026-04-16",
    frontPageSummary: "A good local-first shipping day.",
    createdAt: "2026-04-16T12:00:00Z",
    runId: "run-1",
    view: "x",
    sections: [],
    ...overrides
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

function createEditionWithStories(overrides: Partial<Edition> = {}): Edition {
  return createEdition({
    sections: [
      {
        id: "releases",
        title: "Releases",
        dek: "Worth your attention",
        cards: [
          {
            itemId: "card-1",
            authorName: "Ada",
            authorHandle: "ada",
            sourceUrl: "https://x.com/ada/status/1",
            postedAt: "2026-04-16T12:00:00Z",
            category: "Releases",
            headline: "A fast local model shipped",
            summary: "A fast local model shipped with a better developer workflow.",
            whyItMatters: "It makes on-device experimentation easier."
          }
        ]
      }
    ],
    ...overrides
  });
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

function createSessionState(overrides: Partial<BrowserSessionState> = {}): BrowserSessionState {
  return {
    isOpen: false,
    isVisible: false,
    isAuthenticated: false,
    lastKnownUrl: null,
    mode: "native-webview",
    ...overrides
  };
}

async function renderLoadedApp({
  bootstrap = createBootstrapState(),
  session = createSessionState(),
  linkedinSession = createSessionState(),
  redditSession = createSessionState()
}: {
  bootstrap?: BootstrapState;
  session?: BrowserSessionState;
  linkedinSession?: BrowserSessionState;
  redditSession?: BrowserSessionState;
} = {}) {
  getBootstrapStateMock.mockResolvedValue(bootstrap);
  getXSessionStateMock.mockResolvedValue(session);
  getLinkedInSessionStateMock.mockResolvedValue(linkedinSession);
  getRedditSessionStateMock.mockResolvedValue(redditSession);

  render(<App />);

  await screen.findByText(/SIFT is ready\./);
}

beforeEach(() => {
  vi.clearAllMocks();
  listenMock.mockResolvedValue(() => undefined);
  isAutostartEnabledMock.mockResolvedValue(true);
  enableAutostartMock.mockResolvedValue(undefined);
  isNotificationPermissionGrantedMock.mockResolvedValue(true);
  requestNotificationPermissionMock.mockResolvedValue("granted");
  saveSettingsMock.mockImplementation(async (settings) => ({
    ...settings,
    lmStudio: {
      ...settings.lmStudio,
      authToken: null
    }
  }));
  runSyncMock.mockResolvedValue(createBootstrapState());
  openXSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: true }));
  getLinkedInSessionStateMock.mockResolvedValue(createSessionState());
  getRedditSessionStateMock.mockResolvedValue(createSessionState());
  openLinkedInSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: true }));
  openRedditSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: true }));
  hideXSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: false }));
  hideLinkedInSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: false }));
  hideRedditSessionWindowMock.mockResolvedValue(createSessionState({ isOpen: true, isVisible: false }));
  logoutXSessionWindowMock.mockResolvedValue(createSessionState());
  logoutLinkedInSessionWindowMock.mockResolvedValue(createSessionState());
  logoutRedditSessionWindowMock.mockResolvedValue(createSessionState());
  disconnectXMock.mockResolvedValue(createBootstrapState());
  deleteRunMock.mockResolvedValue(createBootstrapState());
  deleteAllEditionsMock.mockResolvedValue(createBootstrapState());
  openExternalUrlMock.mockResolvedValue(undefined);
  vi.spyOn(console, "info").mockImplementation(() => undefined);
  vi.spyOn(console, "error").mockImplementation(() => undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("App", () => {
  it("loads the latest edition on startup", async () => {
    const edition = createEdition();

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [edition],
        latestRun: createRun({
          id: "run-1",
          itemCount: 4,
          keptCount: 2,
          editionId: edition.id
        })
      })
    });

    expect(screen.getByRole("heading", { name: edition.title })).toBeInTheDocument();
    expect(screen.getByText("A good local-first shipping day.")).toBeInTheDocument();
    expect(screen.getByText(/SIFT is ready\./)).toBeInTheDocument();
  });

  it("shows when the next auto-run is blocked", async () => {
    vi.spyOn(Date, "now").mockReturnValue(new Date("2026-04-16T08:30:00").valueOf());

    await renderLoadedApp();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    expect(screen.getByText("Scheduler overview")).toBeInTheDocument();
    expect(screen.getByText("1 run due now")).toBeInTheDocument();
    expect(
      screen.getByText("A scheduled run window is open, but SIFT is waiting for you to open X Session.")
    ).toBeInTheDocument();
  });

  it("delegates manual refresh session choreography to the sync command", async () => {
    const freshEdition = createEdition({
      id: "edition-fresh-from-closed",
      title: "Fresh from closed session",
      frontPageSummary: "The refresh opened the session first."
    });
    runSyncMock.mockResolvedValue(
      createBootstrapState({
        editions: [freshEdition],
        latestRun: createRun({
          id: "run-from-closed",
          startedAt: "2026-04-16T13:00:00Z",
          finishedAt: "2026-04-16T13:01:00Z",
          itemCount: 8,
          keptCount: 4,
          editionId: freshEdition.id
        })
      })
    );

    await renderLoadedApp();
    fireEvent.click(screen.getByRole("button", { name: "Refresh edition" }));

    await waitFor(() => {
      expect(runSyncMock).toHaveBeenCalledWith("manual");
    });

    expect(openXSessionWindowMock).not.toHaveBeenCalled();
    expect(hideXSessionWindowMock).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { name: "Fresh from closed session" })).toBeInTheDocument();
    expect(screen.getByText("Showing Fresh from closed session.")).toBeInTheDocument();
  });

  it("verifies LM Studio and saves the preferred model selection", async () => {
    verifyLmStudioMock.mockResolvedValue({
      ok: true,
      serverLabel: "LM Studio",
      message: "Connected",
      models: [
        { id: DEFAULT_MODEL, displayName: DEFAULT_MODEL, loaded: true },
        { id: "mistral-small", displayName: "mistral-small", loaded: false }
      ]
    });

    await renderLoadedApp();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: /Model desk/i }));
    fireEvent.click(screen.getByRole("button", { name: "Update" }));

    await waitFor(() => {
      expect(verifyLmStudioMock).toHaveBeenCalledWith("http://127.0.0.1:1234", null);
      expect(saveSettingsMock).toHaveBeenCalledWith(
        expect.objectContaining({
          lmStudio: expect.objectContaining({
            selectedModel: DEFAULT_MODEL
          })
        })
      );
    });

    expect(await screen.findByText("LM Studio verified.")).toBeInTheDocument();
    expect(
      screen.getByText(DEFAULT_MODEL, {
        selector: ".model-status__selected strong"
      })
    ).toBeInTheDocument();
  });

  it("autosaves newsroom settings when several fields change", async () => {
    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    fireEvent.change(screen.getByLabelText("Daily publish time"), {
      target: { value: "09:15" }
    });
    fireEvent.click(screen.getByLabelText("Drop replies"));
    fireEvent.click(screen.getByLabelText("Drop reposts"));
    fireEvent.click(screen.getByLabelText("Filter common engagement bait"));
    fireEvent.change(screen.getByLabelText("Muted keywords"), {
      target: { value: "ai\n \ncrypto " }
    });
    fireEvent.change(screen.getByLabelText("Muted authors"), {
      target: { value: "@alice\n bob \n" }
    });

    await waitFor(
      () => {
        expect(saveSettingsMock).toHaveBeenCalledWith({
          ...DEFAULT_SETTINGS,
          schedule: {
            ...DEFAULT_SETTINGS.schedule,
            rules: [
              {
                ...DEFAULT_SETTINGS.schedule.rules[0],
                timeOfDay: "09:15"
              }
            ],
            timezone: expect.any(String)
          },
          cleanup: {
            hideReplies: false,
            hideRetweets: false,
            removeBait: false,
            mutedKeywords: ["ai", "crypto"],
            mutedAuthors: ["@alice", "bob"]
          }
        });
      },
      { timeout: 2000 }
    );

    expect(await screen.findByText("Settings autosaved.")).toBeInTheDocument();
  });

  it("autosaves newsroom settings after they change", async () => {
    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.change(screen.getByLabelText("Daily publish time"), {
      target: { value: "09:15" }
    });

    await waitFor(() => {
      expect(saveSettingsMock).toHaveBeenCalledWith({
        ...DEFAULT_SETTINGS,
        schedule: {
          ...DEFAULT_SETTINGS.schedule,
          rules: [
            {
              ...DEFAULT_SETTINGS.schedule.rules[0],
              timeOfDay: "09:15"
            }
          ],
          timezone: expect.any(String)
        }
      });
    }, { timeout: 2000 });

    expect(await screen.findByText("Settings autosaved.")).toBeInTheDocument();
  });

  it("shows interval schedule controls and saves short-run browse depth", async () => {
    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.change(screen.getAllByLabelText("Cadence")[0], {
      target: { value: "interval" }
    });
    fireEvent.change(screen.getByLabelText("Run every hours"), {
      target: { value: "2" }
    });
    fireEvent.change(screen.getByLabelText("X pages to browse"), {
      target: { value: "5" }
    });

    await waitFor(
      () => {
        expect(saveSettingsMock).toHaveBeenCalledWith(
          expect.objectContaining({
            schedule: expect.objectContaining({
              rules: expect.arrayContaining([
                expect.objectContaining({
                  cadence: "interval",
                  intervalHours: 2,
                  browsePageCount: expect.objectContaining({
                    x: 5
                  })
                })
              ])
            })
          })
        );
      },
      { timeout: 2000 }
    );
  });

  it("shows an empty archive state when there are no saved editions", async () => {
    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Archive" }));

    expect(
      screen.getByText("Once your first issue is generated, it will land here.")
    ).toBeInTheDocument();
  });

  it("lets you select an archived edition and return to it on the Today view", async () => {
    const latestEdition = createEdition({
      id: "edition-latest",
      runId: "run-latest",
      title: "Latest edition",
      frontPageSummary: "Today is packed."
    });
    const earlierEdition = createEdition({
      id: "edition-earlier",
      runId: "run-earlier",
      title: "Earlier edition",
      frontPageSummary: "Earlier signal.",
      createdAt: "2026-04-15T12:00:00Z",
      editionDate: "2026-04-15"
    });

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [latestEdition, earlierEdition]
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    fireEvent.click(screen.getByText("Earlier edition").closest("button")!);
    fireEvent.click(screen.getByRole("button", { name: "Today" }));

    expect(screen.getByRole("heading", { name: "Earlier edition" })).toBeInTheDocument();
    expect(screen.getByText("Earlier signal.")).toBeInTheDocument();
  });

  it("clears all archived editions from the archive", async () => {
    deleteAllEditionsMock.mockResolvedValue(createBootstrapState());

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [createEdition()],
        runHistory: [createRun()],
        latestRun: createRun()
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete all editions" }));

    expect(deleteAllEditionsMock).not.toHaveBeenCalled();
    expect(
      await screen.findByText("Click Delete all again to remove every archived edition.")
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Confirm delete all editions" }));

    await waitFor(() => {
      expect(deleteAllEditionsMock).toHaveBeenCalledTimes(1);
    });

    expect(await screen.findByText("All archived editions deleted.")).toBeInTheDocument();
    expect(screen.getByText("Once your first issue is generated, it will land here.")).toBeInTheDocument();
  });

  it("opens source posts from edition cards", async () => {
    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [createEditionWithStories()]
      })
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: "Open source post for A fast local model shipped"
      })
    );

    await waitFor(() => {
      expect(openExternalUrlMock).toHaveBeenCalledWith("https://x.com/ada/status/1");
    });

    expect(
      await screen.findByText("Opened the source post in your default browser.")
    ).toBeInTheDocument();
  });

  it("renders an important story image when the edition card has one", async () => {
    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [
          createEditionWithStories({
            sections: [
              {
                id: "releases",
                title: "Releases",
                dek: "Worth your attention",
                cards: [
                  {
                    itemId: "card-1",
                    authorName: "Ada",
                    authorHandle: "ada",
                    sourceUrl: "https://x.com/ada/status/1",
                    postedAt: "2026-04-16T12:00:00Z",
                    category: "Releases",
                    headline: "A fast local model shipped",
                    summary: "A fast local model shipped with a better developer workflow.",
                    whyItMatters: "It makes on-device experimentation easier.",
                    leadImage: {
                      path: "/tmp/sift-story.jpg",
                      sourceUrl: "https://pbs.twimg.com/media/story.jpg",
                      mimeType: "image/jpeg",
                      alt: "Screenshot of the release UI"
                    }
                  }
                ]
              }
            ]
          })
        ]
      })
    });

    expect(screen.getByRole("img", { name: "Screenshot of the release UI" })).toBeInTheDocument();
  });

  it("runs a manual sync when the X session is already visible without re-hiding it", async () => {
    const freshEdition = createEdition({
      id: "edition-fresh",
      title: "Fresh issue",
      frontPageSummary: "Fresh issue summary."
    });
    runSyncMock.mockResolvedValue(
      createBootstrapState({
        editions: [freshEdition],
        latestRun: createRun({
          id: "run-fresh",
          startedAt: "2026-04-16T13:00:00Z",
          finishedAt: "2026-04-16T13:01:00Z",
          itemCount: 8,
          keptCount: 4,
          editionId: freshEdition.id
        })
      })
    );

    await renderLoadedApp({
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Refresh edition" }));

    await waitFor(() => {
      expect(runSyncMock).toHaveBeenCalledWith("manual");
    });

    expect(openXSessionWindowMock).not.toHaveBeenCalled();
    expect(hideXSessionWindowMock).not.toHaveBeenCalled();

    expect(await screen.findByRole("heading", { name: "Fresh issue" })).toBeInTheDocument();
    expect(screen.getByText("Showing Fresh issue.")).toBeInTheDocument();
  });

  it("does not toggle browser session windows from the React refresh handler", async () => {
    const freshEdition = createEdition({
      id: "edition-mixed-source-visibility",
      title: "Mixed source visibility",
      frontPageSummary: "Refresh orchestration moved behind the sync command."
    });
    const dualSourceSettings: UserSettings = {
      ...DEFAULT_SETTINGS,
      capture: {
        ...DEFAULT_SETTINGS.capture,
        sources: {
          x: true,
          linkedin: true,
          reddit: true
        }
      }
    };

    runSyncMock.mockResolvedValue(
      createBootstrapState({
        settings: dualSourceSettings,
        editions: [freshEdition],
        latestRun: createRun({
          id: "run-mixed-source-visibility",
          startedAt: "2026-04-16T13:00:00Z",
          finishedAt: "2026-04-16T13:01:00Z",
          itemCount: 8,
          keptCount: 4,
          editionId: freshEdition.id
        })
      })
    );

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        settings: dualSourceSettings
      }),
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      }),
      linkedinSession: createSessionState({
        isOpen: true,
        isVisible: false,
        isAuthenticated: true,
        lastKnownUrl: "https://www.linkedin.com/feed/"
      }),
      redditSession: createSessionState({
        isOpen: true,
        isVisible: false,
        isAuthenticated: true,
        lastKnownUrl: "https://www.reddit.com/"
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Refresh edition" }));

    await waitFor(() => {
      expect(runSyncMock).toHaveBeenCalledWith("manual");
    });

    expect(openXSessionWindowMock).not.toHaveBeenCalled();
    expect(hideXSessionWindowMock).not.toHaveBeenCalled();
    expect(openLinkedInSessionWindowMock).not.toHaveBeenCalled();
    expect(hideLinkedInSessionWindowMock).not.toHaveBeenCalled();
    expect(openRedditSessionWindowMock).not.toHaveBeenCalled();
    expect(hideRedditSessionWindowMock).not.toHaveBeenCalled();
  });

  it("renders Reddit session controls and settings when Reddit is available", async () => {
    await renderLoadedApp({
      redditSession: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://www.reddit.com/"
      })
    });

    expect(screen.getByText("Reddit")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hide Reddit session" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));

    expect(screen.getByLabelText("Reddit pages to browse")).toBeInTheDocument();
  });

  it("includes Reddit when autosaving newsroom settings", async () => {
    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const redditSourceTitle = screen
      .getAllByText("Reddit")
      .find((element) => element.className === "settings-source-tile__title");
    fireEvent.click(redditSourceTitle!.closest("section")!.querySelector("input[type='checkbox']")!);

    await waitFor(
      () => {
        expect(saveSettingsMock).toHaveBeenCalledWith(
          expect.objectContaining({
            capture: expect.objectContaining({
              sources: expect.objectContaining({
                reddit: true
              })
            })
          })
        );
      },
      { timeout: 2000 }
    );
  });

  it("keeps the current edition visible when a refresh finds no newer posts", async () => {
    const currentEdition = createEditionWithStories({
      id: "edition-current",
      title: "Current issue",
      frontPageSummary: "Still the latest edition on the desk."
    });
    const noFreshMessage =
      "SIFT cleaned 8 tweets, but none of them were fresh since the last saved edition.";

    runSyncMock.mockResolvedValue(
      createBootstrapState({
        editions: [currentEdition],
        latestRun: createRun({
          id: "run-no-fresh",
          startedAt: "2026-04-16T13:05:00Z",
          finishedAt: "2026-04-16T13:06:00Z",
          itemCount: 0,
          keptCount: 0,
          errorMessage: noFreshMessage,
          editionId: null
        })
      })
    );

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        editions: [currentEdition]
      }),
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Refresh edition" }));

    await waitFor(() => {
      expect(runSyncMock).toHaveBeenCalledWith("manual");
    });

    expect(await screen.findByRole("heading", { name: "Current issue" })).toBeInTheDocument();
    expect(screen.getAllByText(noFreshMessage)).toHaveLength(2);
  });

  it("reloads the desk when the sync response is missing the saved edition", async () => {
    const freshEdition = createEditionWithStories({
      id: "edition-reloaded",
      title: "Reloaded issue",
      frontPageSummary: "Recovered from the saved desk state."
    });

    await renderLoadedApp({
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    runSyncMock.mockResolvedValue(
      createBootstrapState({
        latestRun: createRun({
          id: "run-reloaded",
          startedAt: "2026-04-16T13:00:00Z",
          finishedAt: "2026-04-16T13:01:00Z",
          itemCount: 6,
          keptCount: 3,
          editionId: freshEdition.id
        })
      })
    );
    getBootstrapStateMock.mockResolvedValueOnce(
      createBootstrapState({
        editions: [freshEdition],
        latestRun: createRun({
          id: "run-reloaded",
          startedAt: "2026-04-16T13:00:00Z",
          finishedAt: "2026-04-16T13:01:00Z",
          itemCount: 6,
          keptCount: 3,
          editionId: freshEdition.id
        })
      })
    );

    fireEvent.click(screen.getByRole("button", { name: "Refresh edition" }));

    await waitFor(() => {
      expect(runSyncMock).toHaveBeenCalledWith("manual");
      expect(getBootstrapStateMock).toHaveBeenCalledTimes(2);
    });

    expect(await screen.findByRole("heading", { name: "Reloaded issue" })).toBeInTheDocument();
    expect(screen.getByText("Showing Reloaded issue.")).toBeInTheDocument();
  });

  it("saves model desk connection edits locally after verification", async () => {
    verifyLmStudioMock.mockResolvedValue({
      ok: true,
      serverLabel: "LM Studio",
      message: "Connected",
      models: [
        { id: DEFAULT_MODEL, displayName: DEFAULT_MODEL, loaded: true },
        { id: "mistral-small", displayName: "mistral-small", loaded: false }
      ]
    });

    await renderLoadedApp();

    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: /Model desk/i }));
    fireEvent.change(screen.getByLabelText("LM Studio URL"), {
      target: { value: "http://127.0.0.1:4321" }
    });
    fireEvent.change(screen.getByLabelText(/Auth token/), {
      target: { value: "secret-token" }
    });
    fireEvent.click(screen.getByLabelText("Use attached post images during ranking"));
    fireEvent.click(screen.getByRole("button", { name: "Update" }));

    await waitFor(() => {
      expect(verifyLmStudioMock).toHaveBeenCalledWith("http://127.0.0.1:4321", "secret-token");
    });

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(saveSettingsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          lmStudio: {
            baseUrl: "http://127.0.0.1:4321",
            authToken: "secret-token",
            selectedModel: DEFAULT_MODEL,
            includeImages: true
          }
        })
      );
    });

    expect(await screen.findByText("Settings saved locally.")).toBeInTheDocument();
    expect(screen.queryByLabelText("Selected model")).not.toBeInTheDocument();
  });

  it("reacts to sync progress events from Tauri", async () => {
    let syncListener: ((event: { payload: SyncProgressEvent }) => void) | undefined;
    const windowWithTauri = window as Window & { __TAURI_INTERNALS__?: object };
    windowWithTauri.__TAURI_INTERNALS__ = {};

    listenMock.mockImplementation(async (_eventName, callback) => {
      syncListener = callback as typeof syncListener;
      return () => undefined;
    });

    await renderLoadedApp({
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    await waitFor(() => {
      expect(listenMock).toHaveBeenCalledWith("sync-progress", expect.any(Function));
      expect(syncListener).toBeDefined();
    });

    act(() => {
      syncListener?.({
        payload: {
          runId: "run-1",
          reason: "manual",
          status: "running",
          stage: "ranking-items",
          message: "Ranking the strongest posts",
          itemCount: 18,
          newItemCount: 12,
          keptCount: 7,
          editionId: null,
          timestamp: "2026-04-16T12:00:00Z"
        }
      });
    });

    expect(screen.getByRole("button", { name: "Refreshing..." })).toBeDisabled();
    expect(screen.getByText("Ranking the strongest posts")).toBeInTheDocument();

    act(() => {
      syncListener?.({
        payload: {
          runId: "run-1",
          reason: "manual",
          status: "error",
          stage: "error",
          message: "LM Studio stopped responding",
          itemCount: 18,
          newItemCount: 12,
          keptCount: 7,
          editionId: null,
          timestamp: "2026-04-16T12:01:00Z"
        }
      });
    });

    expect(console.error).toHaveBeenCalled();
    expect(screen.getByText("LM Studio stopped responding")).toBeInTheDocument();

    delete windowWithTauri.__TAURI_INTERNALS__;
  });

  it("refreshes the desk after a successful sync progress event announces a saved edition", async () => {
    let syncListener: ((event: { payload: SyncProgressEvent }) => void) | undefined;
    const edition = createEditionWithStories({
      id: "edition-progress",
      title: "Progress issue",
      frontPageSummary: "Loaded from a post-sync bootstrap refresh."
    });
    const windowWithTauri = window as Window & { __TAURI_INTERNALS__?: object };
    windowWithTauri.__TAURI_INTERNALS__ = {};

    listenMock.mockImplementation(async (_eventName, callback) => {
      syncListener = callback as typeof syncListener;
      return () => undefined;
    });

    await renderLoadedApp({
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    getBootstrapStateMock.mockResolvedValueOnce(
      createBootstrapState({
        editions: [edition],
        latestRun: createRun({
          id: "run-progress",
          startedAt: "2026-04-16T13:10:00Z",
          finishedAt: "2026-04-16T13:11:00Z",
          itemCount: 5,
          keptCount: 2,
          editionId: edition.id
        })
      })
    );

    await waitFor(() => {
      expect(syncListener).toBeDefined();
    });

    act(() => {
      syncListener?.({
        payload: {
          runId: "run-progress",
          reason: "manual",
          status: "success",
          stage: "complete",
          message: "Fresh edition generated: Progress issue.",
          itemCount: 5,
          newItemCount: 5,
          keptCount: 2,
          editionId: edition.id,
          timestamp: "2026-04-16T13:11:00Z"
        }
      });
    });

    await waitFor(() => {
      expect(getBootstrapStateMock).toHaveBeenCalledTimes(2);
    });

    expect(await screen.findByRole("heading", { name: "Progress issue" })).toBeInTheDocument();

    delete windowWithTauri.__TAURI_INTERNALS__;
  });

  it("can hide the X session window and clear a stored legacy connection", async () => {
    hideXSessionWindowMock.mockResolvedValue(
      createSessionState({
        isOpen: true,
        isVisible: false,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    );
    disconnectXMock.mockResolvedValue(createBootstrapState());

    await renderLoadedApp({
      bootstrap: createBootstrapState({
        xConnection: {
          userId: "user-1",
          handle: "legacyuser",
          name: "Legacy User",
          connectedAt: "2026-04-16T10:00:00Z"
        }
      }),
      session: createSessionState({
        isOpen: true,
        isVisible: true,
        isAuthenticated: true,
        lastKnownUrl: "https://x.com/home"
      })
    });

    fireEvent.click(screen.getByRole("button", { name: "Hide X session" }));

    await waitFor(() => {
      expect(hideXSessionWindowMock).toHaveBeenCalled();
    });

    expect(
      await screen.findByText("The X session is hidden. Your sign-in stays alive in the background.")
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));

    await waitFor(() => {
      expect(disconnectXMock).toHaveBeenCalled();
    });

    expect(await screen.findByText("Legacy X API connection cleared.")).toBeInTheDocument();
  });
});
