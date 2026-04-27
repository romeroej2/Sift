use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use regex::Regex;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};
use url::Url;
use uuid::Uuid;

use crate::models::{
    CaptureBrowsePageCount, CleanedItem, CodexHealth, CodexSettings, CodexUsage, Edition,
    EditionCard, EditionImage, EditionSection, EditionView, FeedItem, LmStudioHealth, ModelBackend,
    ModelDescriptor, OAuthSession, PollStatus, ScheduleCadence, ScheduleRule, SyncReason, SyncRun,
    SyncRunTimings, SyncStatus, UserSettings, XClientConfigDraft, XConnectLaunch, XConnectPayload,
    XConnectPollResult,
};
use crate::{AppError, AppState, is_linkedin_domain, is_reddit_domain, is_x_domain};

fn machine_timezone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XSessionCapturePayload {
    pub request_id: String,
    pub current_url: String,
    pub items: Vec<XSessionCaptureItem>,
    pub error: Option<String>,
    #[serde(default)]
    pub completed_passes: Option<usize>,
    #[serde(default)]
    pub total_passes: Option<usize>,
    #[serde(default)]
    pub ended_early: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XSessionCaptureProgressPayload {
    pub request_id: String,
    pub current_url: String,
    pub pass: usize,
    pub total_passes: usize,
    pub item_count: usize,
    pub fresh_count: usize,
    pub stable_passes: usize,
    pub exhausted_passes: usize,
    pub boundary_passes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XSessionCaptureItem {
    pub id: String,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub source_url: String,
    pub posted_at: String,
    pub is_repost: bool,
    pub is_reply: bool,
    #[serde(default)]
    pub is_promoted: bool,
    pub social_context: Option<String>,
    pub shared_urls: Vec<String>,
    #[serde(default)]
    pub media: Vec<XSessionCaptureMedia>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XSessionCaptureMedia {
    pub url: String,
    #[serde(default = "default_capture_media_kind")]
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct XSessionCaptureProgressSnapshot {
    pub pass: usize,
    pub total_passes: usize,
    pub item_count: usize,
    pub fresh_count: usize,
    pub current_url: String,
}

#[derive(Debug)]
pub struct XSessionCaptureRequest {
    pub run_id: String,
    pub reason: SyncReason,
    pub source_label: String,
    pub sender: Option<oneshot::Sender<Result<XSessionCapturePayload, String>>>,
    pub last_progress_at: Instant,
    pub latest_progress: Option<XSessionCaptureProgressSnapshot>,
}

impl XSessionCaptureProgressPayload {
    pub fn snapshot(&self) -> XSessionCaptureProgressSnapshot {
        XSessionCaptureProgressSnapshot {
            pass: self.pass,
            total_passes: self.total_passes,
            item_count: self.item_count,
            fresh_count: self.fresh_count,
            current_url: self.current_url.clone(),
        }
    }
}

const SYNC_PROGRESS_EVENT: &str = "sync-progress";
const CAPTURE_MAX_ITEMS: usize = 400;
const CAPTURE_TARGET_FRESH_ITEMS: usize = 200;
const CAPTURE_STABLE_PASSES: usize = 10;
const CAPTURE_EXHAUSTED_PASSES: usize = 18;
const CAPTURE_WAIT_FOR_ADVANCE_MS: u64 = 5_000;
const CAPTURE_TIMEOUT_SECS: u64 = 480;
const CAPTURE_IDLE_TIMEOUT_SECS: u64 = 90;
const LM_BATCH_SIZE: usize = 6;
const LM_BATCH_MAX_ATTEMPTS: usize = 3;
const LM_STUDIO_REQUEST_TIMEOUT_SECS: u64 = 15;
const LM_STUDIO_COMPLETION_TIMEOUT_SECS: u64 = 600;
const LM_STUDIO_IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;
const CODEX_VERIFY_TIMEOUT_SECS: u64 = 15;
const CODEX_COMPLETION_TIMEOUT_SECS: u64 = 900;
const MAX_DIGEST_ITEMS: usize = 12;

fn default_capture_media_kind() -> String {
    "photo".into()
}

#[derive(Debug, Clone)]
struct CaptureBoundary {
    edition_date: String,
    since_timestamp: Option<String>,
}

#[derive(Debug, Clone)]
struct CaptureOutcome {
    items: Vec<FeedItem>,
    brand_new_count: usize,
    resurfaced_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSourceKind {
    X,
    Linkedin,
    Reddit,
}

#[derive(Debug, Clone)]
struct MultiCaptureOutcome {
    items: Vec<FeedItem>,
    brand_new_count: usize,
    enabled_sources: Vec<CaptureSourceKind>,
}

#[derive(Debug, Clone)]
struct ViewBuildSpec {
    view: EditionView,
    label: &'static str,
    items: Vec<FeedItem>,
}

struct BuiltView {
    index: usize,
    view: EditionView,
    decisions: Vec<ClusterEditorialRecord>,
    kept_count: usize,
    edition: Edition,
    ranking_ms: u64,
    front_page_ms: u64,
    codex_usage: CodexUsage,
}

#[derive(Debug, Clone)]
pub(crate) struct ScheduledRunContext {
    rule_id: String,
    rule_label: String,
    slot_key: String,
    browse_page_count: CaptureBrowsePageCount,
}

impl CaptureBoundary {
    fn collector_label(&self) -> &'static str {
        if self.since_timestamp.is_some() {
            "since the last edition"
        } else {
            "for the current edition day"
        }
    }

    fn digest_label(&self) -> &'static str {
        if self.since_timestamp.is_some() {
            "since the last saved edition"
        } else {
            "for today"
        }
    }
}

impl CaptureSourceKind {
    fn as_feed_source(self) -> &'static str {
        match self {
            Self::X => "x-session",
            Self::Linkedin => "linkedin-session",
            Self::Reddit => "reddit-session",
        }
    }

    fn as_label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Linkedin => "LinkedIn",
            Self::Reddit => "Reddit",
        }
    }

    fn as_edition_view(self) -> EditionView {
        match self {
            Self::X => EditionView::X,
            Self::Linkedin => EditionView::Linkedin,
            Self::Reddit => EditionView::Reddit,
        }
    }
}

fn source_view_index(enabled_sources: &[CaptureSourceKind], source: CaptureSourceKind) -> usize {
    let offset = usize::from(enabled_sources.len() > 1);
    enabled_sources
        .iter()
        .position(|candidate| *candidate == source)
        .map(|index| index + offset)
        .unwrap_or(offset)
}

#[derive(Debug, Clone)]
pub(crate) struct TweetCluster {
    id: String,
    representative: FeedItem,
    members: Vec<FeedItem>,
    shared_urls: HashSet<String>,
    keywords: HashSet<String>,
}

impl TweetCluster {
    fn repeat_count(&self) -> usize {
        self.members.len()
    }

    fn unique_author_count(&self) -> usize {
        self.members
            .iter()
            .map(|item| item.author_handle.to_lowercase())
            .collect::<HashSet<_>>()
            .len()
    }

    fn signal_score(&self) -> usize {
        (self.repeat_count() * 3)
            + (self.unique_author_count() * 2)
            + usize::from(!self.shared_urls.is_empty())
    }

    fn shared_url_list(&self) -> Vec<String> {
        let mut values = self.shared_urls.iter().cloned().collect::<Vec<_>>();
        values.sort();
        values
    }

    fn keyword_list(&self) -> Vec<String> {
        let mut values = self.keywords.iter().cloned().collect::<Vec<_>>();
        values.sort();
        values.truncate(6);
        values
    }
}

#[derive(Debug, Clone)]
struct ClusterEditorialRecord {
    cluster: TweetCluster,
    decision: ClusterDecision,
}

impl ClusterEditorialRecord {
    fn signal_score(&self) -> usize {
        self.cluster.signal_score()
    }

    fn to_cleaned_item(&self) -> CleanedItem {
        self.decision.clone().into_cleaned(&self.cluster)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct FeedMedia {
    url: String,
    kind: String,
}

#[derive(Debug, Clone)]
struct DownloadedImage {
    source_url: String,
    mime_type: String,
    bytes: Vec<u8>,
    data_url: String,
}

impl DownloadedImage {
    fn extension(&self) -> &'static str {
        match self.mime_type.as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct SyncImageCache {
    entries: HashMap<String, Result<DownloadedImage, String>>,
}

impl SyncImageCache {
    async fn get_or_fetch(
        &mut self,
        client: &reqwest::Client,
        url: &str,
    ) -> Option<DownloadedImage> {
        if let Some(entry) = self.entries.get(url) {
            return entry.clone().ok();
        }

        let result = download_image_asset(client, url)
            .await
            .map_err(|error| error.to_string());
        self.entries.insert(url.to_string(), result.clone());
        result.ok()
    }
}

#[derive(Debug)]
pub(crate) struct StructuredGenerationOutcome {
    decisions: Vec<ClusterDecision>,
    fell_back_to_text: bool,
    usage: CodexUsage,
}

#[derive(Debug)]
pub(crate) struct TextGenerationOutcome {
    text: String,
    usage: CodexUsage,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncProgressEvent {
    run_id: String,
    reason: String,
    status: SyncStatus,
    stage: String,
    message: String,
    item_count: Option<usize>,
    new_item_count: Option<usize>,
    kept_count: Option<usize>,
    edition_id: Option<String>,
    timestamp: String,
}

fn emit_sync_progress(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    status: SyncStatus,
    stage: &str,
    message: impl Into<String>,
    item_count: Option<usize>,
    new_item_count: Option<usize>,
    kept_count: Option<usize>,
    edition_id: Option<&str>,
) {
    let message = message.into();
    let payload = SyncProgressEvent {
        run_id: run_id.to_string(),
        reason: reason.as_str().to_string(),
        status,
        stage: stage.to_string(),
        message: message.clone(),
        item_count,
        new_item_count,
        kept_count,
        edition_id: edition_id.map(ToOwned::to_owned),
        timestamp: Utc::now().to_rfc3339(),
    };

    let metrics = [
        item_count.map(|count| format!("{count} captured")),
        new_item_count.map(|count| format!("{count} new")),
        kept_count.map(|count| format!("{count} kept")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");

    if metrics.is_empty() {
        println!("[sift-sync:{run_id}] {stage}: {message}");
    } else {
        println!("[sift-sync:{run_id}] {stage}: {message} ({metrics})");
    }

    if let Err(error) = state.app.emit(SYNC_PROGRESS_EVENT, &payload) {
        eprintln!("[sift-sync:{run_id}] failed to emit progress event: {error}");
    }
}

fn format_capture_progress_message(
    source_label: &str,
    progress: &XSessionCaptureProgressPayload,
) -> String {
    let mut message = if progress.fresh_count > 0 {
        format!(
            "Collecting posts from the live {source_label} session. Pass {}/{} · {} fresh so far.",
            progress.pass, progress.total_passes, progress.fresh_count
        )
    } else {
        format!(
            "Collecting posts from the live {source_label} session. Pass {}/{} · still scanning for fresh posts.",
            progress.pass, progress.total_passes
        )
    };

    let mut notes = Vec::new();
    if progress.boundary_passes > 0 {
        notes.push(format!("boundary {}/3", progress.boundary_passes));
    }
    if progress.stable_passes > 0 {
        notes.push(format!("stable {}/10", progress.stable_passes));
    }
    if progress.exhausted_passes > 0 {
        notes.push(format!("idle {}/18", progress.exhausted_passes));
    }

    if !notes.is_empty() {
        let _ = write!(message, " [{}]", notes.join(" · "));
    }

    message
}

fn format_capture_total_timeout_message(
    source_label: &str,
    progress: Option<&XSessionCaptureProgressSnapshot>,
) -> String {
    if let Some(progress) = progress {
        format!(
            "Timed out waiting for the live {source_label} session to finish collecting feed items after pass {}/{}. Last heartbeat: {} captured, {} fresh at {}.",
            progress.pass,
            progress.total_passes,
            progress.item_count,
            progress.fresh_count,
            progress.current_url
        )
    } else {
        format!("Timed out waiting for the live {source_label} session to return feed items.")
    }
}

fn format_capture_idle_timeout_message(
    source_label: &str,
    progress: Option<&XSessionCaptureProgressSnapshot>,
) -> String {
    if let Some(progress) = progress {
        format!(
            "The live {source_label} session stopped reporting progress after pass {}/{}. Last heartbeat: {} captured, {} fresh at {}.",
            progress.pass,
            progress.total_passes,
            progress.item_count,
            progress.fresh_count,
            progress.current_url
        )
    } else {
        format!(
            "Timed out waiting for the live {source_label} session to start returning feed items."
        )
    }
}

pub(crate) fn emit_capture_progress(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    source_label: &str,
    progress: &XSessionCaptureProgressPayload,
) {
    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "capturing-feed",
        format_capture_progress_message(source_label, progress),
        Some(progress.item_count),
        Some(progress.fresh_count),
        None,
        None,
    );
}

#[async_trait]
pub(crate) trait FeedSource {
    async fn connect(&self, _config: &XClientConfigDraft) -> Result<(), AppError>;
    async fn disconnect(&self) -> Result<(), AppError>;
}

#[async_trait]
pub(crate) trait LocalModelProvider {
    fn label(&self) -> &'static str;
    async fn health_check(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<LmStudioHealth, AppError>;
    async fn list_models(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<ModelDescriptor>, AppError>;
    async fn generate_structured(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        clusters: &[TweetCluster],
        image_cache: &mut SyncImageCache,
    ) -> Result<StructuredGenerationOutcome, AppError>;
    async fn generate_text(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        prompt: &str,
    ) -> Result<TextGenerationOutcome, AppError>;
}

#[derive(Clone)]
pub struct XClient {
    http: reqwest::Client,
    api_base: String,
    auth_base: String,
}

impl Default for XClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(25))
                .build()
                .expect("x client"),
            api_base: "https://api.x.com".into(),
            auth_base: "https://x.com".into(),
        }
    }
}

#[async_trait]
impl FeedSource for XClient {
    async fn connect(&self, config: &XClientConfigDraft) -> Result<(), AppError> {
        if config.client_id.trim().is_empty() {
            return Err(AppError::Message(
                "Enter your X app client ID before connecting SIFT.".into(),
            ));
        }

        Ok(())
    }

    async fn disconnect(&self) -> Result<(), AppError> {
        Ok(())
    }
}

impl XClient {
    pub async fn start_connect(
        &self,
        config: &XClientConfigDraft,
    ) -> Result<(OAuthSession, XConnectLaunch), AppError> {
        let port = 45457;
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let state = Uuid::new_v4().to_string();
        let code_verifier = Uuid::new_v4().to_string().replace('-', "");
        let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(code_verifier.as_bytes()));
        let scope = "tweet.read users.read offline.access";
        let authorize_url = Url::parse_with_params(
            &format!("{}/i/oauth2/authorize", self.auth_base),
            &[
                ("response_type", "code"),
                ("client_id", config.client_id.trim()),
                ("redirect_uri", redirect_uri.as_str()),
                ("scope", scope),
                ("state", state.as_str()),
                ("code_challenge", code_challenge.as_str()),
                ("code_challenge_method", "S256"),
            ],
        )?;

        Ok((
            OAuthSession {
                state: state.clone(),
                client_id: config.client_id.trim().into(),
                client_secret: if config.client_secret.trim().is_empty() {
                    None
                } else {
                    Some(config.client_secret.trim().into())
                },
                code_verifier,
                redirect_uri: redirect_uri.clone(),
                created_at: Utc::now(),
                result: Some(XConnectPollResult {
                    status: PollStatus::Pending,
                    error_message: None,
                    payload: None,
                }),
            },
            XConnectLaunch {
                authorize_url: authorize_url.to_string(),
                redirect_uri,
                state,
            },
        ))
    }

    pub fn spawn_callback_listener(&self, state: AppState, session_state: String) {
        let client = self.clone();

        thread::spawn(move || {
            let port = 45457;
            let listener = match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => listener,
                Err(error) => {
                    state.set_oauth_error(&session_state, error.to_string());
                    return;
                }
            };

            let _ = listener.set_nonblocking(false);
            let _ = listener.set_ttl(30);

            match listener.accept() {
                Ok((mut stream, _)) => {
                    use std::io::{Read, Write};

                    let mut buffer = [0_u8; 8192];
                    let read = match stream.read(&mut buffer) {
                        Ok(read) => read,
                        Err(error) => {
                            state.set_oauth_error(&session_state, error.to_string());
                            return;
                        }
                    };

                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let first_line = request.lines().next().unwrap_or_default();
                    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

                    let response_html = "<html><body style=\"font-family: sans-serif; padding: 24px;\"><h1>SIFT connected</h1><p>You can close this tab and return to the app.</p></body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                        response_html.len(),
                        response_html
                    );
                    let _ = stream.write_all(response.as_bytes());

                    let result = (|| -> Result<(String, String), AppError> {
                        let url = Url::parse(&format!("http://localhost{}", path))?;
                        let code = url
                            .query_pairs()
                            .find(|(key, _)| key == "code")
                            .map(|(_, value)| value.to_string())
                            .ok_or_else(|| {
                                AppError::Message("X did not return an authorization code.".into())
                            })?;
                        let returned_state = url
                            .query_pairs()
                            .find(|(key, _)| key == "state")
                            .map(|(_, value)| value.to_string())
                            .ok_or_else(|| AppError::Message("X did not return state.".into()))?;
                        Ok((code, returned_state))
                    })();

                    match result {
                        Ok((code, returned_state)) => {
                            let rt = tokio::runtime::Runtime::new().expect("oauth runtime");
                            let finish = rt.block_on(async {
                                client
                                    .finish_connect(
                                        state.clone(),
                                        &session_state,
                                        &returned_state,
                                        &code,
                                    )
                                    .await
                            });
                            if let Err(error) = finish {
                                state.set_oauth_error(&session_state, error.to_string());
                            }
                        }
                        Err(error) => state.set_oauth_error(&session_state, error.to_string()),
                    }
                }
                Err(error) => state.set_oauth_error(&session_state, error.to_string()),
            }
        });
    }

