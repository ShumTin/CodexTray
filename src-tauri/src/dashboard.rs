use std::sync::Mutex;

use crate::cli::{fetch_cli_quota, fetch_cli_usage, probe_codex_cli};
use crate::hook_stats::scan_hook_daily_stats;
use crate::models::{
    AccountSnapshot, DashboardMetric, DashboardSnapshot, DiagnosticItem, DiagnosticsSnapshot,
    QuotaFetchResult, QuotaSourceKind, RateLimitSnapshot, TokenActivitySnapshot,
    TokenActivitySource,
};
use crate::settings;
use crate::token_usage::{build_heatmap_days, scan_local_jsonl_usage};

#[derive(Default)]
pub struct DashboardState {
    inner: Mutex<DashboardStateInner>,
}

#[derive(Default)]
struct DashboardStateInner {
    last_success: Option<QuotaFetchResult>,
    last_token_activity: Option<TokenActivitySnapshot>,
}

impl DashboardState {
    pub fn cached_snapshot(&self) -> DashboardSnapshot {
        let inner = self.inner.lock().expect("dashboard state poisoned");
        snapshot_from_state(&inner, empty_diagnostics())
    }

    pub fn has_success(&self) -> bool {
        self.inner
            .lock()
            .expect("dashboard state poisoned")
            .last_success
            .is_some()
    }

    fn remember_success(&self, result: QuotaFetchResult) {
        let mut inner = self.inner.lock().expect("dashboard state poisoned");

        if has_account_changed(inner.last_success.as_ref(), &result) {
            inner.last_token_activity = None;
        }

        if let Some(activity) = result.token_activity.clone() {
            inner.last_token_activity = Some(activity);
        }

        inner.last_success = Some(result);
    }

    fn remember_token_activity(&self, activity: TokenActivitySnapshot) {
        self.inner
            .lock()
            .expect("dashboard state poisoned")
            .last_token_activity = Some(activity);
    }
}

pub async fn refresh_dashboard(state: &DashboardState) -> DashboardSnapshot {
    let mut diagnostics = empty_diagnostics();
    let result = fetch_cli_quota_with_diagnostics(&mut diagnostics).await;

    if let Some(result) = result {
        state.remember_success(result);
    }

    diagnostics.token_activity = DiagnosticItem::skipped("Token 活动", "后台刷新中");

    let inner = state.inner.lock().expect("dashboard state poisoned");
    snapshot_from_state(&inner, diagnostics)
}

pub async fn refresh_token_activity(state: &DashboardState) -> DashboardSnapshot {
    let mut diagnostics = empty_diagnostics();

    match fetch_cli_usage_with_diagnostics(&mut diagnostics).await {
        Some(activity) => state.remember_token_activity(activity),
        None => remember_local_jsonl_fallback(state, &mut diagnostics),
    }

    let inner = state.inner.lock().expect("dashboard state poisoned");
    snapshot_from_state(&inner, diagnostics)
}

async fn fetch_cli_quota_with_diagnostics(
    diagnostics: &mut DiagnosticsSnapshot,
) -> Option<QuotaFetchResult> {
    let probe = match probe_codex_cli(settings::configured_codex_cli_path()).await {
        Ok(probe) => {
            diagnostics.cli_probe =
                DiagnosticItem::ok("CLI 探测", format!("可启动：{}", probe.version));
            probe
        }
        Err(error) => {
            diagnostics.cli_probe = DiagnosticItem::error("CLI 探测", error);
            diagnostics.cli_app_server =
                DiagnosticItem::skipped("CLI app-server", "缺少可启动 Codex CLI");
            return None;
        }
    };

    match fetch_cli_quota(&probe).await {
        Ok(result) => {
            diagnostics.cli_app_server = DiagnosticItem::ok("CLI app-server", "账号与额度读取成功");
            Some(result)
        }
        Err(error) => {
            diagnostics.cli_app_server = DiagnosticItem::error("CLI app-server", error);
            None
        }
    }
}

async fn fetch_cli_usage_with_diagnostics(
    diagnostics: &mut DiagnosticsSnapshot,
) -> Option<TokenActivitySnapshot> {
    let probe = match probe_codex_cli(settings::configured_codex_cli_path()).await {
        Ok(probe) => {
            diagnostics.cli_probe =
                DiagnosticItem::ok("CLI 探测", format!("可启动：{}", probe.version));
            probe
        }
        Err(error) => {
            diagnostics.cli_probe = DiagnosticItem::error("CLI 探测", error);
            diagnostics.cli_app_server =
                DiagnosticItem::skipped("CLI app-server", "缺少可启动 Codex CLI");
            return None;
        }
    };

    match fetch_cli_usage(&probe).await {
        Ok(activity) => {
            diagnostics.cli_app_server = DiagnosticItem::ok("CLI app-server", "Token 活动读取成功");
            diagnostics.token_activity =
                DiagnosticItem::ok("Token 活动", "profile usage API 读取成功");
            Some(activity)
        }
        Err(error) => {
            diagnostics.cli_app_server = DiagnosticItem::error("CLI app-server", error);
            None
        }
    }
}

fn remember_local_jsonl_fallback(state: &DashboardState, diagnostics: &mut DiagnosticsSnapshot) {
    let already_has_profile_usage = state
        .inner
        .lock()
        .expect("dashboard state poisoned")
        .last_token_activity
        .as_ref()
        .map(|activity| activity.source == TokenActivitySource::ProfileUsageApi)
        .unwrap_or(false);

    if already_has_profile_usage {
        diagnostics.token_activity = DiagnosticItem::ok("Token 活动", "profile usage API 读取成功");
        return;
    }

    match scan_local_jsonl_usage() {
        Ok(activity) => {
            diagnostics.token_activity = DiagnosticItem::ok("Token 活动", "已回退本地 JSONL");
            state
                .inner
                .lock()
                .expect("dashboard state poisoned")
                .last_token_activity = Some(activity);
        }
        Err(error) => {
            diagnostics.token_activity = DiagnosticItem::error("Token 活动", error);
        }
    }
}

