mod db;
mod models;
mod services;

use std::collections::HashMap;
use std::panic;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use db::Database;
use models::{
    BootstrapState, BrowserSessionState, OAuthSession, PersistedBrowserSession, PollStatus,
    UserSettings, XClientConfigDraft, XConnectLaunch, XConnectPayload, XConnectPollResult,
};
use services::{
    FeedSource, LmStudioClient, LocalModelProvider, XClient, XSessionCapturePayload,
    XSessionCaptureProgressPayload, XSessionCaptureRequest, emit_capture_progress, generate_paper,
    maybe_run_scheduled_sync, run_scheduler,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::utils::config::BackgroundThrottlingPolicy;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use url::Url;
use uuid::Uuid;

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error("{message}")]
    NoFreshItems { message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub app: tauri::AppHandle,
    pub oauth_sessions: Arc<RwLock<HashMap<String, OAuthSession>>>,
    pub lm_studio_auth_token: Arc<RwLock<Option<String>>>,
    pub x_session_authenticated: Arc<RwLock<bool>>,
    pub x_session_last_known_url: Arc<RwLock<Option<String>>>,
    pub x_session_force_close: Arc<AtomicBool>,
    pub linkedin_session_authenticated: Arc<RwLock<bool>>,
    pub linkedin_session_last_known_url: Arc<RwLock<Option<String>>>,
    pub linkedin_session_force_close: Arc<AtomicBool>,
    pub reddit_session_authenticated: Arc<RwLock<bool>>,
    pub reddit_session_last_known_url: Arc<RwLock<Option<String>>>,
    pub reddit_session_force_close: Arc<AtomicBool>,
    pub x_session_capture_requests: Arc<Mutex<HashMap<String, XSessionCaptureRequest>>>,
    pub sync_guard: Arc<Mutex<()>>,
    pub quit_requested: Arc<AtomicBool>,
}

impl AppState {
    fn new(app: tauri::AppHandle, db: Database) -> Self {
        Self {
            db,
            app,
            oauth_sessions: Arc::new(RwLock::new(HashMap::new())),
            lm_studio_auth_token: Arc::new(RwLock::new(None)),
            x_session_authenticated: Arc::new(RwLock::new(false)),
            x_session_last_known_url: Arc::new(RwLock::new(None)),
            x_session_force_close: Arc::new(AtomicBool::new(false)),
            linkedin_session_authenticated: Arc::new(RwLock::new(false)),
            linkedin_session_last_known_url: Arc::new(RwLock::new(None)),
            linkedin_session_force_close: Arc::new(AtomicBool::new(false)),
            reddit_session_authenticated: Arc::new(RwLock::new(false)),
            reddit_session_last_known_url: Arc::new(RwLock::new(None)),
            reddit_session_force_close: Arc::new(AtomicBool::new(false)),
            x_session_capture_requests: Arc::new(Mutex::new(HashMap::new())),
            sync_guard: Arc::new(Mutex::new(())),
            quit_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    fn set_oauth_error(&self, state: &str, error_message: String) {
        let sessions = self.oauth_sessions.clone();
        let state = state.to_string();

        tauri::async_runtime::spawn(async move {
            if let Some(session) = sessions.write().await.get_mut(&state) {
                session.result = Some(XConnectPollResult {
                    status: PollStatus::Error,
                    error_message: Some(error_message),
                    payload: None,
                });
            }
        });
    }

    fn set_oauth_success(&self, state: &str, payload: XConnectPayload) {
        let sessions = self.oauth_sessions.clone();
        let state = state.to_string();

        tauri::async_runtime::spawn(async move {
            if let Some(session) = sessions.write().await.get_mut(&state) {
                session.result = Some(XConnectPollResult {
                    status: PollStatus::Success,
                    error_message: None,
                    payload: Some(payload),
                });
            }
        });
    }

    async fn get_oauth_session(&self, state: &str) -> Option<OAuthSession> {
        self.oauth_sessions.read().await.get(state).cloned()
    }

    async fn set_lm_studio_auth_token(&self, auth_token: Option<String>) {
        *self.lm_studio_auth_token.write().await = auth_token.and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });
    }

    async fn lm_studio_auth_token(&self) -> Option<String> {
        self.lm_studio_auth_token.read().await.clone()
    }

    async fn browser_session_state(
        &self,
        window_label: &str,
        authenticated: &RwLock<bool>,
        last_known_url: &RwLock<Option<String>>,
    ) -> BrowserSessionState {
        let window = self.app.get_webview_window(window_label);
        let is_open = window.is_some();
        let is_visible = window
            .as_ref()
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        let is_authenticated = if is_open {
            *authenticated.read().await
        } else {
            false
        };
        let last_known_url = if is_open {
            last_known_url.read().await.clone()
        } else {
            None
        };

        BrowserSessionState {
            is_open,
            is_visible,
            is_authenticated,
            last_known_url,
            mode: "native-webview".into(),
        }
    }

    async fn x_session_state(&self) -> BrowserSessionState {
        self.browser_session_state(
            X_SESSION_WINDOW_LABEL,
            &self.x_session_authenticated,
            &self.x_session_last_known_url,
        )
        .await
    }

    async fn linkedin_session_state(&self) -> BrowserSessionState {
        self.browser_session_state(
            LINKEDIN_SESSION_WINDOW_LABEL,
            &self.linkedin_session_authenticated,
            &self.linkedin_session_last_known_url,
        )
        .await
    }

    async fn reddit_session_state(&self) -> BrowserSessionState {
        self.browser_session_state(
            REDDIT_SESSION_WINDOW_LABEL,
            &self.reddit_session_authenticated,
            &self.reddit_session_last_known_url,
        )
        .await
    }

    pub(crate) async fn ensure_x_session_visible_for_refresh(&self) -> Result<bool, AppError> {
        if let Some(window) = self.app.get_webview_window(X_SESSION_WINDOW_LABEL) {
            let was_visible = window.is_visible().unwrap_or(false);
            if !was_visible {
                window
                    .show()
                    .map_err(|error| AppError::Message(error.to_string()))?;
                let _ = window.set_focus();
            }
            return Ok(!was_visible);
        }

        let saved_session = self.db.load_persisted_x_session()?;
        let initial_url = resolve_x_session_launch_url(
            saved_session
                .as_ref()
                .map(|session| session.last_known_url.as_str()),
        );
        let is_authenticated = saved_session
            .as_ref()
            .is_some_and(|session| session.is_authenticated);

        self.remember_x_session(initial_url.to_string(), is_authenticated)
            .await?;

        let window = build_x_session_window(&self.app, self.clone(), initial_url, true, true)
            .map_err(AppError::Message)?;
        window
            .show()
            .map_err(|error| AppError::Message(error.to_string()))?;
        let _ = window.set_focus();

        Ok(true)
    }

    pub(crate) fn hide_x_session_after_refresh(&self) -> Result<(), AppError> {
        hide_x_session_windows(&self.app).map_err(AppError::Message)
    }

    pub(crate) async fn ensure_linkedin_session_visible_for_refresh(
        &self,
    ) -> Result<bool, AppError> {
        if let Some(window) = self.app.get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL) {
            let was_visible = window.is_visible().unwrap_or(false);
            if !was_visible {
                window
                    .show()
                    .map_err(|error| AppError::Message(error.to_string()))?;
                let _ = window.set_focus();
            }
            return Ok(!was_visible);
        }

        let saved_session = self.db.load_persisted_linkedin_session()?;
        let initial_url = resolve_linkedin_session_launch_url(
            saved_session
                .as_ref()
                .map(|session| session.last_known_url.as_str()),
        );
        let is_authenticated = saved_session
            .as_ref()
            .is_some_and(|session| session.is_authenticated);

        self.remember_linkedin_session(initial_url.to_string(), is_authenticated)
            .await?;

        let window =
            build_linkedin_session_window(&self.app, self.clone(), initial_url, true, true)
                .map_err(AppError::Message)?;
        window
            .show()
            .map_err(|error| AppError::Message(error.to_string()))?;
        let _ = window.set_focus();

        Ok(true)
    }

    pub(crate) fn hide_linkedin_session_after_refresh(&self) -> Result<(), AppError> {
        hide_linkedin_session_windows(&self.app).map_err(AppError::Message)
    }

    pub(crate) async fn ensure_reddit_session_visible_for_refresh(&self) -> Result<bool, AppError> {
        if let Some(window) = self.app.get_webview_window(REDDIT_SESSION_WINDOW_LABEL) {
            let was_visible = window.is_visible().unwrap_or(false);
            if !was_visible {
                window
                    .show()
                    .map_err(|error| AppError::Message(error.to_string()))?;
                let _ = window.set_focus();
            }
            return Ok(!was_visible);
        }

        let saved_session = self.db.load_persisted_reddit_session()?;
        let initial_url = resolve_reddit_session_launch_url(
            saved_session
                .as_ref()
                .map(|session| session.last_known_url.as_str()),
        );
        let is_authenticated = saved_session
            .as_ref()
            .is_some_and(|session| session.is_authenticated);

        self.remember_reddit_session(initial_url.to_string(), is_authenticated)
            .await?;

        let window = build_reddit_session_window(&self.app, self.clone(), initial_url, true, true)
            .map_err(AppError::Message)?;
        window
            .show()
            .map_err(|error| AppError::Message(error.to_string()))?;
        let _ = window.set_focus();

        Ok(true)
    }

    pub(crate) fn hide_reddit_session_after_refresh(&self) -> Result<(), AppError> {
        hide_reddit_session_windows(&self.app).map_err(AppError::Message)
    }

    async fn remember_x_session(
        &self,
        last_known_url: String,
        is_authenticated: bool,
    ) -> Result<(), AppError> {
        *self.x_session_last_known_url.write().await = Some(last_known_url.clone());
        *self.x_session_authenticated.write().await = is_authenticated;
        self.db.save_persisted_x_session(&PersistedBrowserSession {
            last_known_url,
            is_authenticated,
        })
    }

    async fn clear_x_session_runtime(&self) {
        *self.x_session_last_known_url.write().await = None;
        *self.x_session_authenticated.write().await = false;
    }

    async fn forget_x_session(&self) -> Result<(), AppError> {
        self.clear_x_session_runtime().await;
        self.db.clear_persisted_x_session()
    }

    async fn remember_linkedin_session(
        &self,
        last_known_url: String,
        is_authenticated: bool,
    ) -> Result<(), AppError> {
        *self.linkedin_session_last_known_url.write().await = Some(last_known_url.clone());
        *self.linkedin_session_authenticated.write().await = is_authenticated;
        self.db
            .save_persisted_linkedin_session(&PersistedBrowserSession {
                last_known_url,
                is_authenticated,
            })
    }

    async fn clear_linkedin_session_runtime(&self) {
        *self.linkedin_session_last_known_url.write().await = None;
        *self.linkedin_session_authenticated.write().await = false;
    }

    async fn forget_linkedin_session(&self) -> Result<(), AppError> {
        self.clear_linkedin_session_runtime().await;
        self.db.clear_persisted_linkedin_session()
    }

    async fn remember_reddit_session(
        &self,
        last_known_url: String,
        is_authenticated: bool,
    ) -> Result<(), AppError> {
        *self.reddit_session_last_known_url.write().await = Some(last_known_url.clone());
        *self.reddit_session_authenticated.write().await = is_authenticated;
        self.db
            .save_persisted_reddit_session(&PersistedBrowserSession {
                last_known_url,
                is_authenticated,
            })
    }

    async fn clear_reddit_session_runtime(&self) {
        *self.reddit_session_last_known_url.write().await = None;
        *self.reddit_session_authenticated.write().await = false;
    }

    async fn forget_reddit_session(&self) -> Result<(), AppError> {
        self.clear_reddit_session_runtime().await;
        self.db.clear_persisted_reddit_session()
    }
}

