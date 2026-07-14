use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::models::{
    AccountSnapshot, QuotaFetchResult, QuotaSourceKind, QuotaWindow, RateLimitResetCredit,
    RateLimitResetCredits, RateLimitSnapshot, TokenActivityBucket, TokenActivitySnapshot,
    TokenActivitySource,
};

#[derive(Debug, Clone)]
pub struct CliProbe {
    pub path: PathBuf,
    pub version: String,
}

pub async fn probe_codex_cli(configured_path: Option<PathBuf>) -> Result<CliProbe, String> {
    let candidates = collect_candidates(configured_path);

    for candidate in candidates {
        if let Ok(version) = read_cli_version(&candidate).await {
            return Ok(CliProbe {
                path: candidate,
                version,
            });
        }
    }

    Err("未找到可启动的 Codex CLI".to_string())
}

pub async fn probe_codex_cli_path(path: PathBuf) -> Result<CliProbe, String> {
    let version = read_cli_version(&path).await?;
    Ok(CliProbe { path, version })
}

pub async fn fetch_cli_quota(probe: &CliProbe) -> Result<QuotaFetchResult, String> {
    let mut command = codex_app_server_command(&probe.path);
    let mut child = command
        .spawn()
        .map_err(|error| format!("CLI app-server 启动失败：{}", error))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "CLI app-server stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "CLI app-server stdout 不可用".to_string())?;
    let mut reader = BufReader::new(stdout);

    initialize_app_server(&mut stdin, &mut reader).await?;

    send_json_line(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"account/read","params":{"refreshToken":true}}),
    )
    .await?;
    let account = read_json_response(&mut reader, 2).await?;

    send_json_line(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":3,"method":"account/rateLimits/read"}),
    )
    .await?;
    let quota = read_json_response(&mut reader, 3).await?;

    let _ = child.kill().await;
    build_cli_result(account, quota, None)
}

pub async fn fetch_cli_usage(probe: &CliProbe) -> Result<TokenActivitySnapshot, String> {
    let mut command = codex_app_server_command(&probe.path);
    let mut child = command
        .spawn()
        .map_err(|error| format!("CLI app-server 启动失败：{}", error))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "CLI app-server stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "CLI app-server stdout 不可用".to_string())?;
    let mut reader = BufReader::new(stdout);

    initialize_app_server(&mut stdin, &mut reader).await?;

    send_json_line(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"account/usage/read"}),
    )
    .await?;
    let usage = read_json_response(&mut reader, 2).await?;
    let _ = child.kill().await;

    parse_cli_usage(usage).ok_or_else(|| "CLI app-server 未返回可识别 Token 活动".to_string())
}

pub async fn read_codex_config(probe: &CliProbe) -> Result<Value, String> {
    let mut responses = run_app_server_requests(probe, vec![("config/read", json!({}))]).await?;
    Ok(responses.remove(0))
}

pub async fn list_codex_hooks(probe: &CliProbe, cwds: Vec<String>) -> Result<Value, String> {
    let mut responses =
        run_app_server_requests(probe, vec![("hooks/list", json!({ "cwds": cwds }))]).await?;
    Ok(responses.remove(0))
}

pub async fn write_codex_config_batch(
    probe: &CliProbe,
    edits: Vec<Value>,
) -> Result<Value, String> {
    let mut responses = run_app_server_requests(
        probe,
        vec![("config/batchWrite", json!({ "edits": edits }))],
    )
    .await?;
    Ok(responses.remove(0))
}

async fn run_app_server_requests(
    probe: &CliProbe,
    requests: Vec<(&str, Value)>,
) -> Result<Vec<Value>, String> {
    let mut command = codex_app_server_command(&probe.path);
    let mut child = command
        .spawn()
        .map_err(|error| format!("CLI app-server 启动失败：{}", error))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "CLI app-server stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "CLI app-server stdout 不可用".to_string())?;
    let mut reader = BufReader::new(stdout);

    initialize_app_server(&mut stdin, &mut reader).await?;

    let mut responses = Vec::with_capacity(requests.len());
    for (index, (method, params)) in requests.into_iter().enumerate() {
        let id = index as i64 + 2;
        send_json_line(
            &mut stdin,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }),
        )
        .await?;
        responses.push(read_json_response(&mut reader, id).await?);
    }

    let _ = child.kill().await;
    Ok(responses)
}

