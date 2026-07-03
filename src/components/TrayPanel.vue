<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { computed, onMounted, onUnmounted, ref } from "vue";
import type {
  DashboardSnapshot,
  HeatmapDay,
  HeatmapMode,
  HookDayStats,
  LogEntry,
  SettingsSnapshot,
  UpdateStatus,
} from "../types/dashboard";

interface HeatmapDetailPayload {
  readonly title: string;
  readonly tokenValue: string;
  readonly intensityLevel: number;
  readonly stats: readonly DetailStat[];
}

interface DetailStat {
  readonly label: string;
  readonly value: string;
  readonly tone: string;
}

interface AnnouncementItem {
  readonly title: string;
  readonly detail: string;
}

declare global {
  interface Window {
    __codexTrayDashboardSnapshot?: DashboardSnapshot;
    __codexTrayDashboardRefreshing?: boolean;
    __codexTrayHeatmapDetail?: HeatmapDetailPayload;
  }
}

const isDetailView = new URLSearchParams(window.location.search).get("view") === "detail";
const isSettingsView = new URLSearchParams(window.location.search).get("view") === "settings";
const dashboardRefreshStartedDomEvent = "codextray-dashboard-refresh-started";
const dashboardRefreshStartedEvent = "codextray://dashboard-refresh-started";
const dashboardDomEvent = "codextray-dashboard-refreshed";
const dashboardRefreshedEvent = "codextray://dashboard-refreshed";
const snapshot = ref<DashboardSnapshot | null>(null);
const settingsSnapshot = ref<SettingsSnapshot | null>(null);
const logs = ref<readonly LogEntry[]>([]);
const activeSettingsTab = ref<"settings" | "announcements" | "logs">("settings");
const heatmapMode = ref<HeatmapMode>("daily");
const hoveredHeatmapDate = ref<string | null>(null);
const detailSnapshot = ref<HeatmapDetailPayload | null>(null);
const shortcutDraft = ref("Ctrl+Shift+C");
const copiedPath = ref<string | null>(null);
const isCheckingUpdates = ref(false);
const isInstallingUpdate = ref(false);
const isDashboardRefreshing = ref(false);
const isChoosingCliPath = ref(false);
const hookToggleTarget = ref<"enable" | "disable" | null>(null);
let unlistenDashboardRefreshStarted: UnlistenFn | undefined;
let unlistenDashboardRefresh: UnlistenFn | undefined;
let hasDashboardRefreshStartedDomListener = false;
let hasDashboardDomListener = false;
let isRefreshingDashboard = false;
let isLoadingSettings = false;
const heatmapModes: readonly { label: string; value: HeatmapMode }[] = [
  { label: "每日", value: "daily" },
  { label: "每周", value: "weekly" },
  { label: "累计", value: "cumulative" },
];
const settingsTabs: readonly { label: string; value: typeof activeSettingsTab.value }[] = [
  { label: "设置", value: "settings" },
  { label: "公告", value: "announcements" },
  { label: "日志", value: "logs" },
];
const announcementItems: readonly AnnouncementItem[] = [
  {
    title: "添加额度重置次数展示",
    detail: "解析 Codex CLI 返回的 rateLimitResetCredits，在额度卡片中展示重置次数和悬停信息卡片。",
  },
];