const X_SESSION_WINDOW_LABEL: &str = "x-session";
const X_SESSION_POPUP_LABEL_PREFIX: &str = "x-session-popup";
const X_AUTH_POPUP_LABEL_PREFIX: &str = "x-auth-popup";
const X_AUTH_POPUP_CLOSE_DELAY_MS: u64 = 750;
const X_SESSION_HOME_URL: &str = "https://x.com/home";
const X_SESSION_DATA_STORE_ID: [u8; 16] = *b"SIFTXSESSION0001";
const LINKEDIN_SESSION_WINDOW_LABEL: &str = "linkedin-session";
const LINKEDIN_SESSION_POPUP_LABEL_PREFIX: &str = "linkedin-session-popup";
const LINKEDIN_SESSION_HOME_URL: &str = "https://www.linkedin.com/feed/";
const LINKEDIN_SESSION_DATA_STORE_ID: [u8; 16] = *b"SIFTLINKEDIN0001";
const REDDIT_SESSION_WINDOW_LABEL: &str = "reddit-session";
const REDDIT_SESSION_POPUP_LABEL_PREFIX: &str = "reddit-session-popup";
const REDDIT_SESSION_HOME_URL: &str = "https://www.reddit.com/";
const REDDIT_SESSION_DATA_STORE_ID: [u8; 16] = *b"SIFTREDDIT000001";
const X_SESSION_BRIDGE_SCRIPT: &str = r#"
(() => {
const siftIsTopFrame = (() => {
  try {
    return window.top === window.self;
  } catch {
    return true;
  }
})();

if (
  siftIsTopFrame
  && ["x.com", "www.x.com", "twitter.com", "www.twitter.com"].includes(window.location.hostname)
) {
  const siftReadText = (node) =>
    (node?.innerText || "")
      .replace(/\s+/g, " ")
      .trim();

  const siftWait = (ms) =>
    new Promise((resolve) => window.setTimeout(resolve, ms));

  const siftControlIcons = {
    hide:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 3l18 18"></path><path d="M10.58 10.58A3 3 0 0 0 9 12a3 3 0 0 0 5.33 1.82"></path><path d="M9.88 5.09A10.94 10.94 0 0 1 12 4.91c5 0 9.27 3.11 11 7.5a11.8 11.8 0 0 1-3.29 4.68"></path><path d="M6.61 6.61A11.81 11.81 0 0 0 1 12.41a11.84 11.84 0 0 0 4.26 5.1"></path></svg>',
    logout:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"></path><path d="M16 17l5-5-5-5"></path><path d="M21 12H9"></path></svg>',
  };

  const siftIconDataUrl = (icon, color) =>
    `data:image/svg+xml;utf8,${encodeURIComponent(icon.replace(/currentColor/g, color))}`;

  const siftDateKey = (value, timeZone) => {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return null;
    }

    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(date);
    const year = parts.find((part) => part.type === "year")?.value;
    const month = parts.find((part) => part.type === "month")?.value;
    const day = parts.find((part) => part.type === "day")?.value;
    if (!year || !month || !day) {
      return null;
    }

    return `${year}-${month}-${day}`;
  };

  const siftFindStatusUrl = (article) => {
    const links = Array.from(article.querySelectorAll('a[href*="/status/"]'));
    for (const link of links) {
      const href = link.getAttribute("href") || "";
      if (href.includes("/status/")) {
        return new URL(href, window.location.origin).toString();
      }
    }
    return null;
  };

  const siftSharedUrls = (article) => {
    const urls = new Set();
    Array.from(article.querySelectorAll("a[href]")).forEach((link) => {
      const href = link.getAttribute("href") || "";
      try {
        const url = new URL(href, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          return;
        }
        if (
          ["x.com", "www.x.com", "twitter.com", "www.twitter.com"].includes(url.hostname)
        ) {
          if (url.pathname.includes("/status/")) {
            return;
          }
          return;
        }
        url.hash = "";
        urls.add(url.toString());
      } catch {
        // Ignore malformed DOM hrefs while scraping.
      }
    });
    return Array.from(urls);
  };

  const siftTweetMedia = (article) => {
    const media = [];
    const seen = new Set();
    const selectors = [
      'a[href*="/photo/"] img',
      '[data-testid="tweetPhoto"] img',
      'img[src*="pbs.twimg.com/media/"]',
      'img[srcset*="pbs.twimg.com/media/"]',
    ];

    Array.from(article.querySelectorAll(selectors.join(","))).forEach((img) => {
      const src =
        img.currentSrc
        || img.getAttribute("src")
        || (img.getAttribute("srcset") || "").split(",").map((part) => part.trim().split(/\s+/)[0]).find(Boolean)
        || "";
      if (!src) {
        return;
      }

      try {
        const url = new URL(src, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          return;
        }
        if (url.hostname !== "pbs.twimg.com") {
          return;
        }
        if (
          /profile_images|profile_banners|emoji|ext_tw_video_thumb|amplify_video_thumb|semantic_core_img/i.test(
            url.pathname,
          )
        ) {
          return;
        }

        const photoHref = img.closest('a[href*="/photo/"]')?.getAttribute("href") || "";
        if (
          !photoHref.includes("/photo/")
          && !img.closest('[data-testid="tweetPhoto"]')
          && !url.pathname.includes("/media/")
        ) {
          return;
        }

        url.hash = "";
        const normalized = url.toString();
        if (seen.has(normalized)) {
          return;
        }
        seen.add(normalized);
        media.push({
          url: normalized,
          kind: "photo",
        });
      } catch {
        // Ignore malformed media URLs while scraping.
      }
    });

    return media;
  };

  const siftEnsureSessionControls = () => {
    if (!window.__TAURI_INTERNALS__?.invoke) {
      return;
    }

    const styleRoot = document.head || document.documentElement;
    if (!styleRoot) {
      return;
    }

    let style = document.getElementById("sift-x-session-controls-style");
    if (!style) {
      style = document.createElement("style");
      style.id = "sift-x-session-controls-style";
      style.textContent = `
        #sift-x-session-controls {
          position: fixed;
          right: max(16px, env(safe-area-inset-right));
          bottom: max(16px, env(safe-area-inset-bottom));
          display: inline-flex;
          align-items: center;
          gap: 8px;
          padding: 10px 12px;
          border-radius: 999px;
          background: rgba(19, 19, 24, 0.88);
          border: 1px solid rgba(255, 255, 255, 0.12);
          box-shadow: 0 16px 40px rgba(0, 0, 0, 0.28);
          backdrop-filter: blur(18px);
          z-index: 2147483647;
          color: #f5f7fa;
          font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
          pointer-events: auto;
          user-select: none;
        }

        .sift-x-session-controls__badge {
          font-size: 11px;
          letter-spacing: 0.18em;
          text-transform: uppercase;
          color: rgba(245, 247, 250, 0.72);
          padding-inline: 4px 2px;
          user-select: none;
        }

        .sift-x-session-controls__button {
          appearance: none;
          -webkit-appearance: none;
          width: 36px;
          height: 36px;
          min-width: 36px;
          min-height: 36px;
          padding: 0;
          border: 0;
          border-radius: 999px;
          display: inline-flex;
          flex: 0 0 auto;
          align-items: center;
          justify-content: center;
          background: rgba(255, 255, 255, 0.08);
          color: inherit;
          cursor: pointer;
          line-height: 0;
          font-size: 0;
          vertical-align: middle;
          transition: background 140ms ease, transform 140ms ease, opacity 140ms ease;
          pointer-events: auto;
          touch-action: manipulation;
        }

        .sift-x-session-controls__button:hover {
          background: rgba(255, 255, 255, 0.16);
          transform: translateY(-1px);
        }

        .sift-x-session-controls__button:disabled {
          opacity: 0.45;
          cursor: wait;
          transform: none;
        }

        .sift-x-session-controls__button--danger {
          background: rgba(205, 76, 76, 0.18);
          color: #ffd7d7;
        }

        .sift-x-session-controls__button--danger:hover {
          background: rgba(205, 76, 76, 0.3);
        }

        .sift-x-session-controls__button svg {
          width: 18px !important;
          height: 18px !important;
          min-width: 18px !important;
          min-height: 18px !important;
          display: block !important;
          flex: 0 0 auto;
          overflow: visible;
          fill: none !important;
          stroke: currentColor !important;
          stroke-width: 1.9 !important;
          stroke-linecap: round !important;
          stroke-linejoin: round !important;
          opacity: 1 !important;
          visibility: visible !important;
          pointer-events: none;
        }

        .sift-x-session-controls__button svg * {
          fill: none !important;
          stroke: currentColor !important;
          vector-effect: non-scaling-stroke;
          opacity: 1 !important;
          visibility: visible !important;
        }
      `;
    }
    styleRoot.appendChild(style);

    const attach = () => {
      const root = document.body || document.documentElement;
      if (!root) {
        window.setTimeout(attach, 120);
        return;
      }

      const existingDock = document.getElementById("sift-x-session-controls");
      if (existingDock) {
        if (existingDock.parentElement !== root) {
          root.appendChild(existingDock);
        }
        return;
      }

      const dock = document.createElement("div");
      dock.id = "sift-x-session-controls";

      const badge = document.createElement("span");
      badge.className = "sift-x-session-controls__badge";
      badge.textContent = "SIFT X";
      dock.appendChild(badge);

      const buildButton = (command, label, icon, tone = "neutral", requiresConfirm = false) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "sift-x-session-controls__button";
        if (tone === "danger") {
          button.classList.add("sift-x-session-controls__button--danger");
        }
        button.setAttribute("aria-label", label);
        button.setAttribute("title", label);
        button.innerHTML = icon;
        const iconColor = tone === "danger" ? '#ffd7d7' : '#f5f7fa';
        button.style.setProperty("color", iconColor, "important");
        button.style.setProperty("-webkit-text-fill-color", iconColor, "important");

        const iconNode = button.querySelector("svg");
        if (iconNode) {
          iconNode.style.setProperty("width", "18px", "important");
          iconNode.style.setProperty("height", "18px", "important");
          iconNode.style.setProperty("min-width", "18px", "important");
          iconNode.style.setProperty("min-height", "18px", "important");
          iconNode.style.setProperty("display", "block", "important");
          iconNode.style.setProperty("fill", "none", "important");
          iconNode.style.setProperty("stroke", "currentColor", "important");
          iconNode.style.setProperty("stroke-width", "1.9", "important");
          iconNode.style.setProperty("stroke-linecap", "round", "important");
          iconNode.style.setProperty("stroke-linejoin", "round", "important");
          iconNode.style.setProperty("opacity", "1", "important");
          iconNode.style.setProperty("visibility", "visible", "important");
          iconNode.querySelectorAll("*").forEach((child) => {
            child.style.setProperty("fill", "none", "important");
            child.style.setProperty("stroke", "currentColor", "important");
            child.style.setProperty("opacity", "1", "important");
            child.style.setProperty("visibility", "visible", "important");
          });
        }

        const stopEvent = (event) => {
          event.preventDefault();
          event.stopPropagation();
          if (typeof event.stopImmediatePropagation === "function") {
            event.stopImmediatePropagation();
          }
        };

        const runCommand = async () => {
          if (
            requiresConfirm
            && !window.confirm("Log out of X in SIFT and clear this browser session?")
          ) {
            return;
          }

          button.disabled = true;

          try {
            await window.__TAURI_INTERNALS__.invoke(command, {});
          } catch (error) {
            console.error("[SIFT] X session control failed.", error);
            button.disabled = false;
          }
        };

        button.addEventListener(
          "pointerdown",
          (event) => {
            if ("button" in event && event.button !== 0) {
              return;
            }
            stopEvent(event);
            void runCommand();
          },
          true,
        );
        button.addEventListener("click", stopEvent, true);
        button.addEventListener("mousedown", stopEvent, true);
        return button;
      };

      dock.appendChild(
        buildButton(
          "hide_x_session_window",
          "Hide this X window",
          siftControlIcons.hide,
        ),
      );
      dock.appendChild(
        buildButton(
          "logout_x_session_window",
          "Log out of X in SIFT",
          siftControlIcons.logout,
          "danger",
          true,
        ),
      );

      root.appendChild(dock);
    };

    attach();
  };

  const siftParseTweet = (article) => {
    const sourceUrl = siftFindStatusUrl(article);
    if (!sourceUrl) {
      return null;
    }

    const idMatch = sourceUrl.match(/status\/(\d+)/);
    if (!idMatch) {
      return null;
    }

    const text = siftReadText(article.querySelector('div[data-testid="tweetText"]'));
    if (!text) {
      return null;
    }

    const userNameNode = article.querySelector('div[data-testid="User-Name"]');
    const nameParts = Array.from(userNameNode?.querySelectorAll("span") || [])
      .map((node) => siftReadText(node))
      .filter(Boolean);
    const handle =
      (nameParts.find((value) => value.startsWith("@")) || "")
        .replace(/^@/, "")
        || sourceUrl.split("/")[3]
        || "unknown";
    const authorName =
      nameParts.find(
        (value) =>
          value
          && !value.startsWith("@")
          && value !== "·"
          && !/^[0-9]+[smhdwy]$/.test(value)
          && value !== "Pinned",
      )
      || handle;
    const socialContext = siftReadText(article.querySelector('[data-testid="socialContext"]'));
    const articleText = siftReadText(article);
    const isPromoted = /\b(promoted|sponsored|advertisement|patrocinad[oa]s?|publicidad|anuncio)\b/i.test(
      [socialContext, articleText].filter(Boolean).join(" "),
    );
    const isReply = Array.from(article.querySelectorAll("span"))
      .map((node) => siftReadText(node))
      .some((value) => value.startsWith("Replying to"));

    return {
      id: idMatch[1],
      authorName,
      authorHandle: handle,
      text,
      sourceUrl,
      postedAt: article.querySelector("time")?.getAttribute("datetime") || new Date().toISOString(),
      isRepost: /retweeted|reposted/i.test(socialContext),
      isReply,
      isPromoted,
      socialContext: socialContext || null,
      sharedUrls: siftSharedUrls(article),
      media: siftTweetMedia(article),
    };
  };

  const siftFeedSelector = 'article[data-testid="tweet"]';

  const siftCollectVisibleTweets = (selector, collected) => {
    document.querySelectorAll(selector).forEach((article) => {
      const parsed = siftParseTweet(article);
      if (parsed) {
        collected.set(parsed.id, parsed);
      }
    });
  };

  const siftReadScrollMetrics = (scroller) => {
    const root = document.scrollingElement || document.documentElement;
    if (!scroller || scroller === root || scroller === document.documentElement || scroller === document.body) {
      return {
        top: window.scrollY || root.scrollTop || 0,
        height: root.scrollHeight || document.documentElement.scrollHeight || 0,
        clientHeight: window.innerHeight || root.clientHeight || document.documentElement.clientHeight || 0,
      };
    }

    return {
      top: scroller.scrollTop,
      height: scroller.scrollHeight,
      clientHeight: scroller.clientHeight || window.innerHeight || 0,
    };
  };

  const siftScrollFeedTo = (scroller, top) => {
    const root = document.scrollingElement || document.documentElement;
    if (!scroller || scroller === root || scroller === document.documentElement || scroller === document.body) {
      window.scrollTo(0, top);
      return;
    }

    if (typeof scroller.scrollTo === "function") {
      scroller.scrollTo({ top, behavior: "auto" });
      return;
    }

    scroller.scrollTop = top;
  };

  const siftMaxScrollTop = (scroller) => {
    const metrics = siftReadScrollMetrics(scroller);
    return Math.max(metrics.height - metrics.clientHeight, 0);
  };

  const siftClampScrollTop = (scroller, top) =>
    Math.max(0, Math.min(Number(top) || 0, siftMaxScrollTop(scroller)));

  const siftAnimateScrollTo = async (scroller, top, durationMs = 280) => {
    const startTop = siftReadScrollMetrics(scroller).top;
    const targetTop = siftClampScrollTop(scroller, top);
    const distance = targetTop - startTop;
    if (Math.abs(distance) < 4) {
      siftScrollFeedTo(scroller, targetTop);
      return targetTop;
    }

    const startedAt = Date.now();
    const totalDuration = Math.max(durationMs, 120);
    while (true) {
      const elapsed = Date.now() - startedAt;
      const progress = Math.min(elapsed / totalDuration, 1);
      const eased = 1 - Math.pow(1 - progress, 3);
      siftScrollFeedTo(scroller, startTop + (distance * eased));
      if (progress >= 1) {
        break;
      }
      await siftWait(16);
    }

    siftScrollFeedTo(scroller, targetTop);
    return targetTop;
  };

  const siftAdvanceFeed = async (selector, scroller, distance, options = {}) => {
    const segments = Math.max(Number(options.segments) || 2, 1);
    const durationMs = Math.max(Number(options.durationMs) || 280, 120);
    const settleMs = Math.max(Number(options.settleMs) || 140, 0);
    const visibleIdsBefore = siftVisibleTweetIds(selector);
    const metricsBefore = siftReadScrollMetrics(scroller);
    const perSegment = distance / segments;
    let lastTop = metricsBefore.top;

    for (let segment = 0; segment < segments; segment += 1) {
      lastTop = await siftAnimateScrollTo(
        scroller,
        lastTop + perSegment,
        durationMs,
      );
      if (settleMs > 0 && segment < segments - 1) {
        await siftWait(settleMs);
      }
    }

    return {
      previousHeight: metricsBefore.height,
      previousIds: visibleIdsBefore,
      top: lastTop,
    };
  };

  const siftVisibleTweetIds = (selector) => {
    const ids = new Set();
    document.querySelectorAll(selector).forEach((article) => {
      const parsed = siftParseTweet(article);
      if (parsed) {
        ids.add(parsed.id);
      }
    });
    return ids;
  };

  const siftFindFeedScroller = (selector) => {
    const root = document.scrollingElement || document.documentElement;
    const firstTweet = document.querySelector(selector);
    const candidates = [];
    const seen = new Set();

    let node = firstTweet;
    while (node instanceof Element) {
      candidates.push(node);
      node = node.parentElement;
    }

    [
      firstTweet instanceof Element ? firstTweet.closest('[data-testid="primaryColumn"]') : null,
      firstTweet instanceof Element ? firstTweet.closest('main[role="main"]') : null,
      firstTweet instanceof Element ? firstTweet.closest("main") : null,
      document.querySelector('[data-testid="primaryColumn"]'),
      document.querySelector('main[role="main"]'),
      document.querySelector("main"),
      root,
      document.documentElement,
    ].forEach((candidate) => {
      if (candidate) {
        candidates.push(candidate);
      }
    });

    for (const candidate of candidates) {
      if (!candidate || seen.has(candidate)) {
        continue;
      }
      seen.add(candidate);

      const metrics = siftReadScrollMetrics(candidate);
      const isRootCandidate =
        candidate === root || candidate === document.documentElement || candidate === document.body;
      const overflowY =
        candidate instanceof Element
          ? window.getComputedStyle(candidate).overflowY || ""
          : "";
      if ((isRootCandidate || /(auto|scroll|overlay)/.test(overflowY)) && metrics.height > metrics.clientHeight + 240) {
        return candidate;
      }
    }

    return root;
  };

  const siftWaitForFeedAdvance = async (
    selector,
    scroller,
    previousIds,
    previousHeight,
    timeoutMs,
    onHeartbeat,
  ) => {
    const startedAt = Date.now();
    let lastHeartbeatAt = startedAt;

    while (Date.now() - startedAt < timeoutMs) {
      await siftWait(250);
      if (typeof onHeartbeat === "function" && Date.now() - lastHeartbeatAt >= 1500) {
        await onHeartbeat();
        lastHeartbeatAt = Date.now();
      }
      const metrics = siftReadScrollMetrics(scroller);
      if (metrics.height > previousHeight + 48) {
        return true;
      }

      const currentIds = siftVisibleTweetIds(selector);
      for (const id of currentIds) {
        if (!previousIds.has(id)) {
          return true;
        }
      }
    }

    return false;
  };

  siftEnsureSessionControls();

  if (!window.__SIFT_X_SESSION_CONTROLS_WATCHDOG__) {
    window.__SIFT_X_SESSION_CONTROLS_WATCHDOG__ = window.setInterval(() => {
      try {
        siftEnsureSessionControls();
      } catch {
        // Keep the session controls best-effort during X feed navigations.
      }
    }, 1500);
  }

  if (!window.__SIFT_X_SESSION_CONTROLS_EVENTS_BOUND__) {
    window.__SIFT_X_SESSION_CONTROLS_EVENTS_BOUND__ = true;

    window.addEventListener("pageshow", () => {
      try {
        siftEnsureSessionControls();
      } catch {
        // Keep the session controls best-effort after X page restores.
      }
    });

    window.addEventListener("focus", () => {
      try {
        siftEnsureSessionControls();
      } catch {
        // Keep the session controls best-effort after focus changes.
      }
    });
  }

  if (window.__SIFT_COLLECT_FEED__) {
    return;
  }

  window.__SIFT_COLLECT_FEED__ = async (requestId, options = {}) => {
    try {
      siftEnsureSessionControls();
      if (!/^\/home(?:$|[/?#])/.test(window.location.pathname)) {
        throw new Error("X session is not on the home timeline yet.");
      }

      const timeZone =
        options.timeZone
        || Intl.DateTimeFormat().resolvedOptions().timeZone
        || "UTC";
      const editionDate = options.editionDate || siftDateKey(new Date().toISOString(), timeZone);
      const sinceTimestamp = options.sinceTimestamp ? new Date(options.sinceTimestamp) : null;
      const hasSinceTimestamp =
        sinceTimestamp instanceof Date && !Number.isNaN(sinceTimestamp.getTime());
      const maxItems = Math.min(Math.max(Number(options.maxItems) || 400, 200), 800);
      const targetFreshItems = Math.min(
        Math.max(Number(options.targetFreshItems) || 200, 80),
        maxItems,
      );
      const maxPasses = Math.max(Number(options.maxPasses) || 12, 1);
      const stableLimit = Math.max(Number(options.stablePasses) || 10, 4);
      const exhaustedLimit = Math.max(Number(options.exhaustedPasses) || 18, stableLimit + 4);
      const waitForAdvanceMs = Math.max(Number(options.waitForAdvanceMs) || 5000, 1500);
      const selector = siftFeedSelector;
      const deadline = Date.now() + 20000;
      while (Date.now() < deadline && document.querySelectorAll(selector).length === 0) {
        await siftWait(250);
      }

      if (document.querySelectorAll(selector).length === 0) {
        throw new Error("Timed out waiting for the X home timeline to render.");
      }

      const collected = new Map();
      let stablePasses = 0;
      let exhaustedPasses = 0;
      let boundaryPasses = 0;
      let lastFreshCount = 0;
      let scroller = siftFindFeedScroller(selector);
      siftScrollFeedTo(scroller, 0);
      await siftWait(750);

      const siftIsFresh = (item) => {
        if (hasSinceTimestamp) {
          const posted = new Date(item.postedAt);
          if (Number.isNaN(posted.getTime())) {
            return true;
          }
          return posted > sinceTimestamp;
        }
        if (!editionDate) {
          return true;
        }
        return siftDateKey(item.postedAt, timeZone) === editionDate;
      };

      const siftReachedBoundary = (items) => {
        if (hasSinceTimestamp) {
          return items.some((item) => {
            const posted = new Date(item.postedAt);
            return !Number.isNaN(posted.getTime()) && posted <= sinceTimestamp;
          });
        }
        if (!editionDate) {
          return false;
        }
        return items.some((item) => siftDateKey(item.postedAt, timeZone) !== editionDate);
      };

      const siftCountFreshItems = () =>
        Array.from(collected.values()).filter((item) => siftIsFresh(item)).length;

      const siftReportProgress = async (pass, itemCount, freshCount) => {
        try {
          await window.__TAURI_INTERNALS__.invoke("submit_x_feed_capture_progress", {
            progress: {
              requestId,
              currentUrl: window.location.href,
              pass,
              totalPasses: maxPasses,
              itemCount,
              freshCount,
              stablePasses,
              exhaustedPasses,
              boundaryPasses,
            },
          });
        } catch (_error) {
          // If the native side times out or finishes first, the page can quietly stop reporting.
        }
      };

      for (let pass = 0; pass < maxPasses; pass += 1) {
        siftCollectVisibleTweets(selector, collected);

        const collectedItems = Array.from(collected.values());
        const freshCount = collectedItems.filter((item) => siftIsFresh(item)).length;
        const reachedBoundary = siftReachedBoundary(collectedItems);
        if (reachedBoundary && freshCount <= lastFreshCount) {
          boundaryPasses += 1;
        } else {
          boundaryPasses = 0;
        }
        lastFreshCount = freshCount;
        await siftReportProgress(pass + 1, collectedItems.length, freshCount);

        if (freshCount >= maxItems) {
          break;
        }
        if (boundaryPasses >= 3 && freshCount >= targetFreshItems) {
          break;
        }
        if (stablePasses >= stableLimit && freshCount >= targetFreshItems) {
          break;
        }
        if (exhaustedPasses >= exhaustedLimit) {
          break;
        }

        const metricsBefore = siftReadScrollMetrics(scroller);
        const baseScrollStep = Math.max(metricsBefore.clientHeight * 0.55, 520);
        const fallbackScrollStep = Math.max(metricsBefore.clientHeight * 0.8, 860);

        const siftHeartbeat = async () => {
          await siftReportProgress(pass + 1, collected.size, siftCountFreshItems());
        };

        const firstAdvance = await siftAdvanceFeed(selector, scroller, baseScrollStep, {
          segments: 2,
          durationMs: 260,
          settleMs: 120,
        });
        let advanced = await siftWaitForFeedAdvance(
          selector,
          scroller,
          firstAdvance.previousIds,
          firstAdvance.previousHeight,
          waitForAdvanceMs,
          siftHeartbeat,
        );

        if (!advanced) {
          const secondAdvance = await siftAdvanceFeed(selector, scroller, fallbackScrollStep, {
            segments: 3,
            durationMs: 320,
            settleMs: 140,
          });
          advanced = await siftWaitForFeedAdvance(
            selector,
            scroller,
            secondAdvance.previousIds,
            secondAdvance.previousHeight,
            waitForAdvanceMs,
            siftHeartbeat,
          );
        }

        scroller = siftFindFeedScroller(selector);
        const before = collected.size;
        siftCollectVisibleTweets(selector, collected);
        const grew = collected.size > before;

        if (advanced || grew) {
          stablePasses = 0;
          exhaustedPasses = 0;
        } else {
          stablePasses += 1;
          exhaustedPasses += 1;
          await siftWait(750);
        }
      }

      siftScrollFeedTo(scroller, 0);

      await window.__TAURI_INTERNALS__.invoke("submit_x_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: Array.from(collected.values()),
          error: null,
        },
      });
    } catch (error) {
      await window.__TAURI_INTERNALS__.invoke("submit_x_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: [],
          error: error instanceof Error ? error.message : String(error),
        },
      });
    }
  };
}
})();
"#;

