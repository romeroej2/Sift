import { useEffect, useMemo, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { enable as enableAutostart, isEnabled as isAutostartEnabled } from "@tauri-apps/plugin-autostart";
import {
  isPermissionGranted as isNotificationPermissionGranted,
  requestPermission as requestNotificationPermission
} from "@tauri-apps/plugin-notification";
import type {
  BootstrapState,
  Edition,
  LmStudioHealth,
  SyncProgressEvent,
  UserSettings,
  XSessionState
} from "./lib/types";
import { DEFAULT_MODEL, DEFAULT_SETTINGS } from "./lib/defaults";
import { formatEditionDate, formatTime } from "./lib/format";
import {
  EMPTY_BOOTSTRAP,
  EMPTY_X_SESSION,
  SYNC_PROGRESS_EVENT,
  getErrorMessage,
  getLmStudioSummary,
  getModelDeskStatusLabel,
  getModelDeskSummary,
  getSyncProgressMeta,
  getXSessionToggleLabel,
  pickFreshEdition,
  pickInitialEdition,
  withTimeout
} from "./lib/app-utils";
import {
  disconnectX,
  getBootstrapState,
  getXSessionState,
  hideXSessionWindow,
  logoutXSessionWindow,
  openExternalUrl,
  openXSessionWindow,
  runSync,
  saveSettings,
  verifyLmStudio
} from "./lib/api";

type Screen = "today" | "archive" | "settings";

function SessionControlButton({
  label,
  tone = "default",
  onClick,
  children
}: {
  label: string;
  tone?: "default" | "danger";
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      className={tone === "danger" ? "icon-action-button icon-action-button--danger" : "icon-action-button"}
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
      <span className="sr-only">{label}</span>
    </button>
  );
}

function ShowIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d="M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6S2 12 2 12" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

function HideIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d="M3 3l18 18" />
      <path d="M10.58 10.58A3 3 0 0 0 9 12a3 3 0 0 0 5.33 1.82" />
      <path d="M9.88 5.09A10.94 10.94 0 0 1 12 4.91c5 0 9.27 3.11 11 7.5a11.8 11.8 0 0 1-3.29 4.68" />
      <path d="M6.61 6.61A11.81 11.81 0 0 0 1 12.41a11.84 11.84 0 0 0 4.26 5.1" />
    </svg>
  );
}

function LogoutIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
      <path d="M16 17l5-5-5-5" />
      <path d="M21 12H9" />
    </svg>
  );
}

