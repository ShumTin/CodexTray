mod cli;
mod dashboard;
mod hook_stats;
mod models;
mod settings;
mod startup_diagnostics;
mod token_usage;
mod tray_status;

use dashboard::DashboardState;
use models::{
    AppSettings, DashboardSnapshot, DiagnosticStatus, HookStatus, LogEntry, SettingsSnapshot,
    StartupStatus, UpdateStatus,
};
use startup_diagnostics::StartupDiagnosticVariant;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::menu::{CheckMenuItem, MenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::webview::Color;
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_positioner::{Position, WindowExt};
use tauri_plugin_updater::UpdaterExt;
use url::Url;

const MENU_TOGGLE_PANEL: &str = "toggle_panel";
const MENU_SETTINGS: &str = "settings";
const MENU_REFRESH: &str = "refresh";
const MENU_TOGGLE_QUOTA_WIDGET: &str = "toggle_quota_widget";
const TRAY_ID: &str = "codextray-main";
const QUOTA_WIDGET_LABEL: &str = "quota-widget";
const EVENT_DASHBOARD_REFRESH_STARTED: &str = "codextray://dashboard-refresh-started";
const EVENT_DASHBOARD_REFRESHED: &str = "codextray://dashboard-refreshed";
const MENU_QUIT: &str = "quit";
const AUTO_REFRESH_INTERVAL_SECONDS: u64 = 60;
const AUTO_REFRESH_CHECK_SECONDS: u64 = 5;
const AUTO_UPDATE_CHECK_INTERVAL_SECONDS: u64 = 60 * 60;
const STARTUP_REFRESH_DELAY_SECONDS: u64 = 3;
const PANEL_HEIGHT: f64 = 468.0;
const PANEL_SHADOW_MARGIN: f64 = 16.0;
const DETAIL_CARD_WIDTH: f64 = 244.0;
const DETAIL_CARD_HEIGHT: f64 = 237.0;
const DETAIL_SHADOW_MARGIN: f64 = 14.0;
const DETAIL_WIDTH: f64 = DETAIL_CARD_WIDTH + DETAIL_SHADOW_MARGIN * 2.0;
const DETAIL_HEIGHT: f64 = DETAIL_CARD_HEIGHT + DETAIL_SHADOW_MARGIN * 2.0;
const DETAIL_GAP: f64 = 8.0;
const PANEL_WORK_AREA_MARGIN: i32 = 8;
const QUOTA_WIDGET_WIDTH: f64 = 160.0;
const QUOTA_WIDGET_HEIGHT: f64 = 48.0;
const QUOTA_WIDGET_TOP_MARGIN: i32 = 96;
const QUOTA_WIDGET_EDGE_REVEAL_SIZE: i32 = 12;
const QUOTA_WIDGET_EDGE_GAP: i32 = 8;
const QUOTA_WIDGET_SNAP_DISTANCE: i32 = 20;
const QUOTA_WIDGET_ANIMATION_DURATION_MILLIS: u64 = 180;
const QUOTA_WIDGET_ANIMATION_FRAMES: u64 = 12;
static LAST_DASHBOARD_REFRESH_STARTED_AT: AtomicU64 = AtomicU64::new(0);
static DASHBOARD_REFRESH_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static LAST_TRAY_ICON_STATE: Mutex<Option<tray_status::TrayIconState>> = Mutex::new(None);
static QUOTA_WIDGET_MOVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static QUOTA_WIDGET_ANIMATION_GENERATION: AtomicU64 = AtomicU64::new(0);
static QUOTA_WIDGET_IGNORE_MOVES_UNTIL: AtomicU64 = AtomicU64::new(0);
static QUOTA_WIDGET_EDGE: Mutex<Option<QuotaWidgetEdge>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuotaWidgetEdge {
    Left,
    Right,
}

enum DashboardRefreshLog {
    Record(&'static str),
    Silent,
}

pub fn run_hook_event_process() -> Result<(), String> {
    hook_stats::run_hook_event_process()
}

pub fn initialize_startup_diagnostics() {
    startup_diagnostics::initialize();
}

fn toggle_panel(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    if window.is_visible().unwrap_or(false) {
        hide_panel_windows(app);
        return;
    }

    let _ = window
        .as_ref()
        .window()
        .move_window_constrained(Position::TrayRight);
    constrain_window_to_work_area(&window);
    let _ = window.show();
    constrain_window_to_work_area(&window);
    sync_cached_dashboard_to_panel(app);
    refresh_dashboard_if_empty(app);
    let _ = window.set_focus();
}

fn show_settings_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    let Ok(window) = WebviewWindowBuilder::new(
        app,
        "settings",
        WebviewUrl::App("index.html?view=settings".into()),
    )
    .title("CodexTray 设置")
    .inner_size(660.0, 540.0)
    .min_inner_size(620.0, 500.0)
    .resizable(true)
    .decorations(true)
    .transparent(false)
    .background_color(Color(248, 250, 252, 255))
    .skip_taskbar(false)
    .build() else {
        settings::append_log("ERROR", "设置窗口创建失败");
        return;
    };

    let _ = window.set_focus();
}

fn toggle_quota_widget(app: &tauri::AppHandle) -> Result<bool, String> {
    let enabled = !settings::read_settings().quota_widget_enabled;
    settings::set_quota_widget_enabled(enabled)?;

    let window = app
        .get_webview_window(QUOTA_WIDGET_LABEL)
        .ok_or_else(|| "额度悬浮窗不存在".to_string())?;

    if enabled {
        window
            .show()
            .map_err(|error| format!("额度悬浮窗显示失败：{}", error))?;
    } else {
        window
            .hide()
            .map_err(|error| format!("额度悬浮窗隐藏失败：{}", error))?;
    }

    Ok(enabled)
}

fn position_quota_widget_at_startup(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let work_area = monitor.work_area();
    let x = quota_widget_startup_x(work_area.position.x, work_area.size.width);
    let y = work_area.position.y + QUOTA_WIDGET_TOP_MARGIN;

    set_quota_widget_edge(QuotaWidgetEdge::Right);
    move_quota_widget(window, x, y);
}

fn quota_widget_startup_x(work_area_left: i32, work_area_width: u32) -> i32 {
    work_area_left + work_area_width as i32 - QUOTA_WIDGET_EDGE_REVEAL_SIZE
}

fn bind_quota_widget_edge_hiding(window: &WebviewWindow) {
    let app = window.app_handle().clone();
    window.on_window_event(move |event| {
        if !matches!(event, WindowEvent::Moved(_))
            || current_epoch_millis() < QUOTA_WIDGET_IGNORE_MOVES_UNTIL.load(Ordering::Relaxed)
        {
            return;
        }

        let generation = QUOTA_WIDGET_MOVE_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(420)).await;
            if QUOTA_WIDGET_MOVE_GENERATION.load(Ordering::Relaxed) == generation {
                settle_quota_widget_at_edge(&app);
            }
        });
    });
}