const LINKEDIN_SESSION_BRIDGE_SCRIPT: &str = r#"
(() => {
const siftIsTopFrame = (() => {
  try {
    return window.top === window.self;
  } catch {
    return true;
  }
})();

if (
  siftIsTopFrame
  && ["linkedin.com", "www.linkedin.com"].includes(window.location.hostname)
) {
  const siftReadText = (node) =>
    (node?.innerText || "")
      .replace(/\s+/g, " ")
      .trim();

  const siftWait = (ms) =>
    new Promise((resolve) => window.setTimeout(resolve, ms));

  const siftDateKey = (value, timeZone) => {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return null;
    }

    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(date);
    const year = parts.find((part) => part.type === "year")?.value;
    const month = parts.find((part) => part.type === "month")?.value;
    const day = parts.find((part) => part.type === "day")?.value;
    return year && month && day ? `${year}-${month}-${day}` : null;
  };

  const siftControlIcons = {
    hide:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 3l18 18"></path><path d="M10.58 10.58A3 3 0 0 0 9 12a3 3 0 0 0 5.33 1.82"></path><path d="M9.88 5.09A10.94 10.94 0 0 1 12 4.91c5 0 9.27 3.11 11 7.5a11.8 11.8 0 0 1-3.29 4.68"></path><path d="M6.61 6.61A11.81 11.81 0 0 0 1 12.41a11.84 11.84 0 0 0 4.26 5.1"></path></svg>',
    logout:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"></path><path d="M16 17l5-5-5-5"></path><path d="M21 12H9"></path></svg>',
  };

  const siftEnsureSessionControls = () => {
    if (!window.__TAURI_INTERNALS__?.invoke) {
      return;
    }

    const styleRoot = document.head || document.documentElement;
    if (!styleRoot) {
      return;
    }

    let style = document.getElementById("sift-linkedin-session-controls-style");
    if (!style) {
      style = document.createElement("style");
      style.id = "sift-linkedin-session-controls-style";
      style.textContent = `
        #sift-linkedin-session-controls {
          position: fixed;
          right: max(16px, env(safe-area-inset-right));
          bottom: max(16px, env(safe-area-inset-bottom));
          display: inline-flex;
          align-items: center;
          gap: 8px;
          padding: 10px 12px;
          border-radius: 999px;
          background: rgba(14, 25, 44, 0.88);
          border: 1px solid rgba(255, 255, 255, 0.12);
          box-shadow: 0 16px 40px rgba(0, 0, 0, 0.28);
          backdrop-filter: blur(18px);
          z-index: 2147483647;
          color: #f5f7fa;
          font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        }
        .sift-linkedin-session-controls__badge {
          font-size: 11px;
          letter-spacing: 0.18em;
          text-transform: uppercase;
          color: rgba(245, 247, 250, 0.72);
          padding-inline: 4px 2px;
        }
        .sift-linkedin-session-controls__button {
          appearance: none;
          -webkit-appearance: none;
          width: auto;
          height: 36px;
          min-width: 0;
          min-height: 36px;
          padding: 0 12px;
          border: 0;
          border-radius: 999px;
          display: inline-flex;
          flex: 0 0 auto;
          align-items: center;
          justify-content: center;
          background: rgba(255, 255, 255, 0.08);
          color: inherit;
          cursor: pointer;
          line-height: 1;
          font-size: 12px;
          font-weight: 600;
          letter-spacing: 0.01em;
          white-space: nowrap;
          vertical-align: middle;
          transition: background 140ms ease, transform 140ms ease, opacity 140ms ease;
          pointer-events: auto;
          touch-action: manipulation;
        }
        .sift-linkedin-session-controls__button:hover {
          background: rgba(255, 255, 255, 0.16);
          transform: translateY(-1px);
        }
        .sift-linkedin-session-controls__button:disabled {
          opacity: 0.45;
          cursor: wait;
          transform: none;
        }
        .sift-linkedin-session-controls__button--danger {
          background: rgba(205, 76, 76, 0.18);
          color: #ffd7d7;
        }
        .sift-linkedin-session-controls__button--danger:hover {
          background: rgba(205, 76, 76, 0.3);
        }
      `;
    }
    styleRoot.appendChild(style);

    const attach = () => {
      const root = document.body || document.documentElement;
      if (!root) {
        return;
      }

      const existingDock = document.getElementById("sift-linkedin-session-controls");
      if (existingDock) {
        if (existingDock.parentElement !== root) {
          root.appendChild(existingDock);
        }
        return;
      }

      const dock = document.createElement("div");
      dock.id = "sift-linkedin-session-controls";

      const badge = document.createElement("span");
      badge.className = "sift-linkedin-session-controls__badge";
      badge.textContent = "SIFT LinkedIn";
      dock.appendChild(badge);

      const buildButton = (command, label, text, requiresConfirm = false) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "sift-linkedin-session-controls__button";
        if (requiresConfirm) {
          button.classList.add("sift-linkedin-session-controls__button--danger");
        }
        button.setAttribute("aria-label", label);
        button.setAttribute("title", label);
        // LinkedIn keeps suppressing the toolbar SVGs, so this session intentionally uses text buttons for now.
        const iconColor = requiresConfirm ? '#ffd7d7' : '#f5f7fa';
        button.textContent = text;
        button.style.setProperty("color", iconColor, "important");
        button.style.setProperty("-webkit-text-fill-color", iconColor, "important");

        const stopEvent = (event) => {
          event.preventDefault();
          event.stopPropagation();
          if (typeof event.stopImmediatePropagation === "function") {
            event.stopImmediatePropagation();
          }
        };

        const runCommand = async () => {
          if (requiresConfirm && !window.confirm("Log out of LinkedIn in SIFT and clear this browser session?")) {
            return;
          }

          button.disabled = true;

          try {
            await window.__TAURI_INTERNALS__.invoke(command, {});
          } catch (error) {
            console.error("[SIFT] LinkedIn session control failed.", error);
            button.disabled = false;
          }
        };

        button.addEventListener(
          "pointerdown",
          (event) => {
            if ("button" in event && event.button !== 0) {
              return;
            }
            stopEvent(event);
            void runCommand();
          },
          true,
        );
        button.addEventListener("click", stopEvent, true);
        button.addEventListener("mousedown", stopEvent, true);
        return button;
      };

      dock.appendChild(buildButton("hide_linkedin_session_window", "Hide this LinkedIn window", "Hide"));
      dock.appendChild(buildButton("logout_linkedin_session_window", "Log out of LinkedIn in SIFT", "Log out", true));
      root.appendChild(dock);
    };

    if (document.body || document.documentElement) {
      attach();
    } else {
      window.setTimeout(attach, 200);
    }
  };

  if (!window.__SIFT_LINKEDIN_SESSION_CONTROLS_WATCHDOG__) {
    window.__SIFT_LINKEDIN_SESSION_CONTROLS_WATCHDOG__ = window.setInterval(() => {
      try {
        siftEnsureSessionControls();
      } catch {
        // Keep the session controls best-effort during LinkedIn feed navigations.
      }
    }, 1500);
  }

  if (!window.__SIFT_LINKEDIN_SESSION_CONTROLS_EVENTS_BOUND__) {
    window.__SIFT_LINKEDIN_SESSION_CONTROLS_EVENTS_BOUND__ = true;

    window.addEventListener("pageshow", () => {
      try {
        siftEnsureSessionControls();
      } catch {
        // Keep the session controls best-effort after LinkedIn page restores.
      }
    });
  }

  if (window.__SIFT_COLLECT_LINKEDIN_FEED__) {
    return;
  }

  const siftFeedSelector = "main article";
  const siftFallbackFeedSelectors = [
    "main article",
    'main [data-id^="urn:li:activity:"]',
    'main [data-urn^="urn:li:activity:"]',
    "main .occludable-update",
    "main .feed-shared-update-v2",
    "main [data-finite-scroll-hotkey-item]",
    'main a[href*="/feed/update/"]',
    'main a[href*="/posts/"]',
    'main a[href*="/activity-"]',
  ];
  const siftFeedSurfaceSelectors = [
    "main .scaffold-finite-scroll",
    "main [data-finite-scroll-hotkey-context]",
    "main .share-box-feed-entry__trigger",
    'main button[aria-label*="Start a post"]',
    "main .feed-sort-dropdown",
  ];
  const siftFeedSurfaceTextSignals = [
    "Start a post",
    "Sort by: Top",
    "Sort by Top",
    "New posts",
  ];

  const siftFeedElements = (selector = siftFeedSelector) => {
    const directMatches = Array.from(document.querySelectorAll(selector)).filter(
      (node) => node instanceof Element,
    );
    if (directMatches.length > 0) {
      return directMatches;
    }

    const fallbackMatches = [];
    const seen = new Set();
    const siftRememberCandidate = (node) => {
      if (!(node instanceof Element)) {
        return;
      }

      const container =
        siftFindPostContainer(node)
        || node.closest(
          'article, .occludable-update, .feed-shared-update-v2, [data-id^="urn:li:activity:"], [data-urn^="urn:li:activity:"], [data-finite-scroll-hotkey-item]',
        )
        || node;
      if (!(container instanceof Element) || seen.has(container)) {
        return;
      }

      seen.add(container);
      fallbackMatches.push(container);
    };

    for (const fallbackSelector of siftFallbackFeedSelectors) {
      document.querySelectorAll(fallbackSelector).forEach((node) => {
        siftRememberCandidate(node);
      });
    }

    if (fallbackMatches.length === 0) {
      Array.from(document.querySelectorAll("main div, main section, main li")).slice(0, 800).forEach((node) => {
        const text = siftReadText(node);
        if (!text || text.length < 40) {
          return;
        }
        if (!/Like/i.test(text) || !/Comment/i.test(text)) {
          return;
        }
        siftRememberCandidate(node);
      });
    }

    return fallbackMatches;
  };

  const siftFeedSurfaceReady = (selector = siftFeedSelector) =>
    siftFeedElements(selector).length > 0
    || siftFeedSurfaceSelectors.some((surfaceSelector) => document.querySelector(surfaceSelector))
    || siftFeedSurfaceTextSignals.some((signal) => siftReadText(document.body).includes(signal));

  const siftSharedUrls = (article) => {
    const urls = new Set();
    Array.from(article.querySelectorAll("a[href]")).forEach((link) => {
      const href = link.getAttribute("href") || "";
      try {
        const url = new URL(href, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          return;
        }
        if (["linkedin.com", "www.linkedin.com"].includes(url.hostname)) {
          return;
        }
        url.hash = "";
        urls.add(url.toString());
      } catch {
        // Ignore malformed DOM hrefs while scraping.
      }
    });
    return Array.from(urls);
  };

  const siftLinkedInMedia = (article) => {
    const media = [];
    const seen = new Set();
    Array.from(article.querySelectorAll("img")).forEach((img) => {
      const src =
        img.currentSrc
        || img.getAttribute("src")
        || (img.getAttribute("srcset") || "").split(",").map((part) => part.trim().split(/\s+/)[0]).find(Boolean)
        || "";
      if (!src) {
        return;
      }

      try {
        const url = new URL(src, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          return;
        }
        if (!/(^|\.)licdn\.com$/i.test(url.hostname)) {
          return;
        }
        if (/profile|company-logo|emoji|icon|sprite|badge/i.test(url.pathname)) {
          return;
        }

        const width = Number(img.naturalWidth || img.width || img.getAttribute("width") || 0);
        const height = Number(img.naturalHeight || img.height || img.getAttribute("height") || 0);
        if (width > 0 && height > 0 && (width < 120 || height < 80)) {
          return;
        }

        url.hash = "";
        const normalized = url.toString();
        if (seen.has(normalized)) {
          return;
        }
        seen.add(normalized);
        media.push({
          url: normalized,
          kind: "photo",
        });
      } catch {
        // Ignore malformed media URLs while scraping.
      }
    });
    return media;
  };

  const siftLinkedInActivityId = (value) => {
    if (!value) {
      return null;
    }

    return (
      value.match(/urn:li:activity:(\d+)/)?.[1]
      || value.match(/activity-(\d+)/)?.[1]
      || value.match(/posts\/[^/]+-(\d+)/)?.[1]
      || null
    );
  };

  const siftPostUrl = (article) => {
    const selectors = [
      'a[href*="/feed/update/"]',
      'a[href*="/posts/"]',
      'a[href*="/activity-"]',
    ];
    for (const selector of selectors) {
      const link = article.querySelector(selector);
      const href = link?.getAttribute("href") || "";
      if (href) {
        try {
          return new URL(href, window.location.origin).toString();
        } catch {
          // Ignore malformed post URLs while scraping.
        }
      }
    }

    const hrefs = Array.from(article.querySelectorAll("a[href]"))
      .map((link) => link.getAttribute("href") || "")
      .filter(Boolean);
    for (const href of hrefs) {
      try {
        const url = new URL(href, window.location.origin);
        const decoded = decodeURIComponent(url.toString());
        if (siftLinkedInActivityId(decoded)) {
          url.hash = "";
          return url.toString();
        }
      } catch {
        // Ignore malformed post URLs while scraping.
      }
    }

    const activityId =
      siftLinkedInActivityId(article.getAttribute("data-id") || "")
      || siftLinkedInActivityId(article.getAttribute("data-urn") || "")
      || siftLinkedInActivityId(article.querySelector("[data-id]")?.getAttribute("data-id") || "")
      || siftLinkedInActivityId(article.querySelector("[data-urn]")?.getAttribute("data-urn") || "");
    if (activityId) {
      return `https://www.linkedin.com/feed/update/urn:li:activity:${activityId}/`;
    }

    return null;
  };

  const siftLooksLikeLinkedInUiNoise = (value, authorName) => {
    if (!value) {
      return true;
    }

    const normalized = value.replace(/\s+/g, " ").trim();
    if (!normalized) {
      return true;
    }

    if (authorName && normalized === authorName) {
      return true;
    }

    return (
      /^(Like|Comment|Repost|Send|Follow|Message|Messaging|Reactivate|Home|My Network|Jobs|Notifications|Me|For Business|Premium|Search|Show translation|Start a post|Sort by:? Top|Video|Photo|Write article)$/i.test(normalized)
      || /^\d+\s+(comments?|reposts?|likes?)\b/i.test(normalized)
      || /\bcomments?\b\s*[•·]\s*\d+\s+reposts?\b/i.test(normalized)
      || /^view all recommendations/i.test(normalized)
      || /^about accessibility help center/i.test(normalized)
      || /^privacy\s*&\s*terms/i.test(normalized)
      || /^get the linkedin app/i.test(normalized)
    );
  };

  const siftFallbackPostText = (article, authorName, minimumLength = 40) => {
    const candidates = [];
    Array.from(article.querySelectorAll("span, p, div")).forEach((node) => {
      const text = siftReadText(node);
      if (!text || text.length < minimumLength || siftLooksLikeLinkedInUiNoise(text, authorName)) {
        return;
      }
      candidates.push(text);
    });

    const articleText = siftReadText(article);
    if (articleText && articleText.length >= minimumLength && !siftLooksLikeLinkedInUiNoise(articleText, authorName)) {
      candidates.push(articleText);
    }

    candidates.sort((left, right) => right.length - left.length);
    return candidates[0] || "";
  };

  const siftLooksLikePostContainer = (element) => {
    if (!(element instanceof Element) || element.matches("main")) {
      return false;
    }

    const text = siftReadText(element);
    if (!text || text.length < 40 || text.length > 6000) {
      return false;
    }

    const hasActionText = /Like/i.test(text) && /Comment/i.test(text);
    const hasPostLink = !!element.querySelector('a[href*="/feed/update/"], a[href*="/posts/"], a[href*="/activity-"]');
    const hasIdentity =
      !!element.querySelector(".update-components-actor__name, .feed-shared-actor__name, a[href*=\"/in/\"], a[href*=\"/company/\"]")
      || !!element.querySelector("time");

    return (hasActionText || hasPostLink) && (hasIdentity || hasPostLink);
  };

  const siftFindPostContainer = (node) => {
    let current = node instanceof Element ? node : null;
    let best = null;

    while (current instanceof Element && !current.matches("main")) {
      if (siftLooksLikePostContainer(current)) {
        best = current;
      }
      current = current.parentElement;
    }

    return best;
  };

  const siftLinkedInPostCandidates = (article) => {
    const candidates = [];
    let node = article;

    while (node instanceof Element && candidates.length < 8) {
      candidates.push(node);
      if (node.matches("main")) {
        break;
      }
      node = node.parentElement;
    }

    return candidates;
  };

  const siftParseLinkedInPost = (article) => {
    let target = article;
    let sourceUrl = null;
    let authorName = "LinkedIn author";
    let text = "";
    const fallbackKey = siftFeedElementKey(article);

    for (const candidate of siftLinkedInPostCandidates(article)) {
      const candidateAuthorName =
        siftReadText(
          candidate.querySelector(".update-components-actor__name")
          || candidate.querySelector(".feed-shared-actor__name")
        )
        || authorName;
      const textNode =
        candidate.querySelector('[data-test-id="main-feed-activity-card__commentary"]')
        || candidate.querySelector(".update-components-text")
        || candidate.querySelector(".feed-shared-update-v2__description")
        || candidate.querySelector(".feed-shared-inline-show-more-text")
        || candidate.querySelector(".update-components-update-v2__commentary");
      const candidateText = siftReadText(textNode) || siftFallbackPostText(candidate, candidateAuthorName);
      const candidateSourceUrl = siftPostUrl(candidate);

      if (!authorName || authorName === "LinkedIn author") {
        authorName = candidateAuthorName;
      }
      if (!text && candidateText) {
        text = candidateText;
      }
      if (!sourceUrl && candidateSourceUrl) {
        sourceUrl = candidateSourceUrl;
        target = candidate;
      }

      if (candidateSourceUrl && candidateText) {
        sourceUrl = candidateSourceUrl;
        authorName = candidateAuthorName;
        text = candidateText;
        target = candidate;
        break;
      }
    }

    if (!text) {
      text = siftFallbackPostText(target, authorName, 12);
    }
    if (!sourceUrl && fallbackKey) {
      const fallbackActivityId = siftLinkedInActivityId(fallbackKey);
      sourceUrl = fallbackActivityId
        ? `https://www.linkedin.com/feed/update/urn:li:activity:${fallbackActivityId}/`
        : `${window.location.origin}/feed/#${encodeURIComponent(fallbackKey.slice(0, 120))}`;
    }
    if (!sourceUrl || !text) {
      return null;
    }

    const idMatch = siftLinkedInActivityId(sourceUrl);
    const handleSource =
      target.querySelector('a[href*="/in/"]')?.getAttribute("href")
      || target.querySelector('a[href*="/company/"]')?.getAttribute("href")
      || "";
    const handleMatch = handleSource.match(/\/(?:in|company)\/([^/?#]+)/);
    const socialContext =
      siftReadText(
        target.querySelector(".update-components-header__text-view")
        || target.querySelector(".feed-shared-social-action-bar__text-view")
        || target.querySelector(".feed-shared-social-action-bar")
      )
      || null;
    const targetText = siftReadText(target);
    const isPromoted = /\b(promoted|sponsored|advertisement|patrocinad[oa]s?|publicidad|anuncio)\b/i.test(
      [socialContext, targetText].filter(Boolean).join(" "),
    );
    const postedAt =
      target.querySelector("time")?.getAttribute("datetime")
      || new Date().toISOString();
    return {
      id: idMatch || sourceUrl,
      authorName,
      authorHandle: handleMatch?.[1] || authorName.toLowerCase().replace(/[^a-z0-9]+/g, "-"),
      text,
      sourceUrl,
      postedAt,
      isRepost: /\breposted this\b/i.test(socialContext || ""),
      isReply: false,
      isPromoted,
      socialContext,
      sharedUrls: siftSharedUrls(target),
      media: siftLinkedInMedia(target),
    };
  };

  const siftCollectVisiblePosts = (selector, collected) => {
    siftFeedElements(selector).forEach((article) => {
      const parsed = siftParseLinkedInPost(article);
      if (parsed) {
        collected.set(parsed.id, parsed);
      }
    });
  };

  const siftFeedElementKey = (article) => {
    if (!(article instanceof Element)) {
      return null;
    }

    const attributeKey =
      article.getAttribute("data-id")
      || article.getAttribute("data-urn")
      || article.querySelector("[data-id]")?.getAttribute("data-id")
      || article.querySelector("[data-urn]")?.getAttribute("data-urn");
    if (attributeKey) {
      return attributeKey;
    }

    const sourceUrl = siftPostUrl(article);
    if (sourceUrl) {
      return sourceUrl;
    }

    const publishedAt = article.querySelector("time")?.getAttribute("datetime");
    if (publishedAt) {
      return `time:${publishedAt}`;
    }

    const preview = siftReadText(article).slice(0, 160);
    return preview ? `text:${preview}` : null;
  };

  const siftVisiblePostIds = (selector) => {
    const ids = new Set();
    siftFeedElements(selector).forEach((article) => {
      const key = siftFeedElementKey(article);
      if (key) {
        ids.add(key);
      }
    });
    return ids;
  };

  const siftReadScrollMetrics = (scroller) => {
    const root = document.scrollingElement || document.documentElement;
    if (!scroller || scroller === root || scroller === document.documentElement || scroller === document.body) {
      return {
        top: window.scrollY || root.scrollTop || 0,
        height: root.scrollHeight || document.documentElement.scrollHeight || 0,
        clientHeight: window.innerHeight || root.clientHeight || document.documentElement.clientHeight || 0,
      };
    }

    return {
      top: scroller.scrollTop,
      height: scroller.scrollHeight,
      clientHeight: scroller.clientHeight || window.innerHeight || 0,
    };
  };

  const siftScrollFeedTo = (scroller, top) => {
    const root = document.scrollingElement || document.documentElement;
    if (!scroller || scroller === root || scroller === document.documentElement || scroller === document.body) {
      window.scrollTo(0, top);
      return;
    }

    if (typeof scroller.scrollTo === "function") {
      scroller.scrollTo({ top, behavior: "auto" });
      return;
    }

    scroller.scrollTop = top;
  };

  const siftMaxScrollTop = (scroller) => {
    const metrics = siftReadScrollMetrics(scroller);
    return Math.max(metrics.height - metrics.clientHeight, 0);
  };

  const siftClampScrollTop = (scroller, top) =>
    Math.max(0, Math.min(Number(top) || 0, siftMaxScrollTop(scroller)));

  const siftAnimateScrollTo = async (scroller, top, durationMs = 280) => {
    const startTop = siftReadScrollMetrics(scroller).top;
    const targetTop = siftClampScrollTop(scroller, top);
    const distance = targetTop - startTop;
    if (Math.abs(distance) < 4) {
      siftScrollFeedTo(scroller, targetTop);
      return targetTop;
    }

    const startedAt = Date.now();
    const totalDuration = Math.max(durationMs, 120);
    while (true) {
      const elapsed = Date.now() - startedAt;
      const progress = Math.min(elapsed / totalDuration, 1);
      const eased = 1 - Math.pow(1 - progress, 3);
      siftScrollFeedTo(scroller, startTop + (distance * eased));
      if (progress >= 1) {
        break;
      }
      await siftWait(16);
    }

    siftScrollFeedTo(scroller, targetTop);
    return targetTop;
  };

  const siftAdvanceFeed = async (selector, scroller, distance, options = {}) => {
    const segments = Math.max(Number(options.segments) || 2, 1);
    const durationMs = Math.max(Number(options.durationMs) || 280, 120);
    const settleMs = Math.max(Number(options.settleMs) || 140, 0);
    const visibleIdsBefore = siftVisiblePostIds(selector);
    const metricsBefore = siftReadScrollMetrics(scroller);
    const perSegment = distance / segments;
    let lastTop = metricsBefore.top;

    for (let segment = 0; segment < segments; segment += 1) {
      lastTop = await siftAnimateScrollTo(
        scroller,
        lastTop + perSegment,
        durationMs,
      );
      if (settleMs > 0 && segment < segments - 1) {
        await siftWait(settleMs);
      }
    }

    return {
      previousHeight: metricsBefore.height,
      previousIds: visibleIdsBefore,
      top: lastTop,
    };
  };

  const siftFindFeedScroller = (selector) => {
    const root = document.scrollingElement || document.documentElement;
    const firstPost = siftFeedElements(selector)[0] || null;
    const candidates = [];
    const seen = new Set();

    let node = firstPost;
    while (node instanceof Element) {
      candidates.push(node);
      node = node.parentElement;
    }

    [
      firstPost instanceof Element ? firstPost.closest(".scaffold-finite-scroll") : null,
      firstPost instanceof Element ? firstPost.closest('[data-finite-scroll-hotkey-context]') : null,
      firstPost instanceof Element ? firstPost.closest(".scaffold-layout__main") : null,
      firstPost instanceof Element ? firstPost.closest("main") : null,
      document.querySelector(".scaffold-finite-scroll"),
      document.querySelector('[data-finite-scroll-hotkey-context]'),
      document.querySelector(".scaffold-layout__main"),
      document.querySelector("main"),
      root,
      document.documentElement,
    ].forEach((candidate) => {
      if (candidate) {
        candidates.push(candidate);
      }
    });

    for (const candidate of candidates) {
      if (!candidate || seen.has(candidate)) {
        continue;
      }
      seen.add(candidate);

      const metrics = siftReadScrollMetrics(candidate);
      const isRootCandidate =
        candidate === root || candidate === document.documentElement || candidate === document.body;
      const overflowY =
        candidate instanceof Element
          ? window.getComputedStyle(candidate).overflowY || ""
          : "";
      if ((isRootCandidate || /(auto|scroll|overlay)/.test(overflowY)) && metrics.height > metrics.clientHeight + 240) {
        return candidate;
      }
    }

    return root;
  };

  const siftWaitForFeedAdvance = async (selector, scroller, previousIds, previousHeight, timeoutMs) => {
    const startedAt = Date.now();
    while (Date.now() - startedAt < timeoutMs) {
      await siftWait(250);
      const metrics = siftReadScrollMetrics(scroller);
      if (metrics.height > previousHeight + 48) {
        return true;
      }

      const currentIds = siftVisiblePostIds(selector);
      for (const id of currentIds) {
        if (!previousIds.has(id)) {
          return true;
        }
      }
    }
    return false;
  };

  siftEnsureSessionControls();

  window.__SIFT_COLLECT_LINKEDIN_FEED__ = async (requestId, options = {}) => {
    try {
      siftEnsureSessionControls();
      if (!/^\/feed(?:$|[/?#])/.test(window.location.pathname)) {
        throw new Error("LinkedIn session is not on the home feed yet.");
      }

      const timeZone = options.timeZone || Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
      const editionDate = options.editionDate || siftDateKey(new Date().toISOString(), timeZone);
      const sinceTimestamp = options.sinceTimestamp ? new Date(options.sinceTimestamp) : null;
      const hasSinceTimestamp = sinceTimestamp instanceof Date && !Number.isNaN(sinceTimestamp.getTime());
      const maxItems = Math.min(Math.max(Number(options.maxItems) || 400, 100), 800);
      const maxPasses = Math.max(Number(options.maxPasses) || 8, 1);
      const waitForAdvanceMs = Math.max(Number(options.waitForAdvanceMs) || 4000, 1200);
      const stallLimit = Math.max(Number(options.stablePasses) || 3, 2);
      const deadline = Date.now() + 20000;
      while (Date.now() < deadline && !siftFeedSurfaceReady(siftFeedSelector)) {
        await siftWait(250);
      }

      if (!siftFeedSurfaceReady(siftFeedSelector)) {
        throw new Error("Timed out waiting for the LinkedIn home feed to render.");
      }

      const collected = new Map();
      let scroller = siftFindFeedScroller(siftFeedSelector);
      let stalledPasses = 0;
      let completedPasses = 0;
      let endedEarly = false;
      siftScrollFeedTo(scroller, 0);
      await siftWait(750);
      for (let attempt = 0; attempt < 3 && siftFeedElements(siftFeedSelector).length === 0; attempt += 1) {
        await siftAdvanceFeed(
          siftFeedSelector,
          scroller,
          Math.max(siftReadScrollMetrics(scroller).clientHeight * (0.42 + (attempt * 0.12)), 320 + (attempt * 120)),
          {
            segments: 1,
            durationMs: 220,
            settleMs: 0,
          },
        );
        await siftWait(400);
        scroller = siftFindFeedScroller(siftFeedSelector);
      }
      const siftIsFresh = (item) => {
        if (hasSinceTimestamp) {
          const posted = new Date(item.postedAt);
          if (Number.isNaN(posted.getTime())) {
            return true;
          }
          return posted > sinceTimestamp;
        }
        if (!editionDate) {
          return true;
        }
        return siftDateKey(item.postedAt, timeZone) === editionDate;
      };

      for (let pass = 0; pass < maxPasses; pass += 1) {
        completedPasses = pass + 1;
        siftCollectVisiblePosts(siftFeedSelector, collected);
        const itemCount = collected.size;
        const freshCount = Array.from(collected.values()).filter((item) => siftIsFresh(item)).length;
        await window.__TAURI_INTERNALS__.invoke("submit_linkedin_feed_capture_progress", {
          progress: {
            requestId,
            currentUrl: window.location.href,
            pass: pass + 1,
            totalPasses: maxPasses,
            itemCount,
            freshCount,
            stablePasses: 0,
            exhaustedPasses: stalledPasses,
            boundaryPasses: 0,
          },
        }).catch(() => undefined);

        if (itemCount >= maxItems) {
          break;
        }

        const baseScrollStep = Math.max(siftReadScrollMetrics(scroller).clientHeight * 0.92, 720);
        const advance = await siftAdvanceFeed(siftFeedSelector, scroller, baseScrollStep, {
          segments: 3,
          durationMs: 320,
          settleMs: 120,
        });
        let advanced = await siftWaitForFeedAdvance(
          siftFeedSelector,
          scroller,
          advance.previousIds,
          advance.previousHeight,
          waitForAdvanceMs,
        );
        if (!advanced) {
          const fallbackAdvance = await siftAdvanceFeed(
            siftFeedSelector,
            scroller,
            Math.max(baseScrollStep * 1.35, 980),
            {
              segments: 4,
              durationMs: 380,
              settleMs: 160,
            },
          );
          advanced = await siftWaitForFeedAdvance(
            siftFeedSelector,
            scroller,
            fallbackAdvance.previousIds,
            fallbackAdvance.previousHeight,
            waitForAdvanceMs + 2000,
          );
        }

        const before = collected.size;
        siftCollectVisiblePosts(siftFeedSelector, collected);
        const grew = collected.size > before;
        if (!(advanced || grew)) {
          stalledPasses += 1;
          if (stalledPasses >= stallLimit) {
            endedEarly = completedPasses < maxPasses;
            break;
          }
          await siftWait(900);
        } else {
          stalledPasses = 0;
        }

        if (!(advanced || grew)) {
          scroller = siftFindFeedScroller(siftFeedSelector);
          continue;
        }

        scroller = siftFindFeedScroller(siftFeedSelector);
      }

      siftScrollFeedTo(scroller, 0);
      await window.__TAURI_INTERNALS__.invoke("submit_linkedin_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: Array.from(collected.values()),
          error: null,
          completedPasses,
          totalPasses: maxPasses,
          endedEarly,
        },
      });
    } catch (error) {
      await window.__TAURI_INTERNALS__.invoke("submit_linkedin_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: [],
          error: error instanceof Error ? error.message : String(error),
          completedPasses: null,
          totalPasses: null,
          endedEarly: null,
        },
      });
    }
  };
}
})();
"#;

