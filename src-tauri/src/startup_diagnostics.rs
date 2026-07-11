use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;

const DIAGNOSTIC_OUTPUT_ENV: &str = "CODEXTRAY_DIAGNOSTIC_OUTPUT";
const DIAGNOSTIC_RUN_ID_ENV: &str = "CODEXTRAY_DIAGNOSTIC_RUN_ID";
const SMOKE_SECONDS_ENV: &str = "CODEXTRAY_SMOKE_SECONDS";
const SMOKE_TEST_ARG: &str = "--startup-smoke-test";
const DEFAULT_SMOKE_SECONDS: u64 = 20;

static STARTUP_DIAGNOSTICS: OnceLock<StartupDiagnostics> = OnceLock::new();
static DIAGNOSTIC_LOG_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDiagnosticVariant {
    Disabled,
    Control,
    NoStartupRefresh,
    NoStartupUiUpdate,
}

impl StartupDiagnosticVariant {
    pub fn name(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::Control => "Control",
            Self::NoStartupRefresh => "NoStartupRefresh",
            Self::NoStartupUiUpdate => "NoStartupUiUpdate",
        }
    }

    pub fn skips_startup_refresh(self) -> bool {
        self == Self::NoStartupRefresh
    }

    pub fn skips_startup_ui_update(self) -> bool {
        self == Self::NoStartupUiUpdate
    }
}

struct StartupDiagnostics {
    variant: StartupDiagnosticVariant,
    run_id: String,
    output_path: Option<PathBuf>,
    started_at: Instant,
    smoke_seconds: Option<u64>,
}

pub fn initialize() {
    let diagnostics = STARTUP_DIAGNOSTICS.get_or_init(StartupDiagnostics::from_environment);
    if diagnostics.variant == StartupDiagnosticVariant::Disabled {
        return;
    }

    install_panic_hook();
    record(
        "process.start",
        &format!(
            "pid={} smokeSeconds={}",
            std::process::id(),
            diagnostics
                .smoke_seconds
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string())
        ),
    );
}

pub fn variant() -> StartupDiagnosticVariant {
    STARTUP_DIAGNOSTICS
        .get_or_init(StartupDiagnostics::from_environment)
        .variant
}

pub fn record(stage: &str, message: &str) {
    let Ok(_guard) = DIAGNOSTIC_LOG_LOCK.lock() else {
        return;
    };
    let diagnostics = STARTUP_DIAGNOSTICS.get_or_init(StartupDiagnostics::from_environment);
    let Some(path) = &diagnostics.output_path else {
        return;
    };
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
        "{}\t{}\t{}\t{}\t{}\t{}",
        Utc::now().to_rfc3339(),
        diagnostics.started_at.elapsed().as_millis(),
        diagnostics.run_id,
        diagnostics.variant.name(),
        stage,
        sanitized
    );
}

pub fn schedule_smoke_test_exit(app: &tauri::AppHandle) {
    let Some(seconds) = STARTUP_DIAGNOSTICS
        .get_or_init(StartupDiagnostics::from_environment)
        .smoke_seconds
    else {
        return;
    };
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
        record("run.stable", &format!("survivedSeconds={}", seconds));
        app.exit(0);
    });
}

impl StartupDiagnostics {
    fn from_environment() -> Self {
        let variant = parse_variant(option_env!("CODEXTRAY_DIAGNOSTIC_VARIANT"));
        let smoke_test_requested = env::args().any(|arg| arg == SMOKE_TEST_ARG);
        let smoke_seconds = smoke_test_requested.then(smoke_seconds_from_environment);
        let output_path = if variant == StartupDiagnosticVariant::Disabled {
            None
        } else {
            Some(diagnostic_output_path())
        };

        Self {
            variant,
            run_id: diagnostic_run_id(),
            output_path,
            started_at: Instant::now(),
            smoke_seconds,
        }
    }
}

fn parse_variant(value: Option<&str>) -> StartupDiagnosticVariant {
    match value.unwrap_or_default().trim() {
        "Control" => StartupDiagnosticVariant::Control,
        "NoStartupRefresh" => StartupDiagnosticVariant::NoStartupRefresh,
        "NoStartupUiUpdate" => StartupDiagnosticVariant::NoStartupUiUpdate,
        _ => StartupDiagnosticVariant::Disabled,
    }
}

fn smoke_seconds_from_environment() -> u64 {
    env::var(SMOKE_SECONDS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (5..=300).contains(value))
        .unwrap_or(DEFAULT_SMOKE_SECONDS)
}

fn diagnostic_output_path() -> PathBuf {
    env::var(DIAGNOSTIC_OUTPUT_ENV)
        .ok()
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("."))
                .join("diagnostic-results")
                .join("runtime.tsv")
        })
}

fn diagnostic_run_id() -> String {
    env::var(DIAGNOSTIC_RUN_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or_default();
            format!("{}-{}", timestamp, std::process::id())
        })
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record("panic", &info.to_string());
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::{parse_variant, StartupDiagnosticVariant};

    #[test]
    fn parses_build_variant_so_each_package_has_one_diagnostic_behavior() {
        assert_eq!(
            parse_variant(Some("Control")),
            StartupDiagnosticVariant::Control
        );
        assert_eq!(
            parse_variant(Some("NoStartupRefresh")),
            StartupDiagnosticVariant::NoStartupRefresh
        );
        assert_eq!(
            parse_variant(Some("NoStartupUiUpdate")),
            StartupDiagnosticVariant::NoStartupUiUpdate
        );
        assert_eq!(parse_variant(None), StartupDiagnosticVariant::Disabled);
    }
}