const quotaWindows = computed(() => snapshot.value?.quota?.windows ?? []);
const quotaResetCredits = computed(() => snapshot.value?.quota?.resetCredits ?? null);
const metrics = computed(() => snapshot.value?.metrics ?? []);
const heatmapDays = computed(() => snapshot.value?.heatmapDays ?? []);
const visibleHeatmapDays = computed(() => heatmapDays.value.slice(-224));
const heatmapMaxValue = computed(() =>
  Math.max(...visibleHeatmapDays.value.map((day) => heatmapValue(day)), 0),
);
const weeklyTokenByStartDate = computed(() => {
  const result = new Map<string, number>();

  for (const day of visibleHeatmapDays.value) {
    const weekStart = formatDateKey(weekStartDate(day.date));
    result.set(weekStart, Math.max(result.get(weekStart) ?? 0, day.weeklyTokens));
  }

  return result;
});
const weeklyMaxValue = computed(() => Math.max(...weeklyTokenByStartDate.value.values(), 0));
const cumulativeTokenByStartDate = computed(() => {
  const result = new Map<string, number>();

  for (const day of visibleHeatmapDays.value) {
    if (day.isFuture) {
      continue;
    }

    const weekStart = formatDateKey(weekStartDate(day.date));
    result.set(weekStart, Math.max(result.get(weekStart) ?? 0, day.cumulativeTokens));
  }

  return result;
});
const cumulativeMaxValue = computed(() =>
  Math.max(...cumulativeTokenByStartDate.value.values(), 0),
);
const accountEmail = computed(() =>
  isDashboardRefreshing.value && !snapshot.value?.account.email
    ? "正在读取 Codex 账号"
    : (snapshot.value?.account.email ?? "未读取到 Codex 账号"),
);
const updatedAt = computed(() =>
  isDashboardRefreshing.value ? "正在刷新" : formatDateTime(snapshot.value?.account.updatedAt),
);
const planLabel = computed(() => snapshot.value?.account.plan ?? "CODEX");
const statusLabel = computed(() =>
  isDashboardRefreshing.value ? "刷新中" : (snapshot.value?.account.status ?? "未连接"),
);
const quotaTitle = computed(() => (snapshot.value?.quota?.stale ? "Codex（上次成功）" : "Codex"));
const startupStatus = computed(() => settingsSnapshot.value?.startup);
const hookStatus = computed(() => settingsSnapshot.value?.hook);
const hookMessage = computed(() => {
  if (hookToggleTarget.value === "enable") {
    return "正在写入 Hook 采集";
  }

  if (hookToggleTarget.value === "disable") {
    return "正在移除 Hook 采集";
  }

  return hookStatus.value?.message ?? "读取中";
});
const hookButtonLabel = computed(() => {
  if (hookToggleTarget.value === "enable") {
    return "启动中";
  }

  if (hookToggleTarget.value === "disable") {
    return "关闭中";
  }

  return hookStatus.value?.enabled ? "关闭" : "开启";
});
const updateStatus = computed(() => settingsSnapshot.value?.update);
const updateAvailableVersion = computed(() => updateStatus.value?.availableVersion ?? null);
const updateMessage = computed(() =>
  isInstallingUpdate.value
    ? "正在下载安装更新"
    : isCheckingUpdates.value
      ? "正在检查更新"
      : (updateStatus.value?.message ?? "等待检查"),
);
const updateButtonLabel = computed(() => {
  if (isInstallingUpdate.value) {
    return "安装中";
  }

  if (isCheckingUpdates.value) {
    return "检查中";
  }

  return updateAvailableVersion.value ? "安装" : "检查";
});
const runtimeInfo = computed(() => settingsSnapshot.value?.runtime);

onMounted(() => {
  if (isDetailView) {
    bindDetailWindow();
    return;
  }

  if (isSettingsView) {
    void initializeSettingsPage();
    return;
  }

  void initializeDashboard();
});

onUnmounted(() => {
  stopDashboardRefreshStartedListener();
  stopDashboardRefreshListener();
  stopDashboardRefreshStartedDomListener();
  stopDashboardDomListener();
});

async function initializeDashboard(): Promise<void> {
  startDashboardRefreshStartedDomListener();
  startDashboardDomListener();
  await startDashboardRefreshStartedListener();
  await startDashboardRefreshListener();
  await loadSnapshot();
}

async function initializeSettingsPage(): Promise<void> {
  await loadSettingsSnapshot();
  await loadRecentLogs();
}

async function loadSnapshot(): Promise<void> {
  applyDashboardSnapshot(await invoke<DashboardSnapshot>("get_dashboard_snapshot"));
  await refreshDashboard();
}

async function refreshDashboard(): Promise<void> {
  if (isRefreshingDashboard) {
    return;
  }

  if (window.__codexTrayDashboardRefreshing) {
    markDashboardRefreshing();
    return;
  }

  isRefreshingDashboard = true;
  markDashboardRefreshing();

  try {
    completeDashboardRefresh(await invoke<DashboardSnapshot>("refresh_dashboard"));
    await loadRecentLogs();
  } catch (error) {
    isDashboardRefreshing.value = false;
    window.__codexTrayDashboardRefreshing = false;
    throw error;
  } finally {
    isRefreshingDashboard = false;
  }
}

