use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use serde_json::{Map, Value};

use crate::cli::{
    list_codex_hooks, probe_codex_cli, probe_codex_cli_path, read_codex_config,
    write_codex_config_batch, CliProbe,
};
use crate::models::{
    AppSettings, DiagnosticStatus, HookStatus, LogEntry, RuntimeInfo, SettingsSnapshot,
    StartupStatus, ThemeMode, UpdateStatus,
};

const APP_DATA_DIR: &str = "CodexTray";
const SETTINGS_FILE: &str = "settings.json";
const LOG_DIR: &str = "logs";
const LOG_FILE: &str = "codextray.log";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "CodexTray";
const UPDATE_ENDPOINT_ENV: &str = "CODEXTRAY_UPDATE_ENDPOINT";
const UPDATE_PUBKEY_ENV: &str = "CODEXTRAY_UPDATE_PUBKEY";
const CODEXTRAY_HOOK_ARG: &str = "--hook-event";
const CODEXTRAY_HOOK_SCRIPT: &str = "codextray-hook.ps1";
const CODEX_HOOK_TIMEOUT_SECONDS: u64 = 5;
const CODEX_HOOK_TRUST_STATE_KEY: &str = "hooks.state";
const CODEX_HOOK_EVENTS: [&str; 10] = [
    "PermissionRequest",
    "PostCompact",
    "PostToolUse",
    "PreCompact",
    "PreToolUse",
    "SessionStart",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "UserPromptSubmit",
];

#[derive(Debug, Clone)]
pub struct UpdateChannelConfig {
    pub endpoint: String,
    pub pubkey: String,
}

pub async fn settings_snapshot(config: Option<&tauri::Config>) -> SettingsSnapshot {
    let settings = read_settings();
    let startup = get_startup_status();
    let hook = get_hook_status().await;
    let update = initial_update_status(config);
    let runtime = runtime_info().await;

    SettingsSnapshot {
        settings,
        startup,
        hook,
        update,
        runtime,
    }
}

pub fn read_settings() -> AppSettings {
    let path = settings_path();
    let Ok(content) = fs::read_to_string(path) else {
        return default_settings();
    };

    serde_json::from_str(&content).unwrap_or_else(|_| default_settings())
}

pub fn write_settings(settings: &AppSettings) -> Result<AppSettings, String> {
    let path = settings_path();
    let parent = path
        .parent()
        .ok_or_else(|| "设置路径无父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建设置目录：{}", error))?;
    let content = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("无法序列化设置：{}", error))?;
    fs::write(path, content).map_err(|error| format!("无法写入设置：{}", error))?;
    append_log("INFO", "设置已保存");

    Ok(settings.clone())
}

pub fn save_global_shortcut(shortcut: String) -> Result<AppSettings, String> {
    let normalized = shortcut.trim().to_string();

    if !is_supported_shortcut(&normalized) {
        return Err("当前仅支持 Ctrl+Shift+字母 格式".to_string());
    }

    let mut settings = read_settings();
    settings.global_shortcut = normalized;
    write_settings(&settings)
}

pub async fn save_codex_cli_path(path: String) -> Result<AppSettings, String> {
    let normalized = path.trim().trim_matches('"').to_string();
    if normalized.is_empty() {
        return Err("Codex CLI 路径不能为空".to_string());
    }

    let path = PathBuf::from(&normalized);
    if !path.is_file() {
        return Err("请选择可执行的 Codex CLI 文件".to_string());
    }

    probe_codex_cli_path(path)
        .await
        .map_err(|error| format!("所选 Codex CLI 不可启动：{}", error))?;

    let mut settings = read_settings();
    settings.codex_cli_path = Some(normalized);
    write_settings(&settings)
}

pub fn clear_codex_cli_path() -> Result<AppSettings, String> {
    let mut settings = read_settings();
    settings.codex_cli_path = None;
    write_settings(&settings)
}

pub fn choose_codex_cli_path() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 Codex CLI")
        .add_filter("Codex CLI", &["exe", "cmd"])
        .add_filter("所有文件", &["*"])
        .pick_file()
        .map(|path| path.display().to_string())
}

pub fn configured_codex_cli_path() -> Option<PathBuf> {
    read_settings()
        .codex_cli_path
        .and_then(|path| non_empty_path(path.trim()))
}

pub fn get_startup_status() -> StartupStatus {
    if !cfg!(windows) {
        return StartupStatus {
            enabled: false,
            source: "unsupported".to_string(),
            message: "当前平台暂不支持开机启动".to_string(),
        };
    }

    match query_startup_value() {
        Ok(Some(value)) => StartupStatus {
            enabled: startup_value_matches_current_exe(&value),
            source: "HKCU Run".to_string(),
            message: value,
        },
        Ok(None) => StartupStatus {
            enabled: false,
            source: "HKCU Run".to_string(),
            message: "未启用".to_string(),
        },
        Err(error) => StartupStatus {
            enabled: false,
            source: "HKCU Run".to_string(),
            message: error,
        },
    }
}