export default function App() {
  const [screen, setScreen] = useState<Screen>("today");
  const [bootstrap, setBootstrap] = useState<BootstrapState>(EMPTY_BOOTSTRAP);
  const [selectedEditionId, setSelectedEditionId] = useState<string | null>(null);
  const [message, setMessage] = useState("Opening today\'s desk...");
  const [lmStudioDraft, setLmStudioDraft] = useState(DEFAULT_SETTINGS.lmStudio);
  const [xSession, setXSession] = useState<XSessionState>(EMPTY_X_SESSION);
  const [lmHealth, setLmHealth] = useState<LmStudioHealth | null>(null);
  const [isModelDeskExpanded, setIsModelDeskExpanded] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [syncProgress, setSyncProgress] = useState<SyncProgressEvent | null>(null);
  const availableModels = lmHealth?.models ?? [];
  const selectedModelDescriptor =
    availableModels.find((model) => model.id === lmStudioDraft.selectedModel) ?? null;
  const selectedModelId = selectedModelDescriptor?.id ?? lmStudioDraft.selectedModel;
  const liveSyncProgress = syncProgress?.status === "running" ? syncProgress : null;
  const isRefreshBusy = isRefreshing || liveSyncProgress !== null;
  const statusMessage = liveSyncProgress?.message ?? message;
  const latestEditionTitle = pickFreshEdition(bootstrap)?.title ?? null;
  const statusMeta = liveSyncProgress
    ? getSyncProgressMeta(liveSyncProgress)
    : bootstrap.latestRun
      ? bootstrap.latestRun.status === "success" && !bootstrap.latestRun.editionId && bootstrap.latestRun.errorMessage
        ? bootstrap.latestRun.errorMessage
        : `${latestEditionTitle ? `${latestEditionTitle} · ` : ""}Last run ${formatTime(bootstrap.latestRun.startedAt)} · ${bootstrap.latestRun.status}`
      : "No edition generated yet.";

  const selectedEdition = useMemo(() => {
    if (!bootstrap.editions.length) {
      return null;
    }

    return (
      bootstrap.editions.find((edition) => edition.id === selectedEditionId) ??
      bootstrap.editions[0]
    );
  }, [bootstrap.editions, selectedEditionId]);

  function resolveSelectedEditionId(
    state: BootstrapState,
    currentEditionId: string | null,
    preferredEditionId: string | null = null
  ) {
    if (!state.editions.length) {
      return null;
    }

    if (
      preferredEditionId
      && state.editions.some((edition) => edition.id === preferredEditionId)
    ) {
      return preferredEditionId;
    }

    if (
      currentEditionId
      && state.editions.some((edition) => edition.id === currentEditionId)
    ) {
      return currentEditionId;
    }

    return pickFreshEdition(state)?.id ?? pickInitialEdition(state.editions)?.id ?? null;
  }

  function applyBootstrapState(state: BootstrapState, preferredEditionId: string | null = null) {
    setBootstrap(state);
    setSelectedEditionId((current) =>
      resolveSelectedEditionId(state, current, preferredEditionId)
    );
  }

  useEffect(() => {
    if (bootstrap.editions.length && !selectedEditionId) {
      setSelectedEditionId(bootstrap.editions[0].id);
    }
  }, [bootstrap.editions, selectedEditionId]);

  useEffect(() => {
    if (bootstrap.latestRun?.status !== "running") {
      setSyncProgress((current) =>
        current?.status === "running" ? null : current
      );
    }
  }, [bootstrap.latestRun]);

  useEffect(() => {
    setLmStudioDraft((current) => ({
      ...bootstrap.settings.lmStudio,
      authToken: current.authToken ?? bootstrap.settings.lmStudio.authToken
    }));
  }, [bootstrap.settings.lmStudio]);

  useEffect(() => {
    let isCancelled = false;

    async function loadApp() {
      try {
        const [state, session] = await Promise.all([
          withTimeout(getBootstrapState(), "Loading the newsroom"),
          withTimeout(getXSessionState(), "Loading the X session")
        ]);

        if (isCancelled) {
          return;
        }

        applyBootstrapState(state);
        setXSession(session);
        void hydrateSavedLmStudio(state.settings);
        setMessage(
          session.isAuthenticated
            ? session.isVisible
              ? "SIFT is ready. Your X session is already open."
              : "SIFT is ready. Your last X session is standing by in the background."
            : session.isOpen
              ? session.isVisible
                ? "SIFT is ready. Your X session is open. Finish signing in there if needed."
                : "SIFT is ready. Your X session is hidden. Open it to finish signing in if needed."
              : "SIFT is ready. Open X Session, sign in there, and keep the browser-driven workflow moving."
        );

        void ensureDesktopRuntime().catch((error) => {
          const detail = getErrorMessage(error, "desktop integrations are not ready yet");
          setMessage(`SIFT is ready, but ${detail}.`);
        });
      } catch (error) {
        if (!isCancelled) {
          setMessage(getErrorMessage(error, "Unable to load SIFT."));
        }
      }
    }

    void loadApp();

    const interval = window.setInterval(() => {
      void getXSessionState()
        .then((state) => {
          if (!isCancelled) {
            setXSession(state);
          }
        })
        .catch(() => {
          // The dedicated X session window is optional, so polling failures can stay quiet.
        });
    }, 4000);

    return () => {
      isCancelled = true;
      window.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
      return;
    }

    let isDisposed = false;
    let unlisten: (() => void) | null = null;

    void listen<SyncProgressEvent>(SYNC_PROGRESS_EVENT, (event) => {
      const payload = event.payload;
      const meta = getSyncProgressMeta(payload);

      if (payload.status === "error") {
        console.error(`[SIFT sync ${payload.runId}] ${meta} · ${payload.message}`, payload);
      } else {
        console.info(`[SIFT sync ${payload.runId}] ${meta} · ${payload.message}`, payload);
      }

      setSyncProgress(payload);
      setMessage(payload.message);

      if (payload.status === "success") {
        void refreshBootstrap().catch((error: unknown) => {
          console.error("[SIFT sync] Sync finished but the desk could not be refreshed.", error);
        });

        if (payload.reason === "manual" && payload.editionId) {
          setSelectedEditionId(payload.editionId);
          setScreen("today");
        }
      }
    })
      .then((dispose) => {
        if (isDisposed) {
          dispose();
          return;
        }

        unlisten = dispose;
      })
      .catch((error: unknown) => {
        console.error("[SIFT sync] Unable to subscribe to progress events.", error);
      });

    return () => {
      isDisposed = true;
      unlisten?.();
    };
  }, []);

  async function ensureDesktopRuntime() {
    if (!(await isAutostartEnabled())) {
      await enableAutostart();
    }

    if (!(await isNotificationPermissionGranted())) {
      await requestNotificationPermission();
    }
  }

  async function refreshBootstrap() {
    const state = await getBootstrapState();
    applyBootstrapState(state, state.latestRun?.editionId ?? null);
    return state;
  }

  async function refreshXSessionState() {
    const state = await getXSessionState();
    setXSession(state);
  }

  async function hydrateSavedLmStudio(settings: UserSettings) {
    if (!settings.lmStudio.baseUrl.trim()) {
      setLmHealth(null);
      return;
    }

    try {
      const health = await verifyLmStudio(
        settings.lmStudio.baseUrl,
        settings.lmStudio.authToken
      );
      setLmHealth(health);
    } catch {
      setLmHealth(null);
    }
  }

  async function handleVerifyLmStudio() {
    try {
      const health = await verifyLmStudio(
        lmStudioDraft.baseUrl,
        lmStudioDraft.authToken
      );
      setLmHealth(health);

      const nextSettings: UserSettings = {
        ...bootstrap.settings,
        lmStudio: {
          ...lmStudioDraft,
          selectedModel:
            health.models.find((model) => model.id === DEFAULT_MODEL)?.id ??
            lmStudioDraft.selectedModel ??
            health.models[0]?.id ??
            null
        }
      };

      const saved = await saveSettings(nextSettings);
      setBootstrap((current) => ({ ...current, settings: saved }));
      setLmStudioDraft((current) => ({
        ...saved.lmStudio,
        authToken: current.authToken
      }));
      setMessage(health.models.length ? "LM Studio verified." : "LM Studio connected, but no models are available yet.");
    } catch (error) {
      setMessage(getErrorMessage(error, "LM Studio check failed."));
    }
  }

  async function handleSaveSettings() {
    try {
      const saved = await saveSettings({
        ...bootstrap.settings,
        lmStudio: lmStudioDraft
      });
      setBootstrap((current) => ({ ...current, settings: saved }));
      setLmStudioDraft((current) => ({
        ...saved.lmStudio,
        authToken: current.authToken
      }));
      void hydrateSavedLmStudio({
        ...saved,
        lmStudio: {
          ...saved.lmStudio,
          authToken: lmStudioDraft.authToken
        }
      });
      setIsModelDeskExpanded(false);
      setMessage("Settings saved locally.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to save settings."));
    }
  }

  async function handleRunSync() {
    setIsRefreshing(true);
    setSyncProgress(null);
    console.info("[SIFT sync] Manual refresh requested.");

    try {
      setMessage(
        xSession.isOpen
          ? "Bringing the X session to the foreground for refresh..."
          : "Opening the X session before refresh..."
      );
      const session = await openXSessionWindow();
      setXSession(session);

      setMessage("Starting refresh. Checking the live X session...");
      const state = await runSync("manual");
      let freshEdition = pickFreshEdition(state);
      const noFreshMessage =
        state.latestRun?.status === "success" && !state.latestRun?.editionId
          ? state.latestRun.errorMessage
          : null;

      applyBootstrapState(state, freshEdition?.id ?? state.latestRun?.editionId ?? null);

      if (!freshEdition && !noFreshMessage) {
        try {
          const refreshedState = await refreshBootstrap();
          freshEdition = pickFreshEdition(refreshedState);
        } catch (refreshError) {
          console.error("[SIFT sync] Refresh completed but the desk state could not be reloaded.", refreshError);
        }
      }

      setScreen("today");
      setMessage(
        noFreshMessage
          ? noFreshMessage
          : freshEdition
          ? `Showing ${freshEdition.title}.`
          : "Fresh edition generated."
      );
    } catch (error) {
      const detail = getErrorMessage(error, "Sync failed.");
      console.error("[SIFT sync] Manual refresh failed.", error);
      setMessage(detail);
    } finally {
      try {
        const session = await hideXSessionWindow();
        setXSession(session);
      } catch (hideError) {
        console.error("[SIFT sync] Refresh finished but the X session could not be hidden.", hideError);
      }

      setIsRefreshing(false);
    }
  }

  async function handleOpenXSession() {
    try {
      const session = await openXSessionWindow();
      setXSession(session);
      setMessage(
        session.isAuthenticated
          ? "The X session is ready. Keep your browsing inside that SIFT-managed X window."
          : "The X session window is open. Sign in there and keep your browsing inside that SIFT-managed window."
      );
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to open the X session window."));
    }
  }

  async function handleHideXSession() {
    try {
      const session = await hideXSessionWindow();
      setXSession(session);
      setMessage("The X session is hidden. Your sign-in stays alive in the background.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to hide the X session window."));
    }
  }

  async function handleLogoutXSession() {
    const shouldContinue =
      typeof window.confirm !== "function"
        || window.confirm("Log out of X in SIFT and clear this browser session?");

    if (!shouldContinue) {
      return;
    }

    try {
      const session = await logoutXSessionWindow();
      setXSession(session);
      setMessage("Logged out of X in SIFT. Open the window again whenever you want to sign back in.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to log out of the X session."));
    }
  }

  async function handleDisconnectX() {
    try {
      const state = await disconnectX();
      setBootstrap(state);
      setMessage("Legacy X API connection cleared.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Could not disconnect X."));
    }
  }

  async function handleOpenSourcePost(url: string) {
    try {
      await openExternalUrl(url);
      setMessage("Opened the source post in your default browser.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Could not open the source post in your browser."));
    }
  }

  return (
    <main className="app-shell">
      <header className="masthead masthead--compact">
        <div className="brand-header">
          <img
            className="brand-mark"
            src="/sift-mark.png"
alt="SIFT"
          />
          <div>
            <p className="kicker">SIFT Daily Briefing</p>
<h1>The signal in your feed, without the noise.</h1>
          </div>
        </div>

        <div className="masthead-actions">
          <button
            className={screen === "today" ? "nav-button nav-button--active" : "nav-button"}
            onClick={() => setScreen("today")}
          >
            Today
          </button>
          <button
            className={screen === "archive" ? "nav-button nav-button--active" : "nav-button"}
            onClick={() => setScreen("archive")}
          >
            Archive
          </button>
          <button
            className={screen === "settings" ? "nav-button nav-button--active" : "nav-button"}
            onClick={() => setScreen("settings")}
          >
            Settings
          </button>
          <button className="primary-button" onClick={handleRunSync} disabled={isRefreshBusy}>
            {isRefreshBusy ? "Refreshing..." : "Refresh edition"}
          </button>
        </div>
      </header>

<section className="status-strip" aria-live="polite">
        <span className="status-strip__primary">{statusMessage}</span>
        <span className="status-strip__meta">{statusMeta}</span>
      </section>

      <div className="layout-grid">
        <aside className="sidebar panel">
          <section>
            <h2>Browser</h2>
            <div className="stack">
              <div className="session-control">
                <div className="session-pill">
                  <span
                    className={`session-dot ${xSession.isAuthenticated ? "session-dot--connected" : xSession.isOpen ? "session-dot--open" : "session-dot--closed"}`}
                  />
                  <span className="session-pill__text">
                    {xSession.isAuthenticated
                      ? "Connected"
                      : xSession.isOpen
                        ? "Waiting for sign-in"
                        : "Session closed"}
                  </span>
                  {xSession.lastKnownUrl ? (
                    <span className="session-pill__url">
                      {xSession.lastKnownUrl.replace(/^https?:\/\//, "").split("/")[0]}
                    </span>
                  ) : null}
                </div>
                <div className="session-actions" role="toolbar" aria-label="X session controls">
                  <SessionControlButton
                    label={getXSessionToggleLabel(xSession)}
                    onClick={xSession.isOpen && xSession.isVisible ? handleHideXSession : handleOpenXSession}
                  >
                    {xSession.isOpen && xSession.isVisible ? <HideIcon /> : <ShowIcon />}
                  </SessionControlButton>
                  {xSession.isOpen ? (
                    <SessionControlButton
                      label="Log out of X in SIFT"
                      tone="danger"
                      onClick={handleLogoutXSession}
                    >
                      <LogoutIcon />
                    </SessionControlButton>
                  ) : null}
                </div>
              </div>

              {bootstrap.xConnection ? (
                <div className="mini-card">
                  <strong>Legacy API connection detected</strong>
                  <span>
                    @{bootstrap.xConnection.handle} is still stored locally, but it is not required for the new browser-session path.
                  </span>
                  <button className="secondary-button" onClick={handleDisconnectX}>
                    Clear Legacy Connection
                  </button>
                </div>
              ) : null}
            </div>
          </section>

          <section className="model-desk">
            <button
              className={
                isModelDeskExpanded
                  ? "model-desk__summary model-desk__summary--open"
                  : "model-desk__summary"
              }
              onClick={() => setIsModelDeskExpanded((current) => !current)}
              type="button"
            >
              <span className="model-desk__summary-main">
                <span className="model-desk__icon" aria-hidden="true">
                  <span />
                  <span />
                  <span />
                </span>
                <span className="model-desk__summary-copy">
                  <strong>Model desk</strong>
                  <span>{getModelDeskSummary(selectedModelId, lmHealth)}</span>
                </span>
              </span>
              <span className="model-desk__summary-meta">
                <span className={lmHealth ? "status-badge status-badge--ready" : "status-badge"}>
                  {getModelDeskStatusLabel(selectedModelId, lmHealth)}
                </span>
                <span className="model-desk__chevron" aria-hidden="true">
                  {isModelDeskExpanded ? "−" : "+"}
                </span>
              </span>
            </button>

            {isModelDeskExpanded ? (
              <div className="model-desk__panel">
                <div className="model-desk__group">
                  <label className="field">
                    <span>LM Studio URL</span>
                    <input
                      value={lmStudioDraft.baseUrl}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          baseUrl: event.target.value
                        }))
                      }
                    />
                  </label>
                  <label className="field">
                    <span>Auth token</span>
                    <input
                      type="password"
                      value={lmStudioDraft.authToken ?? ""}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          authToken: event.target.value || null
                        }))
                      }
                      placeholder="Optional"
                    />
                    <small>Optional. Kept only for the current app session.</small>
                  </label>
                  <div className="button-row model-desk__actions">
                    <button className="primary-button" onClick={handleVerifyLmStudio}>
                      Verify
                    </button>
                    <button className="secondary-button" onClick={handleSaveSettings}>
                      Save
                    </button>
                  </div>
                </div>

                <div className="model-desk__group">
                  <label className="field">
                    <span>Selected model</span>
                    <select
                      value={lmStudioDraft.selectedModel ?? ""}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          selectedModel: event.target.value || null
                        }))
                      }
                      disabled={!availableModels.length}
                    >
                      <option value="">
                        {availableModels.length ? "Pick a local model" : "Verify LM Studio first"}
                      </option>
                      {availableModels.map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.id}
                        </option>
                      ))}
                    </select>
                  </label>

                  <div className={lmHealth ? "model-status model-status--verified" : "model-status"}>
                    <strong>
                      {lmHealth
                        ? "LM Studio verified"
                        : selectedModelId
                          ? "Saved model restored"
                          : "Not verified yet"}
                    </strong>
                    <span>
                      {lmHealth
                        ? getLmStudioSummary(lmHealth)
                        : selectedModelId
                          ? "SIFT restored your saved LM Studio selection. Verify to refresh the live model list."
                          : "Verify the connection to load local models."}
                    </span>
                    {selectedModelId ? (
                      <span className="model-status__selected">
                        Active: <strong>{selectedModelId}</strong>
                      </span>
                    ) : null}
                  </div>
                </div>
              </div>
            ) : null}
          </section>
        </aside>

        {screen === "settings" ? (
          <SettingsPanel
            settings={bootstrap.settings}
            onChange={(next) => setBootstrap((current) => ({ ...current, settings: next }))}
            onSave={async (settings) => {
              const saved = await saveSettings({
                ...settings,
                lmStudio: {
                  ...settings.lmStudio,
                  authToken: lmStudioDraft.authToken
                }
              });
              setBootstrap((current) => ({ ...current, settings: saved }));
              setMessage("Paper rules updated.");
            }}
          />
        ) : screen === "archive" ? (
          <ArchivePanel
            editions={bootstrap.editions}
            selectedEditionId={selectedEditionId}
            onSelect={setSelectedEditionId}
          />
        ) : (
          <EditionPanel edition={selectedEdition} onOpenSourcePost={handleOpenSourcePost} />
        )}
      </div>
    </main>
  );
}