async fn initialize_app_server(
    stdin: &mut tokio::process::ChildStdin,
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<(), String> {
    send_json_line(
        stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "CodexTray",
                    "title": "CodexTray",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": false,
                    "requestAttestation": false
                }
            }
        }),
    )
    .await?;
    let _ = read_json_response(reader, 1).await?;
    send_json_line(
        stdin,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    )
    .await
}

fn collect_candidates(configured_path: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = configured_path {
        candidates.push(path);
    }

    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        candidates.extend(collect_local_app_data_candidates(PathBuf::from(
            local_app_data,
        )));
    }

    if let Ok(user_profile) = env::var("USERPROFILE") {
        candidates.extend(collect_vscode_extension_candidates(PathBuf::from(
            user_profile,
        )));
    }

    if let Ok(path) = env::var("PATH") {
        let names = if cfg!(windows) {
            vec!["codex.cmd", "codex.exe", "codex"]
        } else {
            vec!["codex"]
        };

        for dir in env::split_paths(&path) {
            for name in &names {
                candidates.push(dir.join(name));
            }
        }
    }

    deduplicate_candidates(candidates)
}

fn collect_local_app_data_candidates(local_app_data: PathBuf) -> Vec<PathBuf> {
    let bin_dir = local_app_data.join("OpenAI\\Codex\\bin");
    let mut candidates = vec![bin_dir.join("codex.exe")];
    let Ok(entries) = fs::read_dir(&bin_dir) else {
        return candidates;
    };
    let mut versioned_candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("codex.exe"))
        .filter(|path| path.is_file())
        .collect();

    versioned_candidates.sort();
    versioned_candidates.reverse();
    candidates.extend(versioned_candidates);
    candidates
}

fn collect_vscode_extension_candidates(user_profile: PathBuf) -> Vec<PathBuf> {
    let extension_roots = vec![
        user_profile.join(".vscode\\extensions"),
        user_profile.join(".vscode-insiders\\extensions"),
    ];
    let mut candidates = Vec::new();

    for root in extension_roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };

        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() && vscode_extension_may_contain_codex(&path) {
                collect_codex_executables(&path, &mut candidates);
            }
        }
    }

    candidates.sort();
    candidates.reverse();
    candidates
}

fn vscode_extension_may_contain_codex(path: &PathBuf) -> bool {
    let directory_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if text_mentions_codex(&directory_name) {
        return true;
    }

    let Ok(content) = fs::read_to_string(path.join("package.json")) else {
        return false;
    };
    text_mentions_codex(&content)
}

fn text_mentions_codex(value: &str) -> bool {
    let text = value.to_ascii_lowercase();
    text.contains("codex") || text.contains("chatgpt") || text.contains("openai")
}

fn collect_codex_executables(root: &PathBuf, candidates: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_executables(&path, candidates);
            continue;
        }

        if is_codex_executable(&path) {
            candidates.push(path);
        }
    }
}

fn is_codex_executable(path: &PathBuf) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if cfg!(windows) {
        name == "codex.exe" || name == "codex.cmd" || name == "codex"
    } else {
        name == "codex"
    }
}

fn deduplicate_candidates(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduplicated = Vec::new();

    for candidate in candidates {
        if !deduplicated.iter().any(|item| item == &candidate) {
            deduplicated.push(candidate);
        }
    }

    deduplicated
}