const REDDIT_SESSION_BRIDGE_SCRIPT: &str = r#"
const siftIsTopFrame = (() => {
  try {
    return window.top === window.self;
  } catch {
    return true;
  }
})();

if (
  siftIsTopFrame
  && ["reddit.com", "www.reddit.com"].includes(window.location.hostname)
  && !window.__SIFT_COLLECT_REDDIT_FEED__
) {
  const siftWait = (ms) => new Promise((resolve) => window.setTimeout(resolve, ms));
  const siftReadText = (node) => (node?.innerText || node?.textContent || "").replace(/\s+/g, " ").trim();
  const siftControlIcons = {
    hide:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 3l18 18"></path><path d="M10.58 10.58A3 3 0 0 0 9 12a3 3 0 0 0 5.33 1.82"></path><path d="M9.88 5.09A10.94 10.94 0 0 1 12 4.91c5 0 9.27 3.11 11 7.5a11.8 11.8 0 0 1-3.29 4.68"></path><path d="M6.61 6.61A11.81 11.81 0 0 0 1 12.41a11.84 11.84 0 0 0 4.26 5.1"></path></svg>',
    logout:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"></path><path d="M16 17l5-5-5-5"></path><path d="M21 12H9"></path></svg>',
  };

  const siftEnsureSessionControls = () => {
    if (!document.body) {
      return;
    }

    if (!document.getElementById("sift-reddit-session-controls-style")) {
      const style = document.createElement("style");
      style.id = "sift-reddit-session-controls-style";
      style.textContent = `
        #sift-reddit-session-controls {
          position: fixed;
          right: 18px;
          bottom: 18px;
          z-index: 2147483647;
          display: inline-flex;
          align-items: center;
          gap: 8px;
          padding: 10px 12px;
          border-radius: 999px;
          color: #fff;
          background: rgba(11, 19, 31, 0.88);
          box-shadow: 0 12px 36px rgba(0, 0, 0, 0.32);
          font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        }
        .sift-reddit-session-controls__badge { font-size: 12px; font-weight: 700; letter-spacing: 0.01em; }
        .sift-reddit-session-controls__button {
          appearance: none;
          -webkit-appearance: none;
          width: 36px;
          height: 36px;
          min-width: 36px;
          min-height: 36px;
          padding: 0;
          border: 0;
          border-radius: 999px;
          display: inline-flex;
          flex: 0 0 auto;
          align-items: center;
          justify-content: center;
          background: rgba(255, 255, 255, 0.08);
          color: inherit;
          cursor: pointer;
          line-height: 0;
          font-size: 0;
          vertical-align: middle;
        }
        .sift-reddit-session-controls__button--danger { background: rgba(255, 69, 0, 0.18); }
        .sift-reddit-session-controls__button svg {
          width: 18px;
          height: 18px;
          min-width: 18px;
          min-height: 18px;
          display: block;
          flex: 0 0 auto;
          overflow: visible;
          fill: none;
          stroke: currentColor;
          stroke-width: 1.9;
          stroke-linecap: round;
          stroke-linejoin: round;
        }
      `;
      document.head.appendChild(style);
    }

    if (document.getElementById("sift-reddit-session-controls")) {
      return;
    }

    const dock = document.createElement("div");
    dock.id = "sift-reddit-session-controls";
    const badge = document.createElement("span");
    badge.className = "sift-reddit-session-controls__badge";
    badge.textContent = "SIFT Reddit";
    dock.appendChild(badge);

    const buildButton = (command, label, icon, danger = false) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "sift-reddit-session-controls__button";
      if (danger) {
        button.classList.add("sift-reddit-session-controls__button--danger");
      }
      button.setAttribute("aria-label", label);
      button.title = label;
      button.innerHTML = icon;
      button.addEventListener("click", () => {
        window.__TAURI_INTERNALS__?.invoke?.(command).catch(() => undefined);
      });
      return button;
    };

    dock.appendChild(buildButton("hide_reddit_session_window", "Hide this Reddit window", siftControlIcons.hide));
    dock.appendChild(buildButton("logout_reddit_session_window", "Log out of Reddit in SIFT", siftControlIcons.logout, true));
    document.body.appendChild(dock);
  };

  const siftFeedSelector = 'shreddit-post, faceplate-tracker[noun="feed_post"], article[data-testid="post-container"], div[data-testid="post-container"]';
  const siftPostElements = () =>
    Array.from(document.querySelectorAll(siftFeedSelector))
      .filter((node) => node instanceof Element)
      .filter((node, index, items) => items.indexOf(node) === index);

  const siftPostUrl = (article) => {
    const candidates = Array.from(article.querySelectorAll('a[href*="/comments/"]'));
    for (const link of candidates) {
      const href = link.getAttribute("href") || "";
      try {
        const url = new URL(href, window.location.origin);
        if (/\/comments\/[^/]+/i.test(url.pathname)) {
          url.hash = "";
          return url.toString();
        }
      } catch {}
    }
    return null;
  };

  const siftSharedUrls = (article, sourceUrl) => {
    const urls = new Set();
    Array.from(article.querySelectorAll("a[href]")).forEach((link) => {
      const href = link.getAttribute("href") || "";
      try {
        const url = new URL(href, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          return;
        }
        url.hash = "";
        if (sourceUrl && url.toString() === sourceUrl) {
          return;
        }
        if (["reddit.com", "www.reddit.com"].includes(url.hostname) && /\/comments\/[^/]+/i.test(url.pathname)) {
          return;
        }
        urls.add(url.toString());
      } catch {}
    });
    return Array.from(urls);
  };

  const siftRedditMedia = (article) => {
    const media = [];
    const seen = new Set();
    const candidates = [];
    const pushCandidate = (value) => {
      if (value) {
        candidates.push(value);
      }
    };

    pushCandidate(article.getAttribute("content-href"));
    pushCandidate(article.getAttribute("thumbnail"));
    pushCandidate(article.getAttribute("preview"));
    Array.from(article.querySelectorAll("img")).forEach((img) => {
      pushCandidate(img.currentSrc);
      pushCandidate(img.getAttribute("src"));
      (img.getAttribute("srcset") || "")
        .split(",")
        .map((part) => part.trim().split(/\s+/)[0])
        .filter(Boolean)
        .forEach(pushCandidate);
    });

    for (const candidate of candidates) {
      try {
        const url = new URL(candidate, window.location.origin);
        if (!["http:", "https:"].includes(url.protocol)) {
          continue;
        }
        const host = url.hostname.toLowerCase();
        if (
          !(
            host === "i.redd.it"
            || host === "preview.redd.it"
            || host === "external-preview.redd.it"
            || host.endsWith(".redd.it")
            || host === "i.imgur.com"
          )
        ) {
          continue;
        }
        if (/avatar|emoji|icon|award|snoovatar|styles\.redditmedia/i.test(url.pathname)) {
          continue;
        }
        url.hash = "";
        const normalized = url.toString();
        if (seen.has(normalized)) {
          continue;
        }
        seen.add(normalized);
        media.push({
          url: normalized,
          kind: "photo",
        });
      } catch {
        // Ignore malformed media URLs while scraping.
      }
    }

    return media;
  };

  const siftParsePost = (article) => {
    const sourceUrl = siftPostUrl(article);
    const postId = sourceUrl?.match(/\/comments\/([^/]+)/i)?.[1]
      || article.getAttribute("id")
      || article.getAttribute("post-id")
      || article.getAttribute("thingid")
      || null;
    if (!sourceUrl || !postId) {
      return null;
    }

    const authorHandle =
      article.getAttribute("author")
      || article.querySelector('a[href*="/user/"], a[href*="/u/"]')?.getAttribute("href")?.match(/\/(?:user|u)\/([^/?#]+)/i)?.[1]
      || "reddit-user";
    const authorName = article.getAttribute("subreddit-prefixed-name")
      || article.querySelector('[slot="subreddit-name"], [data-testid="subreddit-name"]')?.textContent?.trim()
      || article.querySelector('[slot="authorName"]')?.textContent?.trim()
      || `u/${authorHandle}`;
    const title = article.getAttribute("post-title")
      || siftReadText(article.querySelector('[slot="title"], h3'))
      || "";
    const body = siftReadText(article.querySelector('[slot="text-body"], [data-click-id="text"], [data-adclicklocation="media"], p'));
    const text = [title, body].filter(Boolean).join("\n\n").trim();
    if (!text) {
      return null;
    }

    const postedAt =
      article.querySelector("faceplate-timeago")?.getAttribute("ts")
      || article.querySelector("time")?.getAttribute("datetime")
      || new Date().toISOString();
    const socialContext = siftReadText(article.querySelector('[slot="credit-bar"], [slot="thumbnail"]')) || null;
    const articleText = siftReadText(article);
    const isPromoted = /\b(promoted|sponsored|advertisement|patrocinad[oa]s?|publicidad|anuncio)\b/i.test(
      [socialContext, articleText, article.getAttribute("promoted"), article.getAttribute("is-promoted")].filter(Boolean).join(" "),
    );

    return {
      id: postId,
      authorName,
      authorHandle: authorHandle.replace(/^u\//i, "").replace(/^@/, ""),
      text,
      sourceUrl,
      postedAt,
      isRepost: false,
      isReply: false,
      isPromoted,
      socialContext,
      sharedUrls: siftSharedUrls(article, sourceUrl),
      media: siftRedditMedia(article),
    };
  };

  const siftCollectVisiblePosts = (collected) => {
    siftPostElements().forEach((article) => {
      const parsed = siftParsePost(article);
      if (parsed) {
        collected.set(parsed.id, parsed);
      }
    });
  };

  const siftVisiblePostIds = () => new Set(siftPostElements().map((article) => siftPostUrl(article) || article.id).filter(Boolean));
  const siftReadScrollMetrics = () => {
    const root = document.scrollingElement || document.documentElement;
    return {
      top: window.scrollY || root.scrollTop || 0,
      height: root.scrollHeight || document.documentElement.scrollHeight || 0,
      clientHeight: window.innerHeight || root.clientHeight || document.documentElement.clientHeight || 0,
    };
  };
  const siftScrollTo = (top) => window.scrollTo(0, Math.max(0, top));
  const siftAdvanceFeed = async (distance) => {
    const before = siftReadScrollMetrics();
    const previousIds = siftVisiblePostIds();
    siftScrollTo(before.top + distance);
    return { previousHeight: before.height, previousIds };
  };
  const siftWaitForAdvance = async (previousIds, previousHeight, timeoutMs) => {
    const startedAt = Date.now();
    while (Date.now() - startedAt < timeoutMs) {
      await siftWait(250);
      const metrics = siftReadScrollMetrics();
      if (metrics.height > previousHeight + 48) {
        return true;
      }
      for (const id of siftVisiblePostIds()) {
        if (!previousIds.has(id)) {
          return true;
        }
      }
    }
    return false;
  };

  if (!window.__SIFT_REDDIT_SESSION_CONTROLS_WATCHDOG__) {
    window.__SIFT_REDDIT_SESSION_CONTROLS_WATCHDOG__ = window.setInterval(() => {
      try {
        siftEnsureSessionControls();
      } catch {}
    }, 1500);
  }

  window.__SIFT_COLLECT_REDDIT_FEED__ = async (requestId, options = {}) => {
    try {
      siftEnsureSessionControls();
      if (!/^\/(?:$|best\/?$|hot\/?$|new\/?$)/.test(window.location.pathname)) {
        throw new Error("Reddit session is not on the home feed yet.");
      }

      const maxItems = Math.min(Math.max(Number(options.maxItems) || 400, 100), 800);
      const maxPasses = Math.max(Number(options.maxPasses) || 10, 1);
      const waitForAdvanceMs = Math.max(Number(options.waitForAdvanceMs) || 4000, 1200);
      const stallLimit = Math.max(Number(options.stablePasses) || 3, 2);
      const collected = new Map();
      let stalledPasses = 0;
      let completedPasses = 0;
      let endedEarly = false;
      siftScrollTo(0);
      await siftWait(900);

      for (let pass = 0; pass < maxPasses; pass += 1) {
        completedPasses = pass + 1;
        siftCollectVisiblePosts(collected);
        await window.__TAURI_INTERNALS__.invoke("submit_reddit_feed_capture_progress", {
          progress: {
            requestId,
            currentUrl: window.location.href,
            pass: pass + 1,
            totalPasses: maxPasses,
            itemCount: collected.size,
            freshCount: collected.size,
            stablePasses: stalledPasses,
            exhaustedPasses: stalledPasses,
            boundaryPasses: 0,
          },
        }).catch(() => undefined);

        if (collected.size >= maxItems) {
          break;
        }

        const advance = await siftAdvanceFeed(Math.max(siftReadScrollMetrics().clientHeight * 0.9, 880));
        const advanced = await siftWaitForAdvance(
          advance.previousIds,
          advance.previousHeight,
          waitForAdvanceMs,
        );
        const before = collected.size;
        siftCollectVisiblePosts(collected);
        const grew = collected.size > before;
        if (!(advanced || grew)) {
          stalledPasses += 1;
          if (stalledPasses >= stallLimit) {
            endedEarly = completedPasses < maxPasses;
            break;
          }
          await siftWait(900);
        } else {
          stalledPasses = 0;
        }
      }

      siftScrollTo(0);
      await window.__TAURI_INTERNALS__.invoke("submit_reddit_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: Array.from(collected.values()),
          error: null,
          completedPasses,
          totalPasses: maxPasses,
          endedEarly,
        },
      });
    } catch (error) {
      await window.__TAURI_INTERNALS__.invoke("submit_reddit_feed_capture", {
        capture: {
          requestId,
          currentUrl: window.location.href,
          items: [],
          error: error instanceof Error ? error.message : String(error),
          completedPasses: null,
          totalPasses: null,
          endedEarly: null,
        },
      });
    }
  };
}
"#;

fn is_x_domain(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
    )
}

