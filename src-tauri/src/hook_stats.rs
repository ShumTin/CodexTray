use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime};
use serde_json::{Map, Value};

use crate::models::HookDayStats;

const APP_DATA_DIR: &str = "CodexTray";
const HOOK_EVENTS_DIR: &str = "HookEvents";
const EVENTS_DIR: &str = "events";
const DAILY_STATS_FILE: &str = "daily.jsonl";
const HOOK_RETENTION_DAYS: i64 = 224;

#[derive(serde::Serialize, serde::Deserialize)]
struct DailyHookStats {
    date: String,
    #[serde(flatten)]
    stats: HookDayStats,
}

#[derive(Default)]
struct HookDayAccumulator {
    sessions: BTreeSet<String>,
    turns: BTreeSet<String>,
    tool_calls: BTreeSet<String>,
    stats: HookDayStats,
}

pub fn run_hook_event_process() -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("无法读取 hook stdin：{}", error))?;
    append_hook_event(&input).map_err(|error| format!("无法写入 hook 事件：{}", error))
}

pub fn scan_hook_daily_stats() -> Result<BTreeMap<String, HookDayStats>, String> {
    scan_hook_daily_stats_from_root(&hook_events_root()?, Local::now().date_naive())
}

fn scan_hook_daily_stats_from_root(
    root: &Path,
    today: NaiveDate,
) -> Result<BTreeMap<String, HookDayStats>, String> {
    let events_dir = root.join(EVENTS_DIR);
    let mut daily_stats = read_daily_stats(root)?;
    let retention_start = today - Duration::days(HOOK_RETENTION_DAYS - 1);
    let daily_stats_changed = retain_recent_daily_stats(&mut daily_stats, retention_start);

    if !events_dir.exists() {
        if daily_stats_changed {
            replace_daily_stats(root, &daily_stats)?;
        }
        return Ok(daily_stats);
    }

    let mut active_event_files = Vec::new();
    let mut event_files_to_delete = Vec::new();
    let mut has_completed_events = false;
    for entry in
        fs::read_dir(&events_dir).map_err(|error| format!("无法读取 Hook 目录：{}", error))?
    {
        let entry = entry.map_err(|error| format!("无法读取 Hook 目录项：{}", error))?;
        let path = entry.path();

        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }

        let Some(date) = event_file_date(&path) else {
            active_event_files.push(path);
            continue;
        };

        if date < retention_start {
            event_files_to_delete.push(path);
            continue;
        }

        if date < today {
            merge_event_file_stats(&path, &mut daily_stats)?;
            event_files_to_delete.push(path);
            has_completed_events = true;
            continue;
        }

        active_event_files.push(path);
    }

    if daily_stats_changed || has_completed_events {
        replace_daily_stats(root, &daily_stats)?;
    }

    for path in event_files_to_delete {
        fs::remove_file(path).map_err(|error| format!("无法清理过期 Hook 事件：{}", error))?;
    }

    for path in active_event_files {
        let mut accumulators = BTreeMap::<String, HookDayAccumulator>::new();
        scan_hook_event_file(&path, &mut accumulators)?;
        for (date, accumulator) in accumulators {
            daily_stats.insert(date, accumulator.stats);
        }
    }

    retain_recent_daily_stats(&mut daily_stats, retention_start);

    Ok(daily_stats)
}

fn event_file_date(path: &Path) -> Option<NaiveDate> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn merge_event_file_stats(
    path: &Path,
    daily_stats: &mut BTreeMap<String, HookDayStats>,
) -> Result<(), String> {
    let mut accumulators = BTreeMap::<String, HookDayAccumulator>::new();
    scan_hook_event_file(path, &mut accumulators)?;

    for (date, accumulator) in accumulators {
        daily_stats.insert(date, accumulator.stats);
    }

    Ok(())
}

fn retain_recent_daily_stats(
    daily_stats: &mut BTreeMap<String, HookDayStats>,
    retention_start: NaiveDate,
) -> bool {
    let original_count = daily_stats.len();
    daily_stats.retain(|date, _| {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map(|value| value >= retention_start)
            .unwrap_or(false)
    });
    daily_stats.len() != original_count
}