fn snapshot_from_state(
    inner: &DashboardStateInner,
    diagnostics: DiagnosticsSnapshot,
) -> DashboardSnapshot {
    let last_success = inner.last_success.as_ref();
    let account = last_success
        .map(|result| result.account.clone())
        .unwrap_or_else(disconnected_account);
    let quota = last_success.map(|result| {
        let mut quota = result.quota.clone();
        quota.stale = diagnostics_has_error(&diagnostics);
        quota
    });
    let source = quota_source(last_success.map(|result| &result.quota), &diagnostics);

    DashboardSnapshot {
        account,
        quota,
        last_success_source: last_success.map(|result| result.quota.source),
        source_label: source_label(source),
        diagnostics,
        metrics: metrics_from_activity(inner.last_token_activity.as_ref()),
        heatmap_days: build_heatmap_days(
            inner.last_token_activity.as_ref(),
            &scan_hook_daily_stats().unwrap_or_default(),
        ),
        token_activity_source: inner
            .last_token_activity
            .as_ref()
            .map(|activity| activity.source),
    }
}

fn has_account_changed(previous: Option<&QuotaFetchResult>, current: &QuotaFetchResult) -> bool {
    let Some(previous) = previous else {
        return false;
    };

    previous.account.email != current.account.email
}

fn quota_source(
    quota: Option<&RateLimitSnapshot>,
    diagnostics: &DiagnosticsSnapshot,
) -> Option<QuotaSourceKind> {
    let quota = quota?;

    if diagnostics_has_error(diagnostics) {
        return Some(QuotaSourceKind::Cached);
    }

    Some(quota.source)
}

fn source_label(source: Option<QuotaSourceKind>) -> String {
    match source {
        Some(QuotaSourceKind::CodexCli) => "Codex CLI".to_string(),
        Some(QuotaSourceKind::Cached) => "上次成功数据".to_string(),
        None => "暂无可用来源".to_string(),
    }
}

fn diagnostics_has_error(diagnostics: &DiagnosticsSnapshot) -> bool {
    [
        diagnostics.cli_probe.status,
        diagnostics.cli_app_server.status,
    ]
    .iter()
    .any(|status| matches!(status, crate::models::DiagnosticStatus::Error))
}

fn disconnected_account() -> AccountSnapshot {
    AccountSnapshot {
        email: None,
        plan: None,
        status: "未连接".to_string(),
        updated_at: String::new(),
    }
}

fn empty_diagnostics() -> DiagnosticsSnapshot {
    DiagnosticsSnapshot {
        cli_probe: DiagnosticItem::skipped("CLI 探测", "正在诊断"),
        cli_app_server: DiagnosticItem::skipped("CLI app-server", "正在诊断"),
        token_activity: DiagnosticItem::skipped("Token 活动", "正在诊断"),
    }
}

fn metrics_from_activity(activity: Option<&TokenActivitySnapshot>) -> Vec<DashboardMetric> {
    let Some(activity) = activity else {
        return empty_metrics();
    };

    vec![
        DashboardMetric {
            label: "累计 Token".to_string(),
            value: format_token_count(activity.lifetime_tokens),
        },
        DashboardMetric {
            label: "峰值 Token".to_string(),
            value: format_token_count(activity.peak_daily_tokens),
        },
        DashboardMetric {
            label: "最长任务时长".to_string(),
            value: format_duration(activity.longest_running_turn_sec),
        },
        DashboardMetric {
            label: "当前连续天数".to_string(),
            value: format_days(activity.current_streak_days),
        },
        DashboardMetric {
            label: "最长连续天数".to_string(),
            value: format_days(activity.longest_streak_days),
        },
    ]
}

fn empty_metrics() -> Vec<DashboardMetric> {
    vec![
        DashboardMetric {
            label: "累计 Token".to_string(),
            value: "-".to_string(),
        },
        DashboardMetric {
            label: "峰值 Token".to_string(),
            value: "-".to_string(),
        },
        DashboardMetric {
            label: "最长任务时长".to_string(),
            value: "-".to_string(),
        },
        DashboardMetric {
            label: "当前连续天数".to_string(),
            value: "-".to_string(),
        },
        DashboardMetric {
            label: "最长连续天数".to_string(),
            value: "-".to_string(),
        },
    ]
}

fn format_token_count(value: Option<u64>) -> String {
    match value {
        Some(value) if value >= 1_000 => format_compact_token_count(value),
        Some(value) => value.to_string(),
        None => "-".to_string(),
    }
}

fn format_compact_token_count(value: u64) -> String {
    let units = ["", "K", "M", "B", "T", "P"];
    let mut unit_index = 0_usize;
    let mut scaled_value = value as f64;

    while scaled_value >= 1000.0 && unit_index < units.len() - 1 {
        scaled_value /= 1000.0;
        unit_index += 1;
    }

    let amount = format!("{:.1}", scaled_value)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();

    format!("{} {}", amount, units[unit_index])
}

fn format_duration(value: Option<u64>) -> String {
    let Some(seconds) = value else {
        return "-".to_string();
    };
    let minutes = seconds / 60;
    let seconds = seconds % 60;

    if minutes > 0 {
        format!("{}分{}秒", minutes, seconds)
    } else {
        format!("{}秒", seconds)
    }
}

fn format_days(value: Option<u64>) -> String {
    value
        .map(|days| format!("{}天", days))
        .unwrap_or_else(|| "-".to_string())
}