pub fn set_startup_enabled(enabled: bool) -> Result<StartupStatus, String> {
    if !cfg!(windows) {
        return Err("当前平台暂不支持开机启动".to_string());
    }

    let status = if enabled {
        enable_startup()
    } else {
        disable_startup()
    };

    match status {
        Ok(()) => {
            append_log(
                "INFO",
                if enabled {
                    "开机启动已启用"
                } else {
                    "开机启动已关闭"
                },
            );
            Ok(get_startup_status())
        }
        Err(error) => {
            append_log("ERROR", &format!("开机启动切换失败：{}", error));
            Err(error)
        }
    }
}

pub async fn get_hook_status() -> HookStatus {
    let path = codex_hooks_path();
    let Ok(exe) = current_exe_path() else {
        return HookStatus {
            enabled: false,
            source: path.display().to_string(),
            message: "无法解析当前程序路径".to_string(),
        };
    };

    let config = match read_hooks_config(&path) {
        Ok(config) => config,
        Err(error) => {
            return HookStatus {
                enabled: false,
                source: path.display().to_string(),
                message: error,
            };
        }
    };

    if !hooks_config_has_codextray(&config, &exe) {
        return HookStatus {
            enabled: false,
            source: path.display().to_string(),
            message: "未启用 Hook 采集".to_string(),
        };
    }

    let probe = match codex_cli_probe().await {
        Ok(probe) => probe,
        Err(error) => {
            return HookStatus {
                enabled: false,
                source: path.display().to_string(),
                message: format!("Hook 已写入，但无法验证：{}", error),
            };
        }
    };

    match validate_configured_codextray_hooks(&probe, &exe, &path).await {
        Ok(()) => HookStatus {
            enabled: true,
            source: path.display().to_string(),
            message: "Hook 已配置并受信任，等待 Codex 事件".to_string(),
        },
        Err(error) => HookStatus {
            enabled: false,
            source: path.display().to_string(),
            message: format!("Hook 未生效：{}", error),
        },
    }
}

pub async fn set_hook_enabled(enabled: bool) -> Result<HookStatus, String> {
    let path = codex_hooks_path();
    let exe = current_exe_path()?;
    let hook_script = hook_script_path(&exe);
    if enabled && !hook_script.is_file() {
        return Err(format!("Hook 接收器不存在：{}", hook_script.display()));
    }
    let probe = if enabled {
        let probe = codex_cli_probe().await?;
        ensure_codex_hooks_globally_enabled(&probe).await?;
        Some(probe)
    } else {
        codex_cli_probe().await.ok()
    };
    let trust_keys = if enabled {
        Vec::new()
    } else if let Some(probe) = &probe {
        managed_hook_trust_keys(probe, &exe, &path)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut config = read_hooks_config(&path)?;
    normalize_hooks_config(&mut config);

    if enabled {
        remove_all_codextray_hooks(&mut config);
        install_codextray_hooks(&mut config, &exe);
    } else {
        remove_all_codextray_hooks(&mut config);
    }

    write_hooks_config(&path, &config)?;
    if let Some(probe) = &probe {
        if enabled {
            if let Err(error) = validate_and_trust_codextray_hooks(probe, &exe, &path).await {
                let message = format!("Hook 采集校验失败：{}", error);
                append_log("ERROR", &message);
                return Err(message);
            }
        } else if let Err(error) = remove_hook_trust_keys(probe, trust_keys).await {
            append_log("WARN", &format!("Hook 信任状态清理失败：{}", error));
        }
    }

    append_log(
        "INFO",
        if enabled {
            "Hook 采集已启用"
        } else {
            "Hook 采集已关闭"
        },
    );

    Ok(get_hook_status().await)
}

pub fn update_channel_config(
    config: Option<&tauri::Config>,
) -> Result<UpdateChannelConfig, String> {
    let endpoint = env_update_value(UPDATE_ENDPOINT_ENV)
        .or_else(|| updater_config_endpoint(config))
        .unwrap_or_default();
    let pubkey = env_update_value(UPDATE_PUBKEY_ENV)
        .or_else(|| updater_config_pubkey(config))
        .unwrap_or_default();

    if endpoint.is_empty() {
        return Err(format!("未配置更新端点：{}", UPDATE_ENDPOINT_ENV));
    }

    if pubkey.is_empty() {
        return Err(format!("未配置更新签名公钥：{}", UPDATE_PUBKEY_ENV));
    }

    Ok(UpdateChannelConfig { endpoint, pubkey })
}

pub fn update_channel_unconfigured_message() -> String {
    "未启用自动更新通道".to_string()
}

fn env_update_value(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn updater_config_endpoint(config: Option<&tauri::Config>) -> Option<String> {
    updater_config_value(config).and_then(updater_config_endpoint_from_value)
}

fn updater_config_pubkey(config: Option<&tauri::Config>) -> Option<String> {
    updater_config_value(config).and_then(updater_config_pubkey_from_value)
}

fn updater_config_endpoint_from_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("endpoints")
        .and_then(serde_json::Value::as_array)
        .and_then(|endpoints| endpoints.first())
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn updater_config_pubkey_from_value(value: &serde_json::Value) -> Option<String> {
    value
        .get("pubkey")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn updater_config_value(config: Option<&tauri::Config>) -> Option<&serde_json::Value> {
    config.and_then(|config| config.plugins.0.get("updater"))
}

pub fn recent_logs(limit: usize) -> Vec<LogEntry> {
    let path = log_path();
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut entries: Vec<LogEntry> = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| parse_log_line(&line))
        .collect();

    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }

    entries.reverse();
    entries
}

pub fn append_log(level: &str, message: &str) {
    let path = log_path();
    let Some(parent) = path.parent() else {
        return;
    };

    if fs::create_dir_all(parent).is_err() {
        return;
    }

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let sanitized = message.replace(['\r', '\n', '\t'], " ");
    let _ = writeln!(
        file,
        "{}\t{}\t{}",
        Utc::now().to_rfc3339(),
        level,
        sanitized
    );
}

async fn runtime_info() -> RuntimeInfo {
    let cli_probe = probe_codex_cli(configured_codex_cli_path()).await.ok();
    let install_path = current_exe_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "未知".to_string());
    let run_source = if cfg!(debug_assertions) {
        "开发环境".to_string()
    } else {
        "已发布应用".to_string()
    };

    RuntimeInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        cli_version: cli_probe.as_ref().map(|probe| probe.version.clone()),
        cli_path: cli_probe.map(|probe| probe.path.display().to_string()),
        run_source,
        install_path,
    }
}

