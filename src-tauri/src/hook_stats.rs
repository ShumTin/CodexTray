use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use serde_json::{Map, Value};

use crate::models::HookDayStats;

const APP_DATA_DIR: &str = "CodexTray";
const HOOK_EVENTS_DIR: &str = "HookEvents";
const EVENTS_DIR: &str = "events";

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
    let events_dir = hook_events_root()?.join(EVENTS_DIR);
    let mut accumulators = BTreeMap::<String, HookDayAccumulator>::new();

    if !events_dir.exists() {
        return Ok(BTreeMap::new());
    }

    for entry in
        fs::read_dir(&events_dir).map_err(|error| format!("无法读取 Hook 目录：{}", error))?
    {
        let entry = entry.map_err(|error| format!("无法读取 Hook 目录项：{}", error))?;
        let path = entry.path();

        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            scan_hook_event_file(&path, &mut accumulators)?;
        }
    }

    Ok(accumulators
        .into_iter()
        .map(|(date, accumulator)| (date, accumulator.stats))
        .collect())
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
    let mut value = normalize_hook_input(input);

    if let Value::Object(object) = &mut value {
        object
            .entry("receivedAt")
            .or_insert_with(|| Value::String(now.to_rfc3339()));
    }

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

    let event = string_field(value, &["event", "hook_event", "hookEvent"]).unwrap_or_default();

    match event {
        "UserPromptSubmit" => accumulator.stats.prompt_count += 1,
        "PermissionRequest" => accumulator.stats.permission_request_count += 1,
        "PreCompact" | "PostCompact" => accumulator.stats.compact_count += 1,
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
    [
        string_field(value, &["session", "session_id", "sessionId"]).unwrap_or(""),
        string_field(value, &["turn", "turn_id", "turnId"]).unwrap_or(""),
        string_field(value, &["tool", "tool_name", "toolName"]).unwrap_or(""),
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

    use super::{append_hook_event_to_root, scan_hook_event_file, HookDayAccumulator, EVENTS_DIR};

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
                r#"{"timestamp":"2026-07-02 09:00:00.000","event":"UserPromptSubmit","session":"s1","turn":"t1"}"#,
                "\n",
                r#"{"timestamp":"2026-07-02 09:01:00.000","event":"PostToolUse","session":"s1","turn":"t1","tool":"shell"}"#,
                "\n",
                r#"{"timestamp":"2026-07-02 09:02:00.000","event":"PermissionRequest","session":"s1","turn":"t2"}"#,
                "\n",
                r#"{"timestamp":"2026-07-02 09:03:00.000","event":"SubagentStart","session":"s2","turn":"t3"}"#,
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