fn settle_quota_widget_at_edge(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window(QUOTA_WIDGET_LABEL) else {
        return;
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let work_area = monitor.work_area();
    let left = work_area.position.x;
    let right = left + work_area.size.width as i32;
    let touches_left = position.x <= left + QUOTA_WIDGET_SNAP_DISTANCE;
    let touches_right = position.x + size.width as i32 >= right - QUOTA_WIDGET_SNAP_DISTANCE;

    let edge = match (touches_left, touches_right) {
        (false, false) => return,
        (true, false) => QuotaWidgetEdge::Left,
        (false, true) => QuotaWidgetEdge::Right,
        (true, true) => {
            let left_distance = position.x.abs_diff(left);
            let right_distance = (position.x + size.width as i32).abs_diff(right);
            if left_distance <= right_distance {
                QuotaWidgetEdge::Left
            } else {
                QuotaWidgetEdge::Right
            }
        }
    };

    hide_quota_widget_for_edge(&window, edge);
}

fn hide_quota_widget_for_edge(window: &WebviewWindow, edge: QuotaWidgetEdge) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let work_area = monitor.work_area();
    let left = work_area.position.x;
    let right = left + work_area.size.width as i32;
    let top = work_area.position.y;
    let bottom = top + work_area.size.height as i32;
    let x = match edge {
        QuotaWidgetEdge::Left => left - size.width as i32 + QUOTA_WIDGET_EDGE_REVEAL_SIZE,
        QuotaWidgetEdge::Right => right - QUOTA_WIDGET_EDGE_REVEAL_SIZE,
    };
    let y = clamp_position(position.y, top, bottom - size.height as i32);

    set_quota_widget_edge(edge);
    update_quota_widget_edge_ui(window, Some(edge));
    animate_quota_widget(window, x, y);
}

fn move_quota_widget(window: &WebviewWindow, x: i32, y: i32) {
    QUOTA_WIDGET_IGNORE_MOVES_UNTIL.store(current_epoch_millis() + 320, Ordering::Relaxed);
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

fn animate_quota_widget(window: &WebviewWindow, target_x: i32, target_y: i32) {
    let Ok(start) = window.outer_position() else {
        move_quota_widget(window, target_x, target_y);
        return;
    };
    let generation = QUOTA_WIDGET_ANIMATION_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let window = window.clone();

    tauri::async_runtime::spawn(async move {
        let frame_duration = Duration::from_millis(
            QUOTA_WIDGET_ANIMATION_DURATION_MILLIS / QUOTA_WIDGET_ANIMATION_FRAMES,
        );
        for frame in 1..=QUOTA_WIDGET_ANIMATION_FRAMES {
            tokio::time::sleep(frame_duration).await;
            if QUOTA_WIDGET_ANIMATION_GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }

            let progress = frame as f64 / QUOTA_WIDGET_ANIMATION_FRAMES as f64;
            let eased = 1.0 - (1.0 - progress).powi(3);
            let x = start.x + ((target_x - start.x) as f64 * eased).round() as i32;
            let y = start.y + ((target_y - start.y) as f64 * eased).round() as i32;
            move_quota_widget(&window, x, y);
        }
    });
}

fn set_quota_widget_edge(edge: QuotaWidgetEdge) {
    if let Ok(mut current_edge) = QUOTA_WIDGET_EDGE.lock() {
        *current_edge = Some(edge);
    }
}

fn quota_widget_edge() -> Option<QuotaWidgetEdge> {
    QUOTA_WIDGET_EDGE.lock().ok().and_then(|edge| *edge)
}

fn clear_quota_widget_edge() {
    if let Ok(mut edge) = QUOTA_WIDGET_EDGE.lock() {
        *edge = None;
    }
}

fn update_quota_widget_edge_ui(window: &WebviewWindow, edge: Option<QuotaWidgetEdge>) {
    let value = match edge {
        Some(QuotaWidgetEdge::Left) => "left",
        Some(QuotaWidgetEdge::Right) => "right",
        None => "",
    };
    let _ = window.eval(&format!(
        "document.documentElement.dataset.quotaWidgetEdge = '{}';",
        value
    ));
}

fn create_quota_widget_window(app: &tauri::App) -> tauri::Result<()> {
    let enabled = settings::read_settings().quota_widget_enabled;
    let window = WebviewWindowBuilder::new(
        app,
        QUOTA_WIDGET_LABEL,
        WebviewUrl::App("index.html?view=quota-widget".into()),
    )
    .title("CodexTray 额度")
    .inner_size(QUOTA_WIDGET_WIDTH, QUOTA_WIDGET_HEIGHT)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .shadow(false)
    .visible(false)
    .skip_taskbar(true)
    .always_on_top(true)
    .build()?;

    bind_quota_widget_edge_hiding(&window);
    position_quota_widget_at_startup(&window);
    if enabled {
        window.show()?;
    }
    Ok(())
}

