use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use serde_json::Value;

use crate::models::{
    HeatmapDay, HookDayStats, TokenActivityBucket, TokenActivitySnapshot, TokenActivitySource,
};

const HEATMAP_DAYS: usize = 33 * 7;

pub fn scan_local_jsonl_usage() -> Result<TokenActivitySnapshot, String> {
    let codex_home = resolve_codex_home()?;
    let mut daily_tokens = BTreeMap::<NaiveDate, u64>::new();
    let mut parse_errors = 0_u64;

    for root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        scan_jsonl_root(&root, &mut daily_tokens, &mut parse_errors)?;
    }

    let daily_buckets: Vec<TokenActivityBucket> = daily_tokens
        .iter()
        .map(|(date, tokens)| TokenActivityBucket {
            date: date.to_string(),
            tokens: *tokens,
        })
        .collect();
    let lifetime_tokens = daily_buckets.iter().map(|bucket| bucket.tokens).sum();
    let peak_daily_tokens = daily_buckets.iter().map(|bucket| bucket.tokens).max();
    let (current_streak_days, longest_streak_days) = calculate_streaks(&daily_buckets);

    if daily_buckets.is_empty() && parse_errors > 0 {
        return Err("本地 JSONL 存在解析错误，且没有可用 token_count 事件".to_string());
    }

    Ok(TokenActivitySnapshot {
        source: TokenActivitySource::LocalJsonl,
        lifetime_tokens: Some(lifetime_tokens),
        peak_daily_tokens,
        longest_running_turn_sec: None,
        current_streak_days: Some(current_streak_days),
        longest_streak_days: Some(longest_streak_days),
        daily_buckets,
    })
}

pub fn build_heatmap_days(
    activity: Option<&TokenActivitySnapshot>,
    hook_stats: &BTreeMap<String, HookDayStats>,
) -> Vec<HeatmapDay> {
    let today = local_today();
    build_heatmap_days_for_today(activity, hook_stats, today)
}

fn local_today() -> NaiveDate {
    Local::now().date_naive()
}

fn build_heatmap_days_for_today(
    activity: Option<&TokenActivitySnapshot>,
    hook_stats: &BTreeMap<String, HookDayStats>,
    today: NaiveDate,
) -> Vec<HeatmapDay> {
    let end = heatmap_end_date(today);
    let start = end - Duration::days((HEATMAP_DAYS - 1) as i64);
    let mut by_date = BTreeMap::<NaiveDate, u64>::new();

    if let Some(activity) = activity {
        for bucket in &activity.daily_buckets {
            if let Ok(date) = NaiveDate::parse_from_str(&bucket.date, "%Y-%m-%d") {
                by_date.insert(date, bucket.tokens);
            }
        }
    }

    let mut cumulative_tokens = 0_u64;
    let mut result = Vec::with_capacity(HEATMAP_DAYS);

    for index in 0..HEATMAP_DAYS {
        let date = start + Duration::days(index as i64);
        let is_future = date > today;
        let daily_tokens = if is_future {
            0
        } else {
            *by_date.get(&date).unwrap_or(&0)
        };
        cumulative_tokens = cumulative_tokens.saturating_add(daily_tokens);
        let weekly_tokens = if is_future {
            0
        } else {
            week_tokens(&by_date, date, today)
        };

        result.push(HeatmapDay {
            date: date.to_string(),
            daily_tokens,
            weekly_tokens,
            cumulative_tokens,
            is_future,
            hook_stats: hook_stats.get(&date.to_string()).cloned(),
        });
    }

    result
}

fn heatmap_end_date(today: NaiveDate) -> NaiveDate {
    let days_until_saturday = 6_i64 - today.weekday().num_days_from_sunday() as i64;
    today + Duration::days(days_until_saturday)
}