    async fn finish_connect(
        &self,
        state: AppState,
        expected_state: &str,
        returned_state: &str,
        code: &str,
    ) -> Result<(), AppError> {
        if expected_state != returned_state {
            return Err(AppError::Message(
                "State mismatch during X authorization.".into(),
            ));
        }

        let session = state
            .get_oauth_session(expected_state)
            .await
            .ok_or_else(|| AppError::Message("OAuth session expired.".into()))?;

        let token = self
            .exchange_code(
                &session.client_id,
                session.client_secret.as_deref(),
                &session.code_verifier,
                &session.redirect_uri,
                code,
            )
            .await?;
        let me = self.fetch_current_user(&token.access_token).await?;

        state.set_oauth_success(
            expected_state,
            XConnectPayload {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                user_id: me.user_id,
                handle: me.handle,
                name: me.name,
            },
        );
        Ok(())
    }

    async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: Option<&str>,
        code_verifier: &str,
        redirect_uri: &str,
        code: &str,
    ) -> Result<TokenEnvelope, AppError> {
        let token_url = format!("{}/2/oauth2/token", self.api_base);
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
            ("client_id", client_id),
        ];

        if let Some(client_secret) = client_secret {
            params.push(("client_secret", client_secret));
        }

        let response = self
            .http
            .post(token_url)
            .form(&params)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<TokenEnvelope>().await?)
    }

    async fn fetch_current_user(&self, access_token: &str) -> Result<XMeEnvelope, AppError> {
        let response = self
            .http
            .get(format!(
                "{}/2/users/me?user.fields=name,username",
                self.api_base
            ))
            .header(AUTHORIZATION, bearer(access_token))
            .send()
            .await?
            .error_for_status()?;

        let envelope = response.json::<XMeResponse>().await?;
        Ok(XMeEnvelope {
            user_id: envelope.data.id,
            handle: envelope.data.username,
            name: envelope.data.name,
        })
    }
}

#[derive(Clone)]
pub struct LmStudioClient {
    http: reqwest::Client,
}

impl Default for LmStudioClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(LM_STUDIO_REQUEST_TIMEOUT_SECS))
                .build()
                .expect("lm studio client"),
        }
    }
}

#[async_trait]
impl LocalModelProvider for LmStudioClient {
    fn label(&self) -> &'static str {
        "LM Studio"
    }

    async fn health_check(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<LmStudioHealth, AppError> {
        let models = self.list_models(base_url, auth_token).await?;
        Ok(LmStudioHealth {
            ok: true,
            server_label: format!("LM Studio @ {}", base_url.trim_end_matches('/')),
            message: if models.is_empty() {
                "LM Studio responded, but no models are available yet.".into()
            } else {
                format!("LM Studio is ready with {} model(s).", models.len())
            },
            models,
        })
    }

    async fn list_models(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<Vec<ModelDescriptor>, AppError> {
        let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
        let mut request = self.http.get(url);
        if let Some(auth_token) = auth_token {
            request = request.header(AUTHORIZATION, bearer(auth_token));
        }

        let response = request.send().await?.error_for_status()?;
        let payload = response.json::<LmModelList>().await?;
        Ok(payload
            .data
            .into_iter()
            .map(|model| ModelDescriptor {
                display_name: model.id.clone(),
                id: model.id,
                loaded: true,
            })
            .collect())
    }

    async fn generate_structured(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        clusters: &[TweetCluster],
        image_cache: &mut SyncImageCache,
    ) -> Result<StructuredGenerationOutcome, AppError> {
        let model =
            settings.lm_studio.selected_model.clone().ok_or_else(|| {
                AppError::Message("Select an LM Studio model before syncing.".into())
            })?;

        let prompt = build_structured_prompt(clusters);
        if settings.lm_studio.include_images {
            if let Some(parts) = self
                .build_multimodal_user_content(clusters, &prompt, image_cache)
                .await
            {
                match self
                    .chat_completion_with_parts(
                        &settings.lm_studio.base_url,
                        auth_token,
                        &model,
                        parts,
                        0.2,
                    )
                    .await
                {
                    Ok(content) => {
                        return Ok(StructuredGenerationOutcome {
                            decisions: parse_cluster_decisions(&content, clusters)?,
                            fell_back_to_text: false,
                            usage: CodexUsage::default(),
                        });
                    }
                    Err(error) => {
                        eprintln!(
                            "[sift] LM Studio rejected multimodal ranking input; retrying text-only: {error}"
                        );
                        let fallback = self
                            .chat_completion(
                                &settings.lm_studio.base_url,
                                auth_token,
                                &model,
                                &prompt,
                                0.2,
                            )
                            .await?;
                        return Ok(StructuredGenerationOutcome {
                            decisions: parse_cluster_decisions(&fallback, clusters)?,
                            fell_back_to_text: true,
                            usage: CodexUsage::default(),
                        });
                    }
                }
            }
        }

        let content = self
            .chat_completion(
                &settings.lm_studio.base_url,
                auth_token,
                &model,
                &prompt,
                0.2,
            )
            .await?;

        Ok(StructuredGenerationOutcome {
            decisions: parse_cluster_decisions(&content, clusters)?,
            fell_back_to_text: false,
            usage: CodexUsage::default(),
        })
    }

    async fn generate_text(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        prompt: &str,
    ) -> Result<TextGenerationOutcome, AppError> {
        let model =
            settings.lm_studio.selected_model.clone().ok_or_else(|| {
                AppError::Message("Select an LM Studio model before syncing.".into())
            })?;

        let text = self
            .chat_completion(
                &settings.lm_studio.base_url,
                auth_token,
                &model,
                prompt,
                0.3,
            )
            .await?;
        Ok(TextGenerationOutcome {
            text,
            usage: CodexUsage::default(),
        })
    }
}

#[derive(Clone, Default)]
pub struct CodexCliProvider;

#[async_trait]
impl LocalModelProvider for CodexCliProvider {
    fn label(&self) -> &'static str {
        "Codex"
    }

    async fn health_check(
        &self,
        base_url: &str,
        _auth_token: Option<&str>,
    ) -> Result<LmStudioHealth, AppError> {
        let health = verify_codex_command(base_url).await?;
        Ok(LmStudioHealth {
            ok: true,
            server_label: health.server_label,
            message: health.message,
            models: vec![ModelDescriptor {
                id: health.version.clone(),
                display_name: health.version,
                loaded: true,
            }],
        })
    }

    async fn list_models(
        &self,
        base_url: &str,
        _auth_token: Option<&str>,
    ) -> Result<Vec<ModelDescriptor>, AppError> {
        let health = verify_codex_command(base_url).await?;
        Ok(vec![ModelDescriptor {
            id: health.version.clone(),
            display_name: health.version,
            loaded: true,
        }])
    }

    async fn generate_structured(
        &self,
        settings: &UserSettings,
        _auth_token: Option<&str>,
        clusters: &[TweetCluster],
        image_cache: &mut SyncImageCache,
    ) -> Result<StructuredGenerationOutcome, AppError> {
        let prompt = build_structured_prompt(clusters);
        let schema_path = write_codex_ranking_schema()?;
        let mut image_paths = Vec::new();
        if settings.codex.include_images {
            image_paths = write_codex_image_attachments(clusters, image_cache).await?;
        }
        let output =
            run_codex_exec(&settings.codex, &prompt, Some(&schema_path), &image_paths).await;
        let _ = fs::remove_file(&schema_path);
        cleanup_codex_image_attachments(&image_paths);
        let output = output?;
        Ok(StructuredGenerationOutcome {
            decisions: parse_cluster_decisions(&output.content, clusters)?,
            fell_back_to_text: false,
            usage: output.usage,
        })
    }

    async fn generate_text(
        &self,
        settings: &UserSettings,
        _auth_token: Option<&str>,
        prompt: &str,
    ) -> Result<TextGenerationOutcome, AppError> {
        let output = run_codex_exec(&settings.codex, prompt, None, &[]).await?;
        Ok(TextGenerationOutcome {
            text: output.content,
            usage: output.usage,
        })
    }
}

pub async fn verify_codex_command(command: &str) -> Result<CodexHealth, AppError> {
    let command = resolve_codex_command(command)?;
    let output = timeout(
        Duration::from_secs(CODEX_VERIFY_TIMEOUT_SECS),
        command.command().arg("--version").output(),
    )
    .await
    .map_err(|_| {
        AppError::Message(format!(
            "Codex CLI check timed out after {CODEX_VERIFY_TIMEOUT_SECS} seconds."
        ))
    })?
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AppError::Message(format!("Codex command not found: {}", command.label))
        } else {
            AppError::Io(error)
        }
    })?;

    if !output.status.success() {
        return Err(AppError::Message(format!(
            "Codex CLI check failed with status {}: {}",
            output.status,
            truncate_chars(&String::from_utf8_lossy(&output.stderr), 240)
        )));
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = if version.is_empty() {
        "codex".into()
    } else {
        version
    };

    Ok(CodexHealth {
        ok: true,
        server_label: format!("Codex CLI @ {}", command.label),
        message: format!("Codex CLI is ready ({version})."),
        version,
    })
}

fn normalized_codex_command(command: &str) -> Result<String, AppError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(AppError::Message(
            "Enter a Codex command before verifying.".into(),
        ));
    }
    Ok(command.into())
}

#[derive(Debug, Clone)]
struct ResolvedCodexCommand {
    program: String,
    prefix_args: Vec<String>,
    label: String,
}

impl ResolvedCodexCommand {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.prefix_args);
        command
    }
}

fn resolve_codex_command(command: &str) -> Result<ResolvedCodexCommand, AppError> {
    let command = normalized_codex_command(command)?;
    let path = Path::new(&command);
    let candidates = if path.is_absolute() || command.contains('\\') || command.contains('/') {
        command_path_candidates(path)
    } else {
        codex_search_dirs()
            .into_iter()
            .flat_map(|dir| command_path_candidates(&dir.join(&command)))
            .collect::<Vec<_>>()
    };

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(resolved_codex_command_from_path(candidate));
        }
    }

    Err(AppError::Message(format!(
        "Codex command not found: {command}"
    )))
}

fn codex_search_dirs() -> Vec<PathBuf> {
    let mut dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();

    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(app_data).join("npm"));
        }
    }

    dirs
}

fn command_path_candidates(path: &Path) -> Vec<PathBuf> {
    if path.extension().is_some() {
        return vec![path.to_path_buf()];
    }

    #[cfg(windows)]
    {
        return ["cmd", "exe", "bat", "ps1", ""]
            .into_iter()
            .map(|extension| {
                if extension.is_empty() {
                    path.to_path_buf()
                } else {
                    path.with_extension(extension)
                }
            })
            .collect();
    }

    #[cfg(not(windows))]
    {
        vec![path.to_path_buf()]
    }
}

fn resolved_codex_command_from_path(path: PathBuf) -> ResolvedCodexCommand {
    let label = path.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
        {
            return ResolvedCodexCommand {
                program: "powershell".into(),
                prefix_args: vec![
                    "-NoProfile".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    label.clone(),
                ],
                label,
            };
        }
    }

    ResolvedCodexCommand {
        program: label.clone(),
        prefix_args: Vec::new(),
        label,
    }
}

fn codex_exec_args(
    settings: &CodexSettings,
    schema_path: Option<&Path>,
    image_paths: &[PathBuf],
) -> Vec<String> {
    let mut args = vec![
        "exec".into(),
        "--ephemeral".into(),
        "--sandbox".into(),
        "read-only".into(),
    ];

    if let Some(model) = settings
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        args.push("--model".into());
        args.push(model.into());
    }

    if let Some(profile) = settings
        .profile
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        args.push("--profile".into());
        args.push(profile.into());
    }

    if let Some(schema_path) = schema_path {
        args.push("--output-schema".into());
        args.push(schema_path.to_string_lossy().to_string());
    }

    for image_path in image_paths {
        args.push("--image".into());
        args.push(image_path.to_string_lossy().to_string());
    }

    args.push("-".into());
    args
}

async fn run_codex_exec(
    settings: &CodexSettings,
    prompt: &str,
    schema_path: Option<&Path>,
    image_paths: &[PathBuf],
) -> Result<CodexExecOutput, AppError> {
    let command = resolve_codex_command(&settings.command)?;
    let args = codex_exec_args(settings, schema_path, image_paths);
    let mut child = command
        .command()
        .args(&args)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AppError::Message(format!("Codex command not found: {}", command.label))
            } else {
                AppError::Io(error)
            }
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).await?;
    }

    let output = timeout(
        Duration::from_secs(CODEX_COMPLETION_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        AppError::Message(format!(
            "Codex is still generating after {CODEX_COMPLETION_TIMEOUT_SECS} seconds."
        ))
    })??;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        return Err(AppError::Message(format!(
            "Codex request failed with status {}: {}",
            output.status,
            truncate_chars(&String::from_utf8_lossy(&output.stderr), 240)
        )));
    }

    if stdout.is_empty() {
        return Err(AppError::Message(
            "Codex returned an empty response.".into(),
        ));
    }

    Ok(CodexExecOutput {
        usage: codex_usage_for_call(settings, prompt, &stdout),
        content: stdout,
    })
}

#[derive(Debug)]
struct CodexExecOutput {
    content: String,
    usage: CodexUsage,
}

fn codex_usage_for_call(settings: &CodexSettings, prompt: &str, output: &str) -> CodexUsage {
    let input_tokens = estimate_tokens_from_chars(prompt.chars().count());
    let output_tokens = estimate_tokens_from_chars(output.chars().count());
    let estimated_cost_usd = match (
        settings.input_cost_per_million_tokens,
        settings.output_cost_per_million_tokens,
    ) {
        (Some(input_rate), Some(output_rate)) => Some(
            ((input_tokens as f64 / 1_000_000.0) * input_rate)
                + ((output_tokens as f64 / 1_000_000.0) * output_rate),
        ),
        _ => None,
    };

    CodexUsage {
        call_count: 1,
        prompt_chars: prompt.chars().count() as u64,
        output_chars: output.chars().count() as u64,
        estimated_input_tokens: input_tokens,
        estimated_output_tokens: output_tokens,
        estimated_cost_usd,
    }
}

fn estimate_tokens_from_chars(char_count: usize) -> u64 {
    char_count.div_ceil(4) as u64
}

async fn write_codex_image_attachments(
    clusters: &[TweetCluster],
    image_cache: &mut SyncImageCache,
) -> Result<Vec<PathBuf>, AppError> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(LM_STUDIO_REQUEST_TIMEOUT_SECS))
        .build()
        .expect("codex image client");
    let mut paths = Vec::new();

    for cluster in clusters {
        let Some(media_url) = first_photo_url(&cluster.representative) else {
            continue;
        };
        let Some(image) = image_cache.get_or_fetch(&http, &media_url).await else {
            continue;
        };

        let path = std::env::temp_dir().join(format!(
            "sift-codex-image-{}-{}.{}",
            cluster.id,
            Uuid::new_v4(),
            image.extension()
        ));
        fs::write(&path, &image.bytes)?;
        paths.push(path);
    }

    Ok(paths)
}

fn cleanup_codex_image_attachments(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn write_codex_ranking_schema() -> Result<PathBuf, AppError> {
    let path =
        std::env::temp_dir().join(format!("sift-codex-ranking-{}.schema.json", Uuid::new_v4()));
    fs::write(&path, codex_ranking_schema())?;
    Ok(path)
}

fn codex_ranking_schema() -> &'static str {
    r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["items"],
  "properties": {
    "items": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["clusterId", "keep", "category", "headline", "summary", "whyItMatters", "reasons", "imageImportant", "imageAlt"],
        "properties": {
          "clusterId": { "type": "string" },
          "keep": { "type": "boolean" },
          "category": { "type": "string", "enum": ["Releases", "Tools", "Infrastructure", "Ideas", "People"] },
          "headline": { "type": "string" },
          "summary": { "type": "string" },
          "whyItMatters": { "type": "string" },
          "reasons": { "type": "array", "items": { "type": "string" } },
          "imageImportant": { "type": "boolean" },
          "imageAlt": { "type": ["string", "null"] }
        }
      }
    }
  }
}"#
}

impl LmStudioClient {
    async fn build_multimodal_user_content(
        &self,
        clusters: &[TweetCluster],
        prompt: &str,
        image_cache: &mut SyncImageCache,
    ) -> Option<Vec<serde_json::Value>> {
        let mut parts = vec![serde_json::json!({
            "type": "text",
            "text": prompt,
        })];
        let mut attached_images = 0usize;

        for cluster in clusters {
            let Some(media_url) = first_photo_url(&cluster.representative) else {
                continue;
            };
            let Some(image) = image_cache.get_or_fetch(&self.http, &media_url).await else {
                continue;
            };

            attached_images += 1;
            parts.push(serde_json::json!({
                "type": "text",
                "text": format!(
                    "Attached image for {} from {} (@{}). Use it only if it materially improves editorial judgment.",
                    cluster.id,
                    cluster.representative.author_name,
                    cluster.representative.author_handle,
                ),
            }));
            parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": image.data_url,
                },
            }));
        }

        if attached_images == 0 {
            None
        } else {
            Some(parts)
        }
    }

    async fn chat_completion(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
        model: &str,
        prompt: &str,
        temperature: f32,
    ) -> Result<String, AppError> {
        self.chat_completion_request(
            base_url,
            auth_token,
            serde_json::json!({
              "model": model,
              "temperature": temperature,
              "messages": [
                { "role": "system", "content": "You are a meticulous editor. Reply with exactly what was requested." },
                { "role": "user", "content": prompt }
              ]
            }),
        )
        .await
    }

    async fn chat_completion_with_parts(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
        model: &str,
        parts: Vec<serde_json::Value>,
        temperature: f32,
    ) -> Result<String, AppError> {
        self.chat_completion_request(
            base_url,
            auth_token,
            serde_json::json!({
              "model": model,
              "temperature": temperature,
              "messages": [
                { "role": "system", "content": "You are a meticulous editor. Reply with exactly what was requested." },
                { "role": "user", "content": parts }
              ]
            }),
        )
        .await
    }

    async fn chat_completion_request(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
        body: serde_json::Value,
    ) -> Result<String, AppError> {
        let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
        let mut request = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(LM_STUDIO_COMPLETION_TIMEOUT_SECS))
            .json(&body);

        if let Some(auth_token) = auth_token {
            request = request.header(AUTHORIZATION, bearer(auth_token));
        }

        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                AppError::Message(format!(
                    "LM Studio is still generating after {} seconds. Increase the batch timeout or use a faster local model.",
                    LM_STUDIO_COMPLETION_TIMEOUT_SECS
                ))
            } else {
                AppError::Reqwest(error)
            }
        })?;
        let status = response.status();
        let raw_body = response.text().await?;
        if !status.is_success() {
            return Err(AppError::Message(format!(
                "LM Studio request failed with {status}: {}",
                truncate_chars(&raw_body, 240)
            )));
        }

        let payload =
            serde_json::from_str::<ChatCompletionResponse>(&raw_body).map_err(|error| {
                AppError::Message(format!(
                    "LM Studio returned unreadable completion JSON: {error}. Sample: {}",
                    truncate_chars(&raw_body, 240)
                ))
            })?;
        payload
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_string())
            .ok_or_else(|| AppError::Message("LM Studio returned an empty completion.".into()))
    }
}