fn is_linkedin_domain(url: &Url) -> bool {
    matches!(url.host_str(), Some("linkedin.com" | "www.linkedin.com"))
}

fn is_reddit_domain(url: &Url) -> bool {
    matches!(url.host_str(), Some("reddit.com" | "www.reddit.com"))
}

fn is_google_auth_url(url: &Url) -> bool {
    matches!(url.host_str(), Some("accounts.google.com"))
}

fn is_blank_page_url(url: &Url) -> bool {
    url.scheme() == "about" && url.path() == "blank"
}

fn is_x_auth_popup_label(label: &str) -> bool {
    label.starts_with(X_AUTH_POPUP_LABEL_PREFIX)
}

fn is_x_session_related_label(label: &str) -> bool {
    label == X_SESSION_WINDOW_LABEL
        || label.starts_with(X_SESSION_POPUP_LABEL_PREFIX)
        || is_x_auth_popup_label(label)
}

fn is_linkedin_session_related_label(label: &str) -> bool {
    label == LINKEDIN_SESSION_WINDOW_LABEL || label.starts_with(LINKEDIN_SESSION_POPUP_LABEL_PREFIX)
}

fn is_reddit_session_related_label(label: &str) -> bool {
    label == REDDIT_SESSION_WINDOW_LABEL || label.starts_with(REDDIT_SESSION_POPUP_LABEL_PREFIX)
}