fn resolve_codex_home() -> Result<PathBuf, String> {
    if let Ok(value) = env::var("CODEX_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    let profile =
        env::var("USERPROFILE").map_err(|_| "无法读取 USERPROFILE 环境变量".to_string())?;
    Ok(PathBuf::from(profile).join(".codex"))
}

fn scan_jsonl_root(
    root: &Path,
    daily_tokens: &mut BTreeMap<NaiveDate, u64>,
    parse_errors: &mut u64,
) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(root).map_err(|error| format!("无法读取 JSONL 目录：{}", error))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("无法读取 JSONL 目录项：{}", error))?;
        let path = entry.path();

        if path.is_dir() {
            scan_jsonl_root(&path, daily_tokens, parse_errors)?;
            continue;
        }

        if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            scan_jsonl_file(&path, daily_tokens, parse_errors)?;
        }
    }

    Ok(())
}

fn scan_jsonl_file(
    path: &Path,
    daily_tokens: &mut BTreeMap<NaiveDate, u64>,
    parse_errors: &mut u64,
) -> Result<(), String> {
    let file = File::open(path).map_err(|error| format!("无法打开 JSONL 文件：{}", error))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let Ok(line) = line else {
            *parse_errors += 1;
            continue;
        };

        if line.trim().is_empty() {
            continue;
        }

        match parse_token_count_event(&line) {
            Some((date, tokens)) => {
                let entry = daily_tokens.entry(date).or_default();
                *entry = entry.saturating_add(tokens);
            }
            None => *parse_errors += 1,
        }
    }

    Ok(())
}

fn parse_token_count_event(line: &str) -> Option<(NaiveDate, u64)> {
    let value: Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;

    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }

    let tokens = payload
        .get("info")?
        .get("last_token_usage")?
        .get("total_tokens")?
        .as_u64()?;
    let date = value
        .get("timestamp")
        .or_else(|| value.get("time"))
        .and_then(Value::as_str)
        .and_then(parse_event_date)?;

    Some((date, tokens))
}

fn parse_event_date(value: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Local).date_naive())
        .ok()
        .or_else(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
}

fn week_tokens(by_date: &BTreeMap<NaiveDate, u64>, date: NaiveDate, today: NaiveDate) -> u64 {
    let weekday = date.weekday().num_days_from_sunday() as i64;
    let start = date - Duration::days(weekday);

    (0..7)
        .map(|offset| start + Duration::days(offset))
        .filter(|date| *date <= today)
        .map(|date| *by_date.get(&date).unwrap_or(&0))
        .sum()
}