#[tauri::command]
fn sync_quota_widget_edge_ui(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(QUOTA_WIDGET_LABEL)
        .ok_or_else(|| "额度悬浮条不存在".to_string())?;
    update_quota_widget_edge_ui(&window, quota_widget_edge());
    Ok(())
}

#[tauri::command]
fn reveal_quota_widget_from_edge(app: tauri::AppHandle) -> Result<(), String> {
    let Some(edge) = quota_widget_edge() else {
        return Ok(());
    };
    let window = app
        .get_webview_window(QUOTA_WIDGET_LABEL)
        .ok_or_else(|| "额度悬浮条不存在".to_string())?;
    let monitor = window
        .current_monitor()
        .map_err(|error| format!("无法读取屏幕信息：{}", error))?
        .ok_or_else(|| "未找到额度悬浮条所在屏幕".to_string())?;
    let position = window
        .outer_position()
        .map_err(|error| format!("无法读取额度悬浮条位置：{}", error))?;
    let size = window
        .outer_size()
        .map_err(|error| format!("无法读取额度悬浮条尺寸：{}", error))?;
    let work_area = monitor.work_area();
    let left = work_area.position.x;
    let right = left + work_area.size.width as i32;
    let x = match edge {
        QuotaWidgetEdge::Left => left + QUOTA_WIDGET_EDGE_GAP,
        QuotaWidgetEdge::Right => right - size.width as i32 - QUOTA_WIDGET_EDGE_GAP,
    };

    update_quota_widget_edge_ui(&window, None);
    animate_quota_widget(&window, x, position.y);
    Ok(())
}

#[tauri::command]
fn hide_quota_widget_at_edge(app: tauri::AppHandle) -> Result<(), String> {
    let Some(edge) = quota_widget_edge() else {
        return Ok(());
    };
    let window = app
        .get_webview_window(QUOTA_WIDGET_LABEL)
        .ok_or_else(|| "额度悬浮条不存在".to_string())?;

    hide_quota_widget_for_edge(&window, edge);
    Ok(())
}

#[tauri::command]
fn start_quota_widget_drag(app: tauri::AppHandle) -> Result<(), String> {
    QUOTA_WIDGET_ANIMATION_GENERATION.fetch_add(1, Ordering::Relaxed);
    clear_quota_widget_edge();
    let window = app
        .get_webview_window(QUOTA_WIDGET_LABEL)
        .ok_or_else(|| "额度悬浮条不存在".to_string())?;
    update_quota_widget_edge_ui(&window, None);
    window
        .start_dragging()
        .map_err(|error| format!("额度悬浮条拖动失败：{}", error))
}

fn refresh_dashboard_from_tray(app: &tauri::AppHandle) {
    refresh_dashboard_for_app(app, "托盘菜单刷新完成");
}

fn schedule_startup_dashboard_refresh(app: &tauri::AppHandle) {
    if startup_diagnostics::variant().skips_startup_refresh() {
        startup_diagnostics::record("startup.refresh.skipped", "diagnosticVariant");
        return;
    }

    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(STARTUP_REFRESH_DELAY_SECONDS)).await;
        run_startup_dashboard_refresh(app).await;
    });
}

async fn run_startup_dashboard_refresh(app: tauri::AppHandle) {
    startup_diagnostics::record("startup.refresh.begin", "");
    if !begin_startup_dashboard_refresh(&app) {
        startup_diagnostics::record("startup.refresh.ignored", "refreshAlreadyRunning");
        return;
    }

    let state = app.state::<DashboardState>();
    let snapshot = dashboard::refresh_dashboard(&state).await;
    startup_diagnostics::record(
        "startup.quota.completed",
        &format!(
            "cliProbe={:?} cliAppServer={:?} quotaWindows={}",
            snapshot.diagnostics.cli_probe.status,
            snapshot.diagnostics.cli_app_server.status,
            snapshot
                .quota
                .as_ref()
                .map(|quota| quota.windows.len())
                .unwrap_or_default()
        ),
    );
    finish_dashboard_refresh();

    if startup_diagnostics::variant().skips_startup_ui_update() {
        startup_diagnostics::record("startup.ui.skipped", "diagnosticVariant");
    } else {
        update_startup_dashboard_ui(&app, &snapshot);
    }

    refresh_startup_token_activity(app).await;
    startup_diagnostics::record("startup.refresh.completed", "");
}

fn begin_startup_dashboard_refresh(app: &tauri::AppHandle) -> bool {
    if startup_diagnostics::variant() != StartupDiagnosticVariant::NoStartupUiUpdate {
        return begin_dashboard_refresh(app);
    }

    if DASHBOARD_REFRESH_IN_PROGRESS.swap(true, Ordering::Relaxed) {
        return false;
    }
    record_dashboard_refresh_started();
    true
}

fn update_startup_dashboard_ui(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    startup_diagnostics::record("startup.webview.update.begin", "");
    update_dashboard_window(app, snapshot);
    startup_diagnostics::record("startup.webview.update.completed", "");

    startup_diagnostics::record("startup.tray.update.begin", "");
    update_tray_status(app, snapshot);
    startup_diagnostics::record("startup.tray.update.completed", "");

    startup_diagnostics::record("startup.emit.begin", "");
    let emit_result = app.emit(EVENT_DASHBOARD_REFRESHED, snapshot.clone());
    startup_diagnostics::record(
        "startup.emit.completed",
        if emit_result.is_ok() { "ok" } else { "error" },
    );
}