async function loadSettingsSnapshot(): Promise<void> {
  if (isLoadingSettings) {
    return;
  }

  isLoadingSettings = true;

  try {
    settingsSnapshot.value = await invoke<SettingsSnapshot>("get_settings_snapshot");
    shortcutDraft.value = settingsSnapshot.value.settings.globalShortcut;
  } finally {
    isLoadingSettings = false;
  }
}

async function loadRecentLogs(): Promise<void> {
  logs.value = await invoke<LogEntry[]>("get_recent_logs");
}

function applyDashboardSnapshot(nextSnapshot: DashboardSnapshot): void {
  snapshot.value = nextSnapshot;
}

function completeDashboardRefresh(nextSnapshot: DashboardSnapshot): void {
  applyDashboardSnapshot(nextSnapshot);
  isDashboardRefreshing.value = false;
  window.__codexTrayDashboardRefreshing = false;
}

function markDashboardRefreshing(): void {
  isDashboardRefreshing.value = true;
  window.__codexTrayDashboardRefreshing = true;
}

function startDashboardRefreshStartedDomListener(): void {
  if (hasDashboardRefreshStartedDomListener) {
    return;
  }

  hasDashboardRefreshStartedDomListener = true;
  window.addEventListener(dashboardRefreshStartedDomEvent, handleDashboardRefreshStarted);

  if (window.__codexTrayDashboardRefreshing) {
    markDashboardRefreshing();
  }
}

function stopDashboardRefreshStartedDomListener(): void {
  if (!hasDashboardRefreshStartedDomListener) {
    return;
  }

  window.removeEventListener(dashboardRefreshStartedDomEvent, handleDashboardRefreshStarted);
  hasDashboardRefreshStartedDomListener = false;
}

function handleDashboardRefreshStarted(): void {
  markDashboardRefreshing();
}

function startDashboardDomListener(): void {
  if (hasDashboardDomListener) {
    return;
  }

  hasDashboardDomListener = true;
  window.addEventListener(dashboardDomEvent, handleDashboardDomRefresh);

  if (window.__codexTrayDashboardSnapshot) {
    applyDashboardSnapshot(window.__codexTrayDashboardSnapshot);
  }
}

function stopDashboardDomListener(): void {
  if (!hasDashboardDomListener) {
    return;
  }

  window.removeEventListener(dashboardDomEvent, handleDashboardDomRefresh);
  hasDashboardDomListener = false;
}

function handleDashboardDomRefresh(event: Event): void {
  completeDashboardRefresh((event as CustomEvent<DashboardSnapshot>).detail);
  void loadRecentLogs();
}

async function startDashboardRefreshStartedListener(): Promise<void> {
  if (unlistenDashboardRefreshStarted !== undefined) {
    return;
  }

  unlistenDashboardRefreshStarted = await listen(dashboardRefreshStartedEvent, () => {
    markDashboardRefreshing();
  });
}

function stopDashboardRefreshStartedListener(): void {
  if (unlistenDashboardRefreshStarted === undefined) {
    return;
  }

  unlistenDashboardRefreshStarted();
  unlistenDashboardRefreshStarted = undefined;
}

async function startDashboardRefreshListener(): Promise<void> {
  if (unlistenDashboardRefresh !== undefined) {
    return;
  }

  unlistenDashboardRefresh = await listen<DashboardSnapshot>(dashboardRefreshedEvent, (event) => {
    completeDashboardRefresh(event.payload);
    void loadRecentLogs();
  });
}

function stopDashboardRefreshListener(): void {
  if (unlistenDashboardRefresh === undefined) {
    return;
  }

  unlistenDashboardRefresh();
  unlistenDashboardRefresh = undefined;
}

function showHeatmapDetail(day: HeatmapDay): void {
  hoveredHeatmapDate.value = day.date;
  void invoke("show_heatmap_detail", { detail: heatmapDetailPayload(day) });
}

function hideHeatmapDetail(): void {
  hoveredHeatmapDate.value = null;
  void invoke("hide_heatmap_detail");
}

function setHeatmapMode(mode: HeatmapMode): void {
  heatmapMode.value = mode;
}

function setSettingsTab(tab: typeof activeSettingsTab.value): void {
  activeSettingsTab.value = tab;

  if (tab === "logs") {
    void loadRecentLogs();
  }

  if (tab === "settings") {
    void loadSettingsSnapshot();
  }
}