fn default_settings() -> AppSettings {
    AppSettings {
        theme: ThemeMode::Light,
        global_shortcut: "Ctrl+Shift+C".to_string(),
        codex_cli_path: None,
    }
}

fn non_empty_path(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn is_supported_shortcut(shortcut: &str) -> bool {
    let parts: Vec<String> = shortcut
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();

    parts.len() == 3
        && parts[0] == "ctrl"
        && parts[1] == "shift"
        && parts[2].len() == 1
        && parts[2].chars().all(|value| value.is_ascii_alphabetic())
}

pub fn update_status(status: DiagnosticStatus, message: impl Into<String>) -> UpdateStatus {
    update_status_with_version(status, message, None)
}

pub fn update_status_with_version(
    status: DiagnosticStatus,
    message: impl Into<String>,
    available_version: Option<String>,
) -> UpdateStatus {
    UpdateStatus {
        status,
        message: message.into(),
        checked_at: Utc::now().to_rfc3339(),
        available_version,
    }
}

fn initial_update_status(config: Option<&tauri::Config>) -> UpdateStatus {
    match update_channel_config(config) {
        Ok(_) => update_status(DiagnosticStatus::Skipped, "更新通道已配置，等待检查"),
        Err(_) => update_status(
            DiagnosticStatus::Skipped,
            update_channel_unconfigured_message(),
        ),
    }
}

fn query_startup_value() -> Result<Option<String>, String> {
    let mut command = reg_command();
    let output = command
        .args(["query", RUN_KEY, "/v", RUN_VALUE])
        .output()
        .map_err(|error| format!("无法查询注册表：{}", error))?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_reg_query_value(&stdout))
}

fn enable_startup() -> Result<(), String> {
    let exe = current_exe_path()?;
    let value = format!("\"{}\"", exe.display());
    let mut command = reg_command();
    let output = command
        .args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d"])
        .arg(value)
        .args(["/f"])
        .output()
        .map_err(|error| format!("无法写入注册表：{}", error))?;

    command_result(output.status.success(), output.stderr)
}

fn disable_startup() -> Result<(), String> {
    let mut command = reg_command();
    let output = command
        .args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
        .output()
        .map_err(|error| format!("无法删除注册表：{}", error))?;

    if output.status.success() || query_startup_value()?.is_none() {
        return Ok(());
    }

    command_result(false, output.stderr)
}

fn reg_command() -> Command {
    let mut command = Command::new("reg");
    hide_child_console(&mut command);

    command
}

fn hide_child_console(command: &mut Command) {
    configure_hidden_child_process(command);
}

#[cfg(windows)]
fn configure_hidden_child_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_child_process(_command: &mut Command) {}

fn command_result(success: bool, stderr: Vec<u8>) -> Result<(), String> {
    if success {
        return Ok(());
    }

    let message = String::from_utf8_lossy(&stderr).trim().to_string();
    Err(if message.is_empty() {
        "注册表命令执行失败".to_string()
    } else {
        message
    })
}

fn startup_value_matches_current_exe(value: &str) -> bool {
    let Ok(exe) = current_exe_path() else {
        return false;
    };

    value
        .trim_matches('"')
        .eq_ignore_ascii_case(&exe.display().to_string())
}

async fn codex_cli_probe() -> Result<CliProbe, String> {
    probe_codex_cli(configured_codex_cli_path()).await
}

async fn ensure_codex_hooks_globally_enabled(probe: &CliProbe) -> Result<(), String> {
    let response = read_codex_config(probe).await?;
    if codex_hooks_globally_disabled(&response) {
        return Err("Codex 配置已禁用 Hook，请先移除 [features].hooks = false".to_string());
    }

    Ok(())
}

fn codex_hooks_globally_disabled(response: &Value) -> bool {
    response
        .get("result")
        .unwrap_or(response)
        .get("config")
        .and_then(|config| config.get("features"))
        .map(|features| {
            features.get("hooks").and_then(Value::as_bool) == Some(false)
                || features.get("codex_hooks").and_then(Value::as_bool) == Some(false)
        })
        .unwrap_or(false)
}

