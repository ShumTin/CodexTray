use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuotaSourceKind {
    CodexCli,
    Cached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSnapshot {
    pub email: Option<String>,
    pub plan: Option<String>,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub label: String,
    pub remaining_percent: u8,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    pub source: QuotaSourceKind,
    pub windows: Vec<QuotaWindow>,
    pub fetched_at: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardMetric {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeatmapDay {
    pub date: String,
    pub daily_tokens: u64,
    pub weekly_tokens: u64,
    pub cumulative_tokens: u64,
    pub is_future: bool,
    pub hook_stats: Option<HookDayStats>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookDayStats {
    pub session_count: u64,
    pub prompt_count: u64,
    pub turn_count: u64,
    pub tool_call_count: u64,
    pub permission_request_count: u64,
    pub compact_count: u64,
    pub subagent_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TokenActivitySource {
    ProfileUsageApi,
    LocalJsonl,
}

#[derive(Debug, Clone)]
pub struct TokenActivitySnapshot {
    pub source: TokenActivitySource,
    pub lifetime_tokens: Option<u64>,
    pub peak_daily_tokens: Option<u64>,
    pub longest_running_turn_sec: Option<u64>,
    pub current_streak_days: Option<u64>,
    pub longest_streak_days: Option<u64>,
    pub daily_buckets: Vec<TokenActivityBucket>,
}

#[derive(Debug, Clone)]
pub struct TokenActivityBucket {
    pub date: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub account: AccountSnapshot,
    pub quota: Option<RateLimitSnapshot>,
    pub last_success_source: Option<QuotaSourceKind>,
    pub source_label: String,
    pub diagnostics: DiagnosticsSnapshot,
    pub metrics: Vec<DashboardMetric>,
    pub heatmap_days: Vec<HeatmapDay>,
    pub token_activity_source: Option<TokenActivitySource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsSnapshot {
    pub cli_probe: DiagnosticItem,
    pub cli_app_server: DiagnosticItem,
    pub token_activity: DiagnosticItem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticItem {
    pub label: String,
    pub status: DiagnosticStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticStatus {
    Ok,
    Warning,
    Error,
    Skipped,
}

impl DiagnosticItem {
    pub fn ok(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: DiagnosticStatus::Ok,
            message: message.into(),
        }
    }

    pub fn error(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: DiagnosticStatus::Error,
            message: message.into(),
        }
    }

    pub fn skipped(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            status: DiagnosticStatus::Skipped,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuotaFetchResult {
    pub account: AccountSnapshot,
    pub quota: RateLimitSnapshot,
    pub token_activity: Option<TokenActivitySnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub theme: ThemeMode,
    pub global_shortcut: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupStatus {
    pub enabled: bool,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    pub enabled: bool,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub status: DiagnosticStatus,
    pub message: String,
    pub checked_at: String,
    pub available_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub app_version: String,
    pub cli_version: Option<String>,
    pub cli_path: Option<String>,
    pub run_source: String,
    pub install_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub settings: AppSettings,
    pub startup: StartupStatus,
    pub hook: HookStatus,
    pub update: UpdateStatus,
    pub runtime: RuntimeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
}