async function saveShortcut(): Promise<void> {
  const settings = await invoke<SettingsSnapshot["settings"]>("set_global_shortcut", {
    shortcut: shortcutDraft.value,
  });

  if (settingsSnapshot.value) {
    settingsSnapshot.value = { ...settingsSnapshot.value, settings };
  }
}

async function chooseCodexCliPath(): Promise<void> {
  if (isChoosingCliPath.value) {
    return;
  }

  isChoosingCliPath.value = true;

  try {
    const path = await invoke<string | null>("choose_codex_cli_path");
    if (!path) {
      return;
    }

    const settings = await invoke<SettingsSnapshot["settings"]>("set_codex_cli_path", { path });
    if (settingsSnapshot.value) {
      settingsSnapshot.value = { ...settingsSnapshot.value, settings };
    }

    await loadSettingsSnapshot();
  } finally {
    isChoosingCliPath.value = false;
  }
}

async function clearCodexCliPath(): Promise<void> {
  const settings = await invoke<SettingsSnapshot["settings"]>("clear_codex_cli_path");
  if (settingsSnapshot.value) {
    settingsSnapshot.value = { ...settingsSnapshot.value, settings };
  }

  await loadSettingsSnapshot();
}

async function toggleStartup(): Promise<void> {
  const enabled = !(startupStatus.value?.enabled ?? false);
  const startup = await invoke<SettingsSnapshot["startup"]>("set_startup_enabled", { enabled });

  if (settingsSnapshot.value) {
    settingsSnapshot.value = { ...settingsSnapshot.value, startup };
  }
}

async function toggleHook(): Promise<void> {
  if (hookToggleTarget.value !== null) {
    return;
  }

  const enabled = !(hookStatus.value?.enabled ?? false);
  hookToggleTarget.value = enabled ? "enable" : "disable";

  try {
    const hook = await invoke<SettingsSnapshot["hook"]>("set_hook_enabled", { enabled });

    if (settingsSnapshot.value) {
      settingsSnapshot.value = { ...settingsSnapshot.value, hook };
    }

    await loadSettingsSnapshot();
    await loadRecentLogs();
  } finally {
    hookToggleTarget.value = null;
  }
}

async function checkUpdates(): Promise<void> {
  if (isCheckingUpdates.value || isInstallingUpdate.value) {
    return;
  }

  if (updateAvailableVersion.value) {
    await confirmAndInstallUpdate(updateAvailableVersion.value);
    return;
  }

  isCheckingUpdates.value = true;

  try {
    const update = await invoke<UpdateStatus>("check_for_updates");

    if (settingsSnapshot.value) {
      settingsSnapshot.value = { ...settingsSnapshot.value, update };
    }

    await loadRecentLogs();
  } finally {
    isCheckingUpdates.value = false;
  }
}

async function confirmAndInstallUpdate(version: string): Promise<void> {
  const confirmed = window.confirm(`发现新版本 ${version}，是否立即下载安装？`);

  if (!confirmed) {
    return;
  }

  isInstallingUpdate.value = true;

  try {
    const update = await invoke<UpdateStatus>("install_update");

    if (settingsSnapshot.value) {
      settingsSnapshot.value = { ...settingsSnapshot.value, update };
    }

    await loadRecentLogs();
  } finally {
    isInstallingUpdate.value = false;
  }
}

async function copyPath(value: string | null | undefined): Promise<void> {
  if (!value) {
    return;
  }

  await navigator.clipboard.writeText(value);
  copiedPath.value = value;
  window.setTimeout(() => {
    if (copiedPath.value === value) {
      copiedPath.value = null;
    }
  }, 1200);
}

function pathWasCopied(value: string | null | undefined): boolean {
  return Boolean(value) && copiedPath.value === value;
}

function bindDetailWindow(): void {
  detailSnapshot.value = window.__codexTrayHeatmapDetail ?? null;
  window.addEventListener("codextray-heatmap-detail", (event) => {
    detailSnapshot.value = (event as CustomEvent<HeatmapDetailPayload>).detail;
  });
}

function heatmapValue(day: HeatmapDay): number {
  if (heatmapMode.value === "weekly") {
    return weeklyTokenValue(day);
  }

  if (heatmapMode.value === "cumulative") {
    return cumulativeTokenValue(day);
  }

  if (day.isFuture) {
    return 0;
  }

  return day.dailyTokens;
}