async fn validate_and_trust_codextray_hooks(
    probe: &CliProbe,
    exe: &Path,
    hooks_path: &Path,
) -> Result<(), String> {
    let cwd = codex_home()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()));
    let mut response = list_codex_hooks(probe, vec![cwd.display().to_string()]).await?;
    let entries = hook_trust_entries_needing_update(&response, exe, hooks_path);

    if !entries.is_empty() {
        upsert_hook_trust_entries(probe, entries).await?;
        response = list_codex_hooks(probe, vec![cwd.display().to_string()]).await?;
    }

    if let Some(message) = hook_validation_message(&response, exe, hooks_path) {
        return Err(message);
    }

    Ok(())
}

async fn validate_configured_codextray_hooks(
    probe: &CliProbe,
    exe: &Path,
    hooks_path: &Path,
) -> Result<(), String> {
    let cwd = codex_home()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()));
    let response = list_codex_hooks(probe, vec![cwd.display().to_string()]).await?;

    match hook_validation_message(&response, exe, hooks_path) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

async fn managed_hook_trust_keys(
    probe: &CliProbe,
    exe: &Path,
    hooks_path: &Path,
) -> Result<Vec<String>, String> {
    let cwd = codex_home()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()));
    let response = list_codex_hooks(probe, vec![cwd.display().to_string()]).await?;
    Ok(hook_metadata_items(&response)
        .into_iter()
        .filter(|hook| hook_is_managed_codextray(hook, exe, hooks_path))
        .filter_map(|hook| {
            hook.get("key")
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect())
}

async fn upsert_hook_trust_entries(
    probe: &CliProbe,
    entries: Vec<(String, String)>,
) -> Result<(), String> {
    let value = hook_trust_state_value(entries);
    let edit = serde_json::json!({
        "keyPath": CODEX_HOOK_TRUST_STATE_KEY,
        "value": value,
        "mergeStrategy": "upsert"
    });
    let _ = write_codex_config_batch(probe, vec![edit]).await?;
    Ok(())
}

async fn remove_hook_trust_keys(probe: &CliProbe, keys: Vec<String>) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }

    let response = read_codex_config(probe).await?;
    let mut state = hook_trust_state(&response);
    for key in keys {
        state.remove(&key);
    }

    let edit = serde_json::json!({
        "keyPath": CODEX_HOOK_TRUST_STATE_KEY,
        "value": hook_trust_state_value(state.into_iter().collect()),
        "mergeStrategy": "replace"
    });
    let _ = write_codex_config_batch(probe, vec![edit]).await?;
    Ok(())
}