fn read_daily_stats(root: &Path) -> Result<BTreeMap<String, HookDayStats>, String> {
    let path = root.join(DAILY_STATS_FILE);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let file = File::open(&path).map_err(|error| format!("无法打开 Hook 每日统计：{}", error))?;
    let reader = BufReader::new(file);
    let mut stats = BTreeMap::new();

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<DailyHookStats>(&line) else {
            continue;
        };
        stats.insert(record.date, record.stats);
    }

    Ok(stats)
}

fn replace_daily_stats(
    root: &Path,
    daily_stats: &BTreeMap<String, HookDayStats>,
) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| format!("无法创建 Hook 数据目录：{}", error))?;
    let mut content = Vec::new();
    for (date, stats) in daily_stats {
        let record = DailyHookStats {
            date: date.to_string(),
            stats: stats.clone(),
        };
        let mut line = serde_json::to_vec(&record)
            .map_err(|error| format!("无法序列化 Hook 每日统计：{}", error))?;
        line.push(b'\n');
        content.extend(line);
    }

    let path = root.join(DAILY_STATS_FILE);
    let temporary_path = root.join(format!("{}.tmp", DAILY_STATS_FILE));
    fs::write(&temporary_path, content)
        .map_err(|error| format!("无法写入 Hook 每日统计：{}", error))?;
    fs::rename(&temporary_path, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary_path);
        format!("无法更新 Hook 每日统计：{}", error)
    })
}

fn append_hook_event(input: &str) -> io::Result<()> {
    let now = Local::now();
    let root = hook_events_root_io()?;
    append_hook_event_to_root(input, &root, now)
}

fn append_hook_event_to_root(input: &str, root: &Path, now: DateTime<Local>) -> io::Result<()> {
    let events_dir = root.join(EVENTS_DIR);
    fs::create_dir_all(&events_dir)?;

    let date = now.date_naive().to_string();
    let path = events_dir.join(format!("{}.jsonl", date));
    let value = compact_hook_event(normalize_hook_input(input), now);

    let mut line = serde_json::to_vec(&value)?;
    line.push(b'\n');

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&line)?;

    Ok(())
}

fn normalize_hook_input(input: &str) -> Value {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }

    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(object)) => Value::Object(object),
        Ok(value) => {
            let mut object = Map::new();
            object.insert("payload".to_string(), value);
            Value::Object(object)
        }
        Err(_) => {
            let mut object = Map::new();
            object.insert("raw".to_string(), Value::String(trimmed.to_string()));
            Value::Object(object)
        }
    }
}

/// 仅保留生成 Hook 统计所需的字段，避免命令输入和工具响应长期占用本地空间。
fn compact_hook_event(value: Value, now: DateTime<Local>) -> Value {
    let Value::Object(input) = value else {
        return Value::Object(Map::new());
    };
    let mut compact = Map::new();

    copy_string_field(
        &input,
        &["hook_event_name", "event", "hook_event", "hookEvent"],
        "hook_event_name",
        &mut compact,
    );
    copy_string_field(
        &input,
        &["session_id", "session", "sessionId"],
        "session_id",
        &mut compact,
    );
    copy_string_field(
        &input,
        &["turn_id", "turn", "turnId"],
        "turn_id",
        &mut compact,
    );
    copy_string_field(
        &input,
        &["tool_use_id", "toolUseId"],
        "tool_use_id",
        &mut compact,
    );
    copy_string_field(
        &input,
        &["tool_name", "tool", "toolName"],
        "tool_name",
        &mut compact,
    );
    copy_string_field(
        &input,
        &["receivedAt", "timestamp", "time"],
        "receivedAt",
        &mut compact,
    );
    compact
        .entry("receivedAt")
        .or_insert_with(|| Value::String(now.to_rfc3339()));

    Value::Object(compact)
}

fn copy_string_field(
    input: &Map<String, Value>,
    source_names: &[&str],
    target_name: &str,
    target: &mut Map<String, Value>,
) {
    if let Some(value) = source_names
        .iter()
        .find_map(|name| input.get(*name).and_then(Value::as_str))
    {
        target.insert(target_name.to_string(), Value::String(value.to_string()));
    }
}