fn x_session_window_labels(app: &tauri::AppHandle) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| is_x_session_related_label(label))
        .collect()
}

fn x_auth_popup_window_labels(app: &tauri::AppHandle) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| is_x_auth_popup_label(label))
        .collect()
}

fn linkedin_session_window_labels(app: &tauri::AppHandle) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| is_linkedin_session_related_label(label))
        .collect()
}

fn reddit_session_window_labels(app: &tauri::AppHandle) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| is_reddit_session_related_label(label))
        .collect()
}

fn close_x_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in x_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.close().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn close_x_auth_popup_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in x_auth_popup_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.close().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn hide_x_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in x_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.hide().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn close_linkedin_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in linkedin_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.close().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn hide_linkedin_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in linkedin_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.hide().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn close_reddit_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in reddit_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.close().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn hide_reddit_session_windows(app: &tauri::AppHandle) -> Result<(), String> {
    for label in reddit_session_window_labels(app) {
        if let Some(window) = app.get_webview_window(&label) {
            window.hide().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn is_completed_x_session_url(url: &Url) -> bool {
    if !is_x_domain(url) {
        return false;
    }

    let path = url.path();
    !path.starts_with("/i/flow/login")
        && !path.starts_with("/login")
        && !path.starts_with("/account/access")
}

fn should_close_x_auth_popup_for_url(url: &Url) -> bool {
    is_completed_x_session_url(url) || is_blank_page_url(url)
}

fn default_x_session_url() -> Url {
    Url::parse(X_SESSION_HOME_URL).expect("valid x home url")
}

fn default_linkedin_session_url() -> Url {
    Url::parse(LINKEDIN_SESSION_HOME_URL).expect("valid linkedin home url")
}

fn default_reddit_session_url() -> Url {
    Url::parse(REDDIT_SESSION_HOME_URL).expect("valid reddit home url")
}

fn resolve_x_session_launch_url(saved_url: Option<&str>) -> Url {
    saved_url
        .and_then(|raw| Url::parse(raw).ok())
        .filter(is_x_domain)
        .unwrap_or_else(default_x_session_url)
}

fn resolve_linkedin_session_launch_url(saved_url: Option<&str>) -> Url {
    saved_url
        .and_then(|raw| Url::parse(raw).ok())
        .filter(is_linkedin_domain)
        .unwrap_or_else(default_linkedin_session_url)
}

fn resolve_reddit_session_launch_url(saved_url: Option<&str>) -> Url {
    saved_url
        .and_then(|raw| Url::parse(raw).ok())
        .filter(is_reddit_domain)
        .unwrap_or_else(default_reddit_session_url)
}

fn build_x_session_window(
    app: &tauri::AppHandle,
    state: AppState,
    initial_url: Url,
    is_visible: bool,
    focus_window: bool,
) -> Result<tauri::WebviewWindow, String> {
    let popup_app = app.clone();
    let popup_state = state.clone();
    let page_app = app.clone();
    let page_state = state.clone();

    WebviewWindowBuilder::new(
        app,
        X_SESSION_WINDOW_LABEL,
        WebviewUrl::External(initial_url),
    )
    .data_store_identifier(X_SESSION_DATA_STORE_ID)
    .title("SIFT X Session")
    .inner_size(1320.0, 900.0)
    .min_inner_size(980.0, 700.0)
    .resizable(true)
    .visible(is_visible)
    .focused(focus_window)
    .background_throttling(BackgroundThrottlingPolicy::Disabled)
    .center()
    .prevent_overflow()
    .initialization_script(X_SESSION_BRIDGE_SCRIPT)
    .on_new_window(move |url, features| {
        let popup_label = if is_google_auth_url(&url) {
            format!("{X_AUTH_POPUP_LABEL_PREFIX}-{}", Uuid::new_v4())
        } else {
            format!("{X_SESSION_POPUP_LABEL_PREFIX}-{}", Uuid::new_v4())
        };

        let parent_app = popup_app.clone();
        let is_auth_popup = is_google_auth_url(&url);
        let popup_state = popup_state.clone();

        let popup_window = WebviewWindowBuilder::new(
            &popup_app,
            popup_label,
            WebviewUrl::External(Url::parse("about:blank").expect("valid popup url")),
        )
        .data_store_identifier(X_SESSION_DATA_STORE_ID)
        .window_features(features)
        .title(url.as_str())
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        .on_page_load(move |window, payload| {
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }

            let should_close_popup = should_close_x_auth_popup_for_url(payload.url());
            let completed_x_session = is_completed_x_session_url(payload.url());

            if is_auth_popup && should_close_popup {
                let window = window.clone();
                let parent_window = parent_app.get_webview_window(X_SESSION_WINDOW_LABEL);
                let popup_state = popup_state.clone();
                let completed_url = payload.url().to_string();
                tauri::async_runtime::spawn(async move {
                    if completed_x_session {
                        if let Err(error) =
                            popup_state.remember_x_session(completed_url, true).await
                        {
                            eprintln!("failed to persist X auth popup state: {error}");
                        }
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(
                        X_AUTH_POPUP_CLOSE_DELAY_MS,
                    ))
                    .await;

                    if completed_x_session {
                        if let Some(parent_window) = parent_window {
                            let _ = parent_window.navigate(default_x_session_url());
                        }
                    }

                    let _ = window.close();
                });
            }
        })
        .build()
        .expect("x session popup window");

        tauri::webview::NewWindowResponse::Create {
            window: popup_window,
        }
    })
    .on_page_load(move |window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            let _ = window.eval(X_SESSION_BRIDGE_SCRIPT);
            let page_app = page_app.clone();
            let page_state = page_state.clone();
            let url = payload.url().to_string();
            let is_authenticated = is_completed_x_session_url(payload.url());
            tauri::async_runtime::spawn(async move {
                if let Err(error) = page_state.remember_x_session(url, is_authenticated).await {
                    eprintln!("failed to persist X session page state: {error}");
                }

                if is_authenticated {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        X_AUTH_POPUP_CLOSE_DELAY_MS,
                    ))
                    .await;

                    if let Err(error) = close_x_auth_popup_windows(&page_app) {
                        eprintln!("failed to close X auth popups after login: {error}");
                    }
                }
            });
        }
    })
    .build()
    .map_err(|error| error.to_string())
}