async fn refresh_startup_token_activity(app: tauri::AppHandle) {
    startup_diagnostics::record("startup.usage.begin", "");
    let state = app.state::<DashboardState>();
    let snapshot = dashboard::refresh_token_activity(&state).await;
    startup_diagnostics::record(
        "startup.usage.data.completed",
        &format!("source={:?}", snapshot.token_activity_source),
    );

    if startup_diagnostics::variant().skips_startup_ui_update() {
        startup_diagnostics::record("startup.usage.ui.skipped", "diagnosticVariant");
        return;
    }

    startup_diagnostics::record("startup.usage.webview.begin", "");
    update_dashboard_window(&app, &snapshot);
    startup_diagnostics::record("startup.usage.webview.completed", "");

    startup_diagnostics::record("startup.usage.tray.begin", "");
    update_tray_status(&app, &snapshot);
    startup_diagnostics::record("startup.usage.tray.completed", "");

    startup_diagnostics::record("startup.usage.emit.begin", "");
    let emit_result = app.emit(EVENT_DASHBOARD_REFRESHED, snapshot);
    startup_diagnostics::record(
        "startup.usage.emit.completed",
        if emit_result.is_ok() { "ok" } else { "error" },
    );
}

fn refresh_dashboard_if_empty(app: &tauri::AppHandle) {
    let state = app.state::<DashboardState>();
    if state.has_success() {
        return;
    }

    refresh_dashboard_silently_for_app(app);
}

fn refresh_dashboard_for_app(app: &tauri::AppHandle, success_message: &'static str) {
    refresh_dashboard_with_log(app, DashboardRefreshLog::Record(success_message));
}

fn refresh_dashboard_silently_for_app(app: &tauri::AppHandle) {
    refresh_dashboard_with_log(app, DashboardRefreshLog::Silent);
}

fn refresh_dashboard_with_log(app: &tauri::AppHandle, log: DashboardRefreshLog) {
    if !begin_dashboard_refresh(app) {
        return;
    }

    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let state = app.state::<DashboardState>();
        let snapshot = dashboard::refresh_dashboard(&state).await;
        log_dashboard_refresh_outcome(&log, &snapshot);

        finish_dashboard_refresh();
        update_dashboard_window(&app, &snapshot);
        update_tray_status(&app, &snapshot);

        if app.emit(EVENT_DASHBOARD_REFRESHED, snapshot).is_err()
            && matches!(&log, DashboardRefreshLog::Record(_))
        {
            settings::append_log("WARN", "刷新结果广播失败");
        }

        match log {
            DashboardRefreshLog::Record(_) => refresh_token_activity_for_app(app).await,
            DashboardRefreshLog::Silent => refresh_token_activity_silently_for_app(app).await,
        }
    });
}

fn schedule_periodic_dashboard_refresh(app: &tauri::AppHandle) {
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(AUTO_REFRESH_CHECK_SECONDS));
        interval.tick().await;

        loop {
            interval.tick().await;

            if seconds_since_last_dashboard_refresh() >= AUTO_REFRESH_INTERVAL_SECONDS {
                refresh_dashboard_silently_for_app(&app);
            }
        }
    });
}

fn record_dashboard_refresh_started() {
    LAST_DASHBOARD_REFRESH_STARTED_AT.store(current_epoch_seconds(), Ordering::Relaxed);
}

fn begin_dashboard_refresh(app: &tauri::AppHandle) -> bool {
    if DASHBOARD_REFRESH_IN_PROGRESS.swap(true, Ordering::Relaxed) {
        notify_dashboard_refresh_started(app);
        return false;
    }

    record_dashboard_refresh_started();
    notify_dashboard_refresh_started(app);

    true
}

fn finish_dashboard_refresh() {
    DASHBOARD_REFRESH_IN_PROGRESS.store(false, Ordering::Relaxed);
}

fn is_dashboard_refreshing() -> bool {
    DASHBOARD_REFRESH_IN_PROGRESS.load(Ordering::Relaxed)
}