fn calculate_streaks(daily_buckets: &[TokenActivityBucket]) -> (u64, u64) {
    let active_dates: Vec<NaiveDate> = daily_buckets
        .iter()
        .filter(|bucket| bucket.tokens > 0)
        .filter_map(|bucket| NaiveDate::parse_from_str(&bucket.date, "%Y-%m-%d").ok())
        .collect();

    if active_dates.is_empty() {
        return (0, 0);
    }

    let mut longest = 1_u64;
    let mut current = 1_u64;

    for pair in active_dates.windows(2) {
        if pair[1] == pair[0] + Duration::days(1) {
            current += 1;
        } else {
            longest = longest.max(current);
            current = 1;
        }
    }

    longest = longest.max(current);
    let today = local_today();
    let current_streak = active_dates
        .last()
        .filter(|date| **date == today)
        .map(|_| current)
        .unwrap_or(0);

    (current_streak, longest)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::{DateTime, Local, NaiveDate};

    use super::{build_heatmap_days_for_today, parse_token_count_event, scan_jsonl_root};
    use crate::models::{TokenActivitySnapshot, TokenActivitySource};

    #[test]
    fn parses_last_token_usage_so_daily_increment_is_not_double_counted() {
        let line = r#"{"timestamp":"2026-07-02T10:20:30Z","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":42},"total_token_usage":{"total_tokens":999}}}}"#;

        let parsed = parse_token_count_event(line).expect("token event should parse");

        assert_eq!(parsed, (NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(), 42));
    }

    #[test]
    fn parses_rfc3339_event_date_using_local_calendar_day() {
        let line = r#"{"timestamp":"2026-07-02T16:30:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":42}}}}"#;
        let expected_date = DateTime::parse_from_rfc3339("2026-07-02T16:30:00Z")
            .expect("test timestamp should parse")
            .with_timezone(&Local)
            .date_naive();

        let parsed = parse_token_count_event(line).expect("token event should parse");

        assert_eq!(parsed, (expected_date, 42));
    }

    #[test]
    fn builds_daily_weekly_and_cumulative_values_for_heatmap_modes() {
        let activity = TokenActivitySnapshot {
            source: TokenActivitySource::LocalJsonl,
            lifetime_tokens: Some(30),
            peak_daily_tokens: Some(20),
            longest_running_turn_sec: None,
            current_streak_days: Some(1),
            longest_streak_days: Some(2),
            daily_buckets: vec![
                crate::models::TokenActivityBucket {
                    date: "2026-07-01".to_string(),
                    tokens: 10,
                },
                crate::models::TokenActivityBucket {
                    date: "2026-07-02".to_string(),
                    tokens: 20,
                },
            ],
        };

        let days = build_heatmap_days_for_today(
            Some(&activity),
            &BTreeMap::new(),
            NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(),
        );
        let target = days
            .iter()
            .find(|day| day.date == "2026-07-02")
            .expect("target day should exist in the deterministic heatmap window");

        assert_eq!(target.daily_tokens, 20);
        assert!(target.weekly_tokens >= 20);
        assert!(target.cumulative_tokens >= 20);
    }

    #[test]
    fn aligns_heatmap_columns_to_sunday_through_saturday_weeks() {
        let activity = TokenActivitySnapshot {
            source: TokenActivitySource::LocalJsonl,
            lifetime_tokens: Some(30),
            peak_daily_tokens: Some(20),
            longest_running_turn_sec: None,
            current_streak_days: Some(1),
            longest_streak_days: Some(2),
            daily_buckets: vec![
                crate::models::TokenActivityBucket {
                    date: "2026-06-28".to_string(),
                    tokens: 10,
                },
                crate::models::TokenActivityBucket {
                    date: "2026-07-03".to_string(),
                    tokens: 20,
                },
                crate::models::TokenActivityBucket {
                    date: "2026-07-04".to_string(),
                    tokens: 99,
                },
            ],
        };

        let days = build_heatmap_days_for_today(
            Some(&activity),
            &BTreeMap::new(),
            NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
        );
        let first = days.first().expect("heatmap should include a first day");
        let visible_first = days
            .get(7)
            .expect("visible 32-week heatmap starts after the buffered first week");
        let friday = days
            .iter()
            .find(|day| day.date == "2026-07-03")
            .expect("today should exist in the aligned heatmap");
        let saturday = days.last().expect("heatmap should end at week Saturday");

        assert_eq!(first.date, "2025-11-16");
        assert_eq!(visible_first.date, "2025-11-23");
        assert_eq!(saturday.date, "2026-07-04");
        assert!(saturday.is_future);
        assert_eq!(saturday.daily_tokens, 0);
        assert_eq!(saturday.weekly_tokens, 0);
        assert_eq!(friday.weekly_tokens, 30);
    }

    #[test]
    fn scans_jsonl_tree_so_local_usage_fallback_survives_profile_api_failure() {
        let root = std::env::temp_dir().join(format!(
            "codextray-jsonl-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));
        let nested = root.join("sessions\\2026\\07");
        fs::create_dir_all(&nested).expect("test sessions dir should be created");
        fs::write(
            nested.join("rollout.jsonl"),
            r#"{"timestamp":"2026-07-02T10:20:30Z","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":42}}}}"#,
        )
        .expect("test jsonl should be created");
        let mut daily_tokens = BTreeMap::new();
        let mut parse_errors = 0_u64;

        scan_jsonl_root(&root, &mut daily_tokens, &mut parse_errors)
            .expect("jsonl root should scan");

        assert_eq!(
            daily_tokens.get(&NaiveDate::from_ymd_opt(2026, 7, 2).unwrap()),
            Some(&42)
        );
        assert_eq!(parse_errors, 0);

        fs::remove_dir_all(root).expect("test temp dir should be removed");
    }
}