fn is_completed_linkedin_session_url(url: &Url) -> bool {
    if !is_linkedin_domain(url) {
        return false;
    }

    !url.path().starts_with("/login")
        && !url.path().starts_with("/checkpoint")
        && !url.path().starts_with("/signup")
}

fn is_completed_reddit_session_url(url: &Url) -> bool {
    if !is_reddit_domain(url) {
        return false;
    }

    !url.path().starts_with("/login")
        && !url.path().starts_with("/register")
        && !url.path().starts_with("/password")
}

fn build_linkedin_session_window(
    app: &tauri::AppHandle,
    state: AppState,
    initial_url: Url,
    is_visible: bool,
    focus_window: bool,
) -> Result<tauri::WebviewWindow, String> {
    let popup_app = app.clone();
    let page_state = state.clone();

    WebviewWindowBuilder::new(
        app,
        LINKEDIN_SESSION_WINDOW_LABEL,
        WebviewUrl::External(initial_url),
    )
    .data_store_identifier(LINKEDIN_SESSION_DATA_STORE_ID)
    .title("SIFT LinkedIn Session")
    .inner_size(1320.0, 900.0)
    .min_inner_size(980.0, 700.0)
    .resizable(true)
    .visible(is_visible)
    .focused(focus_window)
    .background_throttling(BackgroundThrottlingPolicy::Disabled)
    .center()
    .prevent_overflow()
    .initialization_script(LINKEDIN_SESSION_BRIDGE_SCRIPT)
    .on_new_window(move |url, features| {
        let popup_label = format!("{LINKEDIN_SESSION_POPUP_LABEL_PREFIX}-{}", Uuid::new_v4());

        let popup_window = WebviewWindowBuilder::new(
            &popup_app,
            popup_label,
            WebviewUrl::External(Url::parse("about:blank").expect("valid popup url")),
        )
        .data_store_identifier(LINKEDIN_SESSION_DATA_STORE_ID)
        .window_features(features)
        .title(url.as_str())
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        .build()
        .expect("linkedin session popup window");

        tauri::webview::NewWindowResponse::Create {
            window: popup_window,
        }
    })
    .on_page_load(move |window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            let _ = window.eval(LINKEDIN_SESSION_BRIDGE_SCRIPT);
            let page_state = page_state.clone();
            let url = payload.url().to_string();
            let is_authenticated = is_completed_linkedin_session_url(payload.url());
            tauri::async_runtime::spawn(async move {
                if let Err(error) = page_state
                    .remember_linkedin_session(url, is_authenticated)
                    .await
                {
                    eprintln!("failed to persist LinkedIn session page state: {error}");
                }
            });
        }
    })
    .build()
    .map_err(|error| error.to_string())
}

fn build_reddit_session_window(
    app: &tauri::AppHandle,
    state: AppState,
    initial_url: Url,
    is_visible: bool,
    focus_window: bool,
) -> Result<tauri::WebviewWindow, String> {
    let popup_app = app.clone();
    let page_state = state.clone();

    WebviewWindowBuilder::new(
        app,
        REDDIT_SESSION_WINDOW_LABEL,
        WebviewUrl::External(initial_url),
    )
    .data_store_identifier(REDDIT_SESSION_DATA_STORE_ID)
    .title("SIFT Reddit Session")
    .inner_size(1320.0, 900.0)
    .min_inner_size(980.0, 700.0)
    .resizable(true)
    .visible(is_visible)
    .focused(focus_window)
    .background_throttling(BackgroundThrottlingPolicy::Disabled)
    .center()
    .prevent_overflow()
    .initialization_script(REDDIT_SESSION_BRIDGE_SCRIPT)
    .on_new_window(move |url, features| {
        let popup_label = format!("{REDDIT_SESSION_POPUP_LABEL_PREFIX}-{}", Uuid::new_v4());

        let popup_window = WebviewWindowBuilder::new(
            &popup_app,
            popup_label,
            WebviewUrl::External(Url::parse("about:blank").expect("valid popup url")),
        )
        .data_store_identifier(REDDIT_SESSION_DATA_STORE_ID)
        .window_features(features)
        .title(url.as_str())
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        .build()
        .expect("reddit session popup window");

        tauri::webview::NewWindowResponse::Create {
            window: popup_window,
        }
    })
    .on_page_load(move |_window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            let page_state = page_state.clone();
            let url = payload.url().to_string();
            let is_authenticated = is_completed_reddit_session_url(payload.url());
            tauri::async_runtime::spawn(async move {
                if let Err(error) = page_state
                    .remember_reddit_session(url, is_authenticated)
                    .await
                {
                    eprintln!("failed to persist Reddit session page state: {error}");
                }
            });
        }
    })
    .build()
    .map_err(|error| error.to_string())
}

fn restore_x_session_window(state: &AppState) -> Result<(), AppError> {
    let Some(saved_session) = state.db.load_persisted_x_session()? else {
        return Ok(());
    };

    if state
        .app
        .get_webview_window(X_SESSION_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let initial_url = resolve_x_session_launch_url(Some(saved_session.last_known_url.as_str()));
    let initial_url_string = initial_url.to_string();
    *state.x_session_last_known_url.blocking_write() = Some(initial_url_string.clone());
    *state.x_session_authenticated.blocking_write() = saved_session.is_authenticated;
    state
        .db
        .save_persisted_x_session(&PersistedBrowserSession {
            last_known_url: initial_url_string,
            is_authenticated: saved_session.is_authenticated,
        })?;
    build_x_session_window(&state.app, state.clone(), initial_url, false, false)
        .map_err(AppError::Message)?;
    Ok(())
}

fn restore_linkedin_session_window(state: &AppState) -> Result<(), AppError> {
    let Some(saved_session) = state.db.load_persisted_linkedin_session()? else {
        return Ok(());
    };

    if state
        .app
        .get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let initial_url =
        resolve_linkedin_session_launch_url(Some(saved_session.last_known_url.as_str()));
    let initial_url_string = initial_url.to_string();
    *state.linkedin_session_last_known_url.blocking_write() = Some(initial_url_string.clone());
    *state.linkedin_session_authenticated.blocking_write() = saved_session.is_authenticated;
    state
        .db
        .save_persisted_linkedin_session(&PersistedBrowserSession {
            last_known_url: initial_url_string,
            is_authenticated: saved_session.is_authenticated,
        })?;
    build_linkedin_session_window(&state.app, state.clone(), initial_url, false, false)
        .map_err(AppError::Message)?;
    Ok(())
}

fn restore_reddit_session_window(state: &AppState) -> Result<(), AppError> {
    let Some(saved_session) = state.db.load_persisted_reddit_session()? else {
        return Ok(());
    };

    if state
        .app
        .get_webview_window(REDDIT_SESSION_WINDOW_LABEL)
        .is_some()
    {
        return Ok(());
    }

    let initial_url =
        resolve_reddit_session_launch_url(Some(saved_session.last_known_url.as_str()));
    let initial_url_string = initial_url.to_string();
    *state.reddit_session_last_known_url.blocking_write() = Some(initial_url_string.clone());
    *state.reddit_session_authenticated.blocking_write() = saved_session.is_authenticated;
    state
        .db
        .save_persisted_reddit_session(&PersistedBrowserSession {
            last_known_url: initial_url_string,
            is_authenticated: saved_session.is_authenticated,
        })?;
    build_reddit_session_window(&state.app, state.clone(), initial_url, false, false)
        .map_err(AppError::Message)?;
    Ok(())
}

fn hide_session_windows_on_startup(app: &tauri::AppHandle) {
    for label in [
        X_SESSION_WINDOW_LABEL,
        LINKEDIN_SESSION_WINDOW_LABEL,
        REDDIT_SESSION_WINDOW_LABEL,
    ] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.hide();
        }
    }
}

fn schedule_session_windows_hidden_on_startup(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let delays_ms = [0_u64, 150, 600, 1500];
        for delay_ms in delays_ms {
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            hide_session_windows_on_startup(&app);
        }
    });
}

#[tauri::command]
async fn get_bootstrap_state(state: tauri::State<'_, AppState>) -> Result<BootstrapState, String> {
    state.db.load_bootstrap().map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_settings(
    state: tauri::State<'_, AppState>,
    settings: UserSettings,
) -> Result<UserSettings, String> {
    state
        .set_lm_studio_auth_token(settings.lm_studio.auth_token.clone())
        .await;
    state
        .db
        .save_settings(&settings)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_x_session_state(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn get_linkedin_session_state(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    Ok(state.linkedin_session_state().await)
}

#[tauri::command]
async fn get_reddit_session_state(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    Ok(state.reddit_session_state().await)
}

#[tauri::command]
async fn open_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(X_SESSION_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(state.x_session_state().await);
    }

    let saved_session = state
        .db
        .load_persisted_x_session()
        .map_err(|error| error.to_string())?;
    let initial_url = resolve_x_session_launch_url(
        saved_session
            .as_ref()
            .map(|session| session.last_known_url.as_str()),
    );
    let is_authenticated = saved_session
        .as_ref()
        .is_some_and(|session| session.is_authenticated);

    state
        .remember_x_session(initial_url.to_string(), is_authenticated)
        .await
        .map_err(|error| error.to_string())?;

    let window =
        build_x_session_window(&state.app, state.inner().clone(), initial_url, true, true)?;

    let _ = window.show();
    let _ = window.set_focus();

    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn open_linkedin_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(state.linkedin_session_state().await);
    }

    let saved_session = state
        .db
        .load_persisted_linkedin_session()
        .map_err(|error| error.to_string())?;
    let initial_url = resolve_linkedin_session_launch_url(
        saved_session
            .as_ref()
            .map(|session| session.last_known_url.as_str()),
    );
    let is_authenticated = saved_session
        .as_ref()
        .is_some_and(|session| session.is_authenticated);

    state
        .remember_linkedin_session(initial_url.to_string(), is_authenticated)
        .await
        .map_err(|error| error.to_string())?;

    let window =
        build_linkedin_session_window(&state.app, state.inner().clone(), initial_url, true, true)?;

    let _ = window.show();
    let _ = window.set_focus();

    Ok(state.linkedin_session_state().await)
}

#[tauri::command]
async fn open_reddit_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(REDDIT_SESSION_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(state.reddit_session_state().await);
    }

    let saved_session = state
        .db
        .load_persisted_reddit_session()
        .map_err(|error| error.to_string())?;
    let initial_url = resolve_reddit_session_launch_url(
        saved_session
            .as_ref()
            .map(|session| session.last_known_url.as_str()),
    );
    let is_authenticated = saved_session
        .as_ref()
        .is_some_and(|session| session.is_authenticated);

    state
        .remember_reddit_session(initial_url.to_string(), is_authenticated)
        .await
        .map_err(|error| error.to_string())?;

    let window =
        build_reddit_session_window(&state.app, state.inner().clone(), initial_url, true, true)?;

    let _ = window.show();
    let _ = window.set_focus();

    Ok(state.reddit_session_state().await)
}

#[tauri::command]
async fn close_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_x_session(state.inner()).await?;
    Ok(state.x_session_state().await)
}

async fn logout_x_session(state: &AppState) -> Result<(), String> {
    state.x_session_force_close.store(true, Ordering::SeqCst);

    let result = (|| -> Result<(), String> {
        if let Some(window) = state.app.get_webview_window(X_SESSION_WINDOW_LABEL) {
            window
                .clear_all_browsing_data()
                .map_err(|error| error.to_string())?;
        }

        close_x_session_windows(&state.app)
    })();

    if let Err(error) = result {
        state.x_session_force_close.store(false, Ordering::SeqCst);
        return Err(error);
    }

    state
        .forget_x_session()
        .await
        .map_err(|error| error.to_string())?;

    if state
        .app
        .get_webview_window(X_SESSION_WINDOW_LABEL)
        .is_none()
    {
        state.x_session_force_close.store(false, Ordering::SeqCst);
    }

    Ok(())
}

async fn logout_linkedin_session(state: &AppState) -> Result<(), String> {
    state
        .linkedin_session_force_close
        .store(true, Ordering::SeqCst);

    let result = (|| -> Result<(), String> {
        if let Some(window) = state.app.get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL) {
            window
                .clear_all_browsing_data()
                .map_err(|error| error.to_string())?;
        }

        close_linkedin_session_windows(&state.app)
    })();

    if let Err(error) = result {
        state
            .linkedin_session_force_close
            .store(false, Ordering::SeqCst);
        return Err(error);
    }

    state
        .forget_linkedin_session()
        .await
        .map_err(|error| error.to_string())?;

    if state
        .app
        .get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL)
        .is_none()
    {
        state
            .linkedin_session_force_close
            .store(false, Ordering::SeqCst);
    }

    Ok(())
}

