use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ScheduleCadence {
    #[default]
    Daily,
    Interval,
}

fn default_schedule_cadence() -> ScheduleCadence {
    ScheduleCadence::Daily
}

fn default_schedule_interval_hours() -> usize {
    1
}

fn default_schedule_rule_window_start() -> String {
    "09:00".into()
}

fn default_schedule_rule_window_end() -> String {
    "17:00".into()
}

fn default_schedule_rule_label() -> String {
    "Morning brief".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRule {
    pub id: String,
    #[serde(default = "default_schedule_rule_label")]
    pub label: String,
    pub enabled: bool,
    #[serde(default = "default_schedule_cadence")]
    pub cadence: ScheduleCadence,
    pub time_of_day: String,
    #[serde(default = "default_schedule_interval_hours")]
    pub interval_hours: usize,
    #[serde(default = "default_schedule_rule_window_start")]
    pub window_start: String,
    #[serde(default = "default_schedule_rule_window_end")]
    pub window_end: String,
    #[serde(default)]
    pub browse_page_count: CaptureBrowsePageCount,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleSettings {
    #[serde(default = "default_schedule_rules")]
    pub rules: Vec<ScheduleRule>,
    pub timezone: String,
}

fn default_schedule_rules() -> Vec<ScheduleRule> {
    vec![ScheduleRule::default()]
}

impl Default for ScheduleRule {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            label: default_schedule_rule_label(),
            enabled: true,
            cadence: ScheduleCadence::Daily,
            time_of_day: "07:30".into(),
            interval_hours: default_schedule_interval_hours(),
            window_start: default_schedule_rule_window_start(),
            window_end: default_schedule_rule_window_end(),
            browse_page_count: CaptureBrowsePageCount::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ScheduleSettingsWire {
    Current(CurrentScheduleSettings),
    Legacy(LegacyScheduleSettings),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CurrentScheduleSettings {
    #[serde(default = "default_schedule_rules")]
    rules: Vec<ScheduleRule>,
    timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyScheduleSettings {
    enabled: bool,
    #[serde(default = "default_schedule_cadence")]
    cadence: ScheduleCadence,
    time_of_day: String,
    #[serde(default = "default_schedule_interval_hours")]
    interval_hours: usize,
    timezone: String,
}

impl<'de> Deserialize<'de> for ScheduleSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match ScheduleSettingsWire::deserialize(deserializer)? {
            ScheduleSettingsWire::Current(current) => Ok(Self {
                rules: current.rules,
                timezone: current.timezone,
            }),
            ScheduleSettingsWire::Legacy(legacy) => Ok(Self {
                rules: vec![ScheduleRule {
                    enabled: legacy.enabled,
                    cadence: legacy.cadence,
                    time_of_day: legacy.time_of_day,
                    interval_hours: legacy.interval_hours,
                    label: match legacy.cadence {
                        ScheduleCadence::Daily => "Morning brief".into(),
                        ScheduleCadence::Interval => "Recurring brief".into(),
                    },
                    ..ScheduleRule::default()
                }],
                timezone: legacy.timezone,
            }),
        }
    }
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
                rules: default_schedule_rules(),
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
    pub run_id: String,
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
pub struct SyncRunTimings {
    #[serde(default)]
    pub capture_ms: u64,
    #[serde(default)]
    pub ranking_ms: u64,
    #[serde(default)]
    pub front_page_ms: u64,
    #[serde(default)]
    pub saving_ms: u64,
    #[serde(default)]
    pub total_ms: u64,
}

impl Default for SyncRunTimings {
    fn default() -> Self {
        Self {
            capture_ms: 0,
            ranking_ms: 0,
            front_page_ms: 0,
            saving_ms: 0,
            total_ms: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    pub id: String,
    pub reason: SyncReason,
    #[serde(default)]
    pub schedule_rule_id: Option<String>,
    #[serde(default)]
    pub schedule_rule_label: Option<String>,
    #[serde(default)]
    pub schedule_slot_key: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: SyncStatus,
    pub item_count: usize,
    pub kept_count: usize,
    pub error_message: Option<String>,
    pub edition_id: Option<String>,
    #[serde(default)]
    pub timings: SyncRunTimings,
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
    pub run_history: Vec<SyncRun>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
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
