import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  isPermissionGranted as isNotificationPermissionGranted,
  requestPermission as requestNotificationPermission
} from "@tauri-apps/plugin-notification";
import { GemBadge } from "gem-badges";
import type {
  BrowserSessionState,
  BrowserSource,
  BootstrapState,
  Edition,
  EditionView,
  LmStudioHealth,
  SyncRun,
  SyncProgressEvent,
  UserSettings
} from "./lib/types";
import {
  DEFAULT_MODEL,
  DEFAULT_SETTINGS,
  getMachineTimeZone
} from "./lib/defaults";
import { formatDuration, formatEditionDate, formatTime } from "./lib/format";
import {
  EMPTY_BROWSER_SESSION,
  EMPTY_BOOTSTRAP,
  SYNC_PROGRESS_EVENT,
  getAvailableEditionViews,
  getErrorMessage,
  getSessionToggleLabel,
  getScheduleSummary,
  getSyncProgressMeta,
  pickFreshEdition,
  pickInitialEdition,
  withTimeout
} from "./lib/app-utils";
import {
  deleteAllEditions,
  deleteRun,
  disconnectX,
  getBootstrapState,
  getLinkedInSessionState,
  getRedditSessionState,
  getXSessionState,
  hideLinkedInSessionWindow,
  hideRedditSessionWindow,
  hideXSessionWindow,
  logoutLinkedInSessionWindow,
  logoutRedditSessionWindow,
  logoutXSessionWindow,
  openExternalUrl,
  openLinkedInSessionWindow,
  openRedditSessionWindow,
  openXSessionWindow,
  runSync,
  saveSettings,
  verifyLmStudio
} from "./lib/api";
import { SettingsPanel } from "./components/SettingsPanel";

type Screen = "today" | "archive" | "settings";
const SETTINGS_AUTOSAVE_DELAY_MS = 900;

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