async fn logout_reddit_session(state: &AppState) -> Result<(), String> {
    state
        .reddit_session_force_close
        .store(true, Ordering::SeqCst);

    let result = (|| -> Result<(), String> {
        if let Some(window) = state.app.get_webview_window(REDDIT_SESSION_WINDOW_LABEL) {
            window
                .clear_all_browsing_data()
                .map_err(|error| error.to_string())?;
        }

        close_reddit_session_windows(&state.app)
    })();

    if let Err(error) = result {
        state
            .reddit_session_force_close
            .store(false, Ordering::SeqCst);
        return Err(error);
    }

    state
        .forget_reddit_session()
        .await
        .map_err(|error| error.to_string())?;

    if state
        .app
        .get_webview_window(REDDIT_SESSION_WINDOW_LABEL)
        .is_none()
    {
        state
            .reddit_session_force_close
            .store(false, Ordering::SeqCst);
    }

    Ok(())
}

#[tauri::command]
async fn close_linkedin_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_linkedin_session(state.inner()).await?;
    Ok(state.linkedin_session_state().await)
}

#[tauri::command]
async fn close_reddit_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_reddit_session(state.inner()).await?;
    Ok(state.reddit_session_state().await)
}

#[tauri::command]
async fn hide_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(X_SESSION_WINDOW_LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }

    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn hide_linkedin_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(LINKEDIN_SESSION_WINDOW_LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }

    Ok(state.linkedin_session_state().await)
}

#[tauri::command]
async fn hide_reddit_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    if let Some(window) = state.app.get_webview_window(REDDIT_SESSION_WINDOW_LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }

    Ok(state.reddit_session_state().await)
}

#[tauri::command]
async fn logout_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_x_session(state.inner()).await?;
    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn logout_linkedin_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_linkedin_session(state.inner()).await?;
    Ok(state.linkedin_session_state().await)
}

#[tauri::command]
async fn logout_reddit_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<BrowserSessionState, String> {
    logout_reddit_session(state.inner()).await?;
    Ok(state.reddit_session_state().await)
}

#[tauri::command]
async fn verify_lm_studio(
    _state: tauri::State<'_, AppState>,
    base_url: String,
    auth_token: Option<String>,
) -> Result<models::LmStudioHealth, String> {
    LmStudioClient::default()
        .health_check(&base_url, auth_token.as_deref())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn start_x_connect(
    state: tauri::State<'_, AppState>,
    client_config: XClientConfigDraft,
) -> Result<XConnectLaunch, String> {
    let client = XClient::default();
    client
        .connect(&client_config)
        .await
        .map_err(|error| error.to_string())?;
    let (session, launch) = client
        .start_connect(&client_config)
        .await
        .map_err(|error| error.to_string())?;

    state
        .oauth_sessions
        .write()
        .await
        .insert(session.state.clone(), session);

    client.spawn_callback_listener(state.inner().clone(), launch.state.clone());
    Ok(launch)
}

#[tauri::command]
async fn poll_x_connect(
    state: tauri::State<'_, AppState>,
    state_id: String,
) -> Result<XConnectPollResult, String> {
    let mut sessions = state.oauth_sessions.write().await;

    let Some(session) = sessions.get_mut(&state_id) else {
        return Ok(XConnectPollResult {
            status: PollStatus::Expired,
            error_message: Some("The X authorization session expired.".into()),
            payload: None,
        });
    };

    if Utc::now().signed_duration_since(session.created_at) > chrono::Duration::minutes(10) {
        return Ok(XConnectPollResult {
            status: PollStatus::Expired,
            error_message: Some("The X authorization window timed out. Start again.".into()),
            payload: None,
        });
    }

    Ok(session.result.clone().unwrap_or(XConnectPollResult {
        status: PollStatus::Pending,
        error_message: None,
        payload: None,
    }))
}

#[tauri::command]
async fn submit_x_feed_capture(
    state: tauri::State<'_, AppState>,
    capture: XSessionCapturePayload,
) -> Result<(), String> {
    let sender = {
        let mut requests = state.x_session_capture_requests.lock().await;
        let request = requests.get_mut(&capture.request_id).ok_or_else(|| {
            "The feed capture request expired before the page responded.".to_string()
        })?;
        request.last_progress_at = Instant::now();
        request.sender.take().ok_or_else(|| {
            "The feed capture receiver was already claimed before the page responded.".to_string()
        })?
    };

    sender
        .send(if let Some(error) = capture.error.clone() {
            Err(error)
        } else {
            Ok(capture)
        })
        .map_err(|_| "The feed capture receiver was dropped before the page responded.".to_string())
}

#[tauri::command]
async fn submit_linkedin_feed_capture(
    state: tauri::State<'_, AppState>,
    capture: XSessionCapturePayload,
) -> Result<(), String> {
    submit_x_feed_capture(state, capture).await
}

#[tauri::command]
async fn submit_reddit_feed_capture(
    state: tauri::State<'_, AppState>,
    capture: XSessionCapturePayload,
) -> Result<(), String> {
    submit_x_feed_capture(state, capture).await
}

#[tauri::command]
async fn submit_x_feed_capture_progress(
    state: tauri::State<'_, AppState>,
    progress: XSessionCaptureProgressPayload,
) -> Result<(), String> {
    let (run_id, reason, source_label) = {
        let mut requests = state.x_session_capture_requests.lock().await;
        let request = requests.get_mut(&progress.request_id).ok_or_else(|| {
            "The feed capture request expired before the page reported progress.".to_string()
        })?;
        request.last_progress_at = Instant::now();
        request.latest_progress = Some(progress.snapshot());
        (
            request.run_id.clone(),
            request.reason.clone(),
            request.source_label.clone(),
        )
    };

    emit_capture_progress(state.inner(), &run_id, &reason, &source_label, &progress);
    Ok(())
}

#[tauri::command]
async fn submit_linkedin_feed_capture_progress(
    state: tauri::State<'_, AppState>,
    progress: XSessionCaptureProgressPayload,
) -> Result<(), String> {
    submit_x_feed_capture_progress(state, progress).await
}

#[tauri::command]
async fn submit_reddit_feed_capture_progress(
    state: tauri::State<'_, AppState>,
    progress: XSessionCaptureProgressPayload,
) -> Result<(), String> {
    submit_x_feed_capture_progress(state, progress).await
}

#[tauri::command]
async fn run_sync(
    state: tauri::State<'_, AppState>,
    reason: String,
) -> Result<BootstrapState, String> {
    let sync_reason = if reason == "scheduled" {
        models::SyncReason::Scheduled
    } else {
        models::SyncReason::Manual
    };
    generate_paper(state.inner(), sync_reason, None)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn disconnect_x(state: tauri::State<'_, AppState>) -> Result<BootstrapState, String> {
    XClient::default()
        .disconnect()
        .await
        .map_err(|error| error.to_string())?;
    state
        .db
        .clear_x_connection()
        .map_err(|error| error.to_string())?;
    state.db.load_bootstrap().map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<BootstrapState, String> {
    state
        .db
        .delete_run(&run_id)
        .map_err(|error| error.to_string())?;
    state.db.load_bootstrap().map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_all_editions(state: tauri::State<'_, AppState>) -> Result<BootstrapState, String> {
    state
        .db
        .delete_all_editions()
        .map_err(|error| error.to_string())?;
    state.db.load_bootstrap().map_err(|error| error.to_string())
}

#[tauri::command]
async fn open_external_url(url: String) -> Result<(), String> {
    let parsed = Url::parse(&url).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only http and https links can be opened externally.".into());
    }

    webbrowser::open(parsed.as_str()).map_err(|error| error.to_string())?;
    Ok(())
}

fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, AppError> {
    app.path()
        .app_local_data_dir()
        .map_err(|error| AppError::Message(error.to_string()))
}

fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".into());

        let payload = if let Some(message) = panic_info.payload().downcast_ref::<&str>() {
            (*message).to_string()
        } else if let Some(message) = panic_info.payload().downcast_ref::<String>() {
            message.clone()
        } else {
            "non-string panic payload".into()
        };

        let message = format!("Unhandled panic at {location}: {payload}");
        eprintln!("{message}");
        log::error!("{message}");
        default_hook(panic_info);
    }));
}

fn build_tray(app: &tauri::AppHandle, state: &AppState) -> Result<(), AppError> {
    let show = MenuItem::with_id(app, "show", "Show SIFT", true, None::<&str>)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let sync = MenuItem::with_id(app, "sync", "Publish now", true, None::<&str>)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)
        .map_err(|error| AppError::Message(error.to_string()))?;

    let menu = Menu::with_items(app, &[&show, &sync, &quit])
        .map_err(|error| AppError::Message(error.to_string()))?;

    let mut builder = TrayIconBuilder::with_id("sift-tray")
        .menu(&menu)
        .on_menu_event({
            let app = app.clone();
            let state = state.clone();
            move |_app, event| match event.id.as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "sync" => {
                    let state = state.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = generate_paper(&state, models::SyncReason::Manual, None).await;
                    });
                }
                "quit" => {
                    state.quit_requested.store(true, Ordering::SeqCst);
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event({
            let app = app.clone();
            move |_tray, event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder
        .build(app)
        .map_err(|error| AppError::Message(error.to_string()))?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .clear_targets()
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("sift".into()),
                    },
                ))
                .level(tauri_plugin_log::log::LevelFilter::Info)
                .filter(|metadata| metadata.target().starts_with("sift"))
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(5))
                .max_file_size(512 * 1024)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            let base_dir = data_dir(&app_handle)?;
            let db = Database::new(base_dir.join("data").join("sift.sqlite"))?;
            let state = AppState::new(app_handle.clone(), db);
            install_panic_hook();
            log::info!("SIFT starting up");
            build_tray(&app_handle, &state)?;
            app.manage(state.clone());
            if let Err(error) = restore_x_session_window(&state) {
                eprintln!("failed to restore X session window: {error}");
                log::error!("failed to restore X session window: {error}");
            }
            if let Err(error) = restore_linkedin_session_window(&state) {
                eprintln!("failed to restore LinkedIn session window: {error}");
                log::error!("failed to restore LinkedIn session window: {error}");
            }
            if let Err(error) = restore_reddit_session_window(&state) {
                eprintln!("failed to restore Reddit session window: {error}");
                log::error!("failed to restore Reddit session window: {error}");
            }
            schedule_session_windows_hidden_on_startup(app_handle.clone());

            tauri::async_runtime::spawn(run_scheduler(state.clone()));
            tauri::async_runtime::spawn(async move {
                let _ = maybe_run_scheduled_sync(&state).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if let Some(state) = window.try_state::<AppState>() {
                    if window.label() == "main" {
                        if !state.quit_requested.load(Ordering::SeqCst) {
                            api.prevent_close();
                            let _ = window.hide();
                        }
                    } else if window.label() == X_SESSION_WINDOW_LABEL
                        && !state.quit_requested.load(Ordering::SeqCst)
                        && !state.x_session_force_close.load(Ordering::SeqCst)
                    {
                        api.prevent_close();
                        let _ = window.hide();
                    } else if window.label() == LINKEDIN_SESSION_WINDOW_LABEL
                        && !state.quit_requested.load(Ordering::SeqCst)
                        && !state.linkedin_session_force_close.load(Ordering::SeqCst)
                    {
                        api.prevent_close();
                        let _ = window.hide();
                    } else if window.label() == REDDIT_SESSION_WINDOW_LABEL
                        && !state.quit_requested.load(Ordering::SeqCst)
                        && !state.reddit_session_force_close.load(Ordering::SeqCst)
                    {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
            tauri::WindowEvent::Destroyed => {
                if window.label() == X_SESSION_WINDOW_LABEL {
                    if let Some(state) = window.try_state::<AppState>() {
                        let state = state.inner().clone();
                        state.x_session_force_close.store(false, Ordering::SeqCst);
                        tauri::async_runtime::spawn(async move {
                            state.clear_x_session_runtime().await;
                        });
                    }
                } else if window.label() == LINKEDIN_SESSION_WINDOW_LABEL {
                    if let Some(state) = window.try_state::<AppState>() {
                        let state = state.inner().clone();
                        state
                            .linkedin_session_force_close
                            .store(false, Ordering::SeqCst);
                        tauri::async_runtime::spawn(async move {
                            state.clear_linkedin_session_runtime().await;
                        });
                    }
                } else if window.label() == REDDIT_SESSION_WINDOW_LABEL {
                    if let Some(state) = window.try_state::<AppState>() {
                        let state = state.inner().clone();
                        state
                            .reddit_session_force_close
                            .store(false, Ordering::SeqCst);
                        tauri::async_runtime::spawn(async move {
                            state.clear_reddit_session_runtime().await;
                        });
                    }
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_bootstrap_state,
            save_settings,
            get_x_session_state,
            get_linkedin_session_state,
            get_reddit_session_state,
            open_x_session_window,
            open_linkedin_session_window,
            open_reddit_session_window,
            close_x_session_window,
            close_linkedin_session_window,
            close_reddit_session_window,
            hide_x_session_window,
            hide_linkedin_session_window,
            hide_reddit_session_window,
            logout_x_session_window,
            logout_linkedin_session_window,
            logout_reddit_session_window,
            verify_lm_studio,
            start_x_connect,
            poll_x_connect,
            submit_x_feed_capture,
            submit_linkedin_feed_capture,
            submit_reddit_feed_capture,
            submit_x_feed_capture_progress,
            submit_linkedin_feed_capture_progress,
            submit_reddit_feed_capture_progress,
            run_sync,
            disconnect_x,
            delete_run,
            delete_all_editions,
            open_external_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running SIFT");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_auth_popup_detection_covers_blank_google_fallback() {
        let home = Url::parse("https://x.com/home").expect("valid x url");
        let login = Url::parse("https://x.com/i/flow/login").expect("valid x login url");
        let blank = Url::parse("about:blank").expect("valid blank url");

        assert!(should_close_x_auth_popup_for_url(&home));
        assert!(!should_close_x_auth_popup_for_url(&login));
        assert!(should_close_x_auth_popup_for_url(&blank));
    }

    #[test]
    fn x_auth_popup_labels_stay_scoped_to_google_auth_windows() {
        assert!(is_x_auth_popup_label("x-auth-popup-123"));
        assert!(is_x_session_related_label("x-auth-popup-123"));
        assert!(is_x_session_related_label("x-session-popup-123"));
        assert!(!is_x_auth_popup_label("x-session-popup-123"));
        assert!(!is_x_auth_popup_label(X_SESSION_WINDOW_LABEL));
    }
}
