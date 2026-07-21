export interface QuotaWindow {
  readonly label: string;
  readonly remainingPercent: number;
  readonly resetAt: string | null;
}

export interface RateLimitResetCredit {
  readonly id: string;
  readonly title: string;
  readonly expiresAt: string | null;
}

export interface RateLimitResetCredits {
  readonly availableCount: number;
  readonly credits: readonly RateLimitResetCredit[];
}

export interface DashboardMetric {
  readonly label: string;
  readonly value: string;
}

export interface HeatmapDay {
  readonly date: string;
  readonly dailyTokens: number;
  readonly weeklyTokens: number;
  readonly cumulativeTokens: number;
  readonly isFuture: boolean;
  readonly hookStats: HookDayStats | null;
}

export interface HookDayStats {
  readonly sessionCount: number;
  readonly promptCount: number;
  readonly turnCount: number;
  readonly toolCallCount: number;
  readonly permissionRequestCount: number;
  readonly compactCount: number;
  readonly subagentCount: number;
}

export type HeatmapMode = "daily" | "weekly" | "cumulative";

export type QuotaSourceKind = "codexCli" | "cached";

export type DiagnosticStatus = "ok" | "warning" | "error" | "skipped";

export interface AccountSnapshot {
  readonly email: string | null;
  readonly plan: string | null;
  readonly status: string;
  readonly updatedAt: string;
}

export interface RateLimitSnapshot {
  readonly source: QuotaSourceKind;
  readonly windows: readonly QuotaWindow[];
  readonly resetCredits: RateLimitResetCredits | null;
  readonly fetchedAt: string;
  readonly stale: boolean;
}

export interface DiagnosticItem {
  readonly label: string;
  readonly status: DiagnosticStatus;
  readonly message: string;
}

export interface DiagnosticsSnapshot {
  readonly cliProbe: DiagnosticItem;
  readonly cliAppServer: DiagnosticItem;
  readonly tokenActivity: DiagnosticItem;
}

export interface DashboardSnapshot {
  readonly account: AccountSnapshot;
  readonly quota: RateLimitSnapshot | null;
  readonly lastSuccessSource: QuotaSourceKind | null;
  readonly sourceLabel: string;
  readonly diagnostics: DiagnosticsSnapshot;
  readonly metrics: readonly DashboardMetric[];
  readonly heatmapDays: readonly HeatmapDay[];
  readonly tokenActivitySource: "profileUsageApi" | "localJsonl" | null;
}

export interface AppSettings {
  readonly globalShortcut: string;
  readonly codexCliPath: string | null;
  readonly quotaWidgetEnabled: boolean;
}

export interface StartupStatus {
  readonly enabled: boolean;
  readonly source: string;
  readonly message: string;
}

export interface HookStatus {
  readonly enabled: boolean;
  readonly source: string;
  readonly message: string;
}

export interface UpdateStatus {
  readonly status: DiagnosticStatus;
  readonly message: string;
  readonly checkedAt: string;
  readonly availableVersion: string | null;
}

export interface RuntimeInfo {
  readonly appVersion: string;
  readonly cliVersion: string | null;
  readonly cliPath: string | null;
  readonly runSource: string;
  readonly installPath: string;
}

export interface SettingsSnapshot {
  readonly settings: AppSettings;
  readonly startup: StartupStatus;
  readonly hook: HookStatus;
  readonly update: UpdateStatus;
  readonly runtime: RuntimeInfo;
}

export interface LogEntry {
  readonly timestamp: string;
  readonly level: string;
  readonly message: string;
}