async fn read_cli_version(path: &PathBuf) -> Result<String, String> {
    let output = timeout(Duration::from_secs(3), codex_version_command(path).output())
        .await
        .map_err(|_| "CLI 探测超时".to_string())?
        .map_err(|error| format!("CLI 不可启动：{}", error))?;

    if !output.status.success() {
        return Err("CLI --version 返回失败".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

fn codex_app_server_command(path: &PathBuf) -> Command {
    let mut command = Command::new(path);
    command
        .arg("app-server")
        .arg("--listen")
        .arg("stdio://")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_child_console(&mut command);

    command
}

fn codex_version_command(path: &PathBuf) -> Command {
    let mut command = Command::new(path);
    command.arg("--version");
    hide_child_console(&mut command);

    command
}

fn hide_child_console(command: &mut Command) {
    configure_hidden_child_process(command);
}

#[cfg(windows)]
fn configure_hidden_child_process(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_child_process(_command: &mut Command) {}

async fn send_json_line(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> Result<(), String> {
    let mut line = serde_json::to_vec(&value).map_err(|_| "JSON-RPC 请求序列化失败".to_string())?;
    line.push(b'\n');
    timeout(Duration::from_secs(5), stdin.write_all(&line))
        .await
        .map_err(|_| "CLI app-server 写入超时".to_string())?
        .map_err(|error| format!("CLI app-server 写入失败：{}", error))
}

async fn read_json_line(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Value, String> {
    let mut line = String::new();
    timeout(Duration::from_secs(8), reader.read_line(&mut line))
        .await
        .map_err(|_| "CLI app-server 响应超时".to_string())?
        .map_err(|error| format!("CLI app-server 读取失败：{}", error))?;

    if line.trim().is_empty() {
        return Err("CLI app-server 未返回响应".to_string());
    }

    serde_json::from_str(line.trim()).map_err(|_| "CLI app-server 协议响应不兼容".to_string())
}

async fn read_json_response(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    expected_id: i64,
) -> Result<Value, String> {
    loop {
        let value = read_json_line(reader).await?;

        if value.get("id").and_then(Value::as_i64) == Some(expected_id) {
            if let Some(error) = value.get("error") {
                return Err(format!(
                    "CLI app-server 返回错误：{}",
                    sanitize_json_rpc_error(error)
                ));
            }

            return Ok(value);
        }
    }
}

fn sanitize_json_rpc_error(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("未知错误")
        .replace("access_token", "token")
        .replace("refresh_token", "token")
}

fn build_cli_result(
    account: Value,
    quota: Value,
    usage: Option<Value>,
) -> Result<QuotaFetchResult, String> {
    let account_result = account.get("result").unwrap_or(&account);
    let quota_result = quota.get("result").unwrap_or(&quota);
    let fetched_at = Utc::now().to_rfc3339();
    let account_value = account_result.get("account").unwrap_or(account_result);
    let email = account_value
        .get("email")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let windows = extract_cli_windows(quota_result);
    let reset_credits = extract_reset_credits(quota_result);

    if windows.is_empty() {
        return Err("CLI app-server 未返回可识别额度窗口".to_string());
    }

    Ok(QuotaFetchResult {
        account: AccountSnapshot {
            email,
            plan: account_value
                .get("planType")
                .or_else(|| account_value.get("plan"))
                .and_then(Value::as_str)
                .map(|value| value.to_uppercase()),
            status: "已连接".to_string(),
            updated_at: fetched_at.clone(),
        },
        quota: RateLimitSnapshot {
            source: QuotaSourceKind::CodexCli,
            windows,
            reset_credits,
            fetched_at,
            stale: false,
        },
        token_activity: usage.and_then(parse_cli_usage),
    })
}

fn parse_cli_usage(value: Value) -> Option<TokenActivitySnapshot> {
    let result = value.get("result").unwrap_or(&value);
    let summary = result.get("summary")?;
    let daily_buckets = result
        .get("dailyUsageBuckets")
        .and_then(Value::as_array)
        .map(|buckets| {
            buckets
                .iter()
                .filter_map(|bucket| {
                    Some(TokenActivityBucket {
                        date: bucket.get("startDate")?.as_str()?.to_string(),
                        tokens: parse_u64_field(bucket.get("tokens")?)?,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(TokenActivitySnapshot {
        source: TokenActivitySource::ProfileUsageApi,
        lifetime_tokens: summary.get("lifetimeTokens").and_then(parse_u64_field),
        peak_daily_tokens: summary.get("peakDailyTokens").and_then(parse_u64_field),
        longest_running_turn_sec: summary
            .get("longestRunningTurnSec")
            .and_then(parse_u64_field),
        current_streak_days: summary.get("currentStreakDays").and_then(parse_u64_field),
        longest_streak_days: summary.get("longestStreakDays").and_then(parse_u64_field),
        daily_buckets,
    })
}

fn parse_u64_field(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
}

fn extract_cli_windows(value: &Value) -> Vec<QuotaWindow> {
    if let Some(rate_limits_by_id) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        let mut windows = Vec::new();

        for (limit_id, snapshot) in rate_limits_by_id {
            append_rate_limit_snapshot(&mut windows, limit_id, snapshot);
        }

        if !windows.is_empty() {
            return windows;
        }
    }

    if let Some(snapshot) = value.get("rateLimits") {
        let mut windows = Vec::new();
        append_rate_limit_snapshot(&mut windows, "codex", snapshot);

        if !windows.is_empty() {
            return windows;
        }
    }

    let source = value
        .get("rate_limits")
        .or_else(|| value.get("rateLimits"))
        .or_else(|| value.get("limits"))
        .unwrap_or(value);

    match source {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let label = item
                    .get("window")
                    .or_else(|| item.get("label"))
                    .and_then(Value::as_str)?
                    .to_string();
                Some(QuotaWindow {
                    label,
                    remaining_percent: remaining_percent(item),
                    reset_at: item
                        .get("reset_at")
                        .or_else(|| item.get("resetAt"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                })
            })
            .collect(),
        Value::Object(map) => map
            .iter()
            .map(|(label, item)| QuotaWindow {
                label: label.clone(),
                remaining_percent: remaining_percent(item),
                reset_at: item
                    .get("reset_at")
                    .or_else(|| item.get("resetAt"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn append_rate_limit_snapshot(windows: &mut Vec<QuotaWindow>, limit_id: &str, snapshot: &Value) {
    let limit_name = snapshot
        .get("limitName")
        .or_else(|| snapshot.get("limit_name"))
        .and_then(Value::as_str)
        .unwrap_or(limit_id);

    if let Some(primary) = snapshot.get("primary") {
        let duration_label =
            rate_limit_window_duration_label(primary).unwrap_or_else(|| "5H".to_string());
        append_rate_limit_window(
            windows,
            format!("{} {}", limit_name, duration_label),
            primary,
        );
    }

    if let Some(secondary) = snapshot.get("secondary") {
        let duration_label =
            rate_limit_window_duration_label(secondary).unwrap_or_else(|| "7D".to_string());
        append_rate_limit_window(
            windows,
            format!("{} {}", limit_name, duration_label),
            secondary,
        );
    }
}

fn rate_limit_window_duration_label(window: &Value) -> Option<String> {
    let duration_mins = window.get("windowDurationMins")?.as_u64()?;

    if duration_mins % (24 * 60) == 0 {
        return Some(format!("{}D", duration_mins / (24 * 60)));
    }

    if duration_mins % 60 == 0 {
        return Some(format!("{}H", duration_mins / 60));
    }

    Some(format!("{}M", duration_mins))
}

fn append_rate_limit_window(windows: &mut Vec<QuotaWindow>, label: String, window: &Value) {
    let Some(used_percent) = window.get("usedPercent").and_then(Value::as_f64) else {
        return;
    };
    let remaining_percent = (100.0 - used_percent).clamp(0.0, 100.0).round() as u8;
    let reset_at = window
        .get("resetsAt")
        .and_then(Value::as_i64)
        .map(normalize_unix_timestamp)
        .map(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0))
        .and_then(|value| value.map(|time| time.to_rfc3339()));

    windows.push(QuotaWindow {
        label,
        remaining_percent,
        reset_at,
    });
}

fn extract_reset_credits(value: &Value) -> Option<RateLimitResetCredits> {
    let credits = value.get("rateLimitResetCredits")?;
    if credits.is_null() {
        return None;
    }

    let available_count = credits
        .get("availableCount")
        .or_else(|| credits.get("available_count"))
        .and_then(parse_u64_field)?;
    let credit_items = credits
        .get("credits")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(index, item)| extract_reset_credit(item, index))
                .collect()
        })
        .unwrap_or_default();

    Some(RateLimitResetCredits {
        available_count,
        credits: credit_items,
    })
}

fn extract_reset_credit(value: &Value, index: usize) -> RateLimitResetCredit {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("reset-credit-{index}"));
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "限额重置".to_string());
    let expires_at = value
        .get("expiresAt")
        .or_else(|| value.get("expires_at"))
        .or_else(|| value.get("expirationTime"))
        .and_then(parse_optional_time_field);

    RateLimitResetCredit {
        id,
        title,
        expires_at,
    }
}

fn parse_optional_time_field(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }

    if let Some(timestamp) = value.as_i64() {
        return chrono::DateTime::from_timestamp(normalize_unix_timestamp(timestamp), 0)
            .map(|time| time.to_rfc3339());
    }

    let text = value.as_str()?;
    if let Ok(timestamp) = text.parse::<i64>() {
        return chrono::DateTime::from_timestamp(normalize_unix_timestamp(timestamp), 0)
            .map(|time| time.to_rfc3339());
    }

    Some(text.to_string())
}

fn normalize_unix_timestamp(timestamp: i64) -> i64 {
    if timestamp > 10_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    }
}

fn remaining_percent(value: &Value) -> u8 {
    if let Some(percent) = value
        .get("remaining_percent")
        .or_else(|| value.get("remainingPercent"))
        .and_then(Value::as_f64)
    {
        return percent.clamp(0.0, 100.0).round() as u8;
    }

    let remaining = value.get("remaining").and_then(Value::as_f64);
    let limit = value
        .get("limit")
        .or_else(|| value.get("total"))
        .and_then(Value::as_f64);

    match (remaining, limit) {
        (Some(remaining), Some(limit)) if limit > 0.0 => {
            ((remaining / limit) * 100.0).clamp(0.0, 100.0).round() as u8
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{
        build_cli_result, collect_local_app_data_candidates, collect_vscode_extension_candidates,
        extract_cli_windows, parse_cli_usage,
    };

    #[test]
    fn includes_codex_app_versioned_cli_without_requiring_path() {
        let root = std::env::temp_dir().join(format!(
            "codextray-cli-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let versioned_dir = root.join("OpenAI\\Codex\\bin\\ea1c60319a1dcb19");
        fs::create_dir_all(&versioned_dir).expect("test bin dir should be created");
        fs::write(versioned_dir.join("codex.exe"), b"").expect("test codex.exe should be created");

        let candidates = collect_local_app_data_candidates(root.clone());

        assert!(candidates.contains(&root.join("OpenAI\\Codex\\bin\\ea1c60319a1dcb19\\codex.exe")));

        fs::remove_dir_all(root).expect("test temp dir should be removed");
    }

    #[test]
    fn includes_codex_cli_from_vscode_extension_directory() {
        let root = std::env::temp_dir().join(format!(
            "codextray-vscode-cli-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let extension_dir = root.join(".vscode\\extensions\\openai.chatgpt-26.623.101652-win32-x64");
        let cli_dir = extension_dir.join("bin\\windows-x86_64");
        fs::create_dir_all(&cli_dir).expect("test CLI dir should be created");
        fs::write(
            extension_dir.join("package.json"),
            r#"{"publisher":"OpenAI","name":"codex"}"#,
        )
        .expect("test extension package should be created");
        fs::write(cli_dir.join("codex.exe"), b"").expect("test codex.exe should be created");

        let candidates = collect_vscode_extension_candidates(root.clone());

        assert!(candidates.contains(&cli_dir.join("codex.exe")));

        fs::remove_dir_all(root).expect("test temp dir should be removed");
    }

    #[test]
    fn parses_generated_rate_limit_schema_so_cli_quota_can_render() {
        let value = json!({
            "rateLimits": {
                "limitId": "codex",
                "limitName": "Codex",
                "primary": {
                    "usedPercent": 34.0,
                    "windowDurationMins": 300,
                    "resetsAt": 1_783_000_000
                },
                "secondary": {
                    "usedPercent": 20.0,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_783_400_000_000_i64
                },
                "credits": null,
                "individualLimit": null,
                "planType": "plus",
                "rateLimitReachedType": null
            },
            "rateLimitsByLimitId": null,
            "rateLimitResetCredits": {
                "availableCount": 2,
                "credits": [
                    {
                        "id": "reset-credit-1",
                        "status": "available",
                        "expiresAt": 1_783_600_000,
                        "title": "Full reset"
                    },
                    {
                        "id": "reset-credit-2",
                        "status": "available",
                        "expiresAt": 1_784_600_000,
                        "title": "Another reset"
                    }
                ]
            }
        });

        let windows = extract_cli_windows(&value);

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "Codex 5H");
        assert_eq!(windows[0].remaining_percent, 66);
        assert_eq!(windows[1].label, "Codex 7D");
        assert_eq!(windows[1].remaining_percent, 80);
        assert!(windows[1].reset_at.is_some());

        let result = build_cli_result(json!({ "account": {} }), json!({ "result": value }), None)
            .expect("CLI result should include reset credits");
        let reset_credits = result
            .quota
            .reset_credits
            .expect("reset credits should parse");
        assert_eq!(reset_credits.available_count, 2);
        assert_eq!(reset_credits.credits.len(), 2);
        assert_eq!(reset_credits.credits[0].title, "Full reset");
        assert!(reset_credits
            .credits
            .iter()
            .all(|credit| credit.expires_at.is_some()));
    }

    #[test]
    fn uses_reported_duration_when_seven_day_quota_is_the_primary_window() {
        let value = json!({
            "rateLimits": {
                "limitName": "Codex",
                "primary": {
                    "usedPercent": 1.0,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_784_500_000
                },
                "secondary": null
            }
        });

        let windows = extract_cli_windows(&value);

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Codex 7D");
        assert_eq!(windows[0].remaining_percent, 99);
    }

    #[test]
    fn parses_account_response_so_header_shows_the_signed_in_cli_user() {
        let account = json!({
            "id": 2,
            "result": {
                "account": {
                    "type": "chatgpt",
                    "email": "person@example.com",
                    "planType": "plus"
                },
                "requiresOpenaiAuth": false
            }
        });
        let quota = json!({
            "id": 3,
            "result": {
                "rateLimits": {
                    "limitName": "Codex",
                    "primary": { "usedPercent": 50.0, "windowDurationMins": 300, "resetsAt": null },
                    "secondary": null
                }
            }
        });

        let result = build_cli_result(account, quota, None).expect("CLI result should parse");

        assert_eq!(result.account.email.as_deref(), Some("person@example.com"));
        assert_eq!(result.account.plan.as_deref(), Some("PLUS"));
        assert_eq!(result.quota.windows[0].remaining_percent, 50);
    }

    #[test]
    fn parses_profile_usage_summary_so_p3_metrics_are_real() {
        let usage = json!({
            "id": 4,
            "result": {
                "summary": {
                    "lifetimeTokens": "300",
                    "peakDailyTokens": 200,
                    "longestRunningTurnSec": 90,
                    "currentStreakDays": 2,
                    "longestStreakDays": 5
                },
                "dailyUsageBuckets": [
                    { "startDate": "2026-07-01", "tokens": "100" },
                    { "startDate": "2026-07-02", "tokens": 200 }
                ]
            }
        });

        let activity = parse_cli_usage(usage).expect("usage response should parse");

        assert_eq!(activity.lifetime_tokens, Some(300));
        assert_eq!(activity.peak_daily_tokens, Some(200));
        assert_eq!(activity.daily_buckets.len(), 2);
    }
}
