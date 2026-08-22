---
layout: default
title: App Manager UI 设计
nav_order: 6
---

# Alex OS 应用管理 UI 设计

状态：设计提案  
适用版本：Alex OS 0.1.x 原型  
优先级：P0

## 1. 目标与定位

Alex OS 需要一个独立的“应用管理中心”，让普通用户无需 CLI 即可完成应用安装、查看、
启动、停止、更新、权限管理和卸载。

管理中心是 Alex OS 的内置系统组件，不是普通第三方 Alex APP。它需要管理其他应用、
安装目录、发布者信任和权限决定，其权限等级高于普通应用。

第一阶段只建设本地应用管理器，不同时建设在线 Alex Store。

## 2. 当前基础与缺口

| 能力 | 底层状态 | UI 状态 |
| --- | --- | --- |
| 已安装应用列表 | 已实现 | 未实现 |
| 安装 `.alex` 包 | 已实现 | 未实现 |
| 启动应用 | Shell 已支持 | 缺少统一入口 |
| 卸载应用 | 已实现 | 未实现 |
| 本地及远程更新 | 已实现基础流程 | 未实现 |
| 权限查看与修改 | 已实现 | 未实现 |
| 发布者信任管理 | 已实现 | 未实现 |
| Runtime 状态 | 已有部分状态 | 未统一展示 |
| 应用图标、描述、作者 | Manifest 字段不足 | 无法完整展示 |
| 存储占用 | 未实现统计 | 未实现 |
| 日志与诊断 | 有日志基础 | 缺少查询和展示接口 |
| 更新进度、暂停与重试 | 缺少任务模型 | 未实现 |
| 搜索、筛选与排序 | 未实现 | 未实现 |

现有 CLI 必须继续保留。CLI 和 UI 应复用同一套 Rust 服务层；UI 不得通过启动 CLI 并解析
控制台文本来操作应用。

## 3. 信息架构

### 3.1 应用首页

展示所有已安装应用：

- 图标、名称、版本和发布者；
- 运行状态、更新状态和最近使用时间；
- 启动、停止和更多操作；
- 按名称、安装时间和更新时间排序；
- 按正在运行、可更新、权限异常筛选；
- 支持搜索和拖入 `.alex` 文件安装。

桌面端默认使用高信息密度列表，可在后续增加卡片视图。

### 3.2 应用详情

应用详情包含以下区域：

#### 概览

- 名称、图标、版本、应用 ID；
- 发布者、公钥指纹和签名状态；
- 安装路径、安装时间和最近更新时间；
- 前端及 Runtime 类型；
- 启动、停止、重启、打开应用目录和卸载。

#### 权限

- 显示 Manifest 声明的权限和具体作用域；
- 显示用户决定：允许、拒绝或使用时询问；
- 不允许授予 Manifest 未声明的能力；
- 路径、域名等范围必须完整可见；
- 修改权限写入审计日志。

#### 更新

- 当前版本和 stable/beta/dev 更新通道；
- 手动检查更新；
- 新版本、发布者、签名状态及更新说明；
- 下载、验证、安装、失败和回滚结果。

#### 日志与诊断

- Runtime 标准输出和错误输出；
- PID、启动时间、运行状态和重启次数；
- 最近崩溃、IPC 错误和权限审计记录；
- 导出诊断包。

#### 存储

- 安装目录、应用数据和缓存占用；
- 打开数据目录、清理缓存和重置数据；
- 清理与重置必须二次确认并明确删除范围。

### 3.3 信任中心

- 列出发布者名称、公钥指纹、添加时间和关联应用；
- 添加或删除受信任发布者；
- 查看完整公钥；
- 删除信任前提示：已安装应用不会自动删除，但后续更新可能无法验证。

身份判断必须以公钥指纹为准，显示名称只用于帮助识别。

### 3.4 全局设置

- 默认安装目录；
- 默认更新通道；
- 自动检查更新及后台下载策略；
- 开机启动；
- DevTools 和开发者模式；
- 日志保留时间；
- Alex OS 版本与 WebView2 状态。