fn seconds_since_last_dashboard_refresh() -> u64 {
    current_epoch_seconds()
        .saturating_sub(LAST_DASHBOARD_REFRESH_STARTED_AT.load(Ordering::Relaxed))
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn notify_dashboard_refresh_started(app: &tauri::AppHandle) {
    if app.emit(EVENT_DASHBOARD_REFRESH_STARTED, ()).is_err() {
        settings::append_log("WARN", "刷新开始广播失败");
    }

    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let script = r#"
        window.__codexTrayDashboardRefreshing = true;
        window.dispatchEvent(new CustomEvent("codextray-dashboard-refresh-started"));
        "#;

    if window.eval(script).is_err() {
        settings::append_log("WARN", "仪表盘刷新开始同步失败");
    }
}

async fn refresh_token_activity_for_app(app: tauri::AppHandle) {
    refresh_token_activity_with_log(app, true).await;
}

async fn refresh_token_activity_silently_for_app(app: tauri::AppHandle) {
    refresh_token_activity_with_log(app, false).await;
}

async fn refresh_token_activity_with_log(app: tauri::AppHandle, should_log: bool) {
    let state = app.state::<DashboardState>();
    let snapshot = dashboard::refresh_token_activity(&state).await;
    if should_log {
        log_token_activity_refresh_outcome(&snapshot);
    }
    update_dashboard_window(&app, &snapshot);
    update_tray_status(&app, &snapshot);

    if app.emit(EVENT_DASHBOARD_REFRESHED, snapshot).is_err() && should_log {
        settings::append_log("WARN", "Token 活动刷新结果广播失败");
    }
}

fn sync_cached_dashboard_to_panel(app: &tauri::AppHandle) {
    let state = app.state::<DashboardState>();
    let snapshot = state.cached_snapshot();

    if is_dashboard_refreshing() {
        sync_refreshing_dashboard_window(app, &snapshot);
        return;
    }

    update_dashboard_window(app, &snapshot);
}

fn sync_refreshing_dashboard_window(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(payload) = serde_json::to_string(snapshot) else {
        return;
    };
    let script = format!(
        r#"
        window.__codexTrayDashboardRefreshing = true;
        window.__codexTrayDashboardSnapshot = {payload};
        window.dispatchEvent(new CustomEvent("codextray-dashboard-refresh-started"));
        "#
    );

    if window.eval(&script).is_err() {
        settings::append_log("WARN", "仪表盘刷新状态同步失败");
    }
}

fn log_token_activity_refresh_outcome(snapshot: &DashboardSnapshot) {
    if snapshot.token_activity_source.is_some() {
        settings::append_log("INFO", "Token 活动刷新完成");
        return;
    }

    if matches!(
        snapshot.diagnostics.token_activity.status,
        DiagnosticStatus::Error
    ) {
        settings::append_log(
            "ERROR",
            &format!(
                "{}：{}",
                snapshot.diagnostics.token_activity.label,
                snapshot.diagnostics.token_activity.message
            ),
        );
    }
}

fn update_dashboard_window(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(payload) = serde_json::to_string(snapshot) else {
        return;
    };
    let script = format!(
        r#"
        window.__codexTrayDashboardRefreshing = false;
        window.__codexTrayDashboardSnapshot = {payload};
        window.dispatchEvent(new CustomEvent("codextray-dashboard-refreshed", {{ detail: {payload} }}));
        "#
    );

    if window.eval(&script).is_err() {
        settings::append_log("WARN", "仪表盘窗口同步失败");
    }
}

fn current_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn update_quota_widget_window(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    let Some(window) = app.get_webview_window(QUOTA_WIDGET_LABEL) else {
        return;
    };
    let Ok(payload) = serde_json::to_string(snapshot) else {
        return;
    };
    let quota_count = snapshot
        .quota
        .as_ref()
        .map(|quota| quota.windows.len())
        .unwrap_or_default();
    if window
        .set_size(tauri::LogicalSize::new(
            QUOTA_WIDGET_WIDTH,
            quota_widget_height(quota_count),
        ))
        .is_err()
    {
        settings::append_log("WARN", "额度悬浮条尺寸更新失败");
    }
    let script = format!(
        r#"
        window.__codexTrayDashboardSnapshot = {payload};
        window.dispatchEvent(new CustomEvent("codextray-dashboard-refreshed", {{ detail: {payload} }}));
        "#
    );

    if window.eval(&script).is_err() {
        settings::append_log("WARN", "额度悬浮条同步失败");
    }
}

fn quota_widget_height(quota_count: usize) -> f64 {
    QUOTA_WIDGET_HEIGHT + quota_count.saturating_sub(1) as f64 * 34.0
}

#[cfg(test)]
mod quota_widget_tests {
    use super::{quota_widget_height, quota_widget_startup_x, QUOTA_WIDGET_EDGE_REVEAL_SIZE};

    #[test]
    fn compact_widget_height_tracks_the_number_of_quota_rows() {
        assert_eq!(quota_widget_height(0), 48.0);
        assert_eq!(quota_widget_height(1), 48.0);
        assert_eq!(quota_widget_height(2), 82.0);
    }

    #[test]
    fn startup_position_keeps_only_the_edge_reveal_strip_visible() {
        let work_area_left = 120;
        let work_area_width = 1920;
        let work_area_right = work_area_left + work_area_width as i32;

        assert_eq!(
            work_area_right - quota_widget_startup_x(work_area_left, work_area_width),
            QUOTA_WIDGET_EDGE_REVEAL_SIZE,
        );
    }
}

fn update_tray_status(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    update_quota_widget_window(app, snapshot);

    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    update_tray_icon_if_changed(&tray, snapshot);

    if tray
        .set_tooltip(Some(tray_status::tooltip_for_snapshot(snapshot)))
        .is_err()
    {
        settings::append_log("WARN", "托盘提示更新失败");
    }
}

fn update_tray_icon_if_changed(tray: &tauri::tray::TrayIcon, snapshot: &DashboardSnapshot) {
    let next_state = tray_status::icon_state_for_snapshot(snapshot);
    let Ok(mut last_state) = LAST_TRAY_ICON_STATE.lock() else {
        settings::append_log("WARN", "托盘图标状态锁定失败");
        return;
    };

    if last_state.as_ref() == Some(&next_state) {
        return;
    }

    if tray
        .set_icon(Some(tray_status::icon_for_state(next_state)))
        .is_err()
    {
        settings::append_log("WARN", "托盘额度图标更新失败");
        return;
    }

    *last_state = Some(next_state);
}

fn log_dashboard_refresh_outcome(log: &DashboardRefreshLog, snapshot: &DashboardSnapshot) {
    let DashboardRefreshLog::Record(success_message) = log else {
        return;
    };

    if let Some(quota) = snapshot.quota.as_ref().filter(|quota| !quota.stale) {
        settings::append_log(
            "INFO",
            &format!(
                "{}，emailPresent={}，quotaWindows={}",
                success_message,
                snapshot.account.email.is_some(),
                quota.windows.len()
            ),
        );
        return;
    }

    for item in [
        &snapshot.diagnostics.cli_probe,
        &snapshot.diagnostics.cli_app_server,
        &snapshot.diagnostics.token_activity,
    ] {
        if matches!(item.status, DiagnosticStatus::Error) {
            settings::append_log("ERROR", &format!("{}：{}", item.label, item.message));
        }
    }
}

fn constrain_window_to_work_area(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let work_area = monitor.work_area();
    let min_x = work_area.position.x + PANEL_WORK_AREA_MARGIN;
    let min_y = work_area.position.y + PANEL_WORK_AREA_MARGIN;
    let max_x = work_area.position.x + work_area.size.width as i32
        - size.width as i32
        - PANEL_WORK_AREA_MARGIN;
    let max_y = work_area.position.y + work_area.size.height as i32
        - size.height as i32
        - PANEL_WORK_AREA_MARGIN;
    let x = clamp_position(position.x, min_x, max_x);
    let y = clamp_position(position.y, min_y, max_y);

    if x != position.x || y != position.y {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

fn clamp_position(value: i32, min: i32, max: i32) -> i32 {
    if max < min {
        min
    } else {
        value.clamp(min, max)
    }
}

fn hide_panel_on_focus_loss(window: &tauri::WebviewWindow) {
    let app = window.app_handle().clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Focused(false)) {
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(120));
                hide_panel_windows_if_unfocused(&app);
            });
        }
    });
}