fn scan_hook_event_file(
    path: &Path,
    accumulators: &mut BTreeMap<String, HookDayAccumulator>,
) -> Result<(), String> {
    let fallback_date = path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_string);
    let file = File::open(path).map_err(|error| format!("无法打开 Hook 事件：{}", error))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };

        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        let Some(date) = event_date(&value).or_else(|| fallback_date.clone()) else {
            continue;
        };

        let accumulator = accumulators.entry(date).or_default();
        accumulate_event(&value, accumulator);
    }

    Ok(())
}

fn accumulate_event(value: &Value, accumulator: &mut HookDayAccumulator) {
    if let Some(session) = string_field(value, &["session", "session_id", "sessionId"]) {
        if accumulator.sessions.insert(session.to_string()) {
            accumulator.stats.session_count += 1;
        }
    }

    if let Some(turn) = string_field(value, &["turn", "turn_id", "turnId"]) {
        if accumulator.turns.insert(turn.to_string()) {
            accumulator.stats.turn_count += 1;
        }
    }

    let event = string_field(
        value,
        &["hook_event_name", "event", "hook_event", "hookEvent"],
    )
    .unwrap_or_default();

    match event {
        "UserPromptSubmit" => accumulator.stats.prompt_count += 1,
        "PermissionRequest" => accumulator.stats.permission_request_count += 1,
        "PostCompact" => accumulator.stats.compact_count += 1,
        "SubagentStart" => accumulator.stats.subagent_count += 1,
        "PreToolUse" | "PostToolUse" => {
            let key = tool_call_key(value);
            if accumulator.tool_calls.insert(key) {
                accumulator.stats.tool_call_count += 1;
            }
        }
        _ => {}
    }
}

fn tool_call_key(value: &Value) -> String {
    if let Some(tool_use_id) = string_field(value, &["tool_use_id", "toolUseId"]) {
        return tool_use_id.to_string();
    }

    [
        string_field(value, &["session", "session_id", "sessionId"]).unwrap_or(""),
        string_field(value, &["turn", "turn_id", "turnId"]).unwrap_or(""),
        string_field(value, &["tool_name", "tool", "toolName"]).unwrap_or(""),
    ]
    .join("|")
}

fn event_date(value: &Value) -> Option<String> {
    string_field(value, &["timestamp", "time", "receivedAt"])
        .and_then(parse_event_date)
        .map(|date| date.to_string())
}