## 4. 安装流程

安装流程必须显式包含安全检查：

```text
选择或拖入 .alex 包
        ↓
校验 ZIP、完整性清单和 Manifest
        ↓
校验包签名和发布者信任
        ↓
展示身份、版本、权限和风险
        ↓
用户确认
        ↓
原子安装
        ↓
显示成功或可操作的失败原因
```

确认页必须展示应用名称、ID、版本、发布者指纹、签名状态、权限、安装位置，以及是否覆盖
现有版本或构成降级。

未知发布者建议提供“取消”“仅本次允许”“信任并安装”。其中“仅本次允许”当前尚无底层
策略，需要开发临时信任上下文，且不得写入持久 Trust Store。

## 5. 数据模型扩展

Manifest 需要增加可展示的静态元数据：

```json
{
  "id": "com.example.notes",
  "name": "Notes",
  "version": "1.2.0",
  "description": "A local notes application",
  "author": {
    "name": "Example Studio",
    "url": "https://example.com"
  },
  "icons": {
    "16": "assets/icon-16.png",
    "32": "assets/icon-32.png",
    "64": "assets/icon-64.png",
    "256": "assets/icon-256.png"
  },
  "homepage": "https://example.com/notes",
  "license": "MIT",
  "update": {
    "manifestUrl": "https://example.com/updates/stable.json",
    "channel": "stable"
  }
}
```

安装时间等动态状态不能回写第三方 Manifest，应由 Alex OS 维护 App Registry：

```json
{
  "installedAt": "2026-08-21T10:00:00Z",
  "updatedAt": "2026-08-21T10:00:00Z",
  "lastLaunchedAt": null,
  "publisherFingerprint": "...",
  "source": "local-package",
  "packageSha256": "..."
}
```

App Registry 写入必须原子化，并能在记录损坏时从已安装目录安全重建。

## 6. 后端架构

```text
内置 App Manager WebView
            │ typed IPC
            ▼
App Management Service
  ├── App Registry
  ├── Package Installer
  ├── Runtime Supervisor
  ├── Permission Store
  ├── Trust Store
  ├── Update Manager
  └── Audit Log
```

建议抽取统一服务接口，供 CLI 和管理 UI 共用：

```rust
trait AppManager {
    fn list_apps(&self) -> Result<Vec<AppSummary>>;
    fn get_app(&self, id: &str) -> Result<AppDetails>;
    fn install(&self, package: &Path, options: InstallOptions) -> Result<TaskId>;
    fn uninstall(&self, id: &str, options: UninstallOptions) -> Result<TaskId>;
    fn launch(&self, id: &str) -> Result<RuntimeStatus>;
    fn stop(&self, id: &str) -> Result<()>;
    fn permissions(&self, id: &str) -> Result<Vec<PermissionState>>;
    fn set_permission(&self, id: &str, permission: &str, decision: Decision) -> Result<()>;
    fn check_update(&self, id: &str) -> Result<Option<UpdateInfo>>;
    fn update(&self, id: &str) -> Result<TaskId>;
}
```

安装、卸载和更新必须成为异步任务，通过 IPC 事件报告进度。统一任务状态为：

```text
queued → validating → verifyingSignature → downloading → extracting
       → installing → completed | failed | cancelled
```

事件至少包含 `taskId`、操作类型、应用 ID、阶段、进度和结构化错误。UI 关闭后任务状态仍应
保留，重新打开时可以恢复展示。

## 7. 安全边界

- 管理中心只加载随 Alex OS 发布并经过校验的内置资源；
- 禁止加载远程页面、弹出新窗口和任意导航；
- 第三方应用不能进入管理中心或调用其系统 IPC；
- 管理 IPC 使用独立方法表，不暴露通用文件读写能力；
- Rust 层必须重新校验应用 ID、路径、签名和调用来源；
- 调用来源绑定为专用系统身份，例如 `alex://system/app-manager`；
- 安装、卸载、降级、发布者信任和删除数据全部写审计日志；
- 删除操作必须防御路径穿越、符号链接和安装根目录逃逸；
- 管理 UI 不得实现为拥有通配文件权限的普通 Alex APP。