function heatmapLevel(day: HeatmapDay): number {
  if (isColumnHeatmapMode()) {
    return 0;
  }

  const value = heatmapValue(day);
  const maxValue = heatmapMaxValue.value;

  if (value <= 0 || maxValue <= 0) {
    return 0;
  }

  return Math.min(4, Math.max(1, Math.ceil((value / maxValue) * 4)));
}

function heatmapCellClasses(day: HeatmapDay): (string | Record<string, boolean>)[] {
  return [
    `level-${heatmapLevel(day)}`,
    {
      "heatmap-cell-future": day.isFuture && heatmapMode.value === "daily",
      "heatmap-cell-column-active": isColumnCellActive(day),
      "heatmap-cell-selected": isHeatmapCellSelectedWithTokens(day),
      "heatmap-cell-selected-empty": isHeatmapCellSelectedWithoutTokens(day),
    },
  ];
}

function isHeatmapCellSelected(day: HeatmapDay): boolean {
  const hoveredDate = hoveredHeatmapDate.value;

  if (!hoveredDate) {
    return false;
  }

  if (isColumnHeatmapMode()) {
    return formatDateKey(weekStartDate(day.date)) === formatDateKey(weekStartDate(hoveredDate));
  }

  return day.date === hoveredDate;
}

function isHeatmapCellSelectedWithTokens(day: HeatmapDay): boolean {
  return isHeatmapCellSelected(day) && heatmapValue(day) > 0;
}

function isHeatmapCellSelectedWithoutTokens(day: HeatmapDay): boolean {
  return isHeatmapCellSelected(day) && heatmapValue(day) <= 0;
}

function isColumnCellActive(day: HeatmapDay): boolean {
  const columnValue = columnTokenValue(day);
  const maxValue = columnMaxValue();

  if (!isColumnHeatmapMode() || columnValue <= 0 || maxValue <= 0) {
    return false;
  }

  const activeCells = Math.min(7, Math.ceil((columnValue / maxValue) * 7));
  const rowFromBottom = 6 - weekdayIndex(day.date);

  return rowFromBottom < activeCells;
}

function columnTokenValue(day: HeatmapDay): number {
  if (heatmapMode.value === "cumulative") {
    return cumulativeTokenValue(day);
  }

  return weeklyTokenValue(day);
}

function columnMaxValue(): number {
  if (heatmapMode.value === "cumulative") {
    return cumulativeMaxValue.value;
  }

  return weeklyMaxValue.value;
}

function isColumnHeatmapMode(): boolean {
  return heatmapMode.value === "weekly" || heatmapMode.value === "cumulative";
}

function weeklyTokenValue(day: HeatmapDay): number {
  return weeklyTokenByStartDate.value.get(formatDateKey(weekStartDate(day.date))) ?? 0;
}

function cumulativeTokenValue(day: HeatmapDay): number {
  return cumulativeTokenByStartDate.value.get(formatDateKey(weekStartDate(day.date))) ?? 0;
}

function weekdayIndex(value: string): number {
  const [year, month, day] = value.split("-").map(Number);
  const date = new Date(year, month - 1, day);

  return date.getDay();
}

function heatmapDetailPayload(day: HeatmapDay): HeatmapDetailPayload {
  const hookStats = hookStatsValue(day);

  return {
    title: heatmapDetailTitle(day),
    tokenValue: formatTokenAmount(heatmapValue(day)),
    intensityLevel: heatmapIntensityLevel(day),
    stats: [
      { label: "用量强度", value: heatmapIntensityLabel(day), tone: "blue" },
      { label: "会话总数", value: formatCount(hookStats.sessionCount), tone: "green" },
      { label: "对话轮次", value: formatCount(hookStats.turnCount), tone: "teal" },
      { label: "子智能体", value: formatCount(hookStats.subagentCount), tone: "violet" },
      { label: "工具调用", value: formatCount(hookStats.toolCallCount), tone: "amber" },
      { label: "权限请求", value: formatCount(hookStats.permissionRequestCount), tone: "rose" },
      { label: "上下文压缩", value: formatCount(hookStats.compactCount), tone: "fuchsia" },
    ],
  };
}

function heatmapDetailTitle(day: HeatmapDay): string {
  if (isColumnHeatmapMode()) {
    return formatDateKey(weekStartDate(day.date));
  }

  return day.date;
}