fn hook_trust_state(response: &Value) -> std::collections::HashMap<String, String> {
    response
        .get("result")
        .unwrap_or(response)
        .get("config")
        .and_then(|config| config.get("hooks"))
        .and_then(|hooks| hooks.get("state"))
        .and_then(Value::as_object)
        .map(|state| {
            state
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .get("trusted_hash")
                        .and_then(Value::as_str)
                        .map(|hash| (key.clone(), hash.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn hook_trust_state_value(entries: Vec<(String, String)>) -> Value {
    let mut root = Map::new();
    for (key, trusted_hash) in entries {
        let mut entry = Map::new();
        entry.insert("trusted_hash".to_string(), Value::String(trusted_hash));
        root.insert(key, Value::Object(entry));
    }

    Value::Object(root)
}

fn hook_trust_entries_needing_update(
    response: &Value,
    exe: &Path,
    hooks_path: &Path,
) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for hook in hook_metadata_items(response) {
        if !hook_is_managed_codextray(hook, exe, hooks_path) || !hook_needs_trust_update(hook) {
            continue;
        }
        let Some(key) = hook.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(current_hash) = hook.get("currentHash").and_then(Value::as_str) else {
            continue;
        };
        if !key.is_empty() && !current_hash.is_empty() {
            entries.push((key.to_string(), current_hash.to_string()));
        }
    }

    entries
}

fn hook_validation_message(response: &Value, exe: &Path, hooks_path: &Path) -> Option<String> {
    let Some(data) = response
        .get("result")
        .unwrap_or(response)
        .get("data")
        .and_then(Value::as_array)
    else {
        return Some("Codex 没有返回 Hook 状态".to_string());
    };
    if data.is_empty() {
        return Some("Codex 没有返回 Hook 状态".to_string());
    }

    if data
        .iter()
        .flat_map(|entry| {
            entry
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .next()
        .is_some()
    {
        return Some("Codex 返回 Hook 配置错误".to_string());
    }

    let hooks = hook_metadata_items(response);
    for event in CODEX_HOOK_EVENTS {
        let Some(hook) = matching_hook_for_event(&hooks, event, exe, hooks_path) else {
            return Some("CodexTray Hook 已不完整".to_string());
        };
        if hook.get("enabled").and_then(Value::as_bool) == Some(false) {
            return Some("CodexTray Hook 已被 Codex 禁用".to_string());
        }
        if hook_needs_trust_update(hook) {
            return Some("CodexTray Hook 未被信任".to_string());
        }
        if !hook_source_matches(hook, hooks_path) {
            return Some("CodexTray Hook 来源不是全局 hooks.json".to_string());
        }
    }

    None
}

fn hook_metadata_items(response: &Value) -> Vec<&Value> {
    response
        .get("result")
        .unwrap_or(response)
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect()
}

fn matching_hook_for_event<'a>(
    hooks: &'a [&'a Value],
    event: &str,
    exe: &Path,
    hooks_path: &Path,
) -> Option<&'a Value> {
    hooks
        .iter()
        .copied()
        .filter(|hook| {
            hook_event_matches(hook, event)
                && hook
                    .get("command")
                    .and_then(Value::as_str)
                    .map(|command| command_matches_current_exe(command, exe))
                    .unwrap_or(false)
        })
        .find(|hook| hook_source_matches(hook, hooks_path))
        .or_else(|| {
            hooks.iter().copied().find(|hook| {
                hook_event_matches(hook, event)
                    && hook
                        .get("command")
                        .and_then(Value::as_str)
                        .map(|command| command_matches_current_exe(command, exe))
                        .unwrap_or(false)
            })
        })
}

fn hook_event_matches(hook: &Value, event: &str) -> bool {
    hook.get("eventName")
        .and_then(Value::as_str)
        .map(|value| normalized_event_name(value) == normalized_event_name(event))
        .unwrap_or(false)
}

fn normalized_event_name(value: &str) -> String {
    value
        .chars()
        .filter(|value| !matches!(value, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn hook_is_managed_codextray(hook: &Value, exe: &Path, hooks_path: &Path) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .map(|command| command_matches_current_exe(command, exe))
        .unwrap_or(false)
        && hook_source_matches(hook, hooks_path)
}

fn hook_source_matches(hook: &Value, hooks_path: &Path) -> bool {
    hook.get("sourcePath")
        .and_then(Value::as_str)
        .map(|source| {
            normalized_path_text(source) == normalized_path_text(&hooks_path.display().to_string())
        })
        .unwrap_or(false)
}

fn hook_needs_trust_update(hook: &Value) -> bool {
    matches!(
        hook.get("trustStatus")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("untrusted" | "modified")
    )
}

fn install_codextray_hooks(config: &mut Value, exe: &Path) {
    let root = config_object_mut(config);
    let hooks_value = root
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks_value.is_object() {
        *hooks_value = Value::Object(Map::new());
    }
    let hooks_root = hooks_value
        .as_object_mut()
        .expect("hooks root should be an object");

    for event in CODEX_HOOK_EVENTS {
        let entry = hooks_root
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = match entry {
            Value::Array(groups) => groups,
            _ => {
                *entry = Value::Array(Vec::new());
                entry
                    .as_array_mut()
                    .expect("event entry should be an array")
            }
        };

        if groups
            .iter()
            .any(|group| group_has_codextray_hook(group, exe))
        {
            continue;
        }

        groups.push(serde_json::json!({
            "hooks": [codextray_hook_entry(exe)]
        }));
    }
}

fn normalize_hooks_config(config: &mut Value) {
    let root = config_object_mut(config);
    let mut legacy_events = Vec::new();

    for event in CODEX_HOOK_EVENTS {
        if let Some(value) = root.remove(event) {
            legacy_events.push((event, value));
        }
    }

    if legacy_events.is_empty() {
        return;
    }

    let hooks_value = root
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks_value.is_object() {
        *hooks_value = Value::Object(Map::new());
    }
    let hooks_root = hooks_value
        .as_object_mut()
        .expect("hooks root should be an object");

    for (event, value) in legacy_events {
        let Value::Array(entries) = value else {
            continue;
        };
        let event_groups = hooks_root
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !event_groups.is_array() {
            *event_groups = Value::Array(Vec::new());
        }
        let groups = event_groups
            .as_array_mut()
            .expect("event entry should be an array");

        for entry in entries {
            if entry.get("hooks").and_then(Value::as_array).is_some() {
                groups.push(entry);
            } else {
                groups.push(serde_json::json!({ "hooks": [entry] }));
            }
        }
    }
}

fn remove_all_codextray_hooks(config: &mut Value) {
    let Some(root) = config.as_object_mut() else {
        return;
    };

    for event in CODEX_HOOK_EVENTS {
        if let Some(Value::Array(hooks)) = root.get_mut(event) {
            hooks.retain(|hook| !is_any_codextray_hook_entry(hook));
        }
    }

    if let Some(Value::Object(nested_root)) = root.get_mut("hooks") {
        for event in CODEX_HOOK_EVENTS {
            if let Some(Value::Array(groups)) = nested_root.get_mut(event) {
                remove_codextray_hooks_from_groups(groups);
            }
        }
    }
}

fn remove_codextray_hooks_from_groups(groups: &mut Vec<Value>) {
    for group in groups.iter_mut() {
        let Some(group) = group.as_object_mut() else {
            continue;
        };
        let Some(Value::Array(hooks)) = group.get_mut("hooks") else {
            continue;
        };

        hooks.retain(|hook| !is_any_codextray_hook_entry(hook));
    }

    groups.retain(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| !hooks.is_empty())
            .unwrap_or(true)
    });
}

fn hooks_config_has_codextray(config: &Value, exe: &Path) -> bool {
    let Some(root) = config.as_object() else {
        return false;
    };

    for event in CODEX_HOOK_EVENTS {
        if root
            .get(event)
            .and_then(Value::as_array)
            .map(|hooks| hooks.iter().any(|hook| is_codextray_hook_entry(hook, exe)))
            .unwrap_or(false)
        {
            return true;
        }
    }

    root.get("hooks")
        .and_then(Value::as_object)
        .map(|nested| nested_hooks_have_codextray(nested, exe))
        .unwrap_or(false)
}

fn nested_hooks_have_codextray(root: &Map<String, Value>, exe: &Path) -> bool {
    for event in CODEX_HOOK_EVENTS {
        let Some(groups) = root.get(event).and_then(Value::as_array) else {
            continue;
        };

        for group in groups {
            if group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|hooks| hooks.iter().any(|hook| is_codextray_hook_entry(hook, exe)))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }

    false
}

fn group_has_codextray_hook(group: &Value, exe: &Path) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| hooks.iter().any(|hook| is_codextray_hook_entry(hook, exe)))
        .unwrap_or(false)
}

fn is_codextray_hook_entry(value: &Value, exe: &Path) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let command_matches = object
        .get("command")
        .and_then(Value::as_str)
        .map(|command| command_mentions_current_exe(command, exe))
        .unwrap_or(false);
    command_matches
        && object
            .get("command")
            .and_then(Value::as_str)
            .map(command_mentions_hook_wrapper)
            .unwrap_or(false)
}