pub async fn generate_paper(
    state: &AppState,
    reason: SyncReason,
    scheduled_run: Option<&ScheduledRunContext>,
) -> Result<crate::models::BootstrapState, AppError> {
    let _sync_guard = state.sync_guard.lock().await;
    let settings = state.db.load_settings()?;
    let lm_studio_auth_token = state.lm_studio_auth_token().await;
    let _edition_date = current_edition_date(&settings)?;
    let run_started = Instant::now();

    let mut run = SyncRun {
        id: Uuid::new_v4().to_string(),
        reason: reason.clone(),
        schedule_rule_id: scheduled_run.map(|value| value.rule_id.clone()),
        schedule_rule_label: scheduled_run.map(|value| value.rule_label.clone()),
        schedule_slot_key: scheduled_run.map(|value| value.slot_key.clone()),
        started_at: Utc::now().to_rfc3339(),
        finished_at: None,
        status: SyncStatus::Running,
        item_count: 0,
        kept_count: 0,
        error_message: None,
        edition_id: None,
        timings: SyncRunTimings::default(),
    };
    state.db.insert_sync_run(&run)?;
    emit_sync_progress(
        state,
        &run.id,
        &reason,
        SyncStatus::Running,
        "starting",
        "Refresh started. Checking the enabled live sessions.",
        None,
        None,
        None,
        None,
    );

    let sync_result = async {
        let capture_started = Instant::now();
        let (capture, built_views) = match collect_items_and_build_views(
            state,
            &settings,
            &run.id,
            &reason,
            scheduled_run,
            lm_studio_auth_token.clone(),
        )
        .await
        {
            Ok(result) => result,
            Err(AppError::NoFreshItems { message }) => {
                run.status = SyncStatus::Success;
                run.finished_at = Some(Utc::now().to_rfc3339());
                run.timings.capture_ms = capture_started.elapsed().as_millis() as u64;
                run.timings.total_ms = run_started.elapsed().as_millis() as u64;
                run.error_message = Some(message.clone());
                state.db.insert_sync_run(&run)?;
                emit_sync_progress(
                    state,
                    &run.id,
                    &reason,
                    SyncStatus::Success,
                    "complete",
                    format!("{message} Keeping the current edition on the desk."),
                    None,
                    Some(0),
                    Some(0),
                    None,
                );
                return state.db.load_bootstrap();
            }
            Err(error) => return Err(error),
        };
        run.timings.capture_ms = capture_started.elapsed().as_millis() as u64;
        let view_specs = build_view_specs(&capture);
        let mut saved_editions = Vec::new();
        let mut consolidated_kept_count = 0usize;

        for built_view in built_views {
            run.timings.ranking_ms += built_view.ranking_ms;
            run.timings.front_page_ms += built_view.front_page_ms;
            run.timings.codex_usage.add(&built_view.codex_usage);
            let decision_items = built_view
                .decisions
                .iter()
                .map(ClusterEditorialRecord::to_cleaned_item)
                .collect::<Vec<_>>();
            let saving_started = Instant::now();
            state
                .db
                .save_edition(&built_view.edition, &decision_items, &run)?;
            run.timings.saving_ms += saving_started.elapsed().as_millis() as u64;
            if built_view.view == EditionView::Consolidated
                || (view_specs.len() == 1 && run.edition_id.is_none())
            {
                consolidated_kept_count = built_view.kept_count;
                run.edition_id = Some(built_view.edition.id.clone());
            }
            saved_editions.push(built_view.edition);
        }

        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Running,
            "saving-edition",
            "Saving the edition views locally and updating the desk.",
            Some(capture.items.len()),
            Some(capture.brand_new_count),
            Some(consolidated_kept_count),
            run.edition_id.as_deref(),
        );

        run.item_count = capture.items.len();
        run.kept_count = consolidated_kept_count;
        run.status = SyncStatus::Success;
        run.finished_at = Some(Utc::now().to_rfc3339());
        run.timings.total_ms = run_started.elapsed().as_millis() as u64;
        state.db.insert_sync_run(&run)?;
        let primary_title = saved_editions
            .iter()
            .find(|edition| run.edition_id.as_deref() == Some(edition.id.as_str()))
            .or_else(|| saved_editions.first())
            .map(|edition| edition.title.as_str())
            .unwrap_or("Your SIFT");
        notify_sync(state, &reason, primary_title).await;
        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Success,
            "complete",
            format!(
                "Fresh edition views generated for {}. Captured {} posts and kept {} digest topics in the primary desk view.",
                capture
                    .enabled_sources
                    .iter()
                    .map(|source| source.as_label())
                    .collect::<Vec<_>>()
                    .join(" + "),
                run.item_count,
                run.kept_count
            ),
            Some(run.item_count),
            Some(capture.brand_new_count),
            Some(run.kept_count),
            run.edition_id.as_deref(),
        );
        state.db.load_bootstrap()
    }
    .await;

    match sync_result {
        Ok(bootstrap) => Ok(bootstrap),
        Err(error) => {
            run.status = SyncStatus::Error;
            run.finished_at = Some(Utc::now().to_rfc3339());
            run.timings.total_ms = run_started.elapsed().as_millis() as u64;
            run.error_message = Some(error.to_string());
            state.db.insert_sync_run(&run)?;
            notify_failure(state, &error.to_string()).await;
            emit_sync_progress(
                state,
                &run.id,
                &reason,
                SyncStatus::Error,
                "error",
                error.to_string(),
                Some(run.item_count).filter(|count| *count > 0),
                None,
                Some(run.kept_count).filter(|count| *count > 0),
                run.edition_id.as_deref(),
            );
            Err(error)
        }
    }
}

async fn notify_sync(state: &AppState, reason: &SyncReason, title: &str) {
    use tauri_plugin_notification::NotificationExt;

    let _ = state
        .app
        .notification()
        .builder()
        .title("SIFT published a fresh edition")
        .body(&format!("{} run complete: {}", reason.as_str(), title))
        .show();
}

async fn notify_failure(state: &AppState, message: &str) {
    use tauri_plugin_notification::NotificationExt;

    let _ = state
        .app
        .notification()
        .builder()
        .title("SIFT could not publish today’s issue")
        .body(message)
        .show();
}

pub async fn maybe_run_scheduled_sync(state: &AppState) -> Result<(), AppError> {
    let settings = state.db.load_settings()?;
    let mut due_rules = Vec::new();
    for rule in settings.schedule.rules.iter().filter(|rule| rule.enabled) {
        if let Some(slot_key) = current_schedule_slot(rule)? {
            due_rules.push(ScheduledRunContext {
                rule_id: rule.id.clone(),
                rule_label: rule.label.clone(),
                slot_key,
                browse_page_count: rule.browse_page_count.clone(),
            });
        }
    }

    if due_rules.is_empty() {
        return Ok(());
    }

    if settings.capture.sources.x
        && (state
            .app
            .get_webview_window(crate::X_SESSION_WINDOW_LABEL)
            .is_none()
            || !*state.x_session_authenticated.read().await)
    {
        return Ok(());
    }

    if settings.capture.sources.linkedin
        && (state
            .app
            .get_webview_window(crate::LINKEDIN_SESSION_WINDOW_LABEL)
            .is_none()
            || !*state.linkedin_session_authenticated.read().await)
    {
        return Ok(());
    }

    if settings.capture.sources.reddit
        && (state
            .app
            .get_webview_window(crate::REDDIT_SESSION_WINDOW_LABEL)
            .is_none()
            || !*state.reddit_session_authenticated.read().await)
    {
        return Ok(());
    }

    for scheduled_run in due_rules {
        if state
            .db
            .has_run_for_schedule_slot(&scheduled_run.rule_id, &scheduled_run.slot_key)?
        {
            continue;
        }

        let _ = generate_paper(state, SyncReason::Scheduled, Some(&scheduled_run)).await?;
    }

    Ok(())
}

pub async fn run_scheduler(state: AppState) {
    loop {
        let _ = maybe_run_scheduled_sync(&state).await;
        sleep(Duration::from_secs(60)).await;
    }
}

async fn build_view(
    state: AppState,
    run_id: String,
    reason: SyncReason,
    settings: UserSettings,
    auth_token: Option<String>,
    spec: ViewBuildSpec,
    index: usize,
    total_item_count: usize,
    brand_new_count: usize,
    resurfaced_count: usize,
) -> Result<BuiltView, AppError> {
    let provider = selected_model_provider(&settings);
    let image_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(LM_STUDIO_REQUEST_TIMEOUT_SECS))
        .build()
        .expect("image client");
    let mut image_cache = SyncImageCache::default();
    let fresh_breakdown = format_fresh_post_breakdown(
        spec.items.len(),
        brand_new_count.min(spec.items.len()),
        resurfaced_count.min(spec.items.len()),
    );
    let clusters = group_tweets(&spec.items);
    emit_sync_progress(
        &state,
        &run_id,
        &reason,
        SyncStatus::Running,
        "ranking-items",
        format!(
            "Prepared {fresh_breakdown} in the {} view into {} topic clusters. Sending them to {} for ranking.",
            spec.label,
            clusters.len(),
            provider.label()
        ),
        Some(total_item_count),
        Some(brand_new_count),
        None,
        None,
    );

    let ranking_started = Instant::now();
    let (decisions, mut codex_usage) = batch_decide(
        &state,
        &run_id,
        &reason,
        provider.as_ref(),
        &settings,
        auth_token.as_deref(),
        &clusters,
        &mut image_cache,
    )
    .await?;
    let ranking_ms = ranking_started.elapsed().as_millis() as u64;
    let kept = keep_useful(&decisions);
    let kept_count = kept.len();
    let (_edition_date, edition, front_page_ms, front_page_usage) = build_edition(
        &state,
        &run_id,
        &reason,
        provider.as_ref(),
        &image_http,
        &settings,
        auth_token.as_deref(),
        &kept,
        &mut image_cache,
        spec.view,
        &run_id,
    )
    .await?;
    codex_usage.add(&front_page_usage);

    Ok(BuiltView {
        index,
        view: spec.view,
        decisions,
        kept_count,
        edition,
        ranking_ms,
        front_page_ms,
        codex_usage,
    })
}

fn selected_model_provider(settings: &UserSettings) -> Box<dyn LocalModelProvider + Send + Sync> {
    match settings.model_backend {
        ModelBackend::LmStudio => Box::new(LmStudioClient::default()),
        ModelBackend::Codex => Box::new(CodexCliProvider),
    }
}

async fn batch_decide(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    provider: &(dyn LocalModelProvider + Send + Sync),
    settings: &UserSettings,
    auth_token: Option<&str>,
    clusters: &[TweetCluster],
    image_cache: &mut SyncImageCache,
) -> Result<(Vec<ClusterEditorialRecord>, CodexUsage), AppError> {
    let mut decisions = Vec::new();
    let mut codex_usage = CodexUsage::default();
    let total_batches = clusters.chunks(LM_BATCH_SIZE).len();

    for (index, batch) in clusters.chunks(LM_BATCH_SIZE).enumerate() {
        let batch_number = index + 1;
        emit_sync_progress(
            state,
            run_id,
            reason,
            SyncStatus::Running,
            "ranking-items",
            format!(
                "Ranking batch {batch_number}/{total_batches} in {} ({} topic clusters).",
                provider.label(),
                batch.len()
            ),
            None,
            None,
            None,
            None,
        );

        let mut attempt = 0;
        loop {
            attempt += 1;
            match provider
                .generate_structured(settings, auth_token, batch, image_cache)
                .await
            {
                Ok(mut outcome) => {
                    codex_usage.add(&outcome.usage);
                    println!(
                        "[sift-sync:{run_id}] ranking batch {batch_number}/{total_batches} succeeded on attempt {attempt}"
                    );
                    if outcome.fell_back_to_text {
                        emit_sync_progress(
                            state,
                            run_id,
                            reason,
                            SyncStatus::Running,
                            "ranking-items",
                            format!(
                                "{} refused image input for batch {batch_number}/{total_batches}. Continued with text-only ranking for these {} topic clusters.",
                                provider.label(),
                                batch.len()
                            ),
                            None,
                            None,
                            None,
                            None,
                        );
                    }
                    decisions.extend(outcome.decisions.drain(..).filter_map(|decision| {
                        batch
                            .iter()
                            .find(|cluster| cluster.id == decision.cluster_id)
                            .cloned()
                            .map(|cluster| ClusterEditorialRecord { cluster, decision })
                    }));
                    break;
                }
                Err(error) if attempt < LM_BATCH_MAX_ATTEMPTS => {
                    let wait_seconds = attempt as u64;
                    eprintln!(
                        "[sift-sync:{run_id}] ranking batch {batch_number}/{total_batches} failed on attempt {attempt}: {error}"
                    );
                    emit_sync_progress(
                        state,
                        run_id,
                        reason,
                        SyncStatus::Running,
                        "ranking-items",
                        format!(
                            "{} batch {batch_number}/{total_batches} failed on attempt {attempt}/{LM_BATCH_MAX_ATTEMPTS}. Retrying in {wait_seconds}s.",
                            provider.label()
                        ),
                        None,
                        None,
                        None,
                        None,
                    );
                    sleep(Duration::from_secs(wait_seconds)).await;
                }
                Err(error) => {
                    eprintln!(
                        "[sift-sync:{run_id}] ranking batch {batch_number}/{total_batches} exhausted retries: {error}"
                    );
                    emit_sync_progress(
                        state,
                        run_id,
                        reason,
                        SyncStatus::Running,
                        "ranking-items",
                        format!(
                            "{} batch {batch_number}/{total_batches} failed after {attempt} attempts. Falling back to local heuristics for these {} topic clusters.",
                            provider.label(),
                            batch.len()
                        ),
                        None,
                        None,
                        None,
                        None,
                    );
                    decisions.extend(batch.iter().map(|cluster| ClusterEditorialRecord {
                        cluster: cluster.clone(),
                        decision: fallback_decision(cluster),
                    }));
                    break;
                }
            }
        }
    }

    Ok((decisions, codex_usage))
}

fn keep_useful(decisions: &[ClusterEditorialRecord]) -> Vec<ClusterEditorialRecord> {
    let mut kept = decisions
        .iter()
        .filter(|item| item.decision.keep)
        .cloned()
        .collect::<Vec<_>>();

    kept.sort_by(|left, right| right.signal_score().cmp(&left.signal_score()));
    if kept.len() > MAX_DIGEST_ITEMS {
        kept.truncate(MAX_DIGEST_ITEMS);
    }

    if kept.is_empty() {
        kept.extend(decisions.iter().take(3).cloned());
    }

    kept
}

async fn build_edition(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    provider: &(dyn LocalModelProvider + Send + Sync),
    image_http: &reqwest::Client,
    settings: &UserSettings,
    auth_token: Option<&str>,
    records: &[ClusterEditorialRecord],
    image_cache: &mut SyncImageCache,
    view: EditionView,
    edition_run_id: &str,
) -> Result<(String, Edition, u64, CodexUsage), AppError> {
    let edition_date = current_edition_date(settings)?;
    let edition_id = Uuid::new_v4().to_string();
    let mut sections = BTreeMap::<String, Vec<EditionCard>>::new();
    let mut cleaned_items = Vec::with_capacity(records.len());

    for record in records {
        let item = record.to_cleaned_item();
        let lead_image =
            maybe_persist_lead_image(state, &edition_id, image_http, image_cache, record).await?;
        sections
            .entry(normalize_category(&item.category))
            .or_default()
            .push(EditionCard {
                item_id: item.item_id.clone(),
                author_name: item.author_name.clone(),
                author_handle: item.author_handle.clone(),
                source_url: item.source_url.clone(),
                posted_at: item.posted_at.clone(),
                category: item.category.clone(),
                headline: item.headline.clone(),
                summary: item.summary.clone(),
                why_it_matters: item.why_it_matters.clone(),
                lead_image,
            });
        cleaned_items.push(item);
    }

    let section_list = sections
        .into_iter()
        .map(|(title, mut cards)| {
            cards.sort_by(|left, right| right.posted_at.cmp(&left.posted_at));
            EditionSection {
                id: title.to_lowercase().replace(' ', "-"),
                dek: format!("{} worth your attention", title),
                title: title.clone(),
                cards,
            }
        })
        .collect::<Vec<_>>();

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "building-edition",
        format!(
            "Organized {} kept posts into {} sections. Drafting the front page.",
            records.len(),
            section_list.len()
        ),
        None,
        None,
        Some(records.len()),
        None,
    );

    let front_page_prompt = format!(
        "Write a calm front-page summary in 2 sentences for this SIFT edition. Focus on launches, tools, and notable ideas.\n{}",
        cleaned_items
            .iter()
            .take(8)
            .map(|item| format!("{}: {}", item.headline, item.summary))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let front_page_started = Instant::now();
    let mut codex_usage = CodexUsage::default();
    let front_page_summary = match provider
        .generate_text(settings, auth_token, &front_page_prompt)
        .await
    {
        Ok(outcome) => {
            codex_usage.add(&outcome.usage);
            outcome.text
        }
        Err(error) => {
            eprintln!("[sift-sync:{run_id}] front-page draft fallback: {error}");
            emit_sync_progress(
                state,
                run_id,
                reason,
                SyncStatus::Running,
                "building-edition",
                format!(
                    "{} could not draft the front page. Using a local fallback summary.",
                    provider.label()
                ),
                None,
                None,
                Some(records.len()),
                None,
            );
            cleaned_items
                .iter()
                .take(3)
                .map(|item| item.summary.clone())
                .collect::<Vec<_>>()
                .join(" ")
        }
    };

    let title = match view {
        EditionView::Consolidated => format!("Your SIFT for {}", edition_date),
        EditionView::X => format!("Your SIFT for {} · X", edition_date),
        EditionView::Linkedin => format!("Your SIFT for {} · LinkedIn", edition_date),
        EditionView::Reddit => format!("Your SIFT for {} · Reddit", edition_date),
    };
    let front_page_ms = front_page_started.elapsed().as_millis() as u64;
    Ok((
        edition_date.clone(),
        Edition {
            id: edition_id,
            edition_date,
            title,
            front_page_summary,
            created_at: Utc::now().to_rfc3339(),
            run_id: edition_run_id.to_string(),
            view,
            sections: section_list,
        },
        front_page_ms,
        codex_usage,
    ))
}