function hookStatsValue(day: HeatmapDay): HookDayStats {
  if (!isColumnHeatmapMode()) {
    return day.hookStats ?? emptyHookStats();
  }

  const weekStart = formatDateKey(weekStartDate(day.date));

  return visibleHeatmapDays.value
    .filter((value) => formatDateKey(weekStartDate(value.date)) === weekStart)
    .reduce((total, value) => addHookStats(total, value.hookStats), emptyHookStats());
}

function addHookStats(total: HookDayStats, value: HookDayStats | null): HookDayStats {
  if (!value) {
    return total;
  }

  return {
    sessionCount: total.sessionCount + value.sessionCount,
    promptCount: total.promptCount + value.promptCount,
    turnCount: total.turnCount + value.turnCount,
    toolCallCount: total.toolCallCount + value.toolCallCount,
    permissionRequestCount: total.permissionRequestCount + value.permissionRequestCount,
    compactCount: total.compactCount + value.compactCount,
    subagentCount: total.subagentCount + value.subagentCount,
  };
}

function emptyHookStats(): HookDayStats {
  return {
    sessionCount: 0,
    promptCount: 0,
    turnCount: 0,
    toolCallCount: 0,
    permissionRequestCount: 0,
    compactCount: 0,
    subagentCount: 0,
  };
}

function heatmapIntensityLabel(day: HeatmapDay): string {
  return `${heatmapIntensityLevel(day)}/5`;
}

function heatmapIntensityLevel(day: HeatmapDay): number {
  const value = heatmapValue(day);

  if (value <= 0) {
    return 0;
  }

  if (isColumnHeatmapMode()) {
    return Math.min(5, Math.ceil((value / columnMaxValue()) * 5));
  }

  return Math.min(5, Math.ceil((value / heatmapMaxValue.value) * 5));
}

function weekStartDate(value: string): Date {
  const [year, month, day] = value.split("-").map(Number);
  const date = new Date(year, month - 1, day);
  date.setDate(date.getDate() - date.getDay());

  return date;
}