fn is_any_codextray_hook_entry(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };

    object
        .get("command")
        .and_then(Value::as_str)
        .map(|command| {
            command_mentions_hook_wrapper(command)
                || (command_mentions_codextray_exe(command) && command.contains(CODEXTRAY_HOOK_ARG))
        })
        .unwrap_or(false)
}

fn command_mentions_codextray_exe(command: &str) -> bool {
    normalized_path_text(command).contains("codextray.exe")
}

fn command_mentions_hook_wrapper(command: &str) -> bool {
    normalized_path_text(command).contains(CODEXTRAY_HOOK_SCRIPT)
}

fn codextray_hook_entry(exe: &Path) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("command".to_string()));
    object.insert(
        "command".to_string(),
        Value::String(codextray_hook_command(exe)),
    );
    object.insert(
        "timeout".to_string(),
        Value::Number(serde_json::Number::from(CODEX_HOOK_TIMEOUT_SECONDS)),
    );
    Value::Object(object)
}

fn codextray_hook_command(exe: &Path) -> String {
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {} -Executable {}",
        shell_quoted_path(&hook_script_path(exe)),
        shell_quoted_path(exe)
    )
}

fn shell_quoted_path(path: &Path) -> String {
    let value = path.display().to_string();
    if cfg!(windows) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn command_matches_current_exe(command: &str, exe: &Path) -> bool {
    command_mentions_current_exe(command, exe)
        && (command_mentions_hook_wrapper(command)
            || command.split_whitespace().any(|part| {
                part.trim_matches(['"', '\'']) == CODEXTRAY_HOOK_ARG
                    || part == CODEXTRAY_HOOK_ARG
            }))
}

fn command_mentions_current_exe(command: &str, exe: &Path) -> bool {
    let normalized_command = normalized_path_text(command);
    let normalized_exe = normalized_path_text(&exe.display().to_string());
    normalized_command.contains(&normalized_exe)
}

fn normalized_path_text(value: &str) -> String {
    value
        .trim_matches(['"', '\''])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn config_object_mut(config: &mut Value) -> &mut Map<String, Value> {
    if !config.is_object() {
        *config = Value::Object(Map::new());
    }

    config.as_object_mut().expect("config should be an object")
}

fn read_hooks_config(path: &Path) -> Result<Value, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(Value::Object(Map::new()));
    };

    serde_json::from_str(&content).map_err(|error| format!("Hook 配置解析失败：{}", error))
}

fn write_hooks_config(path: &Path, config: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Hook 配置路径无父目录".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建 Codex 配置目录：{}", error))?;
    let content = serde_json::to_string_pretty(config)
        .map_err(|error| format!("Hook 配置序列化失败：{}", error))?;
    fs::write(path, content).map_err(|error| format!("无法写入 Hook 配置：{}", error))
}

fn current_exe_path() -> Result<PathBuf, String> {
    env::current_exe().map_err(|error| format!("无法解析当前程序路径：{}", error))
}

fn hook_script_path(exe: &Path) -> PathBuf {
    let executable_dir = exe.parent().unwrap_or_else(|| Path::new("."));
    let candidates = [
        executable_dir.join(CODEXTRAY_HOOK_SCRIPT),
        executable_dir.join("resources").join(CODEXTRAY_HOOK_SCRIPT),
        executable_dir
            .join("..")
            .join("..")
            .join("resources")
            .join(CODEXTRAY_HOOK_SCRIPT),
    ];

    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

fn parse_reg_query_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();

        if !trimmed.starts_with(RUN_VALUE) {
            return None;
        }

        trimmed
            .split_once("REG_SZ")
            .map(|(_, value)| value.trim().to_string())
    })
}