function HeaderBlingLink({ onClick }: { onClick: () => void }) {
  return (
    <a
      className="masthead-bling"
      href="https://github.com/romeroej2/gem-badges"
      target="_blank"
      rel="noreferrer"
      onClick={onClick}
      aria-label="Open the gem-badges repo"
      title="I see you like drip."
    >
      <GemBadge
        config={{
          material: "diamond",
          cut: "round",
          size: 36,
          glow: true,
          glowIntensity: 0.9,
          animate: true,
          renderMode: "auto"
        }}
      />
    </a>
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

  if (source === "reddit") {
    return (
      <span className="session-pill__brand session-pill__brand--reddit" aria-hidden="true">
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <circle cx="12" cy="13" r="5.5" />
          <path d="M9 18c.7.5 1.8.8 3 .8s2.3-.3 3-.8" />
          <circle cx="9.8" cy="13" r=".9" fill="currentColor" stroke="none" />
          <circle cx="14.2" cy="13" r=".9" fill="currentColor" stroke="none" />
          <path d="M10.5 7.2 12.2 9" />
          <circle cx="15.8" cy="6.8" r="1.2" />
          <path d="M7.8 10.2c-.8-.5-1.5-1.2-1.5-2.1 0-1 1-1.8 2.3-1.8.7 0 1.3.2 1.8.5" />
          <path d="M16.2 10.2c.8-.5 1.5-1.2 1.5-2.1 0-1-1-1.8-2.3-1.8-.7 0-1.3.2-1.8.5" />
        </svg>
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
  return view === "consolidated"
    ? "Consolidated"
    : view === "linkedin"
      ? "LinkedIn"
      : view === "reddit"
        ? "Reddit"
        : "X";
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
  onLogout,
  compact = false
}: {
  source: BrowserSource;
  session: BrowserSessionState;
  onOpen: () => void;
  onHide: () => void;
  onLogout: () => void;
  compact?: boolean;
}) {
  const sourceLabel =
    source === "linkedin" ? "LinkedIn" : source === "reddit" ? "Reddit" : "X";
  const connectionLabel = session.isAuthenticated
    ? "Connected"
    : session.isOpen
      ? "Waiting for sign-in"
      : "Session closed";
  const connectionTone = session.isAuthenticated ? "connected" : session.isOpen ? "open" : "closed";

  if (compact) {
    return (
      <div className={`session-chip session-chip--${connectionTone}`}>
        <div className="session-chip__summary">
          <SourceBrandIcon source={source} />
          <div className="session-chip__copy">
            <span className="session-chip__label">{sourceLabel}</span>
            <span className="session-chip__status">{connectionLabel}</span>
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
  const [isArchiveDeleteArmed, setIsArchiveDeleteArmed] = useState(false);
  const [lmStudioDraft, setLmStudioDraft] = useState(DEFAULT_SETTINGS.lmStudio);
  const [sessionStates, setSessionStates] = useState<Record<BrowserSource, BrowserSessionState>>({
    x: EMPTY_BROWSER_SESSION,
    linkedin: EMPTY_BROWSER_SESSION,
    reddit: EMPTY_BROWSER_SESSION
  });
  const [lmHealth, setLmHealth] = useState<LmStudioHealth | null>(null);
  const [isModelDeskExpanded, setIsModelDeskExpanded] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [syncProgress, setSyncProgress] = useState<SyncProgressEvent | null>(null);
  const [clockNow, setClockNow] = useState(() => Date.now());
  const [isSettingsDirty, setIsSettingsDirty] = useState(false);
  const [lastSettingsSavedAt, setLastSettingsSavedAt] = useState<number | null>(null);
  const settingsAutosaveTimeoutRef = useRef<number | null>(null);
  const availableModels = lmHealth?.models ?? [];
  const xSession = sessionStates.x;
  const linkedinSession = sessionStates.linkedin;
  const redditSession = sessionStates.reddit;
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
        : `${latestEditionTitle ? `${latestEditionTitle} · ` : ""}Last run ${formatTime(bootstrap.latestRun.startedAt)} · ${bootstrap.latestRun.status}${bootstrap.latestRun.timings ? ` · ${formatDuration(bootstrap.latestRun.timings.totalMs)}` : ""}`
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
  const selectedRun = useMemo(
    () => (bootstrap.runHistory ?? []).find((run) => run.id === selectedEdition?.runId) ?? null,
    [bootstrap.runHistory, selectedEdition]
  );

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
    setIsSettingsDirty(false);
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
    if (screen !== "archive" || bootstrap.editions.length === 0) {
      setIsArchiveDeleteArmed(false);
    }
  }, [bootstrap.editions.length, screen]);

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
        const [state, xSessionState, linkedinSessionState, redditSessionState] = await Promise.all([
          withTimeout(getBootstrapState(), "Loading the newsroom"),
          withTimeout(getXSessionState(), "Loading the X session"),
          withTimeout(getLinkedInSessionState(), "Loading the LinkedIn session"),
          withTimeout(getRedditSessionState(), "Loading the Reddit session")
        ]);

        if (isCancelled) {
          return;
        }

        applyBootstrapState(state);
        setSessionStates({
          x: xSessionState,
          linkedin: linkedinSessionState,
          reddit: redditSessionState
        });
        void hydrateSavedLmStudio(state.settings);
        setMessage(
          "SIFT is ready. Your enabled browser sessions are standing by."
        );

        void ensureNotificationPermission().catch((error) => {
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
      void Promise.all([getXSessionState(), getLinkedInSessionState(), getRedditSessionState()])
        .then(([xState, linkedinState, redditState]) => {
          if (!isCancelled) {
            setSessionStates({
              x: xState,
              linkedin: linkedinState,
              reddit: redditState
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
    if (!isSettingsDirty || screen !== "settings") {
      return;
    }

    if (
      !bootstrap.settings.capture.sources.x
      && !bootstrap.settings.capture.sources.linkedin
      && !bootstrap.settings.capture.sources.reddit
    ) {
      return;
    }

    if (settingsAutosaveTimeoutRef.current !== null) {
      window.clearTimeout(settingsAutosaveTimeoutRef.current);
    }

    settingsAutosaveTimeoutRef.current = window.setTimeout(() => {
      void persistNewsroomSettings(bootstrap.settings, "Settings autosaved.");
    }, SETTINGS_AUTOSAVE_DELAY_MS);

    return () => {
      if (settingsAutosaveTimeoutRef.current !== null) {
        window.clearTimeout(settingsAutosaveTimeoutRef.current);
        settingsAutosaveTimeoutRef.current = null;
      }
    };
  }, [bootstrap.settings, isSettingsDirty, screen]);

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

  async function ensureNotificationPermission() {
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
      : source === "reddit"
        ? await openRedditSessionWindow()
      : await openXSessionWindow();
    setSessionState(source, session);
    return session;
  }

  async function hideSourceSession(source: BrowserSource) {
    const session = source === "linkedin"
      ? await hideLinkedInSessionWindow()
      : source === "reddit"
        ? await hideRedditSessionWindow()
      : await hideXSessionWindow();
    setSessionState(source, session);
    return session;
  }

  async function logoutSourceSession(source: BrowserSource) {
    const session = source === "linkedin"
      ? await logoutLinkedInSessionWindow()
      : source === "reddit"
        ? await logoutRedditSessionWindow()
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
    await persistModelDeskSettings("Settings saved locally.");
  }

  async function persistModelDeskSettings(successMessage: string) {
    if (settingsAutosaveTimeoutRef.current !== null) {
      window.clearTimeout(settingsAutosaveTimeoutRef.current);
      settingsAutosaveTimeoutRef.current = null;
    }

    if (
      !bootstrap.settings.capture.sources.x
      && !bootstrap.settings.capture.sources.linkedin
      && !bootstrap.settings.capture.sources.reddit
    ) {
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
      setIsSettingsDirty(false);
      setLastSettingsSavedAt(Date.now());
      void hydrateSavedLmStudio({
        ...saved,
        lmStudio: {
          ...saved.lmStudio,
          authToken: lmStudioDraft.authToken
        }
      });
      setIsModelDeskExpanded(false);
      setMessage(successMessage);
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to save settings."));
    }
  }

  async function persistNewsroomSettings(settings: UserSettings, successMessage: string) {
    if (settingsAutosaveTimeoutRef.current !== null) {
      window.clearTimeout(settingsAutosaveTimeoutRef.current);
      settingsAutosaveTimeoutRef.current = null;
    }

    if (
      !settings.capture.sources.x
      && !settings.capture.sources.linkedin
      && !settings.capture.sources.reddit
    ) {
      setMessage("Pick at least one source before saving newsroom settings.");
      return;
    }

    try {
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
      setIsSettingsDirty(false);
      setLastSettingsSavedAt(Date.now());
      setMessage(successMessage);
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to save settings."));
    }
  }

  async function handleRunSync() {
    setIsRefreshing(true);
    setSyncProgress(null);
    console.info("[SIFT sync] Manual refresh requested.");

    try {
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

  async function handleOpenRedditSession() {
    try {
      const session = await openSourceSession("reddit");
      setMessage(
        session.isAuthenticated
          ? "The Reddit session is ready. Keep your browsing inside that SIFT-managed Reddit window."
          : "The Reddit session window is open. Sign in there and keep your browsing inside that SIFT-managed window."
      );
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to open the Reddit session window."));
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

  async function handleHideRedditSession() {
    try {
      await hideSourceSession("reddit");
      setMessage("The Reddit session is hidden. Your sign-in stays alive in the background.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to hide the Reddit session window."));
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

  async function handleLogoutRedditSession() {
    const shouldContinue =
      typeof window.confirm !== "function"
        || window.confirm("Log out of Reddit in SIFT and clear this browser session?");

    if (!shouldContinue) {
      return;
    }

    try {
      await logoutSourceSession("reddit");
      setMessage("Logged out of Reddit in SIFT. Open the window again whenever you want to sign back in.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Unable to log out of the Reddit session."));
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

  async function handleDeleteRun(runId: string) {
    const shouldContinue =
      typeof window.confirm !== "function"
        || window.confirm("Delete this run and all its editions? This cannot be undone.");

    if (!shouldContinue) {
      return;
    }

    try {
      const state = await deleteRun(runId);
      const wasActive = state.editions.every((edition) => edition.runId !== runId);
      if (!wasActive) {
        setSelectedEditionId(null);
      }
      setBootstrap(state);
      setMessage("Run deleted.");
    } catch (error) {
      setMessage(getErrorMessage(error, "Could not delete run."));
    }
  }

  async function handleDeleteAllEditions() {
    if (!isArchiveDeleteArmed) {
      setIsArchiveDeleteArmed(true);
      setMessage("Click Delete all again to remove every archived edition.");
      return;
    }

    try {
      const state = await deleteAllEditions();
      setIsArchiveDeleteArmed(false);
      setSelectedEditionId(null);
      setBootstrap(state);
      setMessage("All archived editions deleted.");
    } catch (error) {
      setIsArchiveDeleteArmed(false);
      setMessage(getErrorMessage(error, "Could not delete all archived editions."));
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

  function handleGemBlingClick() {
    setMessage("I see you like drip.");
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
      <header className="masthead">
        <div className="masthead-identity">
          <div className="masthead-brand">
            <span className="masthead-logo-frame" aria-hidden="true">
              <img className="masthead-logo" src="/sift-logo.png" alt="" />
            </span>
          </div>

          <div className="masthead-utility">
            <section className="session-dots" aria-label="Browser status">
              <BrowserSessionCard
                source="x"
                session={xSession}
                onOpen={handleOpenXSession}
                onHide={handleHideXSession}
                onLogout={handleLogoutXSession}
                compact
              />
              <BrowserSessionCard
                source="linkedin"
                session={linkedinSession}
                onOpen={handleOpenLinkedInSession}
                onHide={handleHideLinkedInSession}
                onLogout={handleLogoutLinkedInSession}
                compact
              />
              <BrowserSessionCard
                source="reddit"
                session={redditSession}
                onOpen={handleOpenRedditSession}
                onHide={handleHideRedditSession}
                onLogout={handleLogoutRedditSession}
                compact
              />
            </section>
            <HeaderBlingLink onClick={handleGemBlingClick} />
          </div>
        </div>

        <div className="masthead-nav-row">
          <nav className="masthead-nav" aria-label="Primary">
            <button
              className={screen === "today" ? "masthead-nav__link masthead-nav__link--active" : "masthead-nav__link"}
              onClick={() => setScreen("today")}
              aria-current={screen === "today" ? "page" : undefined}
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <rect x="3" y="4" width="18" height="18" rx="2" ry="2" />
                <line x1="16" y1="2" x2="16" y2="6" />
                <line x1="8" y1="2" x2="8" y2="6" />
                <line x1="3" y1="10" x2="21" y2="10" />
              </svg>
              <span>Today</span>
            </button>
            <button
              className={screen === "archive" ? "masthead-nav__link masthead-nav__link--active" : "masthead-nav__link"}
              onClick={() => setScreen("archive")}
              aria-current={screen === "archive" ? "page" : undefined}
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
              </svg>
              <span>Archive</span>
            </button>
            <button
              className={screen === "settings" ? "masthead-nav__link masthead-nav__link--active" : "masthead-nav__link"}
              onClick={() => setScreen("settings")}
              aria-current={screen === "settings" ? "page" : undefined}
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <circle cx="12" cy="12" r="3" />
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
              </svg>
              <span>Settings</span>
            </button>
          </nav>

          <div className="masthead-cta">
            {bootstrap.xConnection ? (
              <div className="masthead-legacy-note">
                <span>Legacy X: @{bootstrap.xConnection.handle}</span>
                <button className="secondary-button" onClick={handleDisconnectX}>
                  Clear
                </button>
              </div>
            ) : null}
            <button
              className="primary-button masthead-cta__button"
              onClick={handleRunSync}
              disabled={isRefreshBusy}
              data-busy={isRefreshBusy}
            >
              <span className="masthead-cta__icon" aria-hidden="true">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <polyline points="23 4 23 10 17 10" />
                  <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
                </svg>
              </span>
              {isRefreshBusy ? "Refreshing..." : "Refresh edition"}
            </button>
          </div>
        </div>
      </header>

      <section className={isRefreshBusy ? "status-strip status-strip--busy" : "status-strip"} aria-live="polite">
        <span className="status-strip__primary">{statusMessage}</span>
        <span className="status-strip__meta">{statusMeta}</span>
      </section>

      <div className="layout-grid">
        <div className="desk-column">
          {screen === "settings" ? (
            <SettingsPanel
              settings={bootstrap.settings}
              scheduleSummary={scheduleSummary}
              isModelDeskExpanded={isModelDeskExpanded}
              setIsModelDeskExpanded={setIsModelDeskExpanded}
              lmStudioDraft={lmStudioDraft}
              setLmStudioDraft={setLmStudioDraft}
              lmHealth={lmHealth}
              selectedModelId={selectedModelId}
              availableModels={availableModels}
              onVerifyLmStudio={handleVerifyLmStudio}
              onSaveModelDesk={handleSaveSettings}
              isSettingsDirty={isSettingsDirty}
              lastSettingsSavedAt={lastSettingsSavedAt}
              now={clockNow}
              onChange={(next) => {
                setBootstrap((current) => ({ ...current, settings: next }));
                setIsSettingsDirty(true);
              }}
            />
          ) : screen === "archive" ? (
              <ArchivePanel
                editions={bootstrap.editions}
                runHistory={bootstrap.runHistory}
                selectedEditionId={selectedEditionId}
                onSelect={setSelectedEditionId}
                onOpenSourcePost={handleOpenSourcePost}
                onDeleteRun={handleDeleteRun}
                onDeleteAllEditions={handleDeleteAllEditions}
                isDeleteAllArmed={isArchiveDeleteArmed}
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
              run={selectedRun}
              onOpenSourcePost={handleOpenSourcePost}
            />
          )}
        </div>
      </div>
    </main>
  );
}


function ArchivePanel({
  editions,
  runHistory,
  selectedEditionId,
  onSelect,
  onOpenSourcePost,
  onDeleteRun,
  onDeleteAllEditions,
  isDeleteAllArmed
}: {
  editions: Edition[];
  runHistory: SyncRun[];
  selectedEditionId: string | null;
  onSelect: (value: string) => void;
  onOpenSourcePost: (url: string) => void;
  onDeleteRun?: (runId: string) => void;
  onDeleteAllEditions?: () => void;
  isDeleteAllArmed?: boolean;
}) {
  const runById = new Map(runHistory.map((run) => [run.id, run]));
  const sortedEditions = editions.slice().sort((a, b) => new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime());
  const selectedEdition = sortedEditions.find((e) => e.id === selectedEditionId) ?? null;
  const selectedRun = selectedEdition ? (runById.get(selectedEdition.runId) ?? null) : null;

  const allViews: EditionView[] = ["consolidated", "x", "linkedin", "reddit"];
  const siblingEditions = selectedEdition
    ? sortedEditions.filter((e) => e.runId === selectedEdition.runId)
    : [];
  const siblingViews = allViews.filter((view) =>
    siblingEditions.some((e) => e.view === view)
  );

  const runGroups = useMemo(() => {
    const groups = new Map<string, Edition[]>();
    for (const edition of sortedEditions) {
      const list = groups.get(edition.runId) ?? [];
      list.push(edition);
      groups.set(edition.runId, list);
    }
    return Array.from(groups.entries())
      .map(([runId, runEditions]) => {
        const run = runById.get(runId);
        const primary = runEditions.find((e) => e.view === "consolidated") ?? runEditions[0];
        const pages = allViews
          .map((view) => runEditions.find((e) => e.view === view))
          .filter(Boolean) as Edition[];
        return { runId, run, primary, pages };
      })
      .sort((a, b) => new Date(b.primary.createdAt).getTime() - new Date(a.primary.createdAt).getTime());
  }, [sortedEditions, runById]);

  return (
    <div className="archive-layout">
      <aside className="archive-browser">
        <div className="section-header section-header--archive">
          <div>
            <p className="kicker">Archive</p>
            <h2>{sortedEditions.length} editions</h2>
          </div>
          {sortedEditions.length > 0 && onDeleteAllEditions ? (
            <button
              className={isDeleteAllArmed ? "archive-header__clear archive-header__clear--armed" : "archive-header__clear"}
              type="button"
              onClick={onDeleteAllEditions}
              aria-label={isDeleteAllArmed ? "Confirm delete all editions" : "Delete all editions"}
            >
              {isDeleteAllArmed ? "Confirm delete all" : "Delete all"}
            </button>
          ) : null}
        </div>

        <div className="archive-list">
          {runGroups.length ? (
            runGroups.map(({ runId, run, primary, pages }) => (
              <section key={runId} className="archive-run-group">
                <header className="archive-run-header">
                  <span className="archive-run-header__time">{formatTime(primary.createdAt)}</span>
                  <span className="archive-run-header__label">
                    {run?.scheduleRuleLabel ?? "Manual"}
                  </span>
                  <span className="archive-run-header__right">
                    {run ? (
                      <span className="archive-run-header__duration">
                        {formatDuration(run.timings.totalMs)}
                      </span>
                    ) : null}
                    {onDeleteRun ? (
                      <button
                        className="archive-run-header__delete"
                        type="button"
                        onClick={() => onDeleteRun(runId)}
                        aria-label="Delete this run"
                        title="Delete this run"
                      >
                        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                          <polyline points="3 6 5 6 21 6" />
                          <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                        </svg>
                      </button>
                    ) : null}
                  </span>
                </header>
                <div className="archive-run-pages" role="list">
                  {pages.map((edition) => {
                    const isActive = edition.id === selectedEditionId;
                    return (
                      <button
                        key={edition.id}
                        className={isActive ? "archive-page archive-page--active" : "archive-page"}
                        onClick={() => onSelect(edition.id)}
                        role="listitem"
                        aria-current={isActive ? "true" : undefined}
                      >
                        <span
                          className="archive-page__dot"
                          aria-hidden="true"
                          data-view={edition.view}
                        />
                        <span className="archive-page__label">
                          {getEditionViewLabel(edition.view)}
                        </span>
                        <span className="archive-page__title">{edition.title}</span>
                      </button>
                    );
                  })}
                </div>
              </section>
            ))
          ) : (
            <p className="empty-copy">Once your first issue is generated, it will land here.</p>
          )}
        </div>
      </aside>

      <div className="archive-reader">
        {selectedEdition ? (
          <EditionPanel
            tabs={
              siblingViews.length > 1 ? (
                <EditionViewTabs
                  ariaLabel="Edition pages"
                  availableViews={siblingViews}
                  selectedView={selectedEdition.view}
                  onSelect={(view) => {
                    const target = siblingEditions.find((e) => e.view === view);
                    if (target && target.id !== selectedEditionId) {
                      onSelect(target.id);
                    }
                  }}
                />
              ) : undefined
            }
            edition={selectedEdition}
            run={selectedRun}
            onOpenSourcePost={onOpenSourcePost}
          />
        ) : (
          <section className="panel content-panel paper-panel paper-panel--empty">
            <div className="section-header">
              <p className="kicker">Archive</p>
              <h2>Select an edition</h2>
            </div>
            <p className="empty-copy">
              Choose an edition from the list to read it here.
            </p>
          </section>
        )}
      </div>
    </div>
  );
}

function EditionPanel({
  tabs,
  edition,
  run,
  onOpenSourcePost
}: {
  tabs?: ReactNode;
  edition: Edition | null;
  run: SyncRun | null;
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
        {run ? (
          <p className="paper-date">
            {run.scheduleRuleLabel ? `${run.scheduleRuleLabel} · ` : ""}
            Total {formatDuration(run.timings.totalMs)} · Read {formatDuration(run.timings.captureMs)} · Summaries {formatDuration(run.timings.rankingMs)} · Front page {formatDuration(run.timings.frontPageMs)}
          </p>
        ) : null}
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
                    <div className="story-card__image-wrap">
                      <img
                        className="story-card__image"
                        src={getEditionImageSrc(card.leadImage.path)}
                        alt={card.leadImage.alt}
                        loading="lazy"
                      />
                    </div>
                  ) : null}
                  <div className="story-card__content">
                    <span className="story-card__meta">
                      {card.authorName} · @{card.authorHandle}
                    </span>
                    <h4>{card.headline}</h4>
                    <p>{card.summary}</p>
                    <span className="story-card__why">{card.whyItMatters}</span>
                  </div>
                </button>
              ))}
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