function formatDateKey(date: Date): string {
  return [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("-");
}

function formatTokenAmount(value: number): string {
  const units = ["", "K", "M", "B", "T", "P"];
  let unitIndex = 0;
  let scaledValue = value;

  while (scaledValue >= 1000 && unitIndex < units.length - 1) {
    scaledValue /= 1000;
    unitIndex += 1;
  }

  if (unitIndex === 0) {
    return value.toLocaleString();
  }

  return `${trimTrailingZero(scaledValue.toFixed(1))} ${units[unitIndex]}`;
}

function trimTrailingZero(value: string): string {
  return value.endsWith(".0") ? value.slice(0, -2) : value;
}

function formatCount(value: number): string {
  return value.toLocaleString();
}

function formatDateTime(value: string | undefined): string {
  if (!value) {
    return "等待刷新";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function formatResetAt(value: string | null): string {
  if (!value) {
    return "未知";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return `${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function formatResetCreditExpiresAt(value: string | null): string {
  if (!value) {
    return "未知";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}:${String(date.getSeconds()).padStart(2, "0")}`;
}

function formatQuotaLabel(label: string): string {
  const parts = label.trim().split(/\s+/);
  return parts[parts.length - 1] ?? label;
}

function formatLogTime(value: string): string {
  const date = new Date(value);

  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
</script>

<template>
  <aside v-if="isDetailView" class="detail-card visible" aria-label="热力图详情">
    <div class="detail-head">
      <time>{{ detailSnapshot?.title ?? "等待选择" }}</time>
      <strong>{{ detailSnapshot?.tokenValue ?? "-" }}</strong>
    </div>
    <dl v-if="detailSnapshot">
      <div v-for="stat in detailSnapshot.stats" :key="stat.label">
        <dt><i class="dot" :class="stat.tone" />{{ stat.label }}</dt>
        <dd v-if="stat.label === '用量强度'" class="intensity-bars" aria-label="用量强度">
          <span
            v-for="segment in 5"
            :key="segment"
            :class="{ active: segment <= detailSnapshot.intensityLevel }"
          />
        </dd>
        <dd v-else>{{ stat.value }}</dd>
      </div>
    </dl>
  </aside>

  <main v-else-if="isSettingsView" class="settings-page" aria-label="CodexTray 设置">
    <nav class="view-tabs settings-tabs" aria-label="设置页切换">
      <button
        v-for="tab in settingsTabs"
        :key="tab.value"
        :class="{ active: activeSettingsTab === tab.value }"
        type="button"
        @click="setSettingsTab(tab.value)"
      >
        {{ tab.label }}
      </button>
    </nav>

    <section v-if="activeSettingsTab === 'settings'" class="settings-card" aria-label="设置首页">
      <div class="settings-grid">
        <article class="setting-row">
          <div>
            <strong>全局快捷键</strong>
            <span>{{ settingsSnapshot?.settings.globalShortcut ?? shortcutDraft }}</span>
          </div>
          <form class="shortcut-form" @submit.prevent="saveShortcut">
            <input v-model="shortcutDraft" aria-label="全局快捷键" />
            <button type="submit">保存</button>
          </form>
        </article>

        <article class="setting-row">
          <div>
            <strong>检查更新</strong>
            <span>{{ updateMessage }}</span>
          </div>
          <button type="button" :disabled="isCheckingUpdates || isInstallingUpdate" @click="checkUpdates">
            {{ updateButtonLabel }}
          </button>
        </article>

        <article class="setting-row">
          <div>
            <strong>开机启动</strong>
            <span>{{ startupStatus?.message ?? "读取中" }}</span>
          </div>
          <button type="button" @click="toggleStartup">
            {{ startupStatus?.enabled ? "关闭" : "开启" }}
          </button>
        </article>

        <article class="setting-row">
          <div>
            <strong>Hook 采集</strong>
            <span>{{ hookMessage }}</span>
          </div>
          <button type="button" :disabled="hookToggleTarget !== null" @click="toggleHook">
            {{ hookButtonLabel }}
          </button>
        </article>
      </div>

      <dl class="runtime-list">
        <div>
          <dt>CodexTray 版本</dt>
          <dd>{{ runtimeInfo?.appVersion ?? "-" }}</dd>
        </div>
        <div>
          <dt>Codex CLI</dt>
          <dd>{{ runtimeInfo?.cliVersion ?? "未找到可启动 CLI" }}</dd>
        </div>
        <div>
          <dt>CLI 路径</dt>
          <dd>
            <span class="runtime-path-actions">
              <button type="button" @click="copyPath(runtimeInfo?.cliPath)">
                {{ pathWasCopied(runtimeInfo?.cliPath) ? "已复制" : (runtimeInfo?.cliPath ?? "未找到") }}
              </button>
              <button type="button" :disabled="isChoosingCliPath" @click="chooseCodexCliPath">
                {{ isChoosingCliPath ? "选择中" : "选择" }}
              </button>
              <button
                v-if="settingsSnapshot?.settings.codexCliPath"
                type="button"
                @click="clearCodexCliPath"
              >
                自动
              </button>
            </span>
          </dd>
        </div>
        <div>
          <dt>本地安装路径</dt>
          <dd>
            <button type="button" @click="copyPath(runtimeInfo?.installPath)">
              {{ pathWasCopied(runtimeInfo?.installPath) ? "已复制" : (runtimeInfo?.installPath ?? "-") }}
            </button>
          </dd>
        </div>
      </dl>
    </section>

    <section v-else-if="activeSettingsTab === 'announcements'" class="announcements-card" aria-label="更新公告">
      <header class="announcement-head">
        <strong>CodexTray v{{ runtimeInfo?.appVersion ?? "1.2.2" }}</strong>
        <span>更新公告</span>
      </header>
      <article
        v-for="item in announcementItems"
        :key="item.title"
        class="announcement-row"
      >
        <strong>{{ item.title }}</strong>
        <span>{{ item.detail }}</span>
      </article>
    </section>

    <section v-else class="logs-card" aria-label="最近日志">
      <article v-for="entry in logs" :key="`${entry.timestamp}-${entry.message}`" class="log-row">
        <time>{{ formatLogTime(entry.timestamp) }}</time>
        <strong>{{ entry.level }}</strong>
        <span>{{ entry.message }}</span>
      </article>
      <p v-if="logs.length === 0" class="empty-state">暂无日志</p>
    </section>
  </main>

  <div v-else class="panel-stage">
    <main class="tray-panel" aria-label="CodexTray 状态面板">
      <div v-if="isDashboardRefreshing" class="refresh-banner" role="status">
        <span aria-label="正在刷新"></span>
      </div>

      <section class="account-row" aria-label="账号状态">
        <div class="avatar" aria-hidden="true">
          <svg viewBox="0 0 32 32" role="img">
            <circle cx="16" cy="16" r="15" fill="#0e1420" />
            <circle
              cx="16"
              cy="16"
              r="9.8"
              fill="none"
              stroke="#22d3ee"
              stroke-dasharray="44 18"
              stroke-linecap="round"
              stroke-width="4.8"
              transform="rotate(126 16 16)"
            />
            <circle
              cx="16"
              cy="16"
              r="9.8"
              fill="none"
              stroke="#2dd4bf"
              stroke-dasharray="28 34"
              stroke-linecap="round"
              stroke-width="4.8"
              transform="rotate(-38 16 16)"
            />
            <circle cx="12.2" cy="20.3" r="2.1" fill="#e5e7eb" />
            <circle cx="21.2" cy="11.7" r="1.8" fill="#e5e7eb" />
          </svg>
        </div>
        <div class="account-copy">
          <h1>
            {{ accountEmail }}
          </h1>
          <p>数据更新时间 {{ updatedAt }}</p>
        </div>
        <div class="status-tags" aria-label="账号标签">
          <span class="tag tag-plan">{{ planLabel }}</span>
          <span class="tag tag-connected">{{ statusLabel }}</span>
        </div>
      </section>

      <section class="quota-card" aria-label="额度">
        <div class="quota-heading">
          <h2>{{ quotaTitle }}</h2>
          <span
            v-if="quotaResetCredits"
            class="quota-reset-pill"
            tabindex="0"
            aria-describedby="quota-reset-tooltip"
          >
            重置次数: {{ quotaResetCredits.availableCount }}
            <span id="quota-reset-tooltip" class="quota-reset-card" role="tooltip">
              <span class="quota-reset-card-head">
                <span>过期时间</span>
                <b>可用: {{ quotaResetCredits.availableCount }}</b>
              </span>
              <time v-if="quotaResetCredits.expiresAt">
                {{ formatResetCreditExpiresAt(quotaResetCredits.expiresAt) }}
              </time>
              <strong v-else>暂无过期时间</strong>
            </span>
          </span>
        </div>
        <div v-if="quotaWindows.length > 0" class="quota-list">
          <article v-for="quota in quotaWindows" :key="quota.label" class="quota-row">
            <strong>{{ formatQuotaLabel(quota.label) }}</strong>
            <div class="quota-bar" aria-hidden="true">
              <span
                v-for="segment in 40"
                :key="segment"
                :class="{ active: segment <= Math.round(quota.remainingPercent / 2.5) }"
              />
            </div>
            <b>{{ quota.remainingPercent }}%</b>
            <time>{{ formatResetAt(quota.resetAt) }}</time>
          </article>
        </div>
        <div v-else class="quota-list quota-placeholder" aria-label="额度占位">
          <article v-for="label in ['5H', '7D']" :key="label" class="quota-row">
            <strong>{{ label }}</strong>
            <div class="quota-bar" aria-hidden="true">
              <span v-for="segment in 40" :key="segment" />
            </div>
            <b>--</b>
            <time>--</time>
          </article>
        </div>
      </section>

      <section class="activity-card" aria-label="Token 活动">
        <div class="metric-strip">
          <article v-for="metric in metrics" :key="metric.label" class="metric-item">
            <strong>{{ metric.value }}</strong>
            <span>{{ metric.label }}</span>
          </article>
        </div>

        <div class="heatmap-heading">
          <h2>Token 活动</h2>
          <nav aria-label="热力图模式">
            <button
              v-for="mode in heatmapModes"
              :key="mode.value"
              :class="{ active: heatmapMode === mode.value }"
              type="button"
              @click="setHeatmapMode(mode.value)"
            >
              {{ mode.label }}
            </button>
          </nav>
        </div>

        <div class="heatmap-area" @mouseleave="hideHeatmapDetail">
          <div class="heatmap-grid" aria-label="近 32 周 Token 活动">
            <span
              v-for="day in visibleHeatmapDays"
              :key="day.date"
              class="heatmap-cell"
              :class="heatmapCellClasses(day)"
              @mouseenter="showHeatmapDetail(day)"
            />
          </div>
        </div>

        <div class="month-row" aria-hidden="true">
          <span>11月</span>
          <span>12月</span>
          <span>1月</span>
          <span>3月</span>
          <span>4月</span>
          <span>5月</span>
          <span>6月</span>
        </div>
      </section>
    </main>
  </div>
</template>