fn parse_event_date(value: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Local).date_naive())
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
                .map(|date| date.date())
                .ok()
        })
        .or_else(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn string_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn hook_events_root() -> Result<PathBuf, String> {
    hook_events_root_io().map_err(|error| format!("无法解析 Hook 数据目录：{}", error))
}

fn hook_events_root_io() -> io::Result<PathBuf> {
    if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join(APP_DATA_DIR)
            .join(HOOK_EVENTS_DIR));
    }

    Ok(env::current_dir()?.join(APP_DATA_DIR).join(HOOK_EVENTS_DIR))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::Local;

    use super::{
        append_hook_event_to_root, scan_hook_daily_stats_from_root, scan_hook_event_file,
        DailyHookStats, HookDayAccumulator, DAILY_STATS_FILE, EVENTS_DIR,
    };

    #[test]
    fn compacts_completed_event_files_and_keeps_today_events() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-daily-compaction-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let events_dir = root.join(EVENTS_DIR);
        fs::create_dir_all(&events_dir).expect("events dir should be created");
        let completed_path = events_dir.join("2026-07-10.jsonl");
        fs::write(
            &completed_path,
            concat!(
                r#"{"receivedAt":"2026-07-10T09:00:00+08:00","hook_event_name":"UserPromptSubmit","session_id":"old-session","turn_id":"old-turn"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-10T09:01:00+08:00","hook_event_name":"PreToolUse","session_id":"old-session","turn_id":"old-turn","tool_use_id":"old-call"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-10T09:01:01+08:00","hook_event_name":"PostToolUse","session_id":"old-session","turn_id":"old-turn","tool_use_id":"old-call"}"#,
                "\n",
            ),
        )
        .expect("completed event file should be written");
        let active_path = events_dir.join("2026-07-11.jsonl");
        fs::write(
            &active_path,
            r#"{"receivedAt":"2026-07-11T09:00:00+08:00","hook_event_name":"PermissionRequest","session_id":"active-session","turn_id":"active-turn"}"#,
        )
        .expect("active event file should be written");

        let stats = scan_hook_daily_stats_from_root(
            &root,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 11).expect("test date should be valid"),
        )
        .expect("Hook stats should aggregate");

        assert!(!completed_path.exists());
        assert!(active_path.exists());
        assert_eq!(stats["2026-07-10"].prompt_count, 1);
        assert_eq!(stats["2026-07-10"].tool_call_count, 1);
        assert_eq!(stats["2026-07-11"].permission_request_count, 1);

        let daily_line = fs::read_to_string(root.join(DAILY_STATS_FILE))
            .expect("daily stats file should be written");
        let record: DailyHookStats =
            serde_json::from_str(daily_line.trim()).expect("daily stats line should be valid JSON");
        assert_eq!(record.date, "2026-07-10");
        assert_eq!(record.stats.tool_call_count, 1);

        fs::remove_dir_all(root).expect("test dir should be removed");
    }

    #[test]
    fn removes_hook_data_outside_the_visible_retention_window() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-retention-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let events_dir = root.join(EVENTS_DIR);
        fs::create_dir_all(&events_dir).expect("events dir should be created");
        let expired_path = events_dir.join("2025-01-01.jsonl");
        fs::write(
            &expired_path,
            r#"{"receivedAt":"2025-01-01T09:00:00+08:00","hook_event_name":"UserPromptSubmit"}"#,
        )
        .expect("expired event file should be written");
        fs::write(
            root.join(DAILY_STATS_FILE),
            concat!(
                r#"{"date":"2025-01-01","sessionCount":1,"promptCount":1,"turnCount":1,"toolCallCount":0,"permissionRequestCount":0,"compactCount":0,"subagentCount":0}"#,
                "\n",
                r#"{"date":"2026-07-10","sessionCount":2,"promptCount":2,"turnCount":2,"toolCallCount":1,"permissionRequestCount":0,"compactCount":0,"subagentCount":0}"#,
                "\n",
            ),
        )
        .expect("daily stats should be written");

        let stats = scan_hook_daily_stats_from_root(
            &root,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 11).expect("test date should be valid"),
        )
        .expect("Hook stats should prune expired data");

        assert!(!expired_path.exists());
        assert!(!stats.contains_key("2025-01-01"));
        assert_eq!(stats["2026-07-10"].tool_call_count, 1);
        let daily_content = fs::read_to_string(root.join(DAILY_STATS_FILE))
            .expect("daily stats should be readable");
        assert!(!daily_content.contains("2025-01-01"));
        assert!(daily_content.contains("2026-07-10"));

        fs::remove_dir_all(root).expect("test dir should be removed");
    }

    #[test]
    fn keeps_completed_events_when_daily_stats_cannot_be_written() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-daily-write-failure-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let events_dir = root.join(EVENTS_DIR);
        fs::create_dir_all(&events_dir).expect("events dir should be created");
        let completed_path = events_dir.join("2026-07-10.jsonl");
        fs::write(
            &completed_path,
            r#"{"receivedAt":"2026-07-10T09:00:00+08:00","hook_event_name":"UserPromptSubmit"}"#,
        )
        .expect("completed event file should be written");
        fs::create_dir(root.join(DAILY_STATS_FILE))
            .expect("daily stats path should block file writes");

        let result = scan_hook_daily_stats_from_root(
            &root,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 11).expect("test date should be valid"),
        );

        assert!(result.is_err());
        assert!(completed_path.exists());

        fs::remove_dir_all(root).expect("test dir should be removed");
    }

    #[test]
    fn stores_only_fields_required_for_hook_statistics() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-compaction-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let now = Local::now();
        append_hook_event_to_root(
            r#"{"hook_event_name":"PostToolUse","session_id":"s1","turn_id":"t1","tool_use_id":"call-1","tool_name":"shell","tool_input":{"command":"very sensitive command"},"tool_response":{"output":"large response"},"transcript_path":"C:\\private\\session.jsonl","cwd":"D:\\WorkSpace"}"#,
            &root,
            now,
        )
        .expect("hook event should append");

        let path = root
            .join(EVENTS_DIR)
            .join(format!("{}.jsonl", now.date_naive()));
        let line = fs::read_to_string(path).expect("event file should read");
        let event: serde_json::Value =
            serde_json::from_str(line.trim()).expect("stored event should remain valid JSON");

        assert_eq!(event["hook_event_name"], "PostToolUse");
        assert_eq!(event["session_id"], "s1");
        assert_eq!(event["turn_id"], "t1");
        assert_eq!(event["tool_use_id"], "call-1");
        assert_eq!(event["tool_name"], "shell");
        assert!(event.get("receivedAt").is_some());
        assert!(event.get("tool_input").is_none());
        assert!(event.get("tool_response").is_none());
        assert!(event.get("transcript_path").is_none());
        assert!(event.get("cwd").is_none());

        fs::remove_dir_all(root).expect("test dir should be removed");
    }

    #[test]
    fn aggregates_hook_events_into_daily_workflow_stats() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("test dir should be created");
        let path = root.join("2026-07-02.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"receivedAt":"2026-07-02T09:00:00+08:00","hook_event_name":"UserPromptSubmit","session_id":"s1","turn_id":"t1"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:01:00+08:00","hook_event_name":"PreToolUse","session_id":"s1","turn_id":"t1","tool_name":"Bash","tool_use_id":"call-1"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:01:01+08:00","hook_event_name":"PostToolUse","session_id":"s1","turn_id":"t1","tool_name":"Bash","tool_use_id":"call-1"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:02:00+08:00","hook_event_name":"PermissionRequest","session_id":"s1","turn_id":"t2"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:03:00+08:00","hook_event_name":"SubagentStart","session_id":"s2","turn_id":"t3"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:04:00+08:00","hook_event_name":"PreCompact","session_id":"s2","turn_id":"t3"}"#,
                "\n",
                r#"{"receivedAt":"2026-07-02T09:04:01+08:00","hook_event_name":"PostCompact","session_id":"s2","turn_id":"t3"}"#,
                "\n",
            ),
        )
        .expect("test events should be written");
        let mut accumulators = std::collections::BTreeMap::<String, HookDayAccumulator>::new();

        scan_hook_event_file(&path, &mut accumulators).expect("events should aggregate");
        let stats = &accumulators
            .get("2026-07-02")
            .expect("target day should be aggregated")
            .stats;

        assert_eq!(stats.session_count, 2);
        assert_eq!(stats.prompt_count, 1);
        assert_eq!(stats.turn_count, 3);
        assert_eq!(stats.tool_call_count, 1);
        assert_eq!(stats.permission_request_count, 1);
        assert_eq!(stats.subagent_count, 1);
        assert_eq!(stats.compact_count, 1);

        fs::remove_dir_all(root).expect("test dir should be removed");
    }

    #[test]
    fn appends_concurrent_hook_events_without_corrupting_jsonl_lines() {
        let root = std::env::temp_dir().join(format!(
            "codextray-hook-concurrency-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let now = Local::now();
        let handles: Vec<_> = (0..12)
            .map(|index| {
                let root = root.clone();
                thread::spawn(move || {
                    append_hook_event_to_root(
                        &format!(
                            r#"{{"event":"PostToolUse","session":"s","turn":"t{}","tool":"shell"}}"#,
                            index
                        ),
                        &root,
                        now,
                    )
                    .expect("hook event should append");
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("hook append thread should finish");
        }

        let path = root
            .join(EVENTS_DIR)
            .join(format!("{}.jsonl", now.date_naive()));
        let file = fs::File::open(path).expect("event file should exist");
        let reader = BufReader::new(file);
        let lines: Vec<String> = reader
            .lines()
            .map(|line| line.expect("line should read"))
            .collect();

        assert_eq!(lines.len(), 12);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(&line)
                .expect("line should remain valid JSON");
        }

        fs::remove_dir_all(root).expect("test dir should be removed");
    }
}