function SettingsPanel({
  settings,
  onChange,
  onSave
}: {
  settings: UserSettings;
  onChange: (value: UserSettings) => void;
  onSave: (value: UserSettings) => Promise<void>;
}) {
  return (
    <section className="panel content-panel">
      <div className="section-header">
        <p className="kicker">Settings</p>
        <h2>Shape the paper.</h2>
      </div>

      <div className="settings-grid">
        <label className="field">
          <span>Morning publish time</span>
          <input
            type="time"
            value={settings.schedule.timeOfDay}
            onChange={(event) =>
              onChange({
                ...settings,
                schedule: {
                  ...settings.schedule,
                  timeOfDay: event.target.value
                }
              })
            }
          />
        </label>

        <label className="field">
          <span>Timezone</span>
          <input
            value={settings.schedule.timezone}
            onChange={(event) =>
              onChange({
                ...settings,
                schedule: {
                  ...settings.schedule,
                  timezone: event.target.value
                }
              })
            }
          />
        </label>

        <label className="field field--checkbox">
          <input
            type="checkbox"
            checked={settings.schedule.enabled}
            onChange={(event) =>
              onChange({
                ...settings,
                schedule: {
                  ...settings.schedule,
                  enabled: event.target.checked
                }
              })
            }
          />
          <span>Enable morning auto-run</span>
        </label>

        <label className="field field--checkbox">
          <input
            type="checkbox"
            checked={settings.cleanup.hideReplies}
            onChange={(event) =>
              onChange({
                ...settings,
                cleanup: {
                  ...settings.cleanup,
                  hideReplies: event.target.checked
                }
              })
            }
          />
          <span>Drop replies</span>
        </label>

        <label className="field field--checkbox">
          <input
            type="checkbox"
            checked={settings.cleanup.hideRetweets}
            onChange={(event) =>
              onChange({
                ...settings,
                cleanup: {
                  ...settings.cleanup,
                  hideRetweets: event.target.checked
                }
              })
            }
          />
          <span>Drop reposts</span>
        </label>

        <label className="field field--checkbox">
          <input
            type="checkbox"
            checked={settings.cleanup.removeBait}
            onChange={(event) =>
              onChange({
                ...settings,
                cleanup: {
                  ...settings.cleanup,
                  removeBait: event.target.checked
                }
              })
            }
          />
          <span>Filter common engagement bait</span>
        </label>

        <label className="field">
          <span>Muted keywords</span>
          <textarea
            value={settings.cleanup.mutedKeywords.join("\n")}
            onChange={(event) =>
              onChange({
                ...settings,
                cleanup: {
                  ...settings.cleanup,
                  mutedKeywords: event.target.value
                    .split("\n")
                    .map((value) => value.trim())
                    .filter(Boolean)
                }
              })
            }
            placeholder="One phrase per line"
          />
        </label>

        <label className="field">
          <span>Muted authors</span>
          <textarea
            value={settings.cleanup.mutedAuthors.join("\n")}
            onChange={(event) =>
              onChange({
                ...settings,
                cleanup: {
                  ...settings.cleanup,
                  mutedAuthors: event.target.value
                    .split("\n")
                    .map((value) => value.trim())
                    .filter(Boolean)
                }
              })
            }
            placeholder="One handle per line"
          />
        </label>
      </div>

      <button className="primary-button" onClick={() => void onSave(settings)}>
        Save newsroom settings
      </button>
    </section>
  );
}