fn normalized_schedule_interval_hours(rule: &ScheduleRule) -> u32 {
    rule.interval_hours.clamp(1, 24) as u32
}

fn parse_schedule_time(value: &str, fallback_hour: u32, fallback_minute: u32) -> NaiveTime {
    NaiveTime::parse_from_str(value, "%H:%M").unwrap_or_else(|_| {
        NaiveTime::from_hms_opt(fallback_hour, fallback_minute, 0).expect("default time")
    })
}

#[allow(dead_code)]
pub fn should_run_now(rule: &ScheduleRule) -> Result<bool, AppError> {
    let timezone = machine_timezone();
    let now = Utc::now().with_timezone(&timezone);

    match rule.cadence {
        ScheduleCadence::Daily => {
            let schedule_time = parse_schedule_time(&rule.time_of_day, 7, 30);
            Ok(now.time() >= schedule_time)
        }
        ScheduleCadence::Interval => Ok(current_schedule_slot(rule)?.is_some()),
    }
}

fn current_schedule_slot(rule: &ScheduleRule) -> Result<Option<String>, AppError> {
    if !rule.enabled {
        return Ok(None);
    }

    let timezone = machine_timezone();
    let now = Utc::now().with_timezone(&timezone);

    match rule.cadence {
        ScheduleCadence::Daily => {
            let schedule_time = parse_schedule_time(&rule.time_of_day, 7, 30);
            if now.time() < schedule_time {
                return Ok(None);
            }
            Ok(Some(format!("daily:{}:{}", rule.id, now.date_naive())))
        }
        ScheduleCadence::Interval => {
            let interval_hours = normalized_schedule_interval_hours(rule);
            let window_start = parse_schedule_time(&rule.window_start, 9, 0);
            let window_end = parse_schedule_time(&rule.window_end, 17, 0);
            let current_minutes = (now.hour() * 60 + now.minute()) as i64;
            let start_minutes = (window_start.hour() * 60 + window_start.minute()) as i64;
            let end_minutes = (window_end.hour() * 60 + window_end.minute()) as i64;

            if current_minutes < start_minutes {
                return Ok(None);
            }

            let effective_minutes = current_minutes.min(end_minutes);
            let elapsed_minutes = effective_minutes - start_minutes;
            let interval_minutes = (interval_hours * 60) as i64;
            let slot_index = elapsed_minutes / interval_minutes;
            let slot_minutes = start_minutes + slot_index * interval_minutes;
            if slot_minutes > end_minutes {
                return Ok(None);
            }

            Ok(Some(format!(
                "interval:{}:{}:{:02}:{:02}",
                rule.id,
                now.date_naive(),
                slot_minutes / 60,
                slot_minutes % 60
            )))
        }
    }
}

pub fn current_edition_date(_settings: &UserSettings) -> Result<String, AppError> {
    let timezone = machine_timezone();
    Ok(Utc::now().with_timezone(&timezone).date_naive().to_string())
}

fn heuristically_clean_items(items: Vec<FeedItem>, settings: &UserSettings) -> Vec<FeedItem> {
    let muted_keywords = settings
        .cleanup
        .muted_keywords
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let muted_authors = settings
        .cleanup
        .muted_authors
        .iter()
        .map(|value| value.trim_start_matches('@').to_lowercase())
        .collect::<HashSet<_>>();
    let mut cleaned = Vec::new();

    for item in items {
        if muted_authors.contains(&item.author_handle.to_lowercase()) {
            continue;
        }

        let lowered = item.text.to_lowercase();
        if muted_keywords
            .iter()
            .any(|keyword| lowered.contains(keyword))
        {
            continue;
        }

        if settings.cleanup.remove_bait && is_engagement_bait(&item.text) {
            continue;
        }

        if is_video_only_item(&item) {
            continue;
        }

        if is_sponsored_item(&item) {
            continue;
        }

        if is_low_signal_linkedin_item(&item) {
            continue;
        }

        cleaned.push(item);
    }

    cleaned
}

fn is_video_only_item(item: &FeedItem) -> bool {
    let media = media_from_item(item);
    !media.is_empty()
        && media.iter().any(|media| media.kind == "video")
        && !media.iter().any(|media| media.kind == "photo")
}

