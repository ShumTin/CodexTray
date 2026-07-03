mod cli;
mod dashboard;
mod hook_stats;
mod models;
mod settings;
mod token_usage;
mod tray_status;

use dashboard::DashboardState;
use models::{
    AppSettings, DashboardSnapshot, DiagnosticStatus, HookStatus, LogEntry, SettingsSnapshot,
    StartupStatus, UpdateStatus,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::menu::MenuBuilder;
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
const TRAY_ID: &str = "codextray-main";
const EVENT_DASHBOARD_REFRESH_STARTED: &str = "codextray://dashboard-refresh-started";
const EVENT_DASHBOARD_REFRESHED: &str = "codextray://dashboard-refreshed";
const MENU_QUIT: &str = "quit";
const AUTO_REFRESH_INTERVAL_SECONDS: u64 = 60;
const AUTO_REFRESH_CHECK_SECONDS: u64 = 5;
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
static LAST_DASHBOARD_REFRESH_STARTED_AT: AtomicU64 = AtomicU64::new(0);
static DASHBOARD_REFRESH_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn run_hook_event_process() -> Result<(), String> {
    hook_stats::run_hook_event_process()
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

fn refresh_dashboard_from_tray(app: &tauri::AppHandle) {
    refresh_dashboard_for_app(app, "托盘菜单刷新完成");
}

fn schedule_startup_dashboard_refresh(app: &tauri::AppHandle) {
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(STARTUP_REFRESH_DELAY_SECONDS)).await;
        refresh_dashboard_for_app(&app, "启动刷新完成");
    });
}

fn refresh_dashboard_if_empty(app: &tauri::AppHandle) {
    let state = app.state::<DashboardState>();
    if state.has_success() {
        return;
    }

    refresh_dashboard_for_app(app, "面板打开刷新完成");
}

fn refresh_dashboard_for_app(app: &tauri::AppHandle, success_message: &'static str) {
    if !begin_dashboard_refresh(app) {
        return;
    }

    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let state = app.state::<DashboardState>();
        let snapshot = dashboard::refresh_dashboard(&state).await;
        log_dashboard_refresh_outcome(success_message, &snapshot);

        finish_dashboard_refresh();
        update_dashboard_window(&app, &snapshot);
        update_tray_status(&app, &snapshot);

        if app.emit(EVENT_DASHBOARD_REFRESHED, snapshot).is_err() {
            settings::append_log("WARN", "刷新结果广播失败");
        }

        refresh_token_activity_for_app(app).await;
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
                refresh_dashboard_for_app(&app, "自动刷新完成");
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
    let state = app.state::<DashboardState>();
    let snapshot = dashboard::refresh_token_activity(&state).await;
    log_token_activity_refresh_outcome(&snapshot);
    update_dashboard_window(&app, &snapshot);
    update_tray_status(&app, &snapshot);

    if app.emit(EVENT_DASHBOARD_REFRESHED, snapshot).is_err() {
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

fn update_tray_status(app: &tauri::AppHandle, snapshot: &DashboardSnapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    if tray
        .set_icon(Some(tray_status::icon_for_snapshot(snapshot)))
        .is_err()
    {
        settings::append_log("WARN", "托盘额度图标更新失败");
    }

    if tray
        .set_tooltip(Some(tray_status::tooltip_for_snapshot(snapshot)))
        .is_err()
    {
        settings::append_log("WARN", "托盘提示更新失败");
    }
}

fn log_dashboard_refresh_outcome(success_message: &str, snapshot: &DashboardSnapshot) {
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
        let _ = check_for_updates(app).await;
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
    log_dashboard_refresh_outcome("仪表盘刷新完成", &snapshot);
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
    let updater = match build_updater(&app) {
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

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> UpdateStatus {
    let updater = match build_updater(&app) {
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

fn build_updater(
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
            app.handle().plugin(tauri_plugin_positioner::init())?;
            create_detail_window(app)?;

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
            schedule_startup_dashboard_refresh(app.handle());
            schedule_periodic_dashboard_refresh(app.handle());

            let tray_menu = MenuBuilder::new(app)
                .text(MENU_TOGGLE_PANEL, "显示/隐藏面板")
                .text(MENU_SETTINGS, "设置")
                .text(MENU_REFRESH, "刷新数据")
                .separator()
                .text(MENU_QUIT, "退出 CodexTray")
                .build()?;

            TrayIconBuilder::with_id(TRAY_ID)
                .menu(&tray_menu)
                .icon(tray_status::default_icon())
                .tooltip("CodexTray")
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    MENU_TOGGLE_PANEL => toggle_panel(app),
                    MENU_SETTINGS => show_settings_window(app),
                    MENU_REFRESH => {
                        settings::append_log("INFO", "从托盘菜单请求刷新");
                        refresh_dashboard_from_tray(app);
                        toggle_panel(app);
                    }
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            show_heatmap_detail,
            hide_heatmap_detail,
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
