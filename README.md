# CodexTray

CodexTray 是一款面向 Windows 的 Codex 桌面托盘面板，用于快速查看 Codex 账号额度、Token 活动、Hook 工作统计和更新状态。它以轻量托盘窗口运行，适合需要频繁关注 Codex 使用情况的开发者。

![CodexTray 预览](https://cdn.nodeimage.com/i/FsUYfNjkcOEKUow99Ysb5kGXj7lSeTfD.webp)

---

## 功能特性

### 额度与账号状态

- Codex 账号信息读取：显示当前账号、套餐标签和刷新时间
- 额度窗口展示：支持 5H、7D 等额度窗口的剩余百分比与重置时间
- 托盘额度条：无需打开面板，即可通过托盘图标底部微型进度条判断剩余额度
- 最紧张额度优先：托盘提示会优先展示剩余量最低的额度窗口

### Token 活动

- Token 活动统计：展示累计 Token、峰值 Token、最长任务时长和连续使用天数
- 近 32 周热力图：支持每日、每周和累计三种视图
- 详情浮窗：悬停热力图单元格时查看当天或当周的 Token 与 Hook 活动摘要

### Hook 采集

- 一键开启或关闭 Codex Hook 采集
- 记录会话数、对话轮次、工具调用、权限请求、上下文压缩和子智能体使用情况
- Hook 状态会在设置页中展示，便于确认采集是否正常

### 桌面体验

- 系统托盘常驻：左键打开面板，右键菜单支持刷新、设置和退出
- 全局快捷键：默认 `Ctrl+Shift+C`，可在设置页修改
- 开机启动：可在设置页一键开启或关闭
- 自动更新：内置 Tauri updater，支持从发布元数据检查新版本
- 轻量透明面板：贴近系统托盘弹出，不占用任务栏空间

---

## 快速开始

### 系统要求

| 项目 | 要求 |
| --- | --- |
| 操作系统 | Windows 10 / Windows 11 |
| 架构 | x64 |
| 依赖 | 已内置 WebView2 运行时检测与 Tauri 桌面运行能力 |
| Codex | 本机需要可启动的 Codex CLI 或 Codex.app CLI 入口 |

### 下载安装

1. 前往 GitHub Releases 页面下载最新版安装包或便携版压缩包。
2. 运行 `CodexTray_1.2.1_x64-setup.exe` 并完成安装。
3. 启动后在系统托盘中找到 CodexTray 图标。
4. 点击托盘图标打开面板，等待首次额度刷新完成。

> 提示：如果 Windows SmartScreen 提示“发布者未知”，这是因为当前安装包未进行 Windows 代码签名。请确认安装包来源为官方 Release 页面。

### 更新说明

- 安装版：支持自动更新。
- 便携版：可以手动下载新版压缩包替换；不承诺自动更新。
- 便携版点击检查更新：可能会拉起安装包更新流程，不等同于便携更新。

---

## 使用方式

| 操作 | 功能 |
| --- | --- |
| 左键点击托盘图标 | 显示或隐藏状态面板 |
| 右键点击托盘图标 | 打开托盘菜单 |
| 托盘菜单“刷新数据” | 手动刷新账号、额度和活动数据 |
| 托盘菜单“设置” | 打开设置窗口 |
| `Ctrl+Shift+C` | 显示或隐藏状态面板 |

---

## 数据存储位置

| 数据 | 路径 |
| --- | --- |
| 应用设置 | `%LocalAppData%\CodexTray\settings.json` |
| 应用日志 | `%LocalAppData%\CodexTray\logs\codextray.log` |
| Hook 事件 | `%LocalAppData%\CodexTray\hook-events\` |
| Codex Hook 配置 | `%CODEX_HOME%\hooks.json` 或用户 Codex 配置目录 |

CodexTray 只写入自身管理的设置、日志和 Hook 事件数据。关闭 Hook 采集时，会移除由 CodexTray 管理的 Hook 条目，不会清空用户的 Codex 配置。

---

## 技术栈

| 组件 | 技术 |
| --- | --- |
| 桌面框架 | Tauri 2 |
| 前端 | Vue 3 + TypeScript + Vite |
| 后端 | Rust |
| 托盘与定位 | Tauri Tray + tauri-plugin-positioner |
| 更新 | tauri-plugin-updater |
| 快捷键 | tauri-plugin-global-shortcut |

---

## 开发

```powershell
npm install
npm run typecheck
npm run build
npm run tauri dev
```

Rust 侧测试：

```powershell
cd src-tauri
cargo test
```

---

## 版本

当前发布版本：`1.2.1`

---

## 开源许可证

CodexTray 基于 GPL-3.0 license 开源发布。详情请查看 [LICENSE](LICENSE)。