fn is_sponsored_label(value: &str) -> bool {
    static SPONSORED_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let normalized = value.trim();
    if normalized.is_empty() {
        return false;
    }

    SPONSORED_PATTERNS
        .get_or_init(|| {
            [
                r"(?i)\bpromoted\b",
                r"(?i)\bsponsored\b",
                r"(?i)\badvertisement\b",
                r"(?i)\bpaid\s+partnership\b",
                r"(?i)\bpatrocinad[oa]s?\b",
                r"(?i)\bpublicidad\b",
                r"(?i)\banuncio\b",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("sponsored label regex"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(normalized))
}

fn is_sponsored_item(item: &FeedItem) -> bool {
    if item
        .raw_json
        .get("isPromoted")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if item
        .raw_json
        .get("socialContext")
        .and_then(|value| value.as_str())
        .is_some_and(is_sponsored_label)
    {
        return true;
    }

    is_sponsored_label(&item.text)
}

fn is_engagement_bait(text: &str) -> bool {
    static BAIT_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    BAIT_PATTERNS
        .get_or_init(|| {
            [
                r"reply .{0,30}\b(dm|send)\b",
                r"unpopular opinion",
                r"\$\d+[kKmM]?/mo",
                r"brutal truths",
                r"your 9-5",
                r"here are \d+",
                r"most founders will ignore",
                r"if you're not scared",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("bait regex"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(&text.to_lowercase()))
}

fn is_low_signal_linkedin_item(item: &FeedItem) -> bool {
    if item.source != CaptureSourceKind::Linkedin.as_feed_source() {
        return false;
    }

    let text = item.text.trim();
    if text.is_empty() {
        return true;
    }

    static LINKEDIN_LOW_SIGNAL_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let lowered = text.to_lowercase();
    let has_explicit_noise = LINKEDIN_LOW_SIGNAL_PATTERNS
        .get_or_init(|| {
            [
                r"\bpromoted\b",
                r"\bapply for\b",
                r"\bview job\b",
                r"\bjob alert\b",
                r"\bmessage the job poster\b",
                r"\bwe(?:'|’)re hiring\b",
                r"\bhiring now\b",
                r"\blike\s+comment\s+repost\s+send\b",
                r"\bfollowers\b",
                r"\bconnections\b",
                r"\bschool alumni work here\b",
            ]
            .into_iter()
            .map(|pattern| Regex::new(pattern).expect("linkedin low-signal regex"))
            .collect()
        })
        .iter()
        .any(|pattern| pattern.is_match(&lowered));
    if has_explicit_noise {
        return true;
    }

    text.matches('|').count() >= 2
        && shared_urls_from_item(item).is_empty()
        && !text.contains("http")
        && !text.contains('.')
        && !text.contains('!')
        && !text.contains('?')
}

fn format_fresh_post_breakdown(
    fresh_count: usize,
    brand_new_count: usize,
    resurfaced_count: usize,
) -> String {
    let mut parts = vec![format!("{brand_new_count} brand-new")];
    if resurfaced_count > 0 {
        parts.push(format!("{resurfaced_count} resurfaced"));
    }

    format!("{fresh_count} fresh posts ({})", parts.join(", "))
}

fn enabled_capture_sources(settings: &UserSettings) -> Vec<CaptureSourceKind> {
    let mut sources = Vec::new();
    if settings.capture.sources.x {
        sources.push(CaptureSourceKind::X);
    }
    if settings.capture.sources.linkedin {
        sources.push(CaptureSourceKind::Linkedin);
    }
    if settings.capture.sources.reddit {
        sources.push(CaptureSourceKind::Reddit);
    }
    sources
}

fn build_view_specs(capture: &MultiCaptureOutcome) -> Vec<ViewBuildSpec> {
    let has_x = capture
        .enabled_sources
        .iter()
        .any(|source| *source == CaptureSourceKind::X);
    let has_linkedin = capture
        .enabled_sources
        .iter()
        .any(|source| *source == CaptureSourceKind::Linkedin);
    let has_reddit = capture
        .enabled_sources
        .iter()
        .any(|source| *source == CaptureSourceKind::Reddit);
    let enabled_count = usize::from(has_x) + usize::from(has_linkedin) + usize::from(has_reddit);

    if enabled_count > 1 {
        let mut specs = vec![ViewBuildSpec {
            view: EditionView::Consolidated,
            label: "Consolidated",
            items: capture.items.clone(),
        }];

        if has_x {
            specs.push(ViewBuildSpec {
                view: EditionView::X,
                label: "X",
                items: capture
                    .items
                    .iter()
                    .filter(|item| item.source == CaptureSourceKind::X.as_feed_source())
                    .cloned()
                    .collect(),
            });
        }
        if has_linkedin {
            specs.push(ViewBuildSpec {
                view: EditionView::Linkedin,
                label: "LinkedIn",
                items: capture
                    .items
                    .iter()
                    .filter(|item| item.source == CaptureSourceKind::Linkedin.as_feed_source())
                    .cloned()
                    .collect(),
            });
        }
        if has_reddit {
            specs.push(ViewBuildSpec {
                view: EditionView::Reddit,
                label: "Reddit",
                items: capture
                    .items
                    .iter()
                    .filter(|item| item.source == CaptureSourceKind::Reddit.as_feed_source())
                    .cloned()
                    .collect(),
            });
        }

        return specs;
    }

    if has_linkedin {
        return vec![ViewBuildSpec {
            view: EditionView::Linkedin,
            label: "LinkedIn",
            items: capture.items.clone(),
        }];
    }

    if has_reddit {
        return vec![ViewBuildSpec {
            view: EditionView::Reddit,
            label: "Reddit",
            items: capture.items.clone(),
        }];
    }

    vec![ViewBuildSpec {
        view: EditionView::X,
        label: "X",
        items: capture.items.clone(),
    }]
}

fn browse_page_count_for_source(
    settings: &UserSettings,
    source: CaptureSourceKind,
    scheduled_run: Option<&ScheduledRunContext>,
) -> usize {
    let browse_page_count = scheduled_run
        .map(|value| &value.browse_page_count)
        .or_else(|| {
            settings
                .schedule
                .rules
                .iter()
                .find(|rule| rule.enabled)
                .map(|rule| &rule.browse_page_count)
        })
        .or_else(|| {
            settings
                .schedule
                .rules
                .first()
                .map(|rule| &rule.browse_page_count)
        })
        .expect("default schedule rule should always exist");

    match source {
        CaptureSourceKind::X => browse_page_count.x.max(1),
        CaptureSourceKind::Linkedin => browse_page_count.linkedin.max(1),
        CaptureSourceKind::Reddit => browse_page_count.reddit.max(1),
    }
}

async fn collect_items_and_build_views(
    state: &AppState,
    settings: &UserSettings,
    run_id: &str,
    reason: &SyncReason,
    scheduled_run: Option<&ScheduledRunContext>,
    auth_token: Option<String>,
) -> Result<(MultiCaptureOutcome, Vec<BuiltView>), AppError> {
    let enabled_sources = enabled_capture_sources(settings);
    if enabled_sources.is_empty() {
        return Err(AppError::Message(
            "Enable at least one source in Settings before refreshing the edition.".into(),
        ));
    }

    let boundary = CaptureBoundary {
        edition_date: current_edition_date(settings)?,
        since_timestamp: state
            .db
            .load_latest_edition()?
            .map(|edition| edition.created_at),
    };
    let mut all_items = Vec::new();
    let mut total_brand_new_count = 0usize;
    let mut total_resurfaced_count = 0usize;
    let mut build_tasks = JoinSet::new();

    for source in &enabled_sources {
        hide_all_source_sessions_for_refresh(state)?;
        show_source_session_for_refresh(state, *source).await?;
        let capture = collect_items_from_source_live_session(
            state,
            settings,
            run_id,
            reason,
            scheduled_run,
            &boundary,
            *source,
        )
        .await;

        if let Err(hide_error) = hide_source_session_after_refresh(state, *source) {
            if let Err(capture_error) = capture {
                build_tasks.abort_all();
                return Err(capture_error);
            }
            build_tasks.abort_all();
            return Err(hide_error);
        }

        let capture = match capture {
            Ok(capture) => capture,
            Err(error) => {
                build_tasks.abort_all();
                return Err(error);
            }
        };
        let spec = ViewBuildSpec {
            view: source.as_edition_view(),
            label: source.as_label(),
            items: capture.items.clone(),
        };
        let state = state.clone();
        let run_id = run_id.to_string();
        let reason = reason.clone();
        let settings = settings.clone();
        let auth_token = auth_token.clone();
        let source_item_count = capture.items.len();
        let source_brand_new_count = capture.brand_new_count;
        let source_resurfaced_count = capture.resurfaced_count;
        let index = source_view_index(&enabled_sources, *source);

        build_tasks.spawn(async move {
            build_view(
                state,
                run_id,
                reason,
                settings,
                auth_token,
                spec,
                index,
                source_item_count,
                source_brand_new_count,
                source_resurfaced_count,
            )
            .await
        });

        total_brand_new_count += capture.brand_new_count;
        total_resurfaced_count += capture.resurfaced_count;
        all_items.extend(capture.items);
    }

    if all_items.is_empty() {
        build_tasks.abort_all();
        return Err(AppError::NoFreshItems {
            message: format!(
                "SIFT checked {}, but none of the posts were fresh {}.",
                enabled_sources
                    .iter()
                    .map(|source| source.as_label())
                    .collect::<Vec<_>>()
                    .join(" + "),
                boundary.digest_label()
            ),
        });
    }

    if enabled_sources.len() > 1 {
        let state = state.clone();
        let run_id = run_id.to_string();
        let reason = reason.clone();
        let settings = settings.clone();
        let spec = ViewBuildSpec {
            view: EditionView::Consolidated,
            label: "Consolidated",
            items: all_items.clone(),
        };
        let total_item_count = all_items.len();
        let auth_token = auth_token.clone();

        build_tasks.spawn(async move {
            build_view(
                state,
                run_id,
                reason,
                settings,
                auth_token,
                spec,
                0,
                total_item_count,
                total_brand_new_count,
                total_resurfaced_count,
            )
            .await
        });
    }

    let mut built_views = Vec::with_capacity(if enabled_sources.len() > 1 {
        enabled_sources.len() + 1
    } else {
        enabled_sources.len()
    });
    while let Some(result) = build_tasks.join_next().await {
        built_views.push(result.map_err(|error| {
            AppError::Message(format!("An edition view build task failed: {error}"))
        })??);
    }
    built_views.sort_by_key(|view| view.index);

    Ok((
        MultiCaptureOutcome {
            items: all_items,
            brand_new_count: total_brand_new_count,
            enabled_sources,
        },
        built_views,
    ))
}

async fn show_source_session_for_refresh(
    state: &AppState,
    source: CaptureSourceKind,
) -> Result<bool, AppError> {
    match source {
        CaptureSourceKind::X => state.ensure_x_session_visible_for_refresh().await,
        CaptureSourceKind::Linkedin => state.ensure_linkedin_session_visible_for_refresh().await,
        CaptureSourceKind::Reddit => state.ensure_reddit_session_visible_for_refresh().await,
    }
}

fn hide_source_session_after_refresh(
    state: &AppState,
    source: CaptureSourceKind,
) -> Result<(), AppError> {
    match source {
        CaptureSourceKind::X => state.hide_x_session_after_refresh(),
        CaptureSourceKind::Linkedin => state.hide_linkedin_session_after_refresh(),
        CaptureSourceKind::Reddit => state.hide_reddit_session_after_refresh(),
    }
}

fn hide_all_source_sessions_for_refresh(state: &AppState) -> Result<(), AppError> {
    for source in [
        CaptureSourceKind::X,
        CaptureSourceKind::Linkedin,
        CaptureSourceKind::Reddit,
    ] {
        hide_source_session_after_refresh(state, source)?;
    }

    Ok(())
}

async fn collect_items_from_source_live_session(
    state: &AppState,
    settings: &UserSettings,
    run_id: &str,
    reason: &SyncReason,
    scheduled_run: Option<&ScheduledRunContext>,
    boundary: &CaptureBoundary,
    source: CaptureSourceKind,
) -> Result<CaptureOutcome, AppError> {
    let timezone = machine_timezone();
    let window = match source {
        CaptureSourceKind::X => ensure_live_x_session_on_home(state, run_id, reason).await?,
        CaptureSourceKind::Linkedin => {
            ensure_live_linkedin_session_on_home(state, run_id, reason).await?
        }
        CaptureSourceKind::Reddit => {
            ensure_live_reddit_session_on_home(state, run_id, reason).await?
        }
    };
    let request_id = Uuid::new_v4().to_string();
    let (sender, receiver) = oneshot::channel::<Result<XSessionCapturePayload, String>>();
    state.x_session_capture_requests.lock().await.insert(
        request_id.clone(),
        XSessionCaptureRequest {
            run_id: run_id.to_string(),
            reason: reason.clone(),
            source_label: source.as_label().to_string(),
            sender: Some(sender),
            last_progress_at: Instant::now(),
            latest_progress: None,
        },
    );
    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "capturing-feed",
        format!(
            "Collecting posts from the live {} session.",
            source.as_label()
        ),
        None,
        None,
        None,
        None,
    );

    let capture_script = format!(
        "{collector}({request_id}, {options});",
        collector = match source {
            CaptureSourceKind::X => "window.__SIFT_COLLECT_FEED__",
            CaptureSourceKind::Linkedin => "window.__SIFT_COLLECT_LINKEDIN_FEED__",
            CaptureSourceKind::Reddit => "window.__SIFT_COLLECT_REDDIT_FEED__",
        },
        request_id = serde_json::to_string(&request_id)?,
        options = serde_json::to_string(&serde_json::json!({
            "editionDate": boundary.edition_date.clone(),
            "sinceTimestamp": boundary.since_timestamp,
            "timeZone": timezone.name(),
            "maxItems": CAPTURE_MAX_ITEMS,
            "targetFreshItems": CAPTURE_TARGET_FRESH_ITEMS,
            "maxPasses": browse_page_count_for_source(settings, source, scheduled_run),
            "stablePasses": CAPTURE_STABLE_PASSES,
            "exhaustedPasses": CAPTURE_EXHAUSTED_PASSES,
            "waitForAdvanceMs": CAPTURE_WAIT_FOR_ADVANCE_MS,
        }))?,
    );

    if let Err(error) = window.eval(capture_script) {
        state
            .x_session_capture_requests
            .lock()
            .await
            .remove(&request_id);
        return Err(AppError::Message(format!(
            "SIFT could not start the live {} capture: {error}",
            source.as_label()
        )));
    }

    let capture_started_at = Instant::now();
    let mut receiver = receiver;
    let capture = loop {
        let (last_progress_at, latest_progress) = {
            let requests = state.x_session_capture_requests.lock().await;
            let request = requests.get(&request_id).ok_or_else(|| {
                AppError::Message(format!(
                    "The live {} capture request disappeared before the page responded.",
                    source.as_label()
                ))
            })?;
            (request.last_progress_at, request.latest_progress.clone())
        };

        if capture_started_at.elapsed() >= Duration::from_secs(CAPTURE_TIMEOUT_SECS) {
            state
                .x_session_capture_requests
                .lock()
                .await
                .remove(&request_id);
            return Err(AppError::Message(format_capture_total_timeout_message(
                source.as_label(),
                latest_progress.as_ref(),
            )));
        }

        if last_progress_at.elapsed() >= Duration::from_secs(CAPTURE_IDLE_TIMEOUT_SECS) {
            state
                .x_session_capture_requests
                .lock()
                .await
                .remove(&request_id);
            return Err(AppError::Message(format_capture_idle_timeout_message(
                source.as_label(),
                latest_progress.as_ref(),
            )));
        }

        let total_remaining = Duration::from_secs(CAPTURE_TIMEOUT_SECS)
            .checked_sub(capture_started_at.elapsed())
            .unwrap_or_default();
        let idle_remaining = Duration::from_secs(CAPTURE_IDLE_TIMEOUT_SECS)
            .checked_sub(last_progress_at.elapsed())
            .unwrap_or_default();
        let wait_for = total_remaining.min(idle_remaining);

        match timeout(wait_for, &mut receiver).await {
            Ok(Ok(Ok(capture))) => {
                state
                    .x_session_capture_requests
                    .lock()
                    .await
                    .remove(&request_id);
                break capture;
            }
            Ok(Ok(Err(message))) => {
                state
                    .x_session_capture_requests
                    .lock()
                    .await
                    .remove(&request_id);
                return Err(AppError::Message(message));
            }
            Ok(Err(_)) => {
                state
                    .x_session_capture_requests
                    .lock()
                    .await
                    .remove(&request_id);
                return Err(AppError::Message(format!(
                    "The live {} capture finished before SIFT could receive the results.",
                    source.as_label()
                )));
            }
            Err(_) => continue,
        }
    };

    let raw_count = capture.items.len();
    let cleaned_items = normalize_session_capture(capture.items, settings, source);
    let cleaned_count = cleaned_items.len();
    let filtered_out_count = raw_count.saturating_sub(cleaned_count);
    let known_entries = state.db.load_tweetdb_entries(
        &cleaned_items
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut fresh_items = Vec::new();
    let mut fresh_brand_new_count = 0;
    for item in cleaned_items {
        let existing = known_entries.get(&item.id);
        if is_item_new_for_boundary(&item, existing, &boundary, timezone) {
            if existing.is_none() {
                fresh_brand_new_count += 1;
            }
            fresh_items.push(item);
        }
    }
    let fresh_seen_again_count = fresh_items.len().saturating_sub(fresh_brand_new_count);
    let skipped_old = cleaned_count.saturating_sub(fresh_items.len());
    let seen_at = Utc::now().to_rfc3339();
    state.db.insert_feed_items(&fresh_items)?;
    state.db.upsert_tweets(&fresh_items, &seen_at, run_id)?;
    let fresh_breakdown = format_fresh_post_breakdown(
        fresh_items.len(),
        fresh_brand_new_count,
        fresh_seen_again_count,
    );
    let pass_summary = match (capture.completed_passes, capture.total_passes) {
        (Some(completed), Some(total)) => format!(" after {completed}/{total} passes"),
        _ => String::new(),
    };
    let early_stop_note = if capture.ended_early.unwrap_or(false) {
        match source {
            CaptureSourceKind::Linkedin => {
                " LinkedIn stopped before the configured cap because the feed did not advance further."
            }
            CaptureSourceKind::Reddit => {
                " Reddit stopped before the configured cap because the home feed did not advance further."
            }
            CaptureSourceKind::X => "",
        }
    } else {
        ""
    };

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "capturing-feed",
        format!(
            "Captured {raw_count} posts from {}{pass_summary}. {fresh_breakdown} remain {} after cleanup.{}{}{}",
            source.as_label(),
            boundary.collector_label(),
            if filtered_out_count > 0 {
                format!(" {filtered_out_count} posts were filtered out by cleanup.")
            } else {
                String::new()
            },
            if skipped_old > 0 {
                format!(" {skipped_old} were already covered before that boundary.")
            } else {
                String::new()
            },
            early_stop_note
        ),
        Some(raw_count),
        Some(fresh_brand_new_count),
        None,
        None,
    );

    if cleaned_count == 0 {
        return Err(AppError::Message(format!(
            "SIFT captured {raw_count} posts from {}, but none of them survived your cleanup filters. Keep browsing a bit longer and try again.",
            source.as_label()
        )));
    }

    if fresh_items.is_empty() {
        return Err(AppError::NoFreshItems {
            message: format!(
                "SIFT cleaned {cleaned_count} {} posts, but none of them were fresh {}.",
                source.as_label(),
                boundary.digest_label()
            ),
        });
    }

    Ok(CaptureOutcome {
        items: fresh_items,
        brand_new_count: fresh_brand_new_count,
        resurfaced_count: fresh_seen_again_count,
    })
}

async fn ensure_live_x_session_on_home(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
) -> Result<tauri::WebviewWindow, AppError> {
    let window = state
        .app
        .get_webview_window(crate::X_SESSION_WINDOW_LABEL)
        .ok_or_else(|| {
            AppError::Message(
                "Open X Session first so SIFT has a live browser session to collect from.".into(),
            )
        })?;

    if !*state.x_session_authenticated.read().await {
        return Err(AppError::Message(
            "Sign in to X inside the native SIFT session before refreshing the edition.".into(),
        ));
    }

    let already_home = state
        .x_session_last_known_url
        .read()
        .await
        .clone()
        .and_then(|value| Url::parse(&value).ok())
        .is_some_and(|url| is_home_timeline_url(&url));

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "navigating-home",
        if already_home {
            "Refreshing the Home timeline before capture."
        } else {
            "Opening the Home timeline and forcing a feed refresh."
        },
        None,
        None,
        None,
        None,
    );

    let previous_url = state.x_session_last_known_url.read().await.clone();
    *state.x_session_last_known_url.write().await = None;
    if let Err(error) = window.navigate(build_home_timeline_refresh_url()) {
        *state.x_session_last_known_url.write().await = previous_url;
        return Err(AppError::Message(error.to_string()));
    }
    wait_for_session_url(state, is_home_timeline_url, Duration::from_secs(15)).await?;

    Ok(window)
}

async fn ensure_live_linkedin_session_on_home(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
) -> Result<tauri::WebviewWindow, AppError> {
    let window = state
        .app
        .get_webview_window(crate::LINKEDIN_SESSION_WINDOW_LABEL)
        .ok_or_else(|| {
            AppError::Message(
                "Open LinkedIn Session first so SIFT has a live browser session to collect from."
                    .into(),
            )
        })?;

    if !*state.linkedin_session_authenticated.read().await {
        return Err(AppError::Message(
            "Sign in to LinkedIn inside the native SIFT session before refreshing the edition."
                .into(),
        ));
    }

    let already_home = state
        .linkedin_session_last_known_url
        .read()
        .await
        .clone()
        .and_then(|value| Url::parse(&value).ok())
        .is_some_and(|url| is_linkedin_home_feed_url(&url));

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "navigating-home",
        if already_home {
            "Refreshing the LinkedIn home feed before capture."
        } else {
            "Opening the LinkedIn home feed and forcing a refresh."
        },
        None,
        None,
        None,
        None,
    );

    let previous_url = state.linkedin_session_last_known_url.read().await.clone();
    *state.linkedin_session_last_known_url.write().await = None;
    if let Err(error) = window.navigate(build_linkedin_home_feed_refresh_url()) {
        *state.linkedin_session_last_known_url.write().await = previous_url;
        return Err(AppError::Message(error.to_string()));
    }
    wait_for_linkedin_session_url(state, is_linkedin_home_feed_url, Duration::from_secs(15))
        .await?;

    Ok(window)
}

async fn ensure_live_reddit_session_on_home(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
) -> Result<tauri::WebviewWindow, AppError> {
    let window = state
        .app
        .get_webview_window(crate::REDDIT_SESSION_WINDOW_LABEL)
        .ok_or_else(|| {
            AppError::Message(
                "Open Reddit Session first so SIFT has a live browser session to collect from."
                    .into(),
            )
        })?;

    if !*state.reddit_session_authenticated.read().await {
        return Err(AppError::Message(
            "Sign in to Reddit inside the native SIFT session before refreshing the edition."
                .into(),
        ));
    }

    let already_home = state
        .reddit_session_last_known_url
        .read()
        .await
        .clone()
        .and_then(|value| Url::parse(&value).ok())
        .is_some_and(|url| is_reddit_home_feed_url(&url));

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "navigating-home",
        if already_home {
            "Refreshing the Reddit home feed before capture."
        } else {
            "Opening the Reddit home feed and forcing a refresh."
        },
        None,
        None,
        None,
        None,
    );

    let previous_url = state.reddit_session_last_known_url.read().await.clone();
    *state.reddit_session_last_known_url.write().await = None;
    if let Err(error) = window.navigate(build_reddit_home_feed_refresh_url()) {
        *state.reddit_session_last_known_url.write().await = previous_url;
        return Err(AppError::Message(error.to_string()));
    }
    wait_for_reddit_session_url(state, is_reddit_home_feed_url, Duration::from_secs(15)).await?;

    Ok(window)
}

async fn wait_for_session_url<F>(
    state: &AppState,
    predicate: F,
    timeout_after: Duration,
) -> Result<String, AppError>
where
    F: Fn(&Url) -> bool,
{
    let started_at = Instant::now();

    while started_at.elapsed() < timeout_after {
        let current = state.x_session_last_known_url.read().await.clone();
        if let Some(current) = current {
            if let Ok(url) = Url::parse(&current) {
                if predicate(&url) {
                    return Ok(current);
                }
            }
        }

        sleep(Duration::from_millis(250)).await;
    }

    Err(AppError::Message(
        "Timed out waiting for the live X session to reach the home timeline.".into(),
    ))
}

fn is_home_timeline_url(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
    ) && url.path().starts_with("/home")
}

fn build_home_timeline_refresh_url() -> Url {
    let mut url = Url::parse("https://x.com/home").expect("valid x home url");
    url.query_pairs_mut()
        .append_pair("sift_refresh", &Uuid::new_v4().to_string());
    url
}

async fn wait_for_linkedin_session_url<F>(
    state: &AppState,
    predicate: F,
    timeout_after: Duration,
) -> Result<String, AppError>
where
    F: Fn(&Url) -> bool,
{
    let started_at = Instant::now();

    while started_at.elapsed() < timeout_after {
        let current = state.linkedin_session_last_known_url.read().await.clone();
        if let Some(current) = current {
            if let Ok(url) = Url::parse(&current) {
                if predicate(&url) {
                    return Ok(current);
                }
            }
        }

        sleep(Duration::from_millis(250)).await;
    }

    Err(AppError::Message(
        "Timed out waiting for the live LinkedIn session to reach the home feed.".into(),
    ))
}

fn is_linkedin_home_feed_url(url: &Url) -> bool {
    is_linkedin_domain(url) && url.path().starts_with("/feed")
}

fn build_linkedin_home_feed_refresh_url() -> Url {
    let mut url = Url::parse("https://www.linkedin.com/feed/").expect("valid linkedin feed url");
    url.query_pairs_mut()
        .append_pair("sift_refresh", &Uuid::new_v4().to_string());
    url
}

async fn wait_for_reddit_session_url<F>(
    state: &AppState,
    predicate: F,
    timeout_after: Duration,
) -> Result<String, AppError>
where
    F: Fn(&Url) -> bool,
{
    let started_at = Instant::now();

    while started_at.elapsed() < timeout_after {
        let current = state.reddit_session_last_known_url.read().await.clone();
        if let Some(current) = current {
            if let Ok(url) = Url::parse(&current) {
                if predicate(&url) {
                    return Ok(current);
                }
            }
        }

        sleep(Duration::from_millis(250)).await;
    }

    Err(AppError::Message(
        "Timed out waiting for the live Reddit session to reach the home feed.".into(),
    ))
}

fn is_reddit_home_feed_url(url: &Url) -> bool {
    is_reddit_domain(url) && matches!(url.path(), "/" | "/best/" | "/hot/" | "/new/")
}

fn build_reddit_home_feed_refresh_url() -> Url {
    let mut url = Url::parse("https://www.reddit.com/").expect("valid reddit home url");
    url.query_pairs_mut()
        .append_pair("sift_refresh", &Uuid::new_v4().to_string());
    url
}

fn normalize_session_capture(
    captured_items: Vec<XSessionCaptureItem>,
    settings: &UserSettings,
    source: CaptureSourceKind,
) -> Vec<FeedItem> {
    let captured = captured_items
        .into_iter()
        .filter(|item| !item.id.trim().is_empty())
        .filter(|item| !item.text.trim().is_empty())
        .filter(|item| !(settings.cleanup.hide_retweets && item.is_repost))
        .filter(|item| !(settings.cleanup.hide_replies && item.is_reply))
        .map(|item| {
            let media = normalize_capture_media(&item.media);
            let normalized_text = match source {
                CaptureSourceKind::Linkedin => sanitize_linkedin_capture_text(
                    &item.text,
                    &item.author_name,
                    item.social_context.as_deref(),
                ),
                _ => item.text.trim().to_string(),
            };
            FeedItem {
                id: format!("{}:{}", source.as_feed_source(), item.id.trim()),
                source: source.as_feed_source().into(),
                author_name: item.author_name.trim().to_string(),
                author_handle: item
                    .author_handle
                    .trim_start_matches('@')
                    .trim()
                    .to_string(),
                text: normalized_text.clone(),
                source_url: item.source_url.clone(),
                posted_at: item.posted_at.clone(),
                raw_json: serde_json::json!({
                  "captureMode": "live-session",
                  "source": source.as_feed_source(),
                  "isRepost": item.is_repost,
                  "isReply": item.is_reply,
                  "isPromoted": item.is_promoted,
                  "socialContext": item.social_context,
                  "sharedUrls": item.shared_urls,
                  "media": media,
                }),
                fingerprint: fingerprint(&normalized_text),
            }
        })
        .collect::<Vec<_>>();

    heuristically_clean_items(captured, settings)
}

fn is_same_edition_day(posted_at: &str, edition_date: &str, timezone: Tz) -> bool {
    DateTime::parse_from_rfc3339(posted_at)
        .map(|value| value.with_timezone(&timezone).date_naive().to_string() == edition_date)
        .unwrap_or(true)
}

fn is_item_new_for_boundary(
    item: &FeedItem,
    existing: Option<&crate::models::TweetDbEntry>,
    boundary: &CaptureBoundary,
    timezone: Tz,
) -> bool {
    if let Some(since_timestamp) = boundary.since_timestamp.as_deref() {
        if existing.is_some_and(|entry| {
            timestamp_is_after(&entry.first_seen_at, since_timestamp) == Some(false)
        }) {
            return false;
        }

        if existing.is_some() {
            return true;
        }

        if timestamp_is_after(&item.posted_at, since_timestamp) == Some(true) {
            return true;
        }

        return is_same_edition_day(&item.posted_at, &boundary.edition_date, timezone);
    }

    is_same_edition_day(&item.posted_at, &boundary.edition_date, timezone)
}

fn fingerprint(text: &str) -> String {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    format!("{:x}", digest)
}

fn normalize_category(value: &str) -> String {
    match value.to_lowercase().as_str() {
        "release" | "releases" => "Releases".into(),
        "tool" | "tools" => "Tools".into(),
        "infrastructure" | "infra" => "Infrastructure".into(),
        "idea" | "ideas" => "Ideas".into(),
        "people" | "person" => "People".into(),
        _ => "Ideas".into(),
    }
}

fn timestamp_is_after(value: &str, boundary: &str) -> Option<bool> {
    Some(
        DateTime::parse_from_rfc3339(value)
            .ok()?
            .with_timezone(&Utc)
            > DateTime::parse_from_rfc3339(boundary)
                .ok()?
                .with_timezone(&Utc),
    )
}

fn normalize_capture_media(media: &[XSessionCaptureMedia]) -> Vec<FeedMedia> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for item in media {
        let kind = item.kind.trim().to_lowercase();
        if kind != "photo" && kind != "video" {
            continue;
        }
        let Some(url) = normalize_media_url(&item.url) else {
            continue;
        };
        if !is_supported_story_media_url(&url, &kind) {
            continue;
        }
        if seen.insert(url.clone()) {
            normalized.push(FeedMedia {
                url,
                kind: kind.clone(),
            });
        }
    }

    normalized
}