fn hide_panel_windows(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }

    if let Some(detail) = app.get_webview_window("detail") {
        let _ = detail.hide();
    }
}

fn hide_panel_windows_if_unfocused(app: &tauri::AppHandle) {
    let main_focused = app
        .get_webview_window("main")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false);
    let detail_focused = app
        .get_webview_window("detail")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false);

    if !main_focused && !detail_focused {
        hide_panel_windows(app);
    }
}

fn register_configured_shortcut(app: &tauri::AppHandle) -> Result<(), String> {
    let shortcut = parse_global_shortcut(&settings::read_settings().global_shortcut)?;
    let shortcut_manager = app.global_shortcut();
    shortcut_manager
        .unregister_all()
        .map_err(|error| format!("无法重置全局快捷键：{}", error))?;
    shortcut_manager
        .register(shortcut)
        .map_err(|error| format!("无法注册全局快捷键：{}", error))?;
    settings::append_log("INFO", "全局快捷键已注册");

    Ok(())
}

fn schedule_startup_update_check(app: &tauri::AppHandle) {
    if settings::update_channel_config(Some(app.config())).is_err() {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        startup_diagnostics::record("startup.update.begin", "");
        let status = check_for_updates_quietly_for_app(&app).await;
        startup_diagnostics::record(
            "startup.update.completed",
            &format!("status={:?}", status.status),
        );
        prompt_startup_update_if_available(app, &status).await;
    });
}

fn schedule_periodic_update_check(app: &tauri::AppHandle) {
    if settings::update_channel_config(Some(app.config())).is_err() {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(AUTO_UPDATE_CHECK_INTERVAL_SECONDS));
        interval.tick().await;

        loop {
            interval.tick().await;
            let status = check_for_updates_quietly_for_app(&app).await;
            prompt_startup_update_if_available(app.clone(), &status).await;
        }
    });
}

fn parse_global_shortcut(value: &str) -> Result<Shortcut, String> {
    let key = value
        .split('+')
        .map(str::trim)
        .next_back()
        .and_then(|part| part.chars().next())
        .ok_or_else(|| "快捷键格式无效".to_string())?;

    let code = match key.to_ascii_uppercase() {
        'A' => Code::KeyA,
        'B' => Code::KeyB,
        'C' => Code::KeyC,
        'D' => Code::KeyD,
        'E' => Code::KeyE,
        'F' => Code::KeyF,
        'G' => Code::KeyG,
        'H' => Code::KeyH,
        'I' => Code::KeyI,
        'J' => Code::KeyJ,
        'K' => Code::KeyK,
        'L' => Code::KeyL,
        'M' => Code::KeyM,
        'N' => Code::KeyN,
        'O' => Code::KeyO,
        'P' => Code::KeyP,
        'Q' => Code::KeyQ,
        'R' => Code::KeyR,
        'S' => Code::KeyS,
        'T' => Code::KeyT,
        'U' => Code::KeyU,
        'V' => Code::KeyV,
        'W' => Code::KeyW,
        'X' => Code::KeyX,
        'Y' => Code::KeyY,
        'Z' => Code::KeyZ,
        _ => return Err("当前仅支持 Ctrl+Shift+字母 格式".to_string()),
    };

    Ok(Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::SHIFT),
        code,
    ))
}

fn show_detail_window(app: &tauri::AppHandle, main: &WebviewWindow) {
    let Some(detail) = app.get_webview_window("detail") else {
        return;
    };

    let Ok(main_position) = main.outer_position() else {
        return;
    };
    let scale_factor = main.scale_factor().unwrap_or(1.0);
    let panel_margin = (PANEL_SHADOW_MARGIN * scale_factor).round() as i32;
    let detail_gap = (DETAIL_GAP * scale_factor).round() as i32;
    let full_width = (DETAIL_WIDTH * scale_factor).round() as i32;
    let full_height = (DETAIL_HEIGHT * scale_factor).round() as i32;
    let detail_margin = (DETAIL_SHADOW_MARGIN * scale_factor).round() as i32;
    let panel_height = (PANEL_HEIGHT * scale_factor).round() as i32;
    let visual_main_left = main_position.x + panel_margin;
    let visual_main_bottom = main_position.y + panel_margin + panel_height;
    let x = visual_main_left - full_width - detail_gap + detail_margin;
    let y = visual_main_bottom - full_height + detail_margin;

    let _ = detail.set_size(tauri::PhysicalSize::new(
        full_width as u32,
        full_height as u32,
    ));
    let _ = detail.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = detail.show();
    let _ = detail.eval(
        r#"
        requestAnimationFrame(() => {
          document.body.classList.remove("detail-revealing");
          void document.body.offsetWidth;
          document.body.classList.add("detail-revealing");
        });
        "#,
    );
}

fn create_detail_window(app: &tauri::App) -> tauri::Result<()> {
    WebviewWindowBuilder::new(
        app,
        "detail",
        WebviewUrl::App("index.html?view=detail".into()),
    )
    .title("CodexTray Detail")
    .inner_size(DETAIL_WIDTH, DETAIL_HEIGHT)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .shadow(false)
    .visible(false)
    .skip_taskbar(true)
    .always_on_top(true)
    .build()?;

    Ok(())
}

#[tauri::command]
fn show_heatmap_detail(app: tauri::AppHandle, detail: serde_json::Value) {
    if app
        .get_webview_window("detail")
        .and_then(|detail| detail.is_visible().ok())
        .unwrap_or(false)
    {
        update_detail_window(&app, &detail);
        return;
    }

    let Some(main) = app.get_webview_window("main") else {
        return;
    };

    show_detail_window(&app, &main);
    update_detail_window(&app, &detail);
}

