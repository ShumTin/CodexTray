use crate::models::{DashboardSnapshot, QuotaWindow};
use tauri::image::Image;

const ICON_SIZE: u32 = 32;
const ICON_PIXELS: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;
const DEFAULT_TOOLTIP: &str = "CodexTray";

#[derive(Debug, Clone, Copy)]
struct RgbaColor {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

#[derive(Debug, Clone, Copy)]
struct TrayQuotaStatus<'a> {
    window: &'a QuotaWindow,
    stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIconState {
    Default,
    Quota(TrayQuotaIcon),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayQuotaIcon {
    remaining_percent: u8,
}

pub fn icon_state_for_snapshot(snapshot: &DashboardSnapshot) -> TrayIconState {
    tray_quota_status(snapshot)
        .map(|status| {
            TrayIconState::Quota(TrayQuotaIcon {
                remaining_percent: status.window.remaining_percent,
            })
        })
        .unwrap_or(TrayIconState::Default)
}

pub fn icon_for_state(state: TrayIconState) -> Image<'static> {
    match state {
        TrayIconState::Default => default_icon(),
        TrayIconState::Quota(icon) => build_icon(Some(icon.remaining_percent)),
    }
}

pub fn default_icon() -> Image<'static> {
    build_icon(None)
}

pub fn tooltip_for_snapshot(snapshot: &DashboardSnapshot) -> String {
    let Some(status) = tray_quota_status(snapshot) else {
        return DEFAULT_TOOLTIP.to_string();
    };

    let stale_label = if status.stale {
        "（上次成功）"
    } else {
        ""
    };

    format!(
        "CodexTray - {} 剩余额度 {}%{}",
        status.window.label, status.window.remaining_percent, stale_label
    )
}

fn tray_quota_status(snapshot: &DashboardSnapshot) -> Option<TrayQuotaStatus<'_>> {
    let quota = snapshot.quota.as_ref()?;
    let window = quota
        .windows
        .iter()
        .min_by_key(|window| window.remaining_percent)?;

    Some(TrayQuotaStatus {
        window,
        stale: quota.stale,
    })
}

fn build_icon(remaining_percent: Option<u8>) -> Image<'static> {
    let mut canvas = vec![0; ICON_PIXELS];

    draw_disc(&mut canvas, 16.0, 13.2, 12.8, color(14, 20, 31, 255));
    draw_disc(&mut canvas, 16.0, 13.2, 10.4, color(34, 211, 238, 255));
    draw_disc(&mut canvas, 16.0, 13.2, 6.7, color(14, 20, 31, 255));
    draw_disc(&mut canvas, 12.3, 17.2, 2.2, color(229, 231, 235, 255));
    draw_disc(&mut canvas, 20.6, 9.0, 1.8, color(229, 231, 235, 255));

    draw_quota_track(&mut canvas, remaining_percent);

    Image::new_owned(canvas, ICON_SIZE, ICON_SIZE)
}

fn draw_quota_track(canvas: &mut [u8], remaining_percent: Option<u8>) {
    let track_color = color(71, 85, 105, 220);
    draw_rounded_rect(canvas, 6, 28, 26, 30, 1.5, track_color);

    let Some(remaining_percent) = remaining_percent else {
        draw_rounded_rect(canvas, 7, 28, 12, 30, 1.5, color(148, 163, 184, 235));
        return;
    };

    let fill_width = ((remaining_percent.clamp(0, 100) as f32 / 100.0) * 18.0).round() as i32;
    if fill_width <= 0 {
        return;
    }

    draw_rounded_rect(
        canvas,
        7,
        28,
        7 + fill_width - 1,
        30,
        1.5,
        quota_color(remaining_percent),
    );
}

fn quota_color(remaining_percent: u8) -> RgbaColor {
    if remaining_percent <= 20 {
        return color(248, 113, 113, 255);
    }

    if remaining_percent <= 50 {
        return color(251, 191, 36, 255);
    }

    color(34, 197, 94, 255)
}

fn draw_disc(canvas: &mut [u8], center_x: f32, center_y: f32, radius: f32, color: RgbaColor) {
    let radius_squared = radius * radius;

    for y in 0..ICON_SIZE as i32 {
        for x in 0..ICON_SIZE as i32 {
            let dx = x as f32 + 0.5 - center_x;
            let dy = y as f32 + 0.5 - center_y;

            if dx * dx + dy * dy <= radius_squared {
                set_pixel(canvas, x, y, color);
            }
        }
    }
}