fn parse_log_line(line: &str) -> Option<LogEntry> {
    let mut parts = line.splitn(3, '\t');

    Some(LogEntry {
        timestamp: parts.next()?.to_string(),
        level: parts.next()?.to_string(),
        message: parts.next()?.to_string(),
    })
}

fn settings_path() -> PathBuf {
    app_data_root().join(SETTINGS_FILE)
}

fn log_path() -> PathBuf {
    app_data_root().join(LOG_DIR).join(LOG_FILE)
}

fn codex_hooks_path() -> PathBuf {
    codex_home().join("hooks.json")
}

fn codex_home() -> PathBuf {
    if let Ok(value) = env::var("CODEX_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if let Ok(profile) = env::var("USERPROFILE") {
        return PathBuf::from(profile).join(".codex");
    }

    env::current_dir()
        .unwrap_or_else(|_| Path::new(".").to_path_buf())
        .join(".codex")
}

fn app_data_root() -> PathBuf {
    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join(APP_DATA_DIR);
    }

    env::current_dir()
        .unwrap_or_else(|_| Path::new(".").to_path_buf())
        .join(APP_DATA_DIR)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        codex_hooks_globally_disabled, hook_validation_message, hooks_config_has_codextray,
        install_codextray_hooks, is_supported_shortcut, normalize_hooks_config, parse_log_line,
        parse_reg_query_value, remove_all_codextray_hooks, update_channel_config,
        updater_config_endpoint_from_value, updater_config_pubkey_from_value, CODEX_HOOK_EVENTS,
        UPDATE_ENDPOINT_ENV, UPDATE_PUBKEY_ENV,
    };

    #[test]
    fn parses_registry_run_value_so_startup_status_reflects_real_state() {
        let output = concat!(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run",
            "\n",
            r"    CodexTray    REG_SZ    C:\Apps\CodexTray\CodexTray.exe",
            "\n",
        );

        let value = parse_reg_query_value(output).expect("run value should parse");

        assert_eq!(value, r"C:\Apps\CodexTray\CodexTray.exe");
    }

    #[test]
    fn parses_plain_log_lines_for_recent_log_view() {
        let entry =
            parse_log_line("2026-07-03T00:00:00Z\tINFO\t刷新完成").expect("log line should parse");

        assert_eq!(entry.level, "INFO");
        assert_eq!(entry.message, "刷新完成");
    }

    #[test]
    fn validates_shortcut_format_before_global_registration() {
        assert!(is_supported_shortcut("Ctrl+Shift+C"));
        assert!(is_supported_shortcut("ctrl + shift + k"));
        assert!(!is_supported_shortcut("Alt+Space"));
    }

    #[test]
    fn requires_update_endpoint_and_pubkey_before_checking_updates() {
        std::env::remove_var(UPDATE_ENDPOINT_ENV);
        std::env::remove_var(UPDATE_PUBKEY_ENV);

        let error = update_channel_config(None).expect_err("missing endpoint should block checks");

        assert!(error.contains(UPDATE_ENDPOINT_ENV));
    }

    #[test]
    fn reads_updater_channel_from_tauri_plugin_config() {
        let config = json!({
            "endpoints": ["https://example.com/latest.json"],
            "pubkey": "public-key"
        });

        assert_eq!(
            updater_config_endpoint_from_value(&config),
            Some("https://example.com/latest.json".to_string())
        );
        assert_eq!(
            updater_config_pubkey_from_value(&config),
            Some("public-key".to_string())
        );
    }

    #[test]
    fn installs_codextray_hook_for_each_lifecycle_event() {
        let mut config = json!({});
        let exe = Path::new(r"C:\Apps\CodexTray.exe");

        install_codextray_hooks(&mut config, exe);

        for event in CODEX_HOOK_EVENTS {
            let hooks = config
                .get("hooks")
                .and_then(serde_json::Value::as_object)
                .and_then(|root| root.get(event))
                .and_then(serde_json::Value::as_array)
                .expect("event should have hook entries");

            assert_eq!(hooks.len(), 1);
        }
        assert!(hooks_config_has_codextray(&config, exe));
    }

    #[test]
    fn migrates_legacy_top_level_hooks_so_codex_can_parse_the_config() {
        let mut config = json!({
            "PreToolUse": [
                {
                    "type": "command",
                    "command": "python legacy.py",
                    "args": ["--flag"]
                }
            ],
            "hooks": {
                "Stop": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python existing.py"
                            }
                        ]
                    }
                ]
            }
        });

        normalize_hooks_config(&mut config);

        assert!(config.get("PreToolUse").is_none());
        let migrated = config
            .get("hooks")
            .and_then(serde_json::Value::as_object)
            .and_then(|root| root.get("PreToolUse"))
            .and_then(serde_json::Value::as_array)
            .and_then(|groups| groups.first())
            .and_then(|group| group.get("hooks"))
            .and_then(serde_json::Value::as_array)
            .and_then(|hooks| hooks.first())
            .expect("legacy hook should move into grouped hooks");
        assert_eq!(
            migrated.get("command").and_then(serde_json::Value::as_str),
            Some("python legacy.py")
        );
    }

    #[test]
    fn accepts_codex_lower_camel_event_names_when_validating_hooks() {
        let exe = Path::new(r"D:\Tools\CodexTray\CodexTray.exe");
        let hooks_path = Path::new(r"C:\Users\person\.codex\hooks.json");
        let hooks = CODEX_HOOK_EVENTS
            .iter()
            .map(|event| {
                json!({
                    "eventName": lower_first(event),
                    "command": "powershell.exe -File \"D:\\Tools\\CodexTray\\codextray-hook.ps1\" -Executable \"D:\\Tools\\CodexTray\\CodexTray.exe\"",
                    "sourcePath": "C:\\Users\\person\\.codex\\hooks.json",
                    "enabled": true,
                    "trustStatus": "trusted"
                })
            })
            .collect::<Vec<_>>();
        let response = json!({
            "result": {
                "data": [
                    {
                        "cwd": "C:\\Users\\person",
                        "hooks": hooks,
                        "warnings": [],
                        "errors": []
                    }
                ]
            }
        });

        assert_eq!(hook_validation_message(&response, exe, hooks_path), None);
    }

    #[test]
    fn rejects_modified_codextray_hook_so_written_config_is_not_reported_as_active() {
        let exe = Path::new(r"D:\Tools\CodexTray\CodexTray.exe");
        let hooks_path = Path::new(r"C:\Users\person\.codex\hooks.json");
        let hooks = CODEX_HOOK_EVENTS
            .iter()
            .map(|event| {
                json!({
                    "eventName": lower_first(event),
                    "command": "powershell.exe -File \"D:\\Tools\\CodexTray\\codextray-hook.ps1\" -Executable \"D:\\Tools\\CodexTray\\CodexTray.exe\"",
                    "sourcePath": "C:\\Users\\person\\.codex\\hooks.json",
                    "enabled": true,
                    "trustStatus": if *event == "PostToolUse" { "modified" } else { "trusted" }
                })
            })
            .collect::<Vec<_>>();
        let response = json!({
            "result": {
                "data": [
                    {
                        "cwd": "C:\\Users\\person",
                        "hooks": hooks,
                        "warnings": [],
                        "errors": []
                    }
                ]
            }
        });

        assert_eq!(
            hook_validation_message(&response, exe, hooks_path),
            Some("CodexTray Hook 未被信任".to_string())
        );
    }

    #[test]
    fn removes_only_codextray_hook_entries_from_existing_config() {
        let exe = Path::new(r"D:\WorkSpace\CodexTray\CodexTray.exe");
        let mut config = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "\"D:\\WorkSpace\\CodexTray\\CodexTray.exe\" --hook-event",
                                "timeout": 5
                            },
                            {
                                "type": "command",
                                "command": "python other.py"
                            }
                        ]
                    }
                ]
            }
        });

        remove_all_codextray_hooks(&mut config);

        let hooks = config
            .get("hooks")
            .and_then(serde_json::Value::as_object)
            .and_then(|root| root.get("PreToolUse"))
            .and_then(serde_json::Value::as_array)
            .and_then(|groups| groups.first())
            .and_then(|group| group.get("hooks"))
            .and_then(serde_json::Value::as_array)
            .expect("event should remain an array");
        assert_eq!(hooks.len(), 1);
        assert_eq!(
            hooks[0].get("command").and_then(serde_json::Value::as_str),
            Some("python other.py")
        );
        assert!(!hooks_config_has_codextray(&config, exe));
    }

    #[test]
    fn removes_stale_codextray_hook_paths_before_installing_current_executable() {
        let current_exe = Path::new(r"D:\Tools\CodexTray\CodexTray.exe");
        let mut config = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "\"D:\\Old\\CodexTray\\CodexTray.exe\" --hook-event"
                            },
                            {
                                "type": "command",
                                "command": "python other.py"
                            }
                        ]
                    }
                ]
            }
        });

        remove_all_codextray_hooks(&mut config);
        install_codextray_hooks(&mut config, current_exe);

        let hooks = config
            .get("hooks")
            .and_then(serde_json::Value::as_object)
            .and_then(|root| root.get("UserPromptSubmit"))
            .and_then(serde_json::Value::as_array)
            .expect("event should have groups");
        let commands = hooks
            .iter()
            .filter_map(|group| group.get("hooks").and_then(serde_json::Value::as_array))
            .flat_map(|hooks| hooks.iter())
            .filter_map(|hook| hook.get("command").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();

        assert_eq!(
            commands,
            vec![
                "python other.py",
                "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"D:\\Tools\\CodexTray\\codextray-hook.ps1\" -Executable \"D:\\Tools\\CodexTray\\CodexTray.exe\""
            ]
        );
    }

    #[test]
    fn reads_global_hook_feature_gate_before_installing_handlers() {
        let config = json!({
            "result": {
                "config": {
                    "features": {
                        "hooks": false
                    }
                }
            }
        });

        assert!(codex_hooks_globally_disabled(&config));
    }

    fn lower_first(value: &str) -> String {
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return String::new();
        };

        first.to_lowercase().collect::<String>() + chars.as_str()
    }
}
