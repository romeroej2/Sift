use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn local_timezone_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupSettings {
    pub hide_replies: bool,
    pub hide_retweets: bool,
    pub remove_bait: bool,
    pub muted_keywords: Vec<String>,
    pub muted_authors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LmStudioSettings {
    pub base_url: String,
    pub auth_token: Option<String>,
    pub selected_model: Option<String>,
    #[serde(default)]
    pub include_images: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourcesSettings {
    #[serde(default = "default_true")]
    pub x: bool,
    #[serde(default)]
    pub linkedin: bool,
    #[serde(default)]
    pub reddit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSettings {
    #[serde(default)]
    pub sources: CaptureSourcesSettings,
    #[serde(default)]
    pub browse_page_count: CaptureBrowsePageCount,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureBrowsePageCount {
    #[serde(default = "default_x_browse_page_count")]
    pub x: usize,
    #[serde(default = "default_linkedin_browse_page_count")]
    pub linkedin: usize,
    #[serde(default = "default_reddit_browse_page_count")]
    pub reddit: usize,
}

fn default_x_browse_page_count() -> usize {
    12
}

fn default_linkedin_browse_page_count() -> usize {
    8
}

fn default_reddit_browse_page_count() -> usize {
    10
}

impl Default for CaptureBrowsePageCount {
    fn default() -> Self {
        Self {
            x: default_x_browse_page_count(),
            linkedin: default_linkedin_browse_page_count(),
            reddit: default_reddit_browse_page_count(),
        }
    }
}

impl Default for CaptureSourcesSettings {
    fn default() -> Self {
        Self {
            x: true,
            linkedin: false,
            reddit: false,
        }
    }
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            sources: CaptureSourcesSettings::default(),
            browse_page_count: CaptureBrowsePageCount::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleSettings {
    pub enabled: bool,
    pub time_of_day: String,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSettings {
    pub schedule: ScheduleSettings,
    pub cleanup: CleanupSettings,
    pub lm_studio: LmStudioSettings,
    #[serde(default)]
    pub capture: CaptureSettings,
}

impl UserSettings {
    pub fn without_secrets(&self) -> Self {
        let mut sanitized = self.clone();
        sanitized.lm_studio.auth_token = None;
        sanitized
    }
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            schedule: ScheduleSettings {
                enabled: true,
                time_of_day: "07:30".into(),
                timezone: local_timezone_name(),
            },
            cleanup: CleanupSettings {
                hide_replies: true,
                hide_retweets: true,
                remove_bait: true,
                muted_keywords: Vec::new(),
                muted_authors: Vec::new(),
            },
            lm_studio: LmStudioSettings {
                base_url: "http://127.0.0.1:1234".into(),
                auth_token: None,
                selected_model: None,
                include_images: false,
            },
            capture: CaptureSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditionView {
    Consolidated,
    X,
    Linkedin,
    Reddit,
}

impl Default for EditionView {
    fn default() -> Self {
        Self::Consolidated
    }
}

impl EditionView {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Consolidated => "consolidated",
            Self::X => "x",
            Self::Linkedin => "linkedin",
            Self::Reddit => "reddit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditionCard {
    pub item_id: String,
    pub author_name: String,
    pub author_handle: String,
    pub source_url: String,
    pub posted_at: String,
    pub category: String,
    pub headline: String,
    pub summary: String,
    pub why_it_matters: String,
    #[serde(default)]
    pub lead_image: Option<EditionImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditionImage {
    pub path: String,
    pub source_url: String,
    pub mime_type: String,
    pub alt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditionSection {
    pub id: String,
    pub title: String,
    pub dek: String,
    pub cards: Vec<EditionCard>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edition {
    pub id: String,
    pub edition_date: String,
    pub title: String,
    pub front_page_summary: String,
    pub created_at: String,
    #[serde(default)]
    pub view: EditionView,
    pub sections: Vec<EditionSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedItem {
    pub id: String,
    pub source: String,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub source_url: String,
    pub posted_at: String,
    pub raw_json: serde_json::Value,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct TweetDbEntry {
    pub tweet_id: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub seen_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanedItem {
    pub item_id: String,
    pub keep: bool,
    pub category: String,
    pub headline: String,
    pub summary: String,
    pub why_it_matters: String,
    pub reasons: Vec<String>,
    pub author_name: String,
    pub author_handle: String,
    pub source_url: String,
    pub posted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: SyncStatus,
    pub item_count: usize,
    pub kept_count: usize,
    pub error_message: Option<String>,
    pub edition_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncStatus {
    Idle,
    Running,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XConnectionSummary {
    pub user_id: String,
    pub handle: String,
    pub name: String,
    pub connected_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSessionState {
    pub is_open: bool,
    pub is_visible: bool,
    pub is_authenticated: bool,
    pub last_known_url: Option<String>,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedBrowserSession {
    pub last_known_url: String,
    pub is_authenticated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    pub settings: UserSettings,
    pub editions: Vec<Edition>,
    pub latest_run: Option<SyncRun>,
    pub x_connection: Option<XConnectionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XClientConfigDraft {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XConnectLaunch {
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XConnectPayload {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub handle: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XConnectPollResult {
    pub status: PollStatus,
    pub error_message: Option<String>,
    pub payload: Option<XConnectPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PollStatus {
    Pending,
    Success,
    Error,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LmStudioHealth {
    pub ok: bool,
    pub server_label: String,
    pub models: Vec<ModelDescriptor>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct OAuthSession {
    pub state: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub code_verifier: String,
    pub redirect_uri: String,
    pub created_at: DateTime<Utc>,
    pub result: Option<XConnectPollResult>,
}

#[derive(Debug, Clone)]
pub enum SyncReason {
    Manual,
    Scheduled,
}

impl SyncReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
        }
    }
}