function ArchivePanel({
  editions,
  selectedEditionId,
  onSelect
}: {
  editions: Edition[];
  selectedEditionId: string | null;
  onSelect: (value: string) => void;
}) {
  return (
    <section className="panel content-panel">
      <div className="section-header">
        <p className="kicker">Archive</p>
        <h2>Past editions</h2>
      </div>

      <div className="archive-list">
        {editions.length ? (
          editions.map((edition) => (
            <button
              key={edition.id}
              className={
                edition.id === selectedEditionId
                  ? "archive-item archive-item--active"
                  : "archive-item"
              }
              onClick={() => onSelect(edition.id)}
            >
              <strong>{formatEditionDate(edition.editionDate)}</strong>
              <span>{edition.title}</span>
              <small>Saved {formatTime(edition.createdAt)}</small>
            </button>
          ))
        ) : (
          <p className="empty-copy">Once your first issue is generated, it will land here.</p>
        )}
      </div>
    </section>
  );
}

function EditionPanel({
  edition,
  onOpenSourcePost
}: {
  edition: Edition | null;
  onOpenSourcePost: (url: string) => void;
}) {
  if (!edition) {
    return (
      <section className="panel content-panel">
        <div className="section-header">
          <p className="kicker">Today</p>
          <h2>No issue on the desk yet.</h2>
        </div>
        <p className="empty-copy">
          Open X Session, verify LM Studio, and finish wiring the browser capture flow to draft your first edition.
        </p>
      </section>
    );
  }

  return (
    <section className="panel content-panel paper-panel">
      <div className="paper-head">
        <p className="paper-flag">SIFT Daily Briefing</p>
        <h2>{edition.title}</h2>
        <p className="paper-date">
          {formatEditionDate(edition.editionDate)} · Saved {formatTime(edition.createdAt)}
        </p>
      </div>

      <p className="paper-summary">{edition.frontPageSummary}</p>

      <div className="paper-sections">
        {edition.sections.map((section) => (
          <article key={section.id} className="paper-section">
            <header>
              <p className="paper-section__eyebrow">{section.dek}</p>
              <h3>{section.title}</h3>
            </header>

            <div className="story-grid">
              {section.cards.map((card) => (
                <button
                  type="button"
                  key={card.itemId}
                  className="story-card"
                  onClick={() => void onOpenSourcePost(card.sourceUrl)}
                  aria-label={`Open source post for ${card.headline}`}
                  title={card.sourceUrl}
                >
                  <span className="story-card__meta">
                    {card.authorName} · @{card.authorHandle}
                  </span>
                  <h4>{card.headline}</h4>
                  <p>{card.summary}</p>
                  <span className="story-card__why">{card.whyItMatters}</span>
                </button>
              ))}
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