fn update_detail_window(app: &tauri::AppHandle, detail: &serde_json::Value) {
    let Some(window) = app.get_webview_window("detail") else {
        return;
    };
    let Ok(payload) = serde_json::to_string(detail) else {
        return;
    };
    let script = format!(
        r#"
        window.__codexTrayHeatmapDetail = {payload};
        window.dispatchEvent(new CustomEvent("codextray-heatmap-detail", {{ detail: {payload} }}));
        "#
    );

    let _ = window.eval(&script);
}

#[tauri::command]
fn hide_heatmap_detail(app: tauri::AppHandle) {
    if let Some(detail) = app.get_webview_window("detail") {
        let _ = detail.eval(r#"document.body.classList.remove("detail-revealing");"#);
        let _ = detail.hide();
    }
}

#[tauri::command]
fn get_dashboard_snapshot(state: State<'_, DashboardState>) -> DashboardSnapshot {
    state.cached_snapshot()
}

#[tauri::command]
async fn refresh_dashboard(
    app: tauri::AppHandle,
    state: State<'_, DashboardState>,
) -> Result<DashboardSnapshot, String> {
    if !begin_dashboard_refresh(&app) {
        return Ok(state.cached_snapshot());
    }

    let snapshot = dashboard::refresh_dashboard(&state).await;
    log_dashboard_refresh_outcome(&DashboardRefreshLog::Record("仪表盘刷新完成"), &snapshot);
    finish_dashboard_refresh();
    update_dashboard_window(&app, &snapshot);
    update_tray_status(&app, &snapshot);
    let app_for_token_activity = app.clone();
    tauri::async_runtime::spawn(async move {
        refresh_token_activity_for_app(app_for_token_activity).await;
    });

    Ok(snapshot)
}

#[tauri::command]
async fn get_settings_snapshot(app: tauri::AppHandle) -> SettingsSnapshot {
    settings::settings_snapshot(Some(app.config())).await
}

#[tauri::command]
fn set_global_shortcut(app: tauri::AppHandle, shortcut: String) -> Result<AppSettings, String> {
    let settings = settings::save_global_shortcut(shortcut)?;
    register_configured_shortcut(&app)?;

    Ok(settings)
}

#[tauri::command]
fn choose_codex_cli_path() -> Option<String> {
    settings::choose_codex_cli_path()
}

#[tauri::command]
async fn set_codex_cli_path(path: String) -> Result<AppSettings, String> {
    settings::save_codex_cli_path(path).await
}

#[tauri::command]
fn clear_codex_cli_path() -> Result<AppSettings, String> {
    settings::clear_codex_cli_path()
}

#[tauri::command]
fn get_recent_logs() -> Vec<LogEntry> {
    settings::recent_logs(80)
}

#[tauri::command]
fn get_startup_status() -> StartupStatus {
    settings::get_startup_status()
}

#[tauri::command]
fn set_startup_enabled(enabled: bool) -> Result<StartupStatus, String> {
    settings::set_startup_enabled(enabled)
}

#[tauri::command]
async fn get_hook_status() -> HookStatus {
    settings::get_hook_status().await
}

#[tauri::command]
async fn set_hook_enabled(enabled: bool) -> Result<HookStatus, String> {
    settings::set_hook_enabled(enabled).await
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> UpdateStatus {
    check_for_updates_for_app(&app).await
}

async fn check_for_updates_for_app(app: &tauri::AppHandle) -> UpdateStatus {
    let updater = match build_updater(app) {
        Ok(updater) => updater,
        Err(status) => return status,
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let message = format!("发现新版本：{}，请确认后安装", version);
            settings::append_log("INFO", &message);
            settings::update_status_with_version(DiagnosticStatus::Warning, message, Some(version))
        }
        Ok(None) => {
            let message = "当前已是最新版本".to_string();
            settings::append_log("INFO", &message);
            settings::update_status(DiagnosticStatus::Ok, message)
        }
        Err(error) => {
            let message = format!("更新检查失败：{}", error_details(&error));
            settings::append_log("ERROR", &message);
            settings::update_status(DiagnosticStatus::Error, message)
        }
    }
}

async fn check_for_updates_quietly_for_app(app: &tauri::AppHandle) -> UpdateStatus {
    let updater = match build_updater_quietly(app) {
        Ok(updater) => updater,
        Err(status) => return status,
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let message = format!("发现新版本：{}，请确认后安装", version);
            settings::update_status_with_version(DiagnosticStatus::Warning, message, Some(version))
        }
        Ok(None) => settings::update_status(DiagnosticStatus::Ok, "当前已是最新版本"),
        Err(error) => settings::update_status(
            DiagnosticStatus::Error,
            format!("更新检查失败：{}", error_details(&error)),
        ),
    }
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> UpdateStatus {
    install_update_for_app(&app).await
}

async fn install_update_for_app(app: &tauri::AppHandle) -> UpdateStatus {
    let updater = match build_updater(app) {
        Ok(updater) => updater,
        Err(status) => return status,
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let message = format!("确认安装新版本：{}，开始下载安装", version);
            settings::append_log("INFO", &message);

            match update
                .download_and_install(
                    |_chunk_length, _content_length| {},
                    || settings::append_log("INFO", "更新包下载完成，开始安装"),
                )
                .await
            {
                Ok(()) => {
                    let message = format!("新版本 {} 已安装，重启后生效", version);
                    settings::append_log("INFO", &message);
                    settings::update_status(DiagnosticStatus::Ok, message)
                }
                Err(error) => {
                    let message = format!("更新安装失败：{}", error_details(&error));
                    settings::append_log("ERROR", &message);
                    settings::update_status(DiagnosticStatus::Error, message)
                }
            }
        }
        Ok(None) => {
            let message = "当前已是最新版本".to_string();
            settings::append_log("INFO", &message);
            settings::update_status(DiagnosticStatus::Ok, message)
        }
        Err(error) => {
            let message = format!("更新检查失败：{}", error_details(&error));
            settings::append_log("ERROR", &message);
            settings::update_status(DiagnosticStatus::Error, message)
        }
    }
}

