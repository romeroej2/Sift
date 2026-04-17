import { useEffect, useMemo, useState, type ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { enable as enableAutostart, isEnabled as isAutostartEnabled } from "@tauri-apps/plugin-autostart";
import {
  isPermissionGranted as isNotificationPermissionGranted,
  requestPermission as requestNotificationPermission
} from "@tauri-apps/plugin-notification";
import type {
  BrowserSessionState,
  BrowserSource,
  BootstrapState,
  Edition,
  EditionView,
  LmStudioHealth,
  SyncProgressEvent,
  UserSettings
} from "./lib/types";
import { DEFAULT_MODEL, DEFAULT_SETTINGS, getMachineTimeZone } from "./lib/defaults";
import { formatEditionDate, formatTime } from "./lib/format";
import {
  EMPTY_BROWSER_SESSION,
  EMPTY_BOOTSTRAP,
  SYNC_PROGRESS_EVENT,
  getAvailableEditionViews,
  getErrorMessage,
  getLmStudioSummary,
  getModelDeskStatusLabel,
  getModelDeskSummary,
  getSessionToggleLabel,
  getScheduleSummary,
  getSyncProgressMeta,
  pickFreshEdition,
  pickInitialEdition,
  withTimeout
} from "./lib/app-utils";
import {
  disconnectX,
  getBootstrapState,
  getLinkedInSessionState,
  getXSessionState,
  hideLinkedInSessionWindow,
  hideXSessionWindow,
  logoutLinkedInSessionWindow,
  logoutXSessionWindow,
  openExternalUrl,
  openLinkedInSessionWindow,
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

function SourceBrandIcon({ source }: { source: BrowserSource }) {
  if (source === "linkedin") {
    return (
      <span className="session-pill__brand session-pill__brand--linkedin" aria-hidden="true">
        <span className="session-pill__brand-text">in</span>
      </span>
    );
  }

  return (
    <span className="session-pill__brand session-pill__brand--x" aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="currentColor">
        <path d="M18.9 2H22l-6.78 7.75L23.2 22h-6.25l-4.9-6.2L6.62 22H3.5l7.2-8.24L1.2 2h6.4l4.43 5.63L18.9 2Zm-1.1 18h1.72L6.67 3.9H4.83Z" />
      </svg>
    </span>
  );
}

function getEditionViewLabel(view: EditionView) {
  return view === "consolidated" ? "Consolidated" : view === "linkedin" ? "LinkedIn" : "X";
}

function EditionViewTabs({
  ariaLabel,
  availableViews,
  selectedView,
  onSelect
}: {
  ariaLabel: string;
  availableViews: EditionView[];
  selectedView: EditionView;
  onSelect: (view: EditionView) => void;
}) {
  if (!availableViews.length) {
    return null;
  }

  return (
    <div className="panel-tabs">
      <div className="book-tabs" role="tablist" aria-label={ariaLabel}>
        {availableViews.map((view) => (
          <button
            key={view}
            type="button"
            role="tab"
            aria-selected={selectedView === view}
            className={selectedView === view ? "book-tab book-tab--active" : "book-tab"}
            onClick={() => onSelect(view)}
          >
            {getEditionViewLabel(view)}
          </button>
        ))}
      </div>
    </div>
  );
}

function BrowserSessionCard({
  source,
  session,
  onOpen,
  onHide,
  onLogout
}: {
  source: BrowserSource;
  session: BrowserSessionState;
  onOpen: () => void;
  onHide: () => void;
  onLogout: () => void;
}) {
  const sourceLabel = source === "linkedin" ? "LinkedIn" : "X";
  const connectionLabel = session.isAuthenticated
    ? "Connected"
    : session.isOpen
      ? "Waiting for sign-in"
      : "Session closed";

  return (
    <div className="session-control">
      <div
        className={`session-pill session-pill--${source}${session.isAuthenticated ? " session-pill--connected" : session.isOpen ? " session-pill--open" : ""}`}
      >
        <SourceBrandIcon source={source} />
        <div className="session-pill__copy">
          <div className="session-pill__header">
            <span className="session-pill__label">{sourceLabel}</span>
          </div>
          <span className="session-pill__status">{connectionLabel}</span>
        </div>
      </div>
      <div className="session-actions" role="toolbar" aria-label={`${sourceLabel} session controls`}>
        <SessionControlButton
          label={getSessionToggleLabel(source, session)}
          onClick={session.isOpen && session.isVisible ? onHide : onOpen}
        >
          {session.isOpen && session.isVisible ? <HideIcon /> : <ShowIcon />}
        </SessionControlButton>
        {session.isOpen ? (
          <SessionControlButton
            label={`Log out of ${sourceLabel} in SIFT`}
            tone="danger"
            onClick={onLogout}
          >
            <LogoutIcon />
          </SessionControlButton>
        ) : null}
      </div>
    </div>
  );
}

function getEditionImageSrc(path: string) {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    try {
      return convertFileSrc(path);
    } catch {
      return path;
    }
  }

  return path;
}

export default function App() {
  const [screen, setScreen] = useState<Screen>("today");
  const [bootstrap, setBootstrap] = useState<BootstrapState>(EMPTY_BOOTSTRAP);
  const [selectedEditionId, setSelectedEditionId] = useState<string | null>(null);
  const [selectedView, setSelectedView] = useState<EditionView>("consolidated");
  const [message, setMessage] = useState("Opening today\'s desk...");
  const [lmStudioDraft, setLmStudioDraft] = useState(DEFAULT_SETTINGS.lmStudio);
  const [sessionStates, setSessionStates] = useState<Record<BrowserSource, BrowserSessionState>>({
    x: EMPTY_BROWSER_SESSION,
    linkedin: EMPTY_BROWSER_SESSION
  });
  const [lmHealth, setLmHealth] = useState<LmStudioHealth | null>(null);
  const [isModelDeskExpanded, setIsModelDeskExpanded] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [syncProgress, setSyncProgress] = useState<SyncProgressEvent | null>(null);
  const [clockNow, setClockNow] = useState(() => Date.now());
  const availableModels = lmHealth?.models ?? [];
  const xSession = sessionStates.x;
  const linkedinSession = sessionStates.linkedin;
  const selectedModelDescriptor =
    availableModels.find((model) => model.id === lmStudioDraft.selectedModel) ?? null;
  const selectedModelId = selectedModelDescriptor?.id ?? lmStudioDraft.selectedModel;
  const liveSyncProgress = syncProgress?.status === "running" ? syncProgress : null;
  const isRefreshBusy = isRefreshing || liveSyncProgress !== null;
  const statusMessage = liveSyncProgress?.message ?? message;
  const availableViews = useMemo(() => getAvailableEditionViews(bootstrap.editions), [bootstrap.editions]);
  const latestEditionTitle = pickFreshEdition(bootstrap, selectedView)?.title ?? null;
  const statusMeta = liveSyncProgress
    ? getSyncProgressMeta(liveSyncProgress)
    : bootstrap.latestRun
      ? bootstrap.latestRun.status === "success" && !bootstrap.latestRun.editionId && bootstrap.latestRun.errorMessage
        ? bootstrap.latestRun.errorMessage
        : `${latestEditionTitle ? `${latestEditionTitle} · ` : ""}Last run ${formatTime(bootstrap.latestRun.startedAt)} · ${bootstrap.latestRun.status}`
      : "No edition generated yet.";
  const scheduleSummary = useMemo(
    () => getScheduleSummary(bootstrap.settings.schedule, sessionStates, bootstrap.settings, new Date(clockNow)),
    [bootstrap.settings.schedule, bootstrap.settings, clockNow, sessionStates]
  );

  const selectedEdition = useMemo(() => {
    const visibleEditions = bootstrap.editions.filter((edition) => edition.view === selectedView);
    if (!visibleEditions.length) {
      return null;
    }

    return (
      visibleEditions.find((edition) => edition.id === selectedEditionId) ??
      visibleEditions[0]
    );
  }, [bootstrap.editions, selectedEditionId, selectedView]);

  function resolveSelectedEditionId(
    state: BootstrapState,
    currentEditionId: string | null,
    currentView: EditionView,
    preferredEditionId: string | null = null
  ) {
    const visibleEditions = state.editions.filter((edition) => edition.view === currentView);

    if (!state.editions.length) {
      return null;
    }

    if (
      preferredEditionId
      && visibleEditions.some((edition) => edition.id === preferredEditionId)
    ) {
      return preferredEditionId;
    }

    if (
      currentEditionId
      && visibleEditions.some((edition) => edition.id === currentEditionId)
    ) {
      return currentEditionId;
    }

    return pickFreshEdition(state, currentView)?.id ?? pickInitialEdition(visibleEditions)?.id ?? null;
  }

  function applyBootstrapState(state: BootstrapState, preferredEditionId: string | null = null) {
    setBootstrap(state);
    setSelectedView((currentView) => {
      const nextView = getAvailableEditionViews(state.editions).includes(currentView)
        ? currentView
        : getAvailableEditionViews(state.editions)[0] ?? "consolidated";
      setSelectedEditionId((current) =>
        resolveSelectedEditionId(state, current, nextView, preferredEditionId)
      );
      return nextView;
    });
  }

  useEffect(() => {
    const visibleEditions = bootstrap.editions.filter((edition) => edition.view === selectedView);
    if (visibleEditions.length && !selectedEditionId) {
      setSelectedEditionId(visibleEditions[0].id);
    }
  }, [bootstrap.editions, selectedEditionId, selectedView]);

  useEffect(() => {
    const interval = window.setInterval(() => {
      setClockNow(Date.now());
    }, 30000);

    return () => {
      window.clearInterval(interval);
    };
  }, []);

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
        const [state, xSessionState, linkedinSessionState] = await Promise.all([
          withTimeout(getBootstrapState(), "Loading the newsroom"),
          withTimeout(getXSessionState(), "Loading the X session"),
          withTimeout(getLinkedInSessionState(), "Loading the LinkedIn session")
        ]);

        if (isCancelled) {
          return;
        }

        applyBootstrapState(state);
        setSessionStates({
          x: xSessionState,
          linkedin: linkedinSessionState
        });
        void hydrateSavedLmStudio(state.settings);
        setMessage(
          "SIFT is ready. Your enabled browser sessions are standing by."
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
      void Promise.all([getXSessionState(), getLinkedInSessionState()])
        .then(([xState, linkedinState]) => {
          if (!isCancelled) {
            setSessionStates({
              x: xState,
              linkedin: linkedinState
            });
          }
        })
        .catch(() => {
          // The dedicated browser session windows are optional, so polling failures can stay quiet.
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

  function setSessionState(source: BrowserSource, session: BrowserSessionState) {
    setSessionStates((current) => ({
      ...current,
      [source]: session
    }));
  }

  async function openSourceSession(source: BrowserSource) {
    const session = source === "linkedin"
      ? await openLinkedInSessionWindow()
      : await openXSessionWindow();
    setSessionState(source, session);
    return session;
  }

  async function hideSourceSession(source: BrowserSource) {
    const session = source === "linkedin"
      ? await hideLinkedInSessionWindow()
      : await hideXSessionWindow();
    setSessionState(source, session);
    return session;
  }

  async function logoutSourceSession(source: BrowserSource) {
    const session = source === "linkedin"
      ? await logoutLinkedInSessionWindow()
      : await logoutXSessionWindow();
    setSessionState(source, session);
    return session;
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
    if (!bootstrap.settings.capture.sources.x && !bootstrap.settings.capture.sources.linkedin) {
      setMessage("Pick at least one source before saving newsroom settings.");
      return;
    }

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
    const enabledSources = (Object.entries(bootstrap.settings.capture.sources) as Array<[BrowserSource, boolean]>)
      .filter(([, enabled]) => enabled)
      .map(([source]) => source);

    try {
      setMessage("Opening the enabled source sessions before refresh...");
      for (const source of enabledSources) {
        await openSourceSession(source);
      }

      setMessage("Starting refresh. Checking the live sessions...");
      const state = await runSync("manual");
      let freshEdition = pickFreshEdition(state, selectedView);
      const noFreshMessage =
        state.latestRun?.status === "success" && !state.latestRun?.editionId
          ? state.latestRun.errorMessage
          : null;

      applyBootstrapState(state, freshEdition?.id ?? state.latestRun?.editionId ?? null);

      if (!freshEdition && !noFreshMessage) {
        try {
          const refreshedState = await refreshBootstrap();
          freshEdition = pickFreshEdition(refreshedState, selectedView);
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
      for (const source of enabledSources) {
        try {
          await hideSourceSession(source);
        } catch (hideError) {
          console.error(`[SIFT sync] Refresh finished but the ${source} session could not be hidden.`, hideError);
        }
      }

      setIsRefreshing(false);
    }
  }

  async function handleOpenXSession() {
    try {
      const session = await openSourceSession("x");
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
      await hideSourceSession("x");
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
      await logoutSourceSession("x");
      setMessage("Logged out of X in SIFT. Open the window again whenever you want to sign back in.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to log out of the X session."));
    }
  }

  async function handleOpenLinkedInSession() {
    try {
      const session = await openSourceSession("linkedin");
      setMessage(
        session.isAuthenticated
          ? "The LinkedIn session is ready. Keep your browsing inside that SIFT-managed LinkedIn window."
          : "The LinkedIn session window is open. Sign in there and keep your browsing inside that SIFT-managed window."
      );
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to open the LinkedIn session window."));
    }
  }

  async function handleHideLinkedInSession() {
    try {
      await hideSourceSession("linkedin");
      setMessage("The LinkedIn session is hidden. Your sign-in stays alive in the background.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to hide the LinkedIn session window."));
    }
  }

  async function handleLogoutLinkedInSession() {
    const shouldContinue =
      typeof window.confirm !== "function"
        || window.confirm("Log out of LinkedIn in SIFT and clear this browser session?");

    if (!shouldContinue) {
      return;
    }

    try {
      await logoutSourceSession("linkedin");
      setMessage("Logged out of LinkedIn in SIFT. Open the window again whenever you want to sign back in.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to log out of the LinkedIn session."));
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

  function handleSelectView(view: EditionView) {
    setSelectedView(view);
    setSelectedEditionId(
      pickInitialEdition(
        bootstrap.editions.filter((edition) => edition.view === view),
        view
      )?.id ?? null
    );
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
              <BrowserSessionCard
                source="x"
                session={xSession}
                onOpen={handleOpenXSession}
                onHide={handleHideXSession}
                onLogout={handleLogoutXSession}
              />
              <BrowserSessionCard
                source="linkedin"
                session={linkedinSession}
                onOpen={handleOpenLinkedInSession}
                onHide={handleHideLinkedInSession}
                onLogout={handleLogoutLinkedInSession}
              />

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

                  <label className="field field--checkbox">
                    <input
                      type="checkbox"
                      checked={lmStudioDraft.includeImages}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          includeImages: event.target.checked
                        }))
                      }
                    />
                    <span>Use attached post images during ranking</span>
                  </label>
                  <p className="field-help">
                    Enable this only for vision-capable local models. SIFT will download attached post photos and send them to LM Studio when ranking digest topics.
                  </p>
                </div>
              </div>
            ) : null}
          </section>
        </aside>

        <div className="desk-column">
          {screen === "settings" ? (
            <SettingsPanel
              settings={bootstrap.settings}
              scheduleSummary={scheduleSummary}
              onChange={(next) => setBootstrap((current) => ({ ...current, settings: next }))}
              onSave={async (settings) => {
                if (!settings.capture.sources.x && !settings.capture.sources.linkedin) {
                  setMessage("Pick at least one source before saving newsroom settings.");
                  return;
                }
                const saved = await saveSettings({
                  ...settings,
                  schedule: {
                    ...settings.schedule,
                    timezone: getMachineTimeZone()
                  },
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
              tabs={(
                <EditionViewTabs
                  ariaLabel="Archive view"
                  availableViews={availableViews}
                  selectedView={selectedView}
                  onSelect={handleSelectView}
                />
              )}
              editions={bootstrap.editions.filter((edition) => edition.view === selectedView)}
              selectedEditionId={selectedEditionId}
              onSelect={setSelectedEditionId}
            />
          ) : (
            <EditionPanel
              tabs={(
                <EditionViewTabs
                  ariaLabel="Edition view"
                  availableViews={availableViews}
                  selectedView={selectedView}
                  onSelect={handleSelectView}
                />
              )}
              edition={selectedEdition}
              onOpenSourcePost={handleOpenSourcePost}
            />
          )}
        </div>
      </div>
    </main>
  );
}

function SettingsPanel({
  settings,
  scheduleSummary,
  onChange,
  onSave
}: {
  settings: UserSettings;
  scheduleSummary: { title: string; detail: string };
  onChange: (value: UserSettings) => void;
  onSave: (value: UserSettings) => Promise<void>;
}) {
  const hasEnabledSources = settings.capture.sources.x || settings.capture.sources.linkedin;
  const updateBrowseCount = (source: BrowserSource, fallback: number) => (rawValue: string) =>
    onChange({
      ...settings,
      capture: {
        ...settings.capture,
        browsePageCount: {
          ...settings.capture.browsePageCount,
          [source]: Math.max(1, Number.parseInt(rawValue || String(fallback), 10) || fallback)
        }
      }
    });

  return (
    <section className="panel content-panel">
      <div className="section-header">
        <p className="kicker">Settings</p>
        <h2>Shape the paper.</h2>
      </div>

      <div className="settings-stack">
        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Capture</p>
              <h3>Source desk</h3>
            </div>
            <p className="settings-card__copy">
              Choose where the paper should pull from and how deep each live feed should be browsed before drafting.
            </p>
          </div>

          <div className="settings-source-grid">
            <section
              className={`settings-source-tile${settings.capture.sources.x ? " settings-source-tile--enabled" : ""}`}
            >
              <div className="settings-source-tile__top">
                <div>
                  <span className="settings-source-tile__title">X</span>
                  <span className="settings-source-tile__eyebrow">Short-form pulse</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.capture.sources.x}
                  onChange={(event) =>
                    onChange({
                      ...settings,
                      capture: {
                        ...settings.capture,
                        sources: {
                          ...settings.capture.sources,
                          x: event.target.checked
                        }
                      }
                    })
                  }
                />
              </div>
              <p className="settings-source-tile__copy">
                Fast, denser posts. Good when you want more breadth and chatter.
              </p>
              <label className="field">
                <span>X pages to browse</span>
                <input
                  type="number"
                  min={1}
                  value={settings.capture.browsePageCount.x}
                  onChange={(event) => updateBrowseCount("x", 12)(event.target.value)}
                />
              </label>
            </section>

            <section
              className={`settings-source-tile${settings.capture.sources.linkedin ? " settings-source-tile--enabled" : ""}`}
            >
              <div className="settings-source-tile__top">
                <div>
                  <span className="settings-source-tile__title">LinkedIn</span>
                  <span className="settings-source-tile__eyebrow">Long-form signal</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.capture.sources.linkedin}
                  onChange={(event) =>
                    onChange({
                      ...settings,
                      capture: {
                        ...settings.capture,
                        sources: {
                          ...settings.capture.sources,
                          linkedin: event.target.checked
                        }
                      }
                    })
                  }
                />
              </div>
              <p className="settings-source-tile__copy">
                Larger, slower cards. Tune this separately when you want fewer but heavier LinkedIn pages.
              </p>
              <label className="field">
                <span>LinkedIn pages to browse</span>
                <input
                  type="number"
                  min={1}
                  value={settings.capture.browsePageCount.linkedin}
                  onChange={(event) => updateBrowseCount("linkedin", 8)(event.target.value)}
                />
              </label>
            </section>
          </div>

          {!hasEnabledSources ? (
            <p className="field-help">Pick at least one source before saving.</p>
          ) : null}
        </section>

        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Schedule</p>
              <h3>Morning run</h3>
            </div>
            <p className="settings-card__copy">
              SIFT uses this machine&apos;s timezone automatically for scheduling and edition boundaries.
            </p>
          </div>

          <div className="settings-schedule-grid">
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

            <div className="mini-card">
              <strong>Morning auto-run</strong>
              <span>{scheduleSummary.title}</span>
              <span>{scheduleSummary.detail}</span>
            </div>
          </div>

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
        </section>

        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Cleanup</p>
              <h3>Filter rules</h3>
            </div>
            <p className="settings-card__copy">
              Keep the ranking pass focused by stripping the content you already know you do not want in the paper.
            </p>
          </div>

          <div className="settings-toggle-grid">
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
          </div>

          <div className="settings-copy-grid">
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
        </section>
      </div>

      <button
        className="primary-button"
        onClick={() => void onSave(settings)}
        disabled={!hasEnabledSources}
      >
        Save newsroom settings
      </button>
    </section>
  );
}

function ArchivePanel({
  tabs,
  editions,
  selectedEditionId,
  onSelect
}: {
  tabs?: ReactNode;
  editions: Edition[];
  selectedEditionId: string | null;
  onSelect: (value: string) => void;
}) {
  return (
    <section className="panel content-panel archive-panel">
      {tabs}
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
  tabs,
  edition,
  onOpenSourcePost
}: {
  tabs?: ReactNode;
  edition: Edition | null;
  onOpenSourcePost: (url: string) => void;
}) {
  if (!edition) {
    return (
      <section className="panel content-panel paper-panel paper-panel--empty">
        {tabs}
        <div className="section-header">
          <p className="kicker">Today</p>
          <h2>No issue on the desk yet.</h2>
        </div>
        <p className="empty-copy">
          Open your source sessions, verify LM Studio, and refresh the desk to draft your first edition.
        </p>
      </section>
    );
  }

  return (
    <section className="panel content-panel paper-panel">
      {tabs}
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
                  {card.leadImage ? (
                    <img
                      className="story-card__image"
                      src={getEditionImageSrc(card.leadImage.path)}
                      alt={card.leadImage.alt}
                      loading="lazy"
                    />
                  ) : null}
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