fn sanitize_linkedin_capture_text(
    text: &str,
    author_name: &str,
    social_context: Option<&str>,
) -> String {
    let mut cleaned =
        normalize_linkedin_collapsed_tokens(text.trim().replace('\u{a0}', " ").as_str());
    cleaned = Regex::new(r"[.]{2,}")
        .expect("valid regex")
        .replace_all(&cleaned, " ")
        .to_string();
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    let author_name = author_name.trim();
    let author_name = if author_name.eq_ignore_ascii_case("LinkedIn author") {
        ""
    } else {
        author_name
    };
    let social_context = social_context
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let noise_patterns = [
        Regex::new(r"(?i)^feed\s*post\b[:\s-]*").expect("valid regex"),
        Regex::new(r"(?i)^linkedin\s+author\b[:\s-]*").expect("valid regex"),
        Regex::new(r"(?i)^[^\d]{2,100}?\s+\d[\d,.]*\s+followers\b(?:\s*[•·]\s*)?")
            .expect("valid regex"),
        Regex::new(r"(?i)^[^\d]{2,100}?\s+\d[\d,.]*\s+connections\b(?:\s*[•·]\s*)?")
            .expect("valid regex"),
        Regex::new(r"(?i)^\d[\d,.]*\s+followers\b(?:\s*[•·]\s*)?").expect("valid regex"),
        Regex::new(r"(?i)^\d[\d,.]*\s+connections\b(?:\s*[•·]\s*)?").expect("valid regex"),
        Regex::new(r"(?i)^[0-9]+\s*(?:m|h|d|w|mo|yr|y)\b(?:\s*[•·]\s*edited)?\s*")
            .expect("valid regex"),
        Regex::new(r"(?i)^edited\b(?:\s*[•·]\s*)?").expect("valid regex"),
        Regex::new(r"(?i)^(?:follow|message)\b\s*").expect("valid regex"),
        Regex::new(r"(?i)^promoted\b(?:\s*[•·]\s*)?").expect("valid regex"),
    ];
    let trailing_noise_patterns = [
        Regex::new(r"(?i)\bview job\b.*$").expect("valid regex"),
        Regex::new(r"(?i)\blike\s+comment\s+repost\s+send\b.*$").expect("valid regex"),
        Regex::new(r"(?i)\b\d+\s+(?:school|company)\s+alumni work here\b.*$").expect("valid regex"),
    ];

    let mut changed = true;
    while changed {
        changed = false;

        if !author_name.is_empty() {
            let next = cleaned
                .strip_prefix(author_name)
                .map(str::trim)
                .unwrap_or(cleaned.as_str())
                .to_string();
            if next != cleaned {
                cleaned = next;
                changed = true;
            }
        }

        if let Some(context) = social_context {
            let next = cleaned
                .strip_prefix(context)
                .map(str::trim)
                .unwrap_or(cleaned.as_str())
                .to_string();
            if next != cleaned {
                cleaned = next;
                changed = true;
            }
        }

        for pattern in &noise_patterns {
            let next = pattern.replace(&cleaned, "").trim().to_string();
            if next != cleaned {
                cleaned = next;
                changed = true;
            }
        }
    }

    for pattern in &trailing_noise_patterns {
        cleaned = pattern.replace(&cleaned, "").trim().to_string();
    }

    cleaned
        .trim_matches(|char: char| matches!(char, '•' | '·' | '-' | '|' | ':'))
        .trim()
        .to_string()
}

fn normalize_linkedin_collapsed_tokens(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len() + 16);
    let mut previous: Option<char> = None;

    for current in text.chars() {
        if let Some(last) = previous {
            let split_boundary = (last.is_ascii_lowercase() && current.is_ascii_uppercase())
                || (last.is_ascii_alphabetic() && current.is_ascii_digit())
                || (last.is_ascii_digit() && current.is_ascii_alphabetic());
            if split_boundary && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
        }
        normalized.push(current);
        previous = Some(current);
    }

    normalized
}

fn media_from_item(item: &FeedItem) -> Vec<FeedMedia> {
    item.raw_json
        .get("media")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<FeedMedia>>(value).ok())
        .unwrap_or_default()
}

fn first_photo_url(item: &FeedItem) -> Option<String> {
    media_from_item(item)
        .into_iter()
        .find(|media| media.kind == "photo")
        .map(|media| media.url)
}

fn shared_urls_from_item(item: &FeedItem) -> HashSet<String> {
    item.raw_json
        .get("sharedUrls")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .filter_map(normalize_shared_url)
        .collect()
}

fn normalize_media_url(value: &str) -> Option<String> {
    let mut parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }

    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn is_supported_story_media_url(value: &str, kind: &str) -> bool {
    let Ok(parsed) = Url::parse(value) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or_default().to_lowercase();
    let path = parsed.path().to_lowercase();

    if [
        "profile_images",
        "profile_banners",
        "media_emoji",
        "semantic_core_img",
        "profile",
        "company-logo",
        "emoji",
        "icon",
        "sprite",
        "badge",
        "avatar",
        "award",
        "snoovatar",
    ]
    .iter()
    .any(|fragment| path.contains(fragment))
    {
        return false;
    }

    if kind == "photo"
        && (path.contains("ext_tw_video_thumb") || path.contains("amplify_video_thumb"))
    {
        return false;
    }

    host == "pbs.twimg.com"
        || host == "i.redd.it"
        || host == "preview.redd.it"
        || host == "external-preview.redd.it"
        || host.ends_with(".redd.it")
        || host == "i.imgur.com"
        || host == "media.licdn.com"
        || host.ends_with(".licdn.com")
}

fn normalize_shared_url(value: &str) -> Option<String> {
    let mut parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || is_x_domain(&parsed) {
        return None;
    }

    let kept_query = parsed
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "utm_source"
                    | "utm_medium"
                    | "utm_campaign"
                    | "utm_term"
                    | "utm_content"
                    | "ref"
                    | "ref_src"
                    | "ref_url"
                    | "si"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    parsed.set_query(None);
    if !kept_query.is_empty() {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in kept_query {
            serializer.append_pair(&key, &value);
        }
        let query = serializer.finish();
        parsed.set_query(Some(&query));
    }
    parsed.set_fragment(None);

    let trimmed_path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(if trimmed_path.is_empty() {
        "/"
    } else {
        trimmed_path.as_str()
    });
    Some(parsed.to_string())
}

fn topic_keywords(text: &str) -> HashSet<String> {
    static TOKEN_CLEANER: OnceLock<Regex> = OnceLock::new();
    static STOPWORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();

    let cleaner = TOKEN_CLEANER.get_or_init(|| {
        Regex::new(r"https?://\S+|www\.\S+|[@#][A-Za-z0-9_]+|[^a-z0-9\s]")
            .expect("topic cleaner regex")
    });
    let stopwords = STOPWORDS.get_or_init(|| {
        [
            "about",
            "after",
            "again",
            "almost",
            "because",
            "before",
            "being",
            "build",
            "builder",
            "building",
            "check",
            "could",
            "demo",
            "from",
            "have",
            "just",
            "launch",
            "launched",
            "launching",
            "more",
            "most",
            "open",
            "post",
            "posts",
            "preview",
            "release",
            "released",
            "shipping",
            "ships",
            "shipped",
            "some",
            "than",
            "that",
            "their",
            "there",
            "these",
            "this",
            "thread",
            "today",
            "tool",
            "tools",
            "using",
            "very",
            "what",
            "when",
            "with",
            "your",
        ]
        .into_iter()
        .collect()
    });
    let lowered = text.to_lowercase();
    let cleaned = cleaner.replace_all(&lowered, " ");

    cleaned
        .split_whitespace()
        .filter(|word| word.len() >= 4)
        .filter(|word| !stopwords.contains(*word))
        .filter(|word| !word.chars().all(|char| char.is_ascii_digit()))
        .map(ToOwned::to_owned)
        .collect()
}

fn cluster_match_score(
    item: &FeedItem,
    item_urls: &HashSet<String>,
    item_keywords: &HashSet<String>,
    cluster: &TweetCluster,
) -> Option<usize> {
    if cluster
        .members
        .iter()
        .any(|member| member.fingerprint == item.fingerprint)
    {
        return Some(500);
    }

    let shared_urls = item_urls.intersection(&cluster.shared_urls).count();
    if shared_urls > 0 {
        return Some(400 + shared_urls);
    }

    let shared_keywords = item_keywords.intersection(&cluster.keywords).count();
    if shared_keywords == 0 {
        return None;
    }

    let union_keywords = item_keywords.union(&cluster.keywords).count().max(1);
    let overlap_ratio = (shared_keywords * 100) / union_keywords;
    if shared_keywords >= 4 && overlap_ratio >= 25 {
        return Some(250 + overlap_ratio);
    }
    if shared_keywords >= 3 && overlap_ratio >= 40 {
        return Some(200 + overlap_ratio);
    }

    None
}

fn should_replace_representative(current: &FeedItem, candidate: &FeedItem) -> bool {
    let current_url_count = shared_urls_from_item(current).len();
    let candidate_url_count = shared_urls_from_item(candidate).len();
    if candidate_url_count != current_url_count {
        return candidate_url_count > current_url_count;
    }

    let current_media_count = media_from_item(current).len();
    let candidate_media_count = media_from_item(candidate).len();
    if candidate_media_count != current_media_count {
        return candidate_media_count > current_media_count;
    }

    candidate.posted_at > current.posted_at
}

fn group_tweets(items: &[FeedItem]) -> Vec<TweetCluster> {
    let mut ordered = items.to_vec();
    ordered.sort_by(|left, right| right.posted_at.cmp(&left.posted_at));

    let mut clusters = Vec::<TweetCluster>::new();
    for item in ordered {
        let item_urls = shared_urls_from_item(&item);
        let item_keywords = topic_keywords(&item.text);
        let best_match = clusters
            .iter()
            .enumerate()
            .filter_map(|(index, cluster)| {
                cluster_match_score(&item, &item_urls, &item_keywords, cluster)
                    .map(|score| (index, score))
            })
            .max_by_key(|(_, score)| *score);

        if let Some((index, _)) = best_match {
            let cluster = &mut clusters[index];
            cluster.members.push(item.clone());
            cluster.shared_urls.extend(item_urls);
            cluster.keywords.extend(item_keywords);
            if should_replace_representative(&cluster.representative, &item) {
                cluster.representative = item;
            }
        } else {
            clusters.push(TweetCluster {
                id: String::new(),
                representative: item.clone(),
                members: vec![item],
                shared_urls: item_urls,
                keywords: item_keywords,
            });
        }
    }

    clusters.sort_by(|left, right| {
        right
            .signal_score()
            .cmp(&left.signal_score())
            .then_with(|| right.repeat_count().cmp(&left.repeat_count()))
            .then_with(|| {
                right
                    .representative
                    .posted_at
                    .cmp(&left.representative.posted_at)
            })
    });
    for (index, cluster) in clusters.iter_mut().enumerate() {
        cluster.id = format!("cluster-{}", index + 1);
    }

    clusters
}

fn fallback_decision(cluster: &TweetCluster) -> ClusterDecision {
    let item = &cluster.representative;
    let repeated = cluster.repeat_count();
    ClusterDecision {
        cluster_id: cluster.id.clone(),
        keep: !is_low_signal_linkedin_cluster(cluster)
            && (repeated > 1 || !is_engagement_bait(&item.text)),
        category: if item.text.to_lowercase().contains("release")
            || item.text.to_lowercase().contains("ships")
        {
            "Releases".into()
        } else if item.text.to_lowercase().contains("tool")
            || item.text.to_lowercase().contains("plugin")
        {
            "Tools".into()
        } else {
            "Ideas".into()
        },
        headline: truncate_words(&item.text, 12),
        summary: if repeated > 1 {
            format!(
                "{} tweets clustered around this topic. {}",
                repeated,
                truncate_words(&item.text, 20)
            )
        } else {
            truncate_words(&item.text, 32)
        },
        why_it_matters: if repeated > 1 {
            "Multiple posts converged on the same topic.".into()
        } else {
            "Useful update surfaced from your feed.".into()
        },
        reasons: vec!["Fallback editorial pass".into()],
        image_important: false,
        image_alt: None,
    }
}

fn build_structured_prompt(clusters: &[TweetCluster]) -> String {
    let mut prompt = String::from(
        "You are SIFT, a calm editor that turns noisy social feeds into a concise daily briefing.\n",
    );
    prompt.push_str("Return strict JSON with this shape: {\"items\":[{\"clusterId\":\"...\",\"keep\":true,\"category\":\"Releases|Tools|Infrastructure|Ideas|People\",\"headline\":\"...\",\"summary\":\"...\",\"whyItMatters\":\"...\",\"reasons\":[\"...\"],\"imageImportant\":false,\"imageAlt\":null}]}\n");
    prompt.push_str("Rules: keep only the most important clusters. Prefer repeated topics across independent authors, concrete releases, notable tools, useful operating ideas, and things that feel widely discussed for a reason. It is good to drop most clusters. Avoid outrage, bait, empty self-promotion, duplicates, sponsored/promoted posts, hiring posts, job listings, profile blurbs, and feed chrome.\n");
    prompt.push_str("Use neutral headlines under 14 words and summaries under 42 words. Set imageImportant to true only when an attached image genuinely adds important context to the digest. When imageImportant is true, provide concise factual alt text in imageAlt. Otherwise set imageImportant to false and imageAlt to null.\n");
    prompt.push_str("Input clusters:\n");

    for cluster in clusters {
        let _ = writeln!(
            prompt,
            "- clusterId: {}\n  source: {}\n  repeats: {}\n  uniqueAuthors: {}\n  sharedUrls: {}\n  keywords: {}\n  attachedPhoto: {}\n  representative: {} (@{})\n  sampleTweets:\n{}",
            cluster.id,
            cluster.representative.source,
            cluster.repeat_count(),
            cluster.unique_author_count(),
            if cluster.shared_urls.is_empty() {
                "none".into()
            } else {
                cluster.shared_url_list().join(", ")
            },
            if cluster.keywords.is_empty() {
                "none".into()
            } else {
                cluster.keyword_list().join(", ")
            },
            if first_photo_url(&cluster.representative).is_some() {
                "yes"
            } else {
                "no"
            },
            cluster.representative.author_name,
            cluster.representative.author_handle,
            cluster
                .members
                .iter()
                .take(4)
                .enumerate()
                .map(|(index, item)| {
                    format!(
                        "    {}. {} (@{}) [{}] {}",
                        index + 1,
                        item.author_name,
                        item.author_handle,
                        item.posted_at,
                        truncate_chars(&item.text, 220)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    prompt
}

fn is_low_signal_linkedin_cluster(cluster: &TweetCluster) -> bool {
    if cluster.representative.source != CaptureSourceKind::Linkedin.as_feed_source() {
        return false;
    }

    if cluster
        .members
        .iter()
        .any(|item| is_low_signal_linkedin_item(item))
    {
        return true;
    }

    cluster.repeat_count() == 1
        && cluster.unique_author_count() == 1
        && shared_urls_from_item(&cluster.representative).is_empty()
        && first_photo_url(&cluster.representative).is_none()
        && cluster.representative.text.matches('|').count() >= 2
}

fn parse_cluster_decisions(
    content: &str,
    clusters: &[TweetCluster],
) -> Result<Vec<ClusterDecision>, AppError> {
    let parsed = extract_json_segment(content).map_err(|error| {
        AppError::Message(format!(
            "{error} Sample response: {}",
            truncate_chars(content, 240)
        ))
    })?;
    let envelope = serde_json::from_str::<ClusterDecisionEnvelope>(&parsed)
        .or_else(|_| {
            serde_json::from_str::<Vec<ClusterDecision>>(&parsed)
                .map(|items| ClusterDecisionEnvelope { items })
        })
        .map_err(|error| {
            AppError::Message(format!(
                "LM Studio returned unreadable ranking JSON: {error}. Sample: {}",
                truncate_chars(&parsed, 240)
            ))
        })?;

    Ok(clusters
        .iter()
        .map(|cluster| {
            envelope
                .items
                .iter()
                .find(|decision| decision.cluster_id == cluster.id)
                .cloned()
                .unwrap_or_else(|| fallback_decision(cluster))
        })
        .collect())
}

async fn maybe_persist_lead_image(
    state: &AppState,
    edition_id: &str,
    http: &reqwest::Client,
    image_cache: &mut SyncImageCache,
    record: &ClusterEditorialRecord,
) -> Result<Option<EditionImage>, AppError> {
    let base_dir = state
        .app
        .path()
        .app_local_data_dir()
        .map_err(|error| AppError::Message(error.to_string()))?;
    persist_record_lead_image(&base_dir, http, edition_id, image_cache, record).await
}

async fn persist_record_lead_image(
    base_dir: &Path,
    http: &reqwest::Client,
    edition_id: &str,
    image_cache: &mut SyncImageCache,
    record: &ClusterEditorialRecord,
) -> Result<Option<EditionImage>, AppError> {
    let Some(media_url) = first_photo_url(&record.cluster.representative) else {
        return Ok(None);
    };
    let Some(image) = image_cache.get_or_fetch(http, &media_url).await else {
        return Ok(None);
    };

    let alt = lead_image_alt(record);

    Ok(Some(persist_downloaded_image(
        base_dir,
        edition_id,
        &record.cluster.representative.id,
        &image,
        &alt,
    )?))
}

fn lead_image_alt(record: &ClusterEditorialRecord) -> String {
    if let Some(alt) = record
        .decision
        .image_alt
        .clone()
        .filter(|value| !value.trim().is_empty())
    {
        return alt;
    }

    if record.decision.image_important && !record.decision.summary.trim().is_empty() {
        return record.decision.summary.clone();
    }

    record.decision.headline.clone()
}

fn truncate_words(text: &str, limit: usize) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.len() <= limit {
        text.to_string()
    } else {
        format!("{}...", words[..limit].join(" "))
    }
}

fn extract_json_segment(text: &str) -> Result<String, AppError> {
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        return Ok(text[start..=end].to_string());
    }

    if let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) {
        return Ok(text[start..=end].to_string());
    }

    Err(AppError::Message("LM Studio did not return JSON.".into()))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut value = text.trim().replace('\n', " ");
    if value.chars().count() <= limit {
        return value;
    }

    value = value.chars().take(limit).collect::<String>();
    format!("{value}...")
}

fn persist_downloaded_image(
    base_dir: &Path,
    edition_id: &str,
    item_id: &str,
    image: &DownloadedImage,
    alt: &str,
) -> Result<EditionImage, AppError> {
    let asset_dir = base_dir.join("assets").join("editions").join(edition_id);
    fs::create_dir_all(&asset_dir)?;

    let asset_path = asset_dir.join(format!(
        "{}-{}.{}",
        sanitize_filename_fragment(item_id),
        short_hash(&image.source_url),
        image.extension()
    ));
    fs::write(&asset_path, &image.bytes)?;

    Ok(EditionImage {
        path: asset_path.to_string_lossy().to_string(),
        source_url: image.source_url.clone(),
        mime_type: image.mime_type.clone(),
        alt: alt.to_string(),
    })
}

async fn download_image_asset(
    client: &reqwest::Client,
    url: &str,
) -> Result<DownloadedImage, AppError> {
    let response = client.get(url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Message(format!(
            "Image download failed with {status} for {url}"
        )));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    let bytes = response.bytes().await?.to_vec();
    if bytes.is_empty() {
        return Err(AppError::Message(format!(
            "Image download returned an empty body for {url}"
        )));
    }
    if bytes.len() > LM_STUDIO_IMAGE_MAX_BYTES {
        return Err(AppError::Message(format!(
            "Image download exceeded the {} byte limit for {url}",
            LM_STUDIO_IMAGE_MAX_BYTES
        )));
    }

    let mime_type = normalize_image_mime_type(content_type.as_deref(), url).ok_or_else(|| {
        AppError::Message(format!(
            "Unsupported image type for multimodal request: {url}"
        ))
    })?;
    let data_url = format!(
        "data:{};base64,{}",
        mime_type,
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );

    Ok(DownloadedImage {
        source_url: url.to_string(),
        mime_type,
        bytes,
        data_url,
    })
}

fn normalize_image_mime_type(header: Option<&str>, url: &str) -> Option<String> {
    let from_header = header
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    if let Some(mime_type) = from_header {
        if matches!(
            mime_type.as_str(),
            "image/jpeg" | "image/jpg" | "image/png" | "image/webp"
        ) {
            return Some(if mime_type == "image/jpg" {
                "image/jpeg".into()
            } else {
                mime_type
            });
        }
    }

    let parsed = Url::parse(url).ok()?;
    if let Some((_, format)) = parsed.query_pairs().find(|(key, _)| key == "format") {
        return match format.as_ref().to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some("image/jpeg".into()),
            "png" => Some("image/png".into()),
            "webp" => Some("image/webp".into()),
            _ => None,
        };
    }

    parsed
        .path_segments()
        .and_then(|segments| segments.last())
        .and_then(|last| last.rsplit('.').next())
        .and_then(|extension| match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some("image/jpeg".into()),
            "png" => Some("image/png".into()),
            "webp" => Some("image/webp".into()),
            _ => None,
        })
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{:x}", digest)[..10].to_string()
}