## 8. 交互与技术选型

建议采用 React、TypeScript 和 Vite，前端资源内置在 Alex OS 中，通过专用 TypeScript SDK
调用 App Management Service。界面采用类似 Windows 11 设置页的结构：左侧导航、右侧详情、
明确的状态标签，并将卸载和清除数据放在独立的危险操作区。

首版必须支持键盘操作、清晰的焦点状态、屏幕阅读器标签和系统缩放。不能在功能完成后再补
无障碍，因为应用安装和权限确认属于关键安全交互。

## 9. 开发阶段与验收

### 阶段一：MVP（P0）

1. App Registry 和安装元数据；
2. App Management Service；
3. 已安装应用列表、搜索和详情；
4. 启动、停止和 Runtime 状态；
5. 本地 `.alex` 安装及安全确认；
6. 卸载及二次确认；
7. 权限查看和修改；
8. 发布者及签名状态展示；
9. 统一错误模型和审计记录。

验收标准：普通用户不使用 CLI，即可完成安装、查看、启动、停止、权限管理和卸载；所有
敏感操作均经过 Rust 端校验并留下审计记录。

### 阶段二：更新与诊断（P0/P1）

- 检查并安装更新；
- 更新通道持久化；
- 后台任务、进度、取消、重试和恢复；
- Runtime 日志、崩溃记录和诊断导出；
- 安装、数据和缓存空间统计。

验收标准：远程更新不会阻塞 UI；下载或安装失败后可明确定位阶段并安全重试，应用仍可回滚
到原版本。

### 阶段三：产品化（P1）

- 自动检查更新和后台下载；
- 批量更新；
- 系统托盘和开机启动；
- 应用快捷方式；
- 应用数据迁移；
- 多语言、主题和完整无障碍测试。

## 10. 明确不在首版范围内

- 在线 Alex Store；
- 用户账户、支付、评分和评论；
- 云端应用同步；
- 插件市场；
- 企业集中管理策略；
- macOS 和 Linux 管理 UI。

先完成本地管理闭环，稳定 App Registry、任务协议和系统权限边界，再扩展在线分发能力。

## 关联文档

- [`status.md`](./status.md) — 本文 §2"当前基础与缺口"一表的"底层状态"列对应 status.md 的事实清单（§2.4 运行时、§2.5 Native API、§2.6 权限系统、§2.7 应用包/签名/信任、§2.10 App Manager Service 状态展示）。每行"未实现"标注都映射到 status.md 的"限制"小节。
- [`roadmap.md`](./roadmap.md) — 本文 §9 开发阶段映射到：MVP / 阶段二 ≈ roadmap P0 §3.2 权限设置 UI + §3.4 安装器；阶段三 ≈ P1 §3.5 插件系统的插件市场部分。
- [`DESKTOP_API_STATUS.md`](./DESKTOP_API_STATUS.md) — 本文 §5 提到的 Manifest 字段（icons / author / license）需要"系统"支持读取；`window.setTitle` 等窗口 API 的 wired 状态以 DESKTOP_API_STATUS.md 为准。
- [`reverse-ipc.md`](./reverse-ipc.md) — 当 App Manager 以自托管 plugin 形式运行（替代内置 `alex manager`）时，frontend 通过普通 Alex IPC 调 `system.listApps / system.install / system.uninstall`，走和普通 app 完全一样的 dispatch 路径；详见 reverse-ipc.md §7 self-hosting 全景。
- [`alex-container-design.md`](./alex-container-design.md) — 0.2 起 Manager plugin 应改用 `ContainerService` trait（见容器设计 §10 内部 API），而不是直接调用 0.1 的 `RuntimeSupervisor`。
- [`index.md`](./index.md) — 文档阅读路径与本文档在整体中的位置。
