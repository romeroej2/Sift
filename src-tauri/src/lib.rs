mod db;
mod models;
mod services;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use db::Database;
use models::{
    BootstrapState, OAuthSession, PersistedXSession, PollStatus, UserSettings, XClientConfigDraft,
    XConnectLaunch, XConnectPayload, XConnectPollResult, XSessionState,
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

    async fn x_session_state(&self) -> XSessionState {
        let window = self.app.get_webview_window(X_SESSION_WINDOW_LABEL);
        let is_open = window.is_some();
        let is_visible = window
            .as_ref()
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        let is_authenticated = if is_open {
            *self.x_session_authenticated.read().await
        } else {
            false
        };
        let last_known_url = if is_open {
            self.x_session_last_known_url.read().await.clone()
        } else {
            None
        };

        XSessionState {
            is_open,
            is_visible,
            is_authenticated,
            last_known_url,
            mode: "native-webview".into(),
        }
    }

    async fn remember_x_session(
        &self,
        last_known_url: String,
        is_authenticated: bool,
    ) -> Result<(), AppError> {
        *self.x_session_last_known_url.write().await = Some(last_known_url.clone());
        *self.x_session_authenticated.write().await = is_authenticated;
        self.db.save_persisted_x_session(&PersistedXSession {
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
}

const X_SESSION_WINDOW_LABEL: &str = "x-session";
const X_SESSION_POPUP_LABEL_PREFIX: &str = "x-session-popup";
const X_AUTH_POPUP_LABEL_PREFIX: &str = "x-auth-popup";
const X_SESSION_HOME_URL: &str = "https://x.com/home";
const X_SESSION_DATA_STORE_ID: [u8; 16] = *b"SIFTXSESSION0001";
const X_SESSION_BRIDGE_SCRIPT: &str = r#"
if (
  ["x.com", "www.x.com", "twitter.com", "www.twitter.com"].includes(window.location.hostname)
  && !window.__SIFT_COLLECT_FEED__
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
    const selectors = ['a[href*="/photo/"] img', '[data-testid="tweetPhoto"] img'];

    Array.from(article.querySelectorAll(selectors.join(","))).forEach((img) => {
      const src = img.currentSrc || img.getAttribute("src") || "";
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
        if (!photoHref.includes("/photo/") && !img.closest('[data-testid="tweetPhoto"]')) {
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

    if (!document.getElementById("sift-x-session-controls-style")) {
      const style = document.createElement("style");
      style.id = "sift-x-session-controls-style";
      style.textContent = `
        #sift-x-session-controls {
          position: fixed;
          right: max(16px, env(safe-area-inset-right));
          bottom: max(16px, env(safe-area-inset-bottom));
          display: inline-flex;
          align-items: center;
          gap: 8px;
          padding: 10px;
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
          width: 36px;
          height: 36px;
          border: 0;
          border-radius: 999px;
          display: inline-flex;
          align-items: center;
          justify-content: center;
          background: rgba(255, 255, 255, 0.08);
          color: inherit;
          cursor: pointer;
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
          width: 18px;
          height: 18px;
          pointer-events: none;
        }
      `;
      (document.head || document.documentElement).appendChild(style);
    }

    const attach = () => {
      if (!document.body) {
        window.setTimeout(attach, 120);
        return;
      }

      if (document.getElementById("sift-x-session-controls")) {
        return;
      }

      const dock = document.createElement("div");
      dock.id = "sift-x-session-controls";

      const badge = document.createElement("span");
      badge.className = "sift-x-session-controls__badge";
      badge.textContent = "SIFT";
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

          if (command === "hide_x_session_window") {
            try {
              window.close();
            } catch {
              // Fall through to the Tauri invoke fallback below.
            }

            window.setTimeout(() => {
              void window.__TAURI_INTERNALS__.invoke(command, {}).catch((error) => {
                console.error("[SIFT] X session hide failed.", error);
              });
            }, 32);
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

      document.body.appendChild(dock);
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

  window.__SIFT_COLLECT_FEED__ = async (requestId, options = {}) => {
    try {
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
      const maxPasses = Math.max(Number(options.maxPasses) || 120, 40);
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

        const visibleTweets = Array.from(document.querySelectorAll(selector));
        const visibleIdsBefore = siftVisibleTweetIds(selector);
        const metricsBefore = siftReadScrollMetrics(scroller);
        const scrollStep = Math.max(metricsBefore.clientHeight * 0.9, 1200);
        const lastVisibleTweet = visibleTweets[visibleTweets.length - 1];

        if (lastVisibleTweet && typeof lastVisibleTweet.scrollIntoView === "function") {
          lastVisibleTweet.scrollIntoView({ block: "end" });
          await siftWait(300);
        }

        const siftHeartbeat = async () => {
          await siftReportProgress(pass + 1, collected.size, siftCountFreshItems());
        };

        siftScrollFeedTo(scroller, metricsBefore.top + scrollStep);
        let advanced = await siftWaitForFeedAdvance(
          selector,
          scroller,
          visibleIdsBefore,
          metricsBefore.height,
          waitForAdvanceMs,
          siftHeartbeat,
        );

        if (!advanced) {
          siftScrollFeedTo(scroller, metricsBefore.top + (scrollStep * 2));
          advanced = await siftWaitForFeedAdvance(
            selector,
            scroller,
            visibleIdsBefore,
            metricsBefore.height,
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
"#;

fn is_x_domain(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
    )
}

fn is_google_auth_url(url: &Url) -> bool {
    matches!(url.host_str(), Some("accounts.google.com"))
}

fn is_x_session_related_label(label: &str) -> bool {
    label == X_SESSION_WINDOW_LABEL
        || label.starts_with(X_SESSION_POPUP_LABEL_PREFIX)
        || label.starts_with(X_AUTH_POPUP_LABEL_PREFIX)
}

fn x_session_window_labels(app: &tauri::AppHandle) -> Vec<String> {
    app.webview_windows()
        .into_keys()
        .filter(|label| is_x_session_related_label(label))
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

fn is_completed_x_session_url(url: &Url) -> bool {
    if !is_x_domain(url) {
        return false;
    }

    let path = url.path();
    !path.starts_with("/i/flow/login")
        && !path.starts_with("/login")
        && !path.starts_with("/account/access")
}

fn default_x_session_url() -> Url {
    Url::parse(X_SESSION_HOME_URL).expect("valid x home url")
}

fn resolve_x_session_launch_url(saved_url: Option<&str>) -> Url {
    saved_url
        .and_then(|raw| Url::parse(raw).ok())
        .filter(is_x_domain)
        .unwrap_or_else(default_x_session_url)
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

            if is_auth_popup && is_completed_x_session_url(payload.url()) {
                let window = window.clone();
                let parent_window = parent_app.get_webview_window(X_SESSION_WINDOW_LABEL);
                let popup_state = popup_state.clone();
                let completed_url = payload.url().to_string();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = popup_state.remember_x_session(completed_url, true).await {
                        eprintln!("failed to persist X auth popup state: {error}");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                    if let Some(parent_window) = parent_window {
                        let _ = parent_window.navigate(default_x_session_url());
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
    .on_page_load(move |_window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            let page_state = page_state.clone();
            let url = payload.url().to_string();
            let is_authenticated = is_completed_x_session_url(payload.url());
            tauri::async_runtime::spawn(async move {
                if let Err(error) = page_state.remember_x_session(url, is_authenticated).await {
                    eprintln!("failed to persist X session page state: {error}");
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
    state.db.save_persisted_x_session(&PersistedXSession {
        last_known_url: initial_url_string,
        is_authenticated: saved_session.is_authenticated,
    })?;
    build_x_session_window(&state.app, state.clone(), initial_url, false, false)
        .map_err(AppError::Message)?;
    Ok(())
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
async fn get_x_session_state(state: tauri::State<'_, AppState>) -> Result<XSessionState, String> {
    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn open_x_session_window(state: tauri::State<'_, AppState>) -> Result<XSessionState, String> {
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
async fn close_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<XSessionState, String> {
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

#[tauri::command]
async fn hide_x_session_window(state: tauri::State<'_, AppState>) -> Result<XSessionState, String> {
    if let Some(window) = state.app.get_webview_window(X_SESSION_WINDOW_LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }

    Ok(state.x_session_state().await)
}

#[tauri::command]
async fn logout_x_session_window(
    state: tauri::State<'_, AppState>,
) -> Result<XSessionState, String> {
    logout_x_session(state.inner()).await?;
    Ok(state.x_session_state().await)
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
async fn submit_x_feed_capture_progress(
    state: tauri::State<'_, AppState>,
    progress: XSessionCaptureProgressPayload,
) -> Result<(), String> {
    let (run_id, reason) = {
        let mut requests = state.x_session_capture_requests.lock().await;
        let request = requests.get_mut(&progress.request_id).ok_or_else(|| {
            "The feed capture request expired before the page reported progress.".to_string()
        })?;
        request.last_progress_at = Instant::now();
        request.latest_progress = Some(progress.snapshot());
        (request.run_id.clone(), request.reason.clone())
    };

    emit_capture_progress(state.inner(), &run_id, &reason, &progress);
    Ok(())
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
    generate_paper(state.inner(), sync_reason)
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
                        let _ = generate_paper(&state, models::SyncReason::Manual).await;
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
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .setup(|app| {
            let app_handle = app.handle().clone();
            let base_dir = data_dir(&app_handle)?;
            let db = Database::new(base_dir.join("data").join("sift.sqlite"))?;
            let state = AppState::new(app_handle.clone(), db);
            build_tray(&app_handle, &state)?;
            app.manage(state.clone());
            if let Err(error) = restore_x_session_window(&state) {
                eprintln!("failed to restore X session window: {error}");
            }

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
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            get_bootstrap_state,
            save_settings,
            get_x_session_state,
            open_x_session_window,
            close_x_session_window,
            hide_x_session_window,
            logout_x_session_window,
            verify_lm_studio,
            start_x_connect,
            poll_x_connect,
            submit_x_feed_capture,
            submit_x_feed_capture_progress,
            run_sync,
            disconnect_x,
            open_external_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running SIFT");
}