fn sanitize_filename_fragment(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || char == '-' || char == '_' {
                char
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if cleaned.is_empty() {
        "image".into()
    } else {
        cleaned
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[derive(Debug, Deserialize)]
struct TokenEnvelope {
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct XMeResponse {
    data: XUser,
}

#[derive(Debug)]
struct XMeEnvelope {
    user_id: String,
    handle: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct XUser {
    id: String,
    name: String,
    username: String,
}

#[derive(Debug, Deserialize)]
struct LmModelList {
    data: Vec<LmModel>,
}

#[derive(Debug, Deserialize)]
struct LmModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: ChatContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatContentPart>),
}

impl ChatContent {
    fn as_string(&self) -> Option<String> {
        match self {
            Self::Text(value) => Some(value.clone()),
            Self::Parts(parts) => Some(
                parts
                    .iter()
                    .filter_map(|part| part.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatContentPart {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClusterDecisionEnvelope {
    items: Vec<ClusterDecision>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClusterDecision {
    cluster_id: String,
    keep: bool,
    category: String,
    headline: String,
    summary: String,
    why_it_matters: String,
    reasons: Vec<String>,
    #[serde(default)]
    image_important: bool,
    #[serde(default)]
    image_alt: Option<String>,
}

impl ClusterDecision {
    fn into_cleaned(self, cluster: &TweetCluster) -> CleanedItem {
        let item = &cluster.representative;
        let repeated = cluster.repeat_count();
        CleanedItem {
            item_id: item.id.clone(),
            keep: self.keep,
            category: normalize_category(&self.category),
            headline: self.headline,
            summary: self.summary,
            why_it_matters: if repeated > 1 {
                format!(
                    "{} Mentioned across {} tweets.",
                    self.why_it_matters, repeated
                )
            } else {
                self.why_it_matters
            },
            reasons: self.reasons,
            author_name: item.author_name.clone(),
            author_handle: item.author_handle.clone(),
            source_url: item.source_url.clone(),
            posted_at: item.posted_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CleanupSettings;
    use httpmock::prelude::*;
    use tempfile::tempdir;

    fn sample_settings(include_images: bool, base_url: String) -> UserSettings {
        let mut settings = UserSettings::default();
        settings.lm_studio.base_url = base_url;
        settings.lm_studio.selected_model = Some("vision-model".into());
        settings.lm_studio.include_images = include_images;
        settings
    }

    fn sample_cluster(media: Vec<FeedMedia>) -> TweetCluster {
        let item = FeedItem {
            id: "post-1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Shipped a local-first release with screenshots".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T12:00:00Z".into(),
            raw_json: serde_json::json!({
                "media": media,
                "sharedUrls": ["https://example.com/release"]
            }),
            fingerprint: fingerprint("Shipped a local-first release with screenshots"),
        };
        TweetCluster {
            id: "cluster-1".into(),
            representative: item.clone(),
            members: vec![item],
            shared_urls: ["https://example.com/release".into()].into_iter().collect(),
            keywords: ["release".into(), "screenshot".into()]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn codex_exec_args_include_fixed_policy_and_optional_schema() {
        let settings = CodexSettings {
            command: "codex".into(),
            model: Some("gpt-5.2".into()),
            profile: Some("sift".into()),
            ..CodexSettings::default()
        };
        let schema_path = Path::new("ranking.schema.json");

        let image_path = PathBuf::from("story.jpg");
        let args = codex_exec_args(
            &settings,
            Some(schema_path),
            std::slice::from_ref(&image_path),
        );

        assert_eq!(
            args,
            vec![
                "exec",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--model",
                "gpt-5.2",
                "--profile",
                "sift",
                "--output-schema",
                "ranking.schema.json",
                "--image",
                "story.jpg",
                "-"
            ]
        );
    }

    #[test]
    fn codex_ranking_json_parses_to_cluster_decisions() {
        let cluster = sample_cluster(Vec::new());
        let content = r#"{"items":[{"clusterId":"cluster-1","keep":true,"category":"Tools","headline":"Local release ships","summary":"A local-first release shipped with screenshots.","whyItMatters":"It improves the workflow.","reasons":["Concrete release"],"imageImportant":false,"imageAlt":null}]}"#;

        let decisions =
            parse_cluster_decisions(content, std::slice::from_ref(&cluster)).expect("decisions");

        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].keep);
        assert_eq!(decisions[0].category, "Tools");
        assert_eq!(decisions[0].headline, "Local release ships");
    }

    #[tokio::test]
    async fn codex_provider_reports_missing_command() {
        let mut settings = UserSettings::default();
        settings.model_backend = ModelBackend::Codex;
        settings.codex.command = "definitely-not-a-real-codex-command-for-sift".into();
        let provider = CodexCliProvider;

        let error = provider
            .generate_text(&settings, None, "Write one sentence.")
            .await
            .expect_err("missing command should fail");

        assert!(error.to_string().contains("Codex command not found"));
    }

    #[test]
    fn codex_usage_estimates_tokens_and_cost_when_rates_are_configured() {
        let settings = CodexSettings {
            input_cost_per_million_tokens: Some(1.0),
            output_cost_per_million_tokens: Some(10.0),
            ..CodexSettings::default()
        };

        let usage = codex_usage_for_call(&settings, "12345678", "1234");

        assert_eq!(usage.call_count, 1);
        assert_eq!(usage.prompt_chars, 8);
        assert_eq!(usage.output_chars, 4);
        assert_eq!(usage.estimated_input_tokens, 2);
        assert_eq!(usage.estimated_output_tokens, 1);
        assert!(
            (usage.estimated_cost_usd.expect("estimated cost") - 0.000012).abs() < f64::EPSILON
        );
    }

    #[test]
    fn engagement_bait_patterns_are_detected() {
        assert!(is_engagement_bait(
            "Reply BLUEPRINT and I'll DM you the exact playbook"
        ));
        assert!(is_engagement_bait(
            "Unpopular opinion: most developers will be mass unemployed"
        ));
        assert!(!is_engagement_bait(
            "Supabase shipped scheduled jobs and websocket support today"
        ));
    }

    #[test]
    fn heuristics_keep_repeated_posts_but_drop_muted_content() {
        let settings = UserSettings {
            cleanup: CleanupSettings {
                muted_keywords: vec!["politics".into()],
                muted_authors: vec!["@noise".into()],
                ..UserSettings::default().cleanup
            },
            ..UserSettings::default()
        };
        let items = vec![
            FeedItem {
                id: "1".into(),
                source: "x".into(),
                author_name: "One".into(),
                author_handle: "noise".into(),
                text: "Interesting launch".into(),
                source_url: "https://x.com/a/status/1".into(),
                posted_at: "2026-04-16T12:00:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Interesting launch"),
            },
            FeedItem {
                id: "2".into(),
                source: "x".into(),
                author_name: "Two".into(),
                author_handle: "builder".into(),
                text: "Interesting launch".into(),
                source_url: "https://x.com/a/status/2".into(),
                posted_at: "2026-04-16T12:01:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Interesting launch"),
            },
            FeedItem {
                id: "3".into(),
                source: "x".into(),
                author_name: "Three".into(),
                author_handle: "builder".into(),
                text: "Politics and drama".into(),
                source_url: "https://x.com/a/status/3".into(),
                posted_at: "2026-04-16T12:02:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Politics and drama"),
            },
            FeedItem {
                id: "4".into(),
                source: "x".into(),
                author_name: "Four".into(),
                author_handle: "builder".into(),
                text: "Shipped a SQLite vector search extension".into(),
                source_url: "https://x.com/a/status/4".into(),
                posted_at: "2026-04-16T12:03:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Shipped a SQLite vector search extension"),
            },
        ];

        let cleaned = heuristically_clean_items(items, &settings);
        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0].id, "2");
        assert_eq!(cleaned[1].id, "4");
    }

    #[test]
    fn schedule_due_checks_timezone_and_hour() {
        let settings = UserSettings::default();
        assert!(current_edition_date(&settings).is_ok());
        assert!(should_run_now(&settings.schedule.rules[0]).is_ok());
    }

    #[test]
    fn interval_schedule_uses_hour_slots() {
        let mut rule = ScheduleRule::default();
        rule.cadence = ScheduleCadence::Interval;
        rule.interval_hours = 3;
        rule.window_start = "00:00".into();
        rule.window_end = "23:00".into();

        let slot = current_schedule_slot(&rule).expect("schedule slot");
        assert!(slot.is_some());
        assert!(slot.expect("slot").starts_with("interval:"));
    }

    #[test]
    fn freshness_boundary_keeps_same_day_unseen_posts() {
        let item = FeedItem {
            id: "1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Interesting launch".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T10:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: fingerprint("Interesting launch"),
        };
        let boundary = CaptureBoundary {
            edition_date: "2026-04-16".into(),
            since_timestamp: Some("2026-04-16T12:00:00Z".into()),
        };

        assert!(is_item_new_for_boundary(
            &item,
            None,
            &boundary,
            chrono_tz::UTC,
        ));
    }

    #[test]
    fn freshness_boundary_skips_posts_seen_before_last_saved_edition() {
        let item = FeedItem {
            id: "1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Interesting launch".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T10:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: fingerprint("Interesting launch"),
        };
        let existing = crate::models::TweetDbEntry {
            tweet_id: item.id.clone(),
            first_seen_at: "2026-04-16T11:30:00Z".into(),
            last_seen_at: "2026-04-16T11:30:00Z".into(),
            seen_count: 1,
        };
        let boundary = CaptureBoundary {
            edition_date: "2026-04-16".into(),
            since_timestamp: Some("2026-04-16T12:00:00Z".into()),
        };

        assert!(!is_item_new_for_boundary(
            &item,
            Some(&existing),
            &boundary,
            chrono_tz::UTC,
        ));
    }

    #[test]
    fn freshness_boundary_keeps_posts_first_seen_after_last_saved_edition() {
        let item = FeedItem {
            id: "1".into(),
            source: "x-session".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "Interesting launch".into(),
            source_url: "https://x.com/builder/status/1".into(),
            posted_at: "2026-04-16T10:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: fingerprint("Interesting launch"),
        };
        let existing = crate::models::TweetDbEntry {
            tweet_id: item.id.clone(),
            first_seen_at: "2026-04-16T12:30:00Z".into(),
            last_seen_at: "2026-04-16T12:30:00Z".into(),
            seen_count: 1,
        };
        let boundary = CaptureBoundary {
            edition_date: "2026-04-16".into(),
            since_timestamp: Some("2026-04-16T12:00:00Z".into()),
        };

        assert!(is_item_new_for_boundary(
            &item,
            Some(&existing),
            &boundary,
            chrono_tz::UTC,
        ));
    }

    #[test]
    fn live_session_capture_respects_reply_and_repost_filters() {
        let settings = UserSettings::default();
        let items = vec![
            XSessionCaptureItem {
                id: "1".into(),
                author_name: "Builder".into(),
                author_handle: "builder".into(),
                text: "Reposted launch".into(),
                source_url: "https://x.com/builder/status/1".into(),
                posted_at: "2026-04-16T12:00:00Z".into(),
                is_repost: true,
                is_reply: false,
                is_promoted: false,
                social_context: Some("Ada reposted".into()),
                shared_urls: Vec::new(),
                media: Vec::new(),
            },
            XSessionCaptureItem {
                id: "2".into(),
                author_name: "Builder".into(),
                author_handle: "builder".into(),
                text: "Replying to @team about the launch".into(),
                source_url: "https://x.com/builder/status/2".into(),
                posted_at: "2026-04-16T12:01:00Z".into(),
                is_repost: false,
                is_reply: true,
                is_promoted: false,
                social_context: None,
                shared_urls: Vec::new(),
                media: Vec::new(),
            },
            XSessionCaptureItem {
                id: "3".into(),
                author_name: "Builder".into(),
                author_handle: "@builder".into(),
                text: "Shipped a local-first search release today".into(),
                source_url: "https://x.com/builder/status/3".into(),
                posted_at: "2026-04-16T12:02:00Z".into(),
                is_repost: false,
                is_reply: false,
                is_promoted: false,
                social_context: None,
                shared_urls: vec!["https://example.com/release".into()],
                media: vec![
                    XSessionCaptureMedia {
                        url: "https://pbs.twimg.com/media/story-photo?format=jpg&name=small".into(),
                        kind: "photo".into(),
                    },
                    XSessionCaptureMedia {
                        url: "https://pbs.twimg.com/profile_images/avatar.jpg".into(),
                        kind: "photo".into(),
                    },
                    XSessionCaptureMedia {
                        url: "https://pbs.twimg.com/media/video-thumb.jpg".into(),
                        kind: "video".into(),
                    },
                ],
            },
        ];

        let cleaned = normalize_session_capture(items, &settings, CaptureSourceKind::X);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].id, "x-session:3");
        assert_eq!(cleaned[0].author_handle, "builder");
        assert_eq!(cleaned[0].source, "x-session");
        assert_eq!(
            cleaned[0].raw_json["sharedUrls"]
                .as_array()
                .expect("shared urls array")
                .len(),
            1
        );
        assert_eq!(
            cleaned[0].raw_json["media"]
                .as_array()
                .expect("media array")
                .len(),
            2
        );
        assert_eq!(
            first_photo_url(&cleaned[0]).as_deref(),
            Some("https://pbs.twimg.com/media/story-photo?format=jpg&name=small")
        );
    }

    #[test]
    fn capture_media_accepts_linkedin_and_reddit_story_images() {
        let media = normalize_capture_media(&[
            XSessionCaptureMedia {
                url: "https://media.licdn.com/dms/image/v2/D4E22AQ/story-image/feedshare-shrink_800/B4EZ.jpg".into(),
                kind: "photo".into(),
            },
            XSessionCaptureMedia {
                url: "https://preview.redd.it/story-image.jpg?width=1080&crop=smart&auto=webp&s=abc".into(),
                kind: "photo".into(),
            },
            XSessionCaptureMedia {
                url: "https://pbs.twimg.com/ext_tw_video_thumb/123/pu/img/thumb.jpg".into(),
                kind: "video".into(),
            },
            XSessionCaptureMedia {
                url: "https://styles.redditmedia.com/avatar.png".into(),
                kind: "photo".into(),
            },
        ]);

        assert_eq!(media.len(), 3);
        assert_eq!(
            media[0].url,
            "https://media.licdn.com/dms/image/v2/D4E22AQ/story-image/feedshare-shrink_800/B4EZ.jpg"
        );
        assert_eq!(
            media[1].url,
            "https://preview.redd.it/story-image.jpg?width=1080&crop=smart&auto=webp&s=abc"
        );
        assert_eq!(media[2].kind, "video");
    }

    #[test]
    fn live_session_capture_drops_video_only_posts() {
        let settings = UserSettings::default();
        let items = vec![XSessionCaptureItem {
            id: "video-1".into(),
            author_name: "Builder".into(),
            author_handle: "builder".into(),
            text: "A launch video without enough context to summarize safely".into(),
            source_url: "https://x.com/builder/status/4".into(),
            posted_at: "2026-04-16T12:03:00Z".into(),
            is_repost: false,
            is_reply: false,
            is_promoted: false,
            social_context: None,
            shared_urls: Vec::new(),
            media: vec![XSessionCaptureMedia {
                url: "https://pbs.twimg.com/ext_tw_video_thumb/123/pu/img/thumb.jpg".into(),
                kind: "video".into(),
            }],
        }];

        let cleaned = normalize_session_capture(items, &settings, CaptureSourceKind::X);
        assert!(cleaned.is_empty());
    }

    #[test]
    fn live_session_capture_drops_promoted_posts_across_sources() {
        let settings = UserSettings::default();
        let cases = [
            (
                CaptureSourceKind::X,
                XSessionCaptureItem {
                    id: "x-ad".into(),
                    author_name: "Advertiser".into(),
                    author_handle: "ad".into(),
                    text: "A promoted launch for teams".into(),
                    source_url: "https://x.com/ad/status/1".into(),
                    posted_at: "2026-04-16T12:00:00Z".into(),
                    is_repost: false,
                    is_reply: false,
                    is_promoted: true,
                    social_context: None,
                    shared_urls: Vec::new(),
                    media: Vec::new(),
                },
            ),
            (
                CaptureSourceKind::Linkedin,
                XSessionCaptureItem {
                    id: "linkedin-ad".into(),
                    author_name: "Advertiser".into(),
                    author_handle: "ad".into(),
                    text: "Patrocinado We can help your team ship faster.".into(),
                    source_url: "https://www.linkedin.com/feed/update/urn:li:activity:2/".into(),
                    posted_at: "2026-04-16T12:00:00Z".into(),
                    is_repost: false,
                    is_reply: false,
                    is_promoted: false,
                    social_context: Some("Patrocinado".into()),
                    shared_urls: Vec::new(),
                    media: Vec::new(),
                },
            ),
            (
                CaptureSourceKind::Reddit,
                XSessionCaptureItem {
                    id: "reddit-ad".into(),
                    author_name: "r/ad".into(),
                    author_handle: "ad".into(),
                    text: "Sponsored developer platform".into(),
                    source_url: "https://www.reddit.com/r/ad/comments/1/post/".into(),
                    posted_at: "2026-04-16T12:00:00Z".into(),
                    is_repost: false,
                    is_reply: false,
                    is_promoted: false,
                    social_context: Some("Promoted".into()),
                    shared_urls: Vec::new(),
                    media: Vec::new(),
                },
            ),
        ];

        for (source, item) in cases {
            let cleaned = normalize_session_capture(vec![item], &settings, source);
            assert!(
                cleaned.is_empty(),
                "{source:?} promoted item should be dropped"
            );
        }
    }

    #[test]
    fn sanitize_linkedin_capture_text_removes_feed_chrome() {
        let cleaned = sanitize_linkedin_capture_text(
            "Feedpost Dio 113,123 followers 2d Edited We shipped a much better search workflow for teams.",
            "Dio",
            None,
        );

        assert_eq!(
            cleaned,
            "We shipped a much better search workflow for teams."
        );
    }

    #[test]
    fn live_session_capture_cleans_linkedin_text_before_headlines() {
        let settings = UserSettings::default();
        let items = vec![XSessionCaptureItem {
            id: "1".into(),
            author_name: "Dio".into(),
            author_handle: "dio".into(),
            text: "Feed post Dio 113123 followers 2d Edited We shipped a much better search workflow for teams.".into(),
            source_url: "https://www.linkedin.com/feed/update/urn:li:activity:1/".into(),
            posted_at: "2026-04-22T12:00:00Z".into(),
            is_repost: false,
            is_reply: false,
            is_promoted: false,
            social_context: None,
            shared_urls: Vec::new(),
            media: Vec::new(),
        }];

        let cleaned = normalize_session_capture(items, &settings, CaptureSourceKind::Linkedin);

        assert_eq!(cleaned.len(), 1);
        assert_eq!(
            cleaned[0].text,
            "We shipped a much better search workflow for teams."
        );
        assert_eq!(
            cleaned[0].fingerprint,
            fingerprint("We shipped a much better search workflow for teams.")
        );
    }

    #[test]
    fn sanitize_linkedin_capture_text_strips_promoted_prefix_without_author_name() {
        let cleaned = sanitize_linkedin_capture_text(
            "Nord Anglia Education 108,536 followers Promoted What matters most for children growing up with AI?",
            "LinkedIn author",
            None,
        );

        assert_eq!(
            cleaned,
            "What matters most for children growing up with AI?"
        );
    }

    #[test]
    fn sanitize_linkedin_capture_text_handles_collapsed_feed_post_tokens() {
        let cleaned = sanitize_linkedin_capture_text(
            "Feed postJohn....... 108,536FollowersPromoted What matters most for children growing up with AI?",
            "LinkedIn author",
            None,
        );

        assert_eq!(
            cleaned,
            "What matters most for children growing up with AI?"
        );
    }

    #[test]
    fn heuristics_drop_low_signal_linkedin_job_posts() {
        let settings = UserSettings::default();
        let items = vec![FeedItem {
            id: "linkedin-session:job-1".into(),
            source: "linkedin-session".into(),
            author_name: "LinkedIn author".into(),
            author_handle: "confidential-career".into(),
            text: "Confidential 7m Apply for newly listed role. Logistics Manager. View job 5 school alumni work here Like Comment Repost Send".into(),
            source_url: "https://www.linkedin.com/feed/update/urn:li:activity:1/".into(),
            posted_at: "2026-04-22T12:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: fingerprint("Confidential 7m Apply for newly listed role. Logistics Manager. View job 5 school alumni work here Like Comment Repost Send"),
        }];

        let cleaned = heuristically_clean_items(items, &settings);
        assert!(cleaned.is_empty());
    }

    #[test]
    fn linkedin_profile_blurb_clusters_are_treated_as_low_signal() {
        let item = FeedItem {
            id: "linkedin-session:profile-1".into(),
            source: "linkedin-session".into(),
            author_name: "LinkedIn author".into(),
            author_handle: "angelajaramillo12".into(),
            text: "Angela Jaramillo | Speaker | Coach de Marcas y Talentos | Directora de RRPP"
                .into(),
            source_url: "https://www.linkedin.com/feed/update/urn:li:activity:2/".into(),
            posted_at: "2026-04-22T12:00:00Z".into(),
            raw_json: serde_json::json!({}),
            fingerprint: fingerprint(
                "Angela Jaramillo | Speaker | Coach de Marcas y Talentos | Directora de RRPP",
            ),
        };
        let cluster = TweetCluster {
            id: "cluster-1".into(),
            representative: item.clone(),
            members: vec![item],
            shared_urls: HashSet::new(),
            keywords: HashSet::new(),
        };

        assert!(is_low_signal_linkedin_cluster(&cluster));
        assert!(!fallback_decision(&cluster).keep);
    }

    #[test]
    fn home_timeline_refresh_url_targets_home_with_refresh_nonce() {
        let url = build_home_timeline_refresh_url();
        let refresh_nonce = url
            .query_pairs()
            .find(|(key, _)| key == "sift_refresh")
            .map(|(_, value)| value.to_string())
            .expect("refresh query");

        assert!(is_home_timeline_url(&url));
        assert!(!refresh_nonce.is_empty());
    }

    #[test]
    fn reddit_refresh_url_targets_home_with_refresh_nonce() {
        let url = build_reddit_home_feed_refresh_url();
        let refresh_nonce = url
            .query_pairs()
            .find(|(key, _)| key == "sift_refresh")
            .map(|(_, value)| value.to_string())
            .expect("refresh query");

        assert!(is_reddit_home_feed_url(&url));
        assert!(!refresh_nonce.is_empty());
    }

    #[test]
    fn live_session_capture_preserves_reddit_source_prefix() {
        let settings = UserSettings::default();
        let items = vec![XSessionCaptureItem {
            id: "abc123".into(),
            author_name: "r/builder".into(),
            author_handle: "builder".into(),
            text: "A thoughtful Reddit launch post".into(),
            source_url: "https://www.reddit.com/r/builder/comments/abc123/post/".into(),
            posted_at: "2026-04-16T12:02:00Z".into(),
            is_repost: false,
            is_reply: false,
            is_promoted: false,
            social_context: None,
            shared_urls: vec!["https://example.com/release".into()],
            media: Vec::new(),
        }];

        let cleaned = normalize_session_capture(items, &settings, CaptureSourceKind::Reddit);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].id, "reddit-session:abc123");
        assert_eq!(cleaned[0].source, "reddit-session");
    }

    #[test]
    fn build_view_specs_adds_reddit_view_when_reddit_is_enabled() {
        let capture = MultiCaptureOutcome {
            items: vec![FeedItem {
                id: "reddit-session:1".into(),
                source: "reddit-session".into(),
                author_name: "Reddit".into(),
                author_handle: "reddit".into(),
                text: "Post".into(),
                source_url: "https://www.reddit.com/r/test/comments/1/post/".into(),
                posted_at: "2026-04-16T12:00:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Post"),
            }],
            brand_new_count: 1,
            enabled_sources: vec![CaptureSourceKind::Reddit],
        };

        let specs = build_view_specs(&capture);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].view, EditionView::Reddit);
        assert_eq!(specs[0].label, "Reddit");
    }

    #[tokio::test]
    async fn generate_structured_stays_text_only_when_images_are_disabled() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET).path("/media/story.jpg");
            then.status(200)
                .header("content-type", "image/jpeg")
                .body(vec![1_u8, 2, 3]);
        });
        let completion = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("\"content\":\"You are SIFT, a calm editor");
            then.status(200).json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"items\":[{\"clusterId\":\"cluster-1\",\"keep\":true,\"category\":\"Tools\",\"headline\":\"Text only result\",\"summary\":\"Summary\",\"whyItMatters\":\"Why\",\"reasons\":[\"reason\"],\"imageImportant\":false,\"imageAlt\":null}]}"
                    }
                }]
            }));
        });

        let settings = sample_settings(false, server.base_url());
        let cluster = sample_cluster(vec![FeedMedia {
            url: format!("{}/media/story.jpg", server.base_url()),
            kind: "photo".into(),
        }]);
        let provider = LmStudioClient::default();
        let mut image_cache = SyncImageCache::default();

        let outcome = provider
            .generate_structured(&settings, None, &[cluster], &mut image_cache)
            .await
            .expect("structured output");

        assert!(!outcome.fell_back_to_text);
        assert_eq!(image.hits(), 0);
        assert_eq!(completion.hits(), 1);
        assert_eq!(outcome.decisions[0].headline, "Text only result");
    }

    #[tokio::test]
    async fn generate_structured_includes_image_parts_when_enabled() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET).path("/media/story.jpg");
            then.status(200)
                .header("content-type", "image/jpeg")
                .body(vec![1_u8, 2, 3]);
        });
        let completion = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("\"image_url\"");
            then.status(200).json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"items\":[{\"clusterId\":\"cluster-1\",\"keep\":true,\"category\":\"Tools\",\"headline\":\"Image aware result\",\"summary\":\"Summary\",\"whyItMatters\":\"Why\",\"reasons\":[\"reason\"],\"imageImportant\":true,\"imageAlt\":\"Screenshot of the release UI\"}]}"
                    }
                }]
            }));
        });

        let settings = sample_settings(true, server.base_url());
        let cluster = sample_cluster(vec![FeedMedia {
            url: format!("{}/media/story.jpg", server.base_url()),
            kind: "photo".into(),
        }]);
        let provider = LmStudioClient::default();
        let mut image_cache = SyncImageCache::default();

        let outcome = provider
            .generate_structured(&settings, None, &[cluster], &mut image_cache)
            .await
            .expect("structured output");

        assert_eq!(image.hits(), 1);
        assert_eq!(completion.hits(), 1);
        assert!(outcome.decisions[0].image_important);
        assert_eq!(
            outcome.decisions[0].image_alt.as_deref(),
            Some("Screenshot of the release UI")
        );
    }

    #[tokio::test]
    async fn generate_structured_falls_back_to_text_when_multimodal_request_is_rejected() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET).path("/media/story.jpg");
            then.status(200)
                .header("content-type", "image/jpeg")
                .body(vec![1_u8, 2, 3]);
        });
        let multimodal_failure = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("\"image_url\"");
            then.status(400).body("vision not supported");
        });
        let text_completion = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("\"content\":\"You are SIFT, a calm editor");
            then.status(200).json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"items\":[{\"clusterId\":\"cluster-1\",\"keep\":true,\"category\":\"Tools\",\"headline\":\"Fallback result\",\"summary\":\"Summary\",\"whyItMatters\":\"Why\",\"reasons\":[\"reason\"],\"imageImportant\":false,\"imageAlt\":null}]}"
                    }
                }]
            }));
        });

        let settings = sample_settings(true, server.base_url());
        let cluster = sample_cluster(vec![FeedMedia {
            url: format!("{}/media/story.jpg", server.base_url()),
            kind: "photo".into(),
        }]);
        let provider = LmStudioClient::default();
        let mut image_cache = SyncImageCache::default();

        let outcome = provider
            .generate_structured(&settings, None, &[cluster], &mut image_cache)
            .await
            .expect("structured output");

        assert!(outcome.fell_back_to_text);
        assert_eq!(image.hits(), 1);
        assert_eq!(multimodal_failure.hits(), 1);
        assert_eq!(text_completion.hits(), 1);
        assert_eq!(outcome.decisions[0].headline, "Fallback result");
    }

    #[tokio::test]
    async fn generate_structured_skips_multimodal_when_image_download_fails() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET).path("/media/story.jpg");
            then.status(500);
        });
        let text_completion = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("\"content\":\"You are SIFT, a calm editor");
            then.status(200).json_body(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"items\":[{\"clusterId\":\"cluster-1\",\"keep\":true,\"category\":\"Tools\",\"headline\":\"Download fallback\",\"summary\":\"Summary\",\"whyItMatters\":\"Why\",\"reasons\":[\"reason\"],\"imageImportant\":false,\"imageAlt\":null}]}"
                    }
                }]
            }));
        });

        let settings = sample_settings(true, server.base_url());
        let cluster = sample_cluster(vec![FeedMedia {
            url: format!("{}/media/story.jpg", server.base_url()),
            kind: "photo".into(),
        }]);
        let provider = LmStudioClient::default();
        let mut image_cache = SyncImageCache::default();

        let outcome = provider
            .generate_structured(&settings, None, &[cluster], &mut image_cache)
            .await
            .expect("structured output");

        assert!(!outcome.fell_back_to_text);
        assert_eq!(image.hits(), 1);
        assert_eq!(text_completion.hits(), 1);
        assert_eq!(outcome.decisions[0].headline, "Download fallback");
    }

    #[test]
    fn repeated_topics_are_grouped_before_summarizing() {
        let items = vec![
            FeedItem {
                id: "1".into(),
                source: "x-session".into(),
                author_name: "Alpha".into(),
                author_handle: "alpha".into(),
                text: "Cursor shipped background agents for code review".into(),
                source_url: "https://x.com/alpha/status/1".into(),
                posted_at: "2026-04-16T12:00:00Z".into(),
                raw_json: serde_json::json!({
                    "sharedUrls": ["https://cursor.com/changelog/agents"]
                }),
                fingerprint: fingerprint("Cursor shipped background agents for code review"),
            },
            FeedItem {
                id: "2".into(),
                source: "x-session".into(),
                author_name: "Beta".into(),
                author_handle: "beta".into(),
                text:
                    "Background agents just landed in Cursor and the code review flow looks strong"
                        .into(),
                source_url: "https://x.com/beta/status/2".into(),
                posted_at: "2026-04-16T12:01:00Z".into(),
                raw_json: serde_json::json!({
                    "sharedUrls": ["https://cursor.com/changelog/agents"]
                }),
                fingerprint: fingerprint(
                    "Background agents just landed in Cursor and the code review flow looks strong",
                ),
            },
            FeedItem {
                id: "3".into(),
                source: "x-session".into(),
                author_name: "Gamma".into(),
                author_handle: "gamma".into(),
                text: "Supabase shipped branching improvements".into(),
                source_url: "https://x.com/gamma/status/3".into(),
                posted_at: "2026-04-16T12:02:00Z".into(),
                raw_json: serde_json::json!({}),
                fingerprint: fingerprint("Supabase shipped branching improvements"),
            },
        ];

        let clusters = group_tweets(&items);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].repeat_count(), 2);
    }

    #[test]
    fn persist_downloaded_image_writes_asset_metadata() {
        let temp_dir = tempdir().expect("temporary dir");
        let image = DownloadedImage {
            source_url: "https://pbs.twimg.com/media/story.jpg?format=jpg&name=large".into(),
            mime_type: "image/jpeg".into(),
            bytes: vec![1_u8, 2, 3, 4],
            data_url: "data:image/jpeg;base64,AQIDBA==".into(),
        };

        let persisted = persist_downloaded_image(
            temp_dir.path(),
            "edition-1",
            "item-1",
            &image,
            "Screenshot of the release UI",
        )
        .expect("persisted image");

        assert!(Path::new(&persisted.path).exists());
        assert_eq!(persisted.source_url, image.source_url);
        assert_eq!(persisted.mime_type, "image/jpeg");
        assert_eq!(persisted.alt, "Screenshot of the release UI");
    }

    #[test]
    fn lead_image_alt_prefers_model_alt_when_present() {
        let record = ClusterEditorialRecord {
            cluster: sample_cluster(vec![]),
            decision: ClusterDecision {
                cluster_id: "cluster-1".into(),
                keep: true,
                category: "Tools".into(),
                headline: "Fallback headline".into(),
                summary: "Summary".into(),
                why_it_matters: "Why".into(),
                reasons: vec!["reason".into()],
                image_important: true,
                image_alt: Some("Screenshot of the release UI".into()),
            },
        };

        assert_eq!(lead_image_alt(&record), "Screenshot of the release UI");
    }

    #[test]
    fn lead_image_alt_falls_back_to_headline_without_analysis_alt() {
        let record = ClusterEditorialRecord {
            cluster: sample_cluster(vec![]),
            decision: ClusterDecision {
                cluster_id: "cluster-1".into(),
                keep: true,
                category: "Tools".into(),
                headline: "Fallback headline".into(),
                summary: "Summary".into(),
                why_it_matters: "Why".into(),
                reasons: vec!["reason".into()],
                image_important: false,
                image_alt: None,
            },
        };

        assert_eq!(lead_image_alt(&record), "Fallback headline");
    }

    #[tokio::test]
    async fn persist_record_lead_image_keeps_post_image_without_analysis_flag() {
        let server = MockServer::start();
        let image = server.mock(|when, then| {
            when.method(GET).path("/media/story.jpg");
            then.status(200)
                .header("content-type", "image/jpeg")
                .body(vec![1_u8, 2, 3]);
        });
        let temp_dir = tempdir().expect("temporary dir");
        let record = ClusterEditorialRecord {
            cluster: sample_cluster(vec![FeedMedia {
                url: format!("{}/media/story.jpg", server.base_url()),
                kind: "photo".into(),
            }]),
            decision: ClusterDecision {
                cluster_id: "cluster-1".into(),
                keep: true,
                category: "Tools".into(),
                headline: "Fallback headline".into(),
                summary: "Summary".into(),
                why_it_matters: "Why".into(),
                reasons: vec!["reason".into()],
                image_important: false,
                image_alt: None,
            },
        };
        let mut image_cache = SyncImageCache::default();

        let persisted = persist_record_lead_image(
            temp_dir.path(),
            &reqwest::Client::new(),
            "edition-1",
            &mut image_cache,
            &record,
        )
        .await
        .expect("persist lead image")
        .expect("lead image to exist");

        assert_eq!(image.hits(), 1);
        assert!(Path::new(&persisted.path).exists());
        assert_eq!(persisted.alt, "Fallback headline");
    }
}