fn draw_rounded_rect(
    canvas: &mut [u8],
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    radius: f32,
    color: RgbaColor,
) {
    let width = right - left + 1;
    let height = bottom - top + 1;
    let radius = radius.min(width.min(height) as f32 / 2.0);
    let left_radius = left as f32 + radius;
    let right_radius = right as f32 + 1.0 - radius;
    let top_radius = top as f32 + radius;
    let bottom_radius = bottom as f32 + 1.0 - radius;

    for y in top..=bottom {
        for x in left..=right {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let closest_x = px.clamp(left_radius, right_radius);
            let closest_y = py.clamp(top_radius, bottom_radius);
            let dx = px - closest_x;
            let dy = py - closest_y;

            if dx * dx + dy * dy <= radius * radius {
                set_pixel(canvas, x, y, color);
            }
        }
    }
}

fn set_pixel(canvas: &mut [u8], x: i32, y: i32, color: RgbaColor) {
    if x < 0 || y < 0 || x >= ICON_SIZE as i32 || y >= ICON_SIZE as i32 {
        return;
    }

    let index = ((y as u32 * ICON_SIZE + x as u32) * 4) as usize;
    canvas[index] = color.red;
    canvas[index + 1] = color.green;
    canvas[index + 2] = color.blue;
    canvas[index + 3] = color.alpha;
}

fn color(red: u8, green: u8, blue: u8, alpha: u8) -> RgbaColor {
    RgbaColor {
        red,
        green,
        blue,
        alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AccountSnapshot, DiagnosticItem, DiagnosticStatus, DiagnosticsSnapshot, QuotaSourceKind,
        RateLimitSnapshot,
    };

    #[test]
    fn tooltip_uses_tightest_quota_window_so_tray_warns_early() {
        let snapshot =
            snapshot_with_quota(vec![quota_window("7D", 88), quota_window("5H", 24)], false);

        assert_eq!(
            tooltip_for_snapshot(&snapshot),
            "CodexTray - 5H 剩余额度 24%"
        );
    }

    #[test]
    fn tooltip_marks_stale_quota_so_cached_data_is_not_mistaken_for_live() {
        let snapshot = snapshot_with_quota(vec![quota_window("7D", 42)], true);

        assert_eq!(
            tooltip_for_snapshot(&snapshot),
            "CodexTray - 7D 剩余额度 42%（上次成功）"
        );
    }

    #[test]
    fn icon_state_uses_tightest_quota_window_so_repeated_refreshes_can_skip_same_icon() {
        let snapshot =
            snapshot_with_quota(vec![quota_window("7D", 88), quota_window("5H", 24)], false);

        assert_eq!(
            icon_state_for_snapshot(&snapshot),
            TrayIconState::Quota(TrayQuotaIcon {
                remaining_percent: 24
            })
        );
    }

    #[test]
    fn icon_renders_low_quota_without_panicking_when_fill_is_one_pixel_wide() {
        let result = std::panic::catch_unwind(|| build_icon(Some(1)));

        assert!(result.is_ok());
    }

    fn snapshot_with_quota(windows: Vec<QuotaWindow>, stale: bool) -> DashboardSnapshot {
        DashboardSnapshot {
            account: AccountSnapshot {
                email: Some("user@example.com".to_string()),
                plan: Some("Plus".to_string()),
                status: "已连接".to_string(),
                updated_at: "2026-07-03T00:00:00Z".to_string(),
            },
            quota: Some(RateLimitSnapshot {
                source: QuotaSourceKind::CodexCli,
                windows,
                reset_credits: None,
                fetched_at: "2026-07-03T00:00:00Z".to_string(),
                stale,
            }),
            last_success_source: Some(QuotaSourceKind::CodexCli),
            source_label: "Codex CLI".to_string(),
            diagnostics: DiagnosticsSnapshot {
                cli_probe: DiagnosticItem::ok("CLI 探测", "可启动"),
                cli_app_server: DiagnosticItem::ok("CLI app-server", "账号与额度读取成功"),
                token_activity: DiagnosticItem {
                    label: "Token 活动".to_string(),
                    status: DiagnosticStatus::Skipped,
                    message: "测试不需要 Token 活动".to_string(),
                },
            },
            metrics: Vec::new(),
            heatmap_days: Vec::new(),
            token_activity_source: None,
        }
    }

    fn quota_window(label: &str, remaining_percent: u8) -> QuotaWindow {
        QuotaWindow {
            label: label.to_string(),
            remaining_percent,
            reset_at: None,
        }
    }
}
