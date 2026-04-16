use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::net::TcpListener;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use regex::Regex;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep, timeout};
use url::Url;
use uuid::Uuid;

use crate::models::{
    CleanedItem, Edition, EditionCard, EditionSection, FeedItem, LmStudioHealth, ModelDescriptor,
    OAuthSession, PollStatus, SyncReason, SyncRun, SyncStatus, UserSettings, XClientConfigDraft,
    XConnectLaunch, XConnectPayload, XConnectPollResult,
};
use crate::{AppError, AppState, is_x_domain};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XSessionCapturePayload {
    pub request_id: String,
    pub current_url: String,
    pub items: Vec<XSessionCaptureItem>,
    pub error: Option<String>,
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
    pub social_context: Option<String>,
    pub shared_urls: Vec<String>,
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
const CAPTURE_MAX_PASSES: usize = 120;
const CAPTURE_STABLE_PASSES: usize = 10;
const CAPTURE_EXHAUSTED_PASSES: usize = 18;
const CAPTURE_WAIT_FOR_ADVANCE_MS: u64 = 5_000;
const CAPTURE_TIMEOUT_SECS: u64 = 480;
const CAPTURE_IDLE_TIMEOUT_SECS: u64 = 90;
const LM_BATCH_SIZE: usize = 6;
const LM_BATCH_MAX_ATTEMPTS: usize = 3;
const LM_STUDIO_REQUEST_TIMEOUT_SECS: u64 = 15;
const LM_STUDIO_COMPLETION_TIMEOUT_SECS: u64 = 600;
const MAX_DIGEST_ITEMS: usize = 12;

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

fn format_capture_progress_message(progress: &XSessionCaptureProgressPayload) -> String {
    let mut message = if progress.fresh_count > 0 {
        format!(
            "Collecting posts from the live X session. Pass {}/{} · {} fresh so far.",
            progress.pass, progress.total_passes, progress.fresh_count
        )
    } else {
        format!(
            "Collecting posts from the live X session. Pass {}/{} · still scanning for fresh posts.",
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
    progress: Option<&XSessionCaptureProgressSnapshot>,
) -> String {
    if let Some(progress) = progress {
        format!(
            "Timed out waiting for the live X session to finish collecting feed items after pass {}/{}. Last heartbeat: {} captured, {} fresh at {}.",
            progress.pass,
            progress.total_passes,
            progress.item_count,
            progress.fresh_count,
            progress.current_url
        )
    } else {
        "Timed out waiting for the live X session to return feed items.".into()
    }
}

fn format_capture_idle_timeout_message(
    progress: Option<&XSessionCaptureProgressSnapshot>,
) -> String {
    if let Some(progress) = progress {
        format!(
            "The live X session stopped reporting progress after pass {}/{}. Last heartbeat: {} captured, {} fresh at {}.",
            progress.pass,
            progress.total_passes,
            progress.item_count,
            progress.fresh_count,
            progress.current_url
        )
    } else {
        "Timed out waiting for the live X session to start returning feed items.".into()
    }
}

pub(crate) fn emit_capture_progress(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    progress: &XSessionCaptureProgressPayload,
) {
    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "capturing-feed",
        format_capture_progress_message(progress),
        Some(progress.item_count),
        Some(progress.fresh_count),
        None,
        None,
    );
}

#[async_trait]
pub trait FeedSource {
    async fn connect(&self, _config: &XClientConfigDraft) -> Result<(), AppError>;
    async fn disconnect(&self) -> Result<(), AppError>;
}

#[async_trait]
pub trait LocalModelProvider {
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
    ) -> Result<Vec<ClusterDecision>, AppError>;
    async fn generate_text(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        prompt: &str,
    ) -> Result<String, AppError>;
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
    ) -> Result<Vec<ClusterDecision>, AppError> {
        let model =
            settings.lm_studio.selected_model.clone().ok_or_else(|| {
                AppError::Message("Select an LM Studio model before syncing.".into())
            })?;

        let mut prompt = String::from(
            "You are SIFT, a calm editor that turns repeated X chatter into a concise daily briefing.\n",
        );
        prompt.push_str("Return strict JSON with this shape: {\"items\":[{\"clusterId\":\"...\",\"keep\":true,\"category\":\"Releases|Tools|Infrastructure|Ideas|People\",\"headline\":\"...\",\"summary\":\"...\",\"whyItMatters\":\"...\",\"reasons\":[\"...\"]}]}\n");
        prompt.push_str("Rules: keep only the most important clusters. Prefer repeated topics across independent authors, concrete releases, notable tools, useful ideas, and things that feel widely discussed for a reason. It is good to drop most clusters. Avoid outrage, bait, empty self-promotion, and duplicates.\n");
        prompt.push_str("Use neutral headlines under 14 words and summaries under 42 words.\n");
        prompt.push_str("Input clusters:\n");

        for cluster in clusters {
            let _ = writeln!(
                prompt,
                "- clusterId: {}\n  repeats: {}\n  uniqueAuthors: {}\n  sharedUrls: {}\n  keywords: {}\n  representative: {} (@{})\n  sampleTweets:\n{}",
                cluster.id,
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

        let content = self
            .chat_completion(
                &settings.lm_studio.base_url,
                auth_token,
                &model,
                &prompt,
                0.2,
            )
            .await?;

        let parsed = extract_json_segment(&content).map_err(|error| {
            AppError::Message(format!(
                "{error} Sample response: {}",
                truncate_chars(&content, 240)
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

    async fn generate_text(
        &self,
        settings: &UserSettings,
        auth_token: Option<&str>,
        prompt: &str,
    ) -> Result<String, AppError> {
        let model =
            settings.lm_studio.selected_model.clone().ok_or_else(|| {
                AppError::Message("Select an LM Studio model before syncing.".into())
            })?;

        self.chat_completion(
            &settings.lm_studio.base_url,
            auth_token,
            &model,
            prompt,
            0.3,
        )
        .await
    }
}

impl LmStudioClient {
    async fn chat_completion(
        &self,
        base_url: &str,
        auth_token: Option<&str>,
        model: &str,
        prompt: &str,
        temperature: f32,
    ) -> Result<String, AppError> {
        let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
        let body = serde_json::json!({
          "model": model,
          "temperature": temperature,
          "messages": [
            { "role": "system", "content": "You are a meticulous editor. Reply with exactly what was requested." },
            { "role": "user", "content": prompt }
          ]
        });

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
) -> Result<crate::models::BootstrapState, AppError> {
    let _sync_guard = state.sync_guard.lock().await;
    let settings = state.db.load_settings()?;
    let lm_studio_auth_token = state.lm_studio_auth_token().await;
    let edition_date = current_edition_date(&settings)?;

    if matches!(reason, SyncReason::Scheduled) && state.db.has_edition_for_date(&edition_date)? {
        return state.db.load_bootstrap();
    }

    let mut run = SyncRun {
        id: Uuid::new_v4().to_string(),
        started_at: Utc::now().to_rfc3339(),
        finished_at: None,
        status: SyncStatus::Running,
        item_count: 0,
        kept_count: 0,
        error_message: None,
        edition_id: None,
    };
    state.db.insert_sync_run(&run)?;
    emit_sync_progress(
        state,
        &run.id,
        &reason,
        SyncStatus::Running,
        "starting",
        "Refresh started. Checking the live X session.",
        None,
        None,
        None,
        None,
    );

    let sync_result = async {
        let capture = match collect_items_from_live_session(state, &settings, &run.id, &reason).await {
            Ok(capture) => capture,
            Err(AppError::NoFreshItems { message }) => {
                run.status = SyncStatus::Success;
                run.finished_at = Some(Utc::now().to_rfc3339());
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
        let brand_new_count = capture.brand_new_count;
        let resurfaced_count = capture.resurfaced_count;
        let fresh_breakdown =
            format_fresh_tweet_breakdown(capture.items.len(), brand_new_count, resurfaced_count);
        let items = capture.items;
        let clusters = group_tweets(&items);
        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Running,
            "ranking-items",
            format!(
                "Prepared {fresh_breakdown} into {} topic clusters. Sending them to LM Studio for ranking.",
                clusters.len()
            ),
            Some(items.len()),
            Some(brand_new_count),
            None,
            None,
        );

        let provider = LmStudioClient::default();
        let decisions = batch_decide(
            state,
            &run.id,
            &reason,
            &provider,
            &settings,
            lm_studio_auth_token.as_deref(),
            &clusters,
        )
        .await?;
        let kept = keep_useful(&decisions);
        let kept_items = kept
            .iter()
            .map(ClusterEditorialRecord::to_cleaned_item)
            .collect::<Vec<_>>();
        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Running,
            "building-edition",
            format!("Kept {} digest topics. Writing the edition.", kept.len()),
            Some(items.len()),
            Some(brand_new_count),
            Some(kept.len()),
            None,
        );
        let (_edition_date, edition) = build_edition(
            state,
            &run.id,
            &reason,
            &provider,
            &settings,
            lm_studio_auth_token.as_deref(),
            &kept_items,
        )
        .await?;
        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Running,
            "saving-edition",
            "Saving the edition locally and updating the desk.",
            Some(items.len()),
            Some(brand_new_count),
            Some(kept.len()),
            Some(&edition.id),
        );

        run.item_count = items.len();
        run.kept_count = kept.len();
        run.edition_id = Some(edition.id.clone());
        run.status = SyncStatus::Success;
        run.finished_at = Some(Utc::now().to_rfc3339());

        let decision_items = decisions
            .iter()
            .map(ClusterEditorialRecord::to_cleaned_item)
            .collect::<Vec<_>>();
        state.db.save_edition(&edition, &decision_items, &run)?;
        state.db.insert_sync_run(&run)?;
        notify_sync(state, &reason, &edition.title).await;
        emit_sync_progress(
            state,
            &run.id,
            &reason,
            SyncStatus::Success,
            "complete",
            format!(
                "Fresh edition generated: {}. {fresh_breakdown}; kept {} digest topics.",
                edition.title, run.kept_count
            ),
            Some(run.item_count),
            Some(brand_new_count),
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
    if !settings.schedule.enabled || !should_run_now(&settings)? {
        return Ok(());
    }

    if state
        .db
        .has_edition_for_date(&current_edition_date(&settings)?)?
    {
        return Ok(());
    }

    let session_window_open = state
        .app
        .get_webview_window(crate::X_SESSION_WINDOW_LABEL)
        .is_some();
    let session_authenticated = *state.x_session_authenticated.read().await;
    if !session_window_open || !session_authenticated {
        return Ok(());
    }

    let _ = generate_paper(state, SyncReason::Scheduled).await?;

    Ok(())
}

pub async fn run_scheduler(state: AppState) {
    loop {
        let _ = maybe_run_scheduled_sync(&state).await;
        sleep(Duration::from_secs(60)).await;
    }
}

async fn batch_decide(
    state: &AppState,
    run_id: &str,
    reason: &SyncReason,
    provider: &LmStudioClient,
    settings: &UserSettings,
    auth_token: Option<&str>,
    clusters: &[TweetCluster],
) -> Result<Vec<ClusterEditorialRecord>, AppError> {
    let mut decisions = Vec::new();
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
                "Ranking batch {batch_number}/{total_batches} in LM Studio ({} topic clusters).",
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
                .generate_structured(settings, auth_token, batch)
                .await
            {
                Ok(mut result) => {
                    println!(
                        "[sift-sync:{run_id}] ranking batch {batch_number}/{total_batches} succeeded on attempt {attempt}"
                    );
                    decisions.extend(result.drain(..).filter_map(|decision| {
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
                            "LM Studio batch {batch_number}/{total_batches} failed on attempt {attempt}/{LM_BATCH_MAX_ATTEMPTS}. Retrying in {wait_seconds}s."
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
                            "LM Studio batch {batch_number}/{total_batches} failed after {attempt} attempts. Falling back to local heuristics for these {} topic clusters.",
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

    Ok(decisions)
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
    provider: &LmStudioClient,
    settings: &UserSettings,
    auth_token: Option<&str>,
    items: &[CleanedItem],
) -> Result<(String, Edition), AppError> {
    let edition_date = current_edition_date(settings)?;
    let mut sections = BTreeMap::<String, Vec<CleanedItem>>::new();

    for item in items {
        sections
            .entry(normalize_category(&item.category))
            .or_default()
            .push(item.clone());
    }

    let section_list = sections
        .into_iter()
        .map(|(title, mut items)| {
            items.sort_by(|left, right| right.posted_at.cmp(&left.posted_at));
            EditionSection {
                id: title.to_lowercase().replace(' ', "-"),
                dek: format!("{} worth your attention", title),
                title: title.clone(),
                cards: items
                    .into_iter()
                    .map(|item| EditionCard {
                        item_id: item.item_id,
                        author_name: item.author_name,
                        author_handle: item.author_handle,
                        source_url: item.source_url,
                        posted_at: item.posted_at,
                        category: item.category,
                        headline: item.headline,
                        summary: item.summary,
                        why_it_matters: item.why_it_matters,
                    })
                    .collect(),
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
            items.len(),
            section_list.len()
        ),
        None,
        None,
        Some(items.len()),
        None,
    );

    let front_page_prompt = format!(
        "Write a calm front-page summary in 2 sentences for this SIFT edition. Focus on launches, tools, and notable ideas.\n{}",
        items
            .iter()
            .take(8)
            .map(|item| format!("{}: {}", item.headline, item.summary))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let front_page_summary = match provider
        .generate_text(settings, auth_token, &front_page_prompt)
        .await
    {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("[sift-sync:{run_id}] front-page draft fallback: {error}");
            emit_sync_progress(
                state,
                run_id,
                reason,
                SyncStatus::Running,
                "building-edition",
                "LM Studio could not draft the front page. Using a local fallback summary.",
                None,
                None,
                Some(items.len()),
                None,
            );
            items
                .iter()
                .take(3)
                .map(|item| item.summary.clone())
                .collect::<Vec<_>>()
                .join(" ")
        }
    };

    let title = format!("Your SIFT for {}", edition_date);
    Ok((
        edition_date.clone(),
        Edition {
            id: Uuid::new_v4().to_string(),
            edition_date,
            title,
            front_page_summary,
            created_at: Utc::now().to_rfc3339(),
            sections: section_list,
        },
    ))
}

pub fn should_run_now(settings: &UserSettings) -> Result<bool, AppError> {
    let timezone: Tz = settings.schedule.timezone.parse().unwrap_or(chrono_tz::UTC);
    let now = Utc::now().with_timezone(&timezone);
    let schedule_time = NaiveTime::parse_from_str(&settings.schedule.time_of_day, "%H:%M")
        .unwrap_or_else(|_| NaiveTime::from_hms_opt(7, 30, 0).expect("default time"));

    Ok(now.time() >= schedule_time)
}

pub fn current_edition_date(settings: &UserSettings) -> Result<String, AppError> {
    let timezone: Tz = settings.schedule.timezone.parse().unwrap_or(chrono_tz::UTC);
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

        cleaned.push(item);
    }

    cleaned
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

fn format_fresh_tweet_breakdown(
    fresh_count: usize,
    brand_new_count: usize,
    resurfaced_count: usize,
) -> String {
    let mut parts = vec![format!("{brand_new_count} brand-new")];
    if resurfaced_count > 0 {
        parts.push(format!("{resurfaced_count} resurfaced"));
    }

    format!("{fresh_count} fresh tweets ({})", parts.join(", "))
}

async fn collect_items_from_live_session(
    state: &AppState,
    settings: &UserSettings,
    run_id: &str,
    reason: &SyncReason,
) -> Result<CaptureOutcome, AppError> {
    let boundary = CaptureBoundary {
        edition_date: current_edition_date(settings)?,
        since_timestamp: state
            .db
            .load_latest_edition()?
            .map(|edition| edition.created_at),
    };
    let timezone = settings.schedule.timezone.parse().unwrap_or(chrono_tz::UTC);
    let window = ensure_live_x_session_on_home(state, run_id, reason).await?;
    let request_id = Uuid::new_v4().to_string();
    let (sender, receiver) = oneshot::channel::<Result<XSessionCapturePayload, String>>();
    state.x_session_capture_requests.lock().await.insert(
        request_id.clone(),
        XSessionCaptureRequest {
            run_id: run_id.to_string(),
            reason: reason.clone(),
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
        "Collecting posts from the live X session.",
        None,
        None,
        None,
        None,
    );

    let capture_script = format!(
        "window.__SIFT_COLLECT_FEED__({request_id}, {options});",
        request_id = serde_json::to_string(&request_id)?,
        options = serde_json::to_string(&serde_json::json!({
            "editionDate": boundary.edition_date.clone(),
            "sinceTimestamp": boundary.since_timestamp,
            "timeZone": settings.schedule.timezone,
            "maxItems": CAPTURE_MAX_ITEMS,
            "targetFreshItems": CAPTURE_TARGET_FRESH_ITEMS,
            "maxPasses": CAPTURE_MAX_PASSES,
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
            "SIFT could not start the live X capture: {error}"
        )));
    }

    let capture_started_at = Instant::now();
    let mut receiver = receiver;
    let capture = loop {
        let (last_progress_at, latest_progress) = {
            let requests = state.x_session_capture_requests.lock().await;
            let request = requests.get(&request_id).ok_or_else(|| {
                AppError::Message(
                    "The live X capture request disappeared before the page responded.".into(),
                )
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
                return Err(AppError::Message(
                    "The live X capture finished before SIFT could receive the results.".into(),
                ));
            }
            Err(_) => continue,
        }
    };

    let raw_count = capture.items.len();
    let cleaned_items = normalize_session_capture(capture.items, settings);
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
    let fresh_breakdown = format_fresh_tweet_breakdown(
        fresh_items.len(),
        fresh_brand_new_count,
        fresh_seen_again_count,
    );

    emit_sync_progress(
        state,
        run_id,
        reason,
        SyncStatus::Running,
        "capturing-feed",
        format!(
            "Captured {raw_count} posts from X. {fresh_breakdown} remain {} after cleanup.{}{}",
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
            }
        ),
        Some(raw_count),
        Some(fresh_brand_new_count),
        None,
        None,
    );

    if cleaned_count == 0 {
        return Err(AppError::Message(format!(
            "SIFT captured {raw_count} posts, but none of them survived your cleanup filters. Keep doomscrolling a bit longer and try again."
        )));
    }

    if fresh_items.is_empty() {
        return Err(AppError::NoFreshItems {
            message: format!(
                "SIFT cleaned {cleaned_count} tweets, but none of them were fresh {}.",
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

fn normalize_session_capture(
    captured_items: Vec<XSessionCaptureItem>,
    settings: &UserSettings,
) -> Vec<FeedItem> {
    let captured = captured_items
        .into_iter()
        .filter(|item| !item.id.trim().is_empty())
        .filter(|item| !item.text.trim().is_empty())
        .filter(|item| !(settings.cleanup.hide_retweets && item.is_repost))
        .filter(|item| !(settings.cleanup.hide_replies && item.is_reply))
        .map(|item| FeedItem {
            id: item.id.clone(),
            source: "x-session".into(),
            author_name: item.author_name.trim().to_string(),
            author_handle: item
                .author_handle
                .trim_start_matches('@')
                .trim()
                .to_string(),
            text: item.text.trim().to_string(),
            source_url: item.source_url.clone(),
            posted_at: item.posted_at.clone(),
            raw_json: serde_json::json!({
              "captureMode": "live-session",
              "isRepost": item.is_repost,
              "isReply": item.is_reply,
              "socialContext": item.social_context,
              "sharedUrls": item.shared_urls,
            }),
            fingerprint: fingerprint(&item.text),
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
        keep: repeated > 1 || !is_engagement_bait(&item.text),
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
    }
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
        assert!(should_run_now(&settings).is_ok());
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
                social_context: Some("Ada reposted".into()),
                shared_urls: Vec::new(),
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
                social_context: None,
                shared_urls: Vec::new(),
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
                social_context: None,
                shared_urls: vec!["https://example.com/release".into()],
            },
        ];

        let cleaned = normalize_session_capture(items, &settings);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].id, "3");
        assert_eq!(cleaned[0].author_handle, "builder");
        assert_eq!(cleaned[0].source, "x-session");
        assert_eq!(
            cleaned[0].raw_json["sharedUrls"]
                .as_array()
                .expect("shared urls array")
                .len(),
            1
        );
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
}