async fn prompt_startup_update_if_available(app: tauri::AppHandle, status: &UpdateStatus) {
    let Some(version) = status.available_version.clone() else {
        return;
    };

    let prompt_result = tauri::async_runtime::spawn_blocking(move || {
        rfd::MessageDialog::new()
            .set_title("CodexTray 发现新版本")
            .set_description(format!("发现新版本 {}，是否打开设置页安装？", version))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
    })
    .await;

    match prompt_result {
        Ok(rfd::MessageDialogResult::Yes) => show_settings_window(&app),
        Ok(_) => {}
        Err(error) => settings::append_log("WARN", &format!("更新提示框显示失败：{}", error)),
    }
}

fn build_updater(app: &tauri::AppHandle) -> Result<tauri_plugin_updater::Updater, UpdateStatus> {
    let config = match settings::update_channel_config(Some(app.config())) {
        Ok(config) => config,
        Err(_) => {
            return Err(settings::update_status(
                DiagnosticStatus::Skipped,
                settings::update_channel_unconfigured_message(),
            ));
        }
    };
    let endpoint = match Url::parse(&config.endpoint) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let message = format!("更新端点格式无效：{}", error);
            settings::append_log("ERROR", &message);
            return Err(settings::update_status(DiagnosticStatus::Error, message));
        }
    };
    match app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.pubkey(config.pubkey).build())
    {
        Ok(updater) => Ok(updater),
        Err(error) => {
            let message = format!("更新检查初始化失败：{}", error_details(&error));
            settings::append_log("ERROR", &message);
            Err(settings::update_status(DiagnosticStatus::Error, message))
        }
    }
}

fn build_updater_quietly(
    app: &tauri::AppHandle,
) -> Result<tauri_plugin_updater::Updater, UpdateStatus> {
    let config = match settings::update_channel_config(Some(app.config())) {
        Ok(config) => config,
        Err(_) => {
            return Err(settings::update_status(
                DiagnosticStatus::Skipped,
                settings::update_channel_unconfigured_message(),
            ));
        }
    };
    let endpoint = match Url::parse(&config.endpoint) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let message = format!("更新端点格式无效：{}", error);
            return Err(settings::update_status(DiagnosticStatus::Error, message));
        }
    };

    app.updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.pubkey(config.pubkey).build())
        .map_err(|error| {
            settings::update_status(
                DiagnosticStatus::Error,
                format!("更新检查初始化失败：{}", error_details(&error)),
            )
        })
}

fn error_details(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();

    while let Some(error) = source {
        message.push_str("；原因：");
        message.push_str(&error.to_string());
        source = error.source();
    }

    message
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DashboardState::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            settings::append_log("INFO", "检测到第二个实例，已唤起现有面板");
            toggle_panel(app);
        }))
        .setup(|app| {
            startup_diagnostics::record("setup.begin", "");
            app.handle().plugin(tauri_plugin_positioner::init())?;
            create_detail_window(app)?;
            create_quota_widget_window(app)?;
            startup_diagnostics::record("setup.webviews.ready", "");

            if let Some(window) = app.get_webview_window("main") {
                hide_panel_on_focus_loss(&window);
            }

            if let Some(window) = app.get_webview_window("detail") {
                hide_panel_on_focus_loss(&window);
            }

            if let Err(error) = register_configured_shortcut(app.handle()) {
                settings::append_log("ERROR", &error);
            }

            schedule_startup_update_check(app.handle());
            schedule_periodic_update_check(app.handle());
            schedule_startup_dashboard_refresh(app.handle());
            schedule_periodic_dashboard_refresh(app.handle());

            let quota_widget_menu = CheckMenuItem::with_id(
                app,
                MENU_TOGGLE_QUOTA_WIDGET,
                "显示额度悬浮条",
                true,
                settings::read_settings().quota_widget_enabled,
                None::<&str>,
            )?;
            let quota_widget_menu_for_event = quota_widget_menu.clone();
            let tray_menu = MenuBuilder::new(app)
                .text(MENU_TOGGLE_PANEL, "显示/隐藏面板")
                .text(MENU_SETTINGS, "设置")
                .text(MENU_REFRESH, "刷新数据")
                .item(&quota_widget_menu)
                .separator()
                .text(MENU_QUIT, "退出 CodexTray")
                .build()?;

            TrayIconBuilder::with_id(TRAY_ID)
                .menu(&tray_menu)
                .icon(tray_status::default_icon())
                .tooltip("CodexTray")
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    MENU_TOGGLE_PANEL => toggle_panel(app),
                    MENU_SETTINGS => show_settings_window(app),
                    MENU_REFRESH => {
                        settings::append_log("INFO", "从托盘菜单请求刷新");
                        refresh_dashboard_from_tray(app);
                        toggle_panel(app);
                    }
                    MENU_TOGGLE_QUOTA_WIDGET => match toggle_quota_widget(app) {
                        Ok(enabled) => {
                            let _ = quota_widget_menu_for_event.set_checked(enabled);
                        }
                        Err(error) => settings::append_log("ERROR", &error),
                    },
                    MENU_QUIT => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);

                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        toggle_panel(tray.app_handle());
                    }
                })
                .build(app)?;

            startup_diagnostics::record("setup.tray.ready", "");
            startup_diagnostics::schedule_smoke_test_exit(app.handle());
            startup_diagnostics::record("setup.completed", "");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            show_heatmap_detail,
            hide_heatmap_detail,
            sync_quota_widget_edge_ui,
            reveal_quota_widget_from_edge,
            hide_quota_widget_at_edge,
            start_quota_widget_drag,
            get_dashboard_snapshot,
            refresh_dashboard,
            get_settings_snapshot,
            set_global_shortcut,
            choose_codex_cli_path,
            set_codex_cli_path,
            clear_codex_cli_path,
            get_recent_logs,
            get_startup_status,
            set_startup_enabled,
            get_hook_status,
            set_hook_enabled,
            check_for_updates,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
