# MyKVM 项目分析与二次开发指南

> 分析基准：**`bate` 分支 commit `fe938ee`**（2026-10-01，v0.9.13-beta.100 之后，二次开发基线）
> 对比参考：`main` 分支 `a2ea416`（2026-07-16，稳定线），差异见第 10 节
> 分析日期：2026-10-02 · 上游仓库：<https://github.com/XxMinor/mykvm> · 许可证：MIT

---

## 1. 项目概览

MyKVM 是一个**开源跨平台软件 KVM**：在一组受信任的局域网机器之间共享键盘、鼠标、滚轮和剪贴板（文本 + 图片），并支持把文件发送到远端设备。用软件替代物理 KVM 切换器，鼠标滑到屏幕边缘即可把控制权"交接"到另一台机器的显示器上。

**核心能力清单**（源自 README + 代码验证）：

| 能力 | 说明 | 主要实现 |
|---|---|---|
| 局域网自动发现 | 单端口 UDP 广播发现同网段设备 | `lib.rs` UDP 发现（端口默认 47833） |
| 手动添加/配对 | 输入 IP 直连 + 配对码确认 | `lib.rs` pairing 流程 |
| 加密传输 | QUIC（TLS 1.3）+ 对端证书固定 | `quic_transport.rs`（端口默认 47834） |
| 输入捕获/注入 | 钩子捕获本机输入 → 远端注入 | `input.rs`（26 万字符大文件） |
| 剪贴板同步 | 文本 + RGBA 图片，双向去重回声 | `clipboard.rs` |
| 文件传输 | 发送文件到远端设备 | `send_files_to_device` 命令；bate 分支大幅增强（拖拽） |
| 多显示器布局 | 跨机器的虚拟屏幕拼接编辑器 | `layout.ts` + `App.tsx` 布局 Tab |
| 锁屏输入（Windows） | Windows 服务 + 会话内 worker，锁屏/安全桌面也能被控 | `input-helper/` crate |
| 应用内更新 | GitHub Releases + minisign 签名的更新清单 | `tauri-plugin-updater` + `release.yml` |
| 性能监控 | 可选的 CPU/内存/包速率采样 | `performance.rs` |
| 双语界面/主题 | 简体中文 + English；浅/深/跟随系统 | `i18n.ts` |

**版本与分支状态**（bate 为当前分析基线）：

- bate 分支 HEAD `fe938ee`（2026-10-01）；beta 版已发到 `v0.9.13-beta.100`；相对 main 领先 68 个提交（+8623/-849 行）。
- main 分支 HEAD 停在 2026-07-16；最新稳定 tag `v0.9.12`；共 82 个提交。
- 仓库内 `package.json`/`Cargo.toml`/`tauri.conf.json` 的版本号恒为 `0.1.0`——真实版本号由 CI 发布时计算并临时写入（见第 8 节）。
- 根目录 `CHANGELOG.md` 的版本段落滞后于 tag 序列（bate 分支已补录部分稳定性修复说明）；发布说明优先取 `[Unreleased]` 段，为空才回退到提交标题。二次开发时应注意沿用或修复这一机制。

## 2. 技术栈与依赖地图

### 2.1 总体形态

```
┌────────────────────────── Tauri 2 桌面应用 ──────────────────────────┐
│  WebView 前端（React 19 + TS 6 + Vite 8）                            │
│    App.tsx（单文件巨石 UI）→ desktopApi.ts（invoke 桥）→ types.ts 等   │
│                    │ invoke() / 能力白名单 capabilities               │
│  Rust 后端（src-tauri）                                              │
│    lib.rs —— 命令、状态、UDP 发现、配对、剪贴板轮询、托盘、服务管理     │
│    quic_transport.rs —— QUIC/TLS1.3 + 证书固定 + 流/数据报双通道       │
│    input.rs —— 输入捕获（钩子）与注入（SendInput/CGEvent/XTest）       │
│    clipboard.rs —— 剪贴板读写（arboard + 平台原生回退）                │
│    shared_input.rs —— 跨进程共享的输入事件协议（MessagePack 帧）       │
│    windows_input.rs —— Windows 注入原语（SendInput 等）               │
│    input-helper/ —— Windows 服务（锁屏输入，独立 exe sidecar）         │
└──────────────────────────────────────────────────────────────────────┘
```

### 2.2 前端依赖（package.json）

| 包 | 版本 | 用途 |
|---|---|---|
| @tauri-apps/api | ^2.11.0 | invoke/事件桥 |
| @tauri-apps/plugin-process | ^2.3.1 | relaunch（更新后重启） |
| @tauri-apps/plugin-updater | ^2.10.1 | 应用内更新 |
| react / react-dom | ^19.2.5 | UI |
| vite ^8 + typescript ~6.0.2 + eslint 10 | dev | 构建/质量 |

构建脚本要点：`__APP_VERSION__` 由 `vite.config.mjs` 从 package.json 注入；`vite.config.mjs` 特意写成 ESM 以规避 Windows 上 Vite 默认配置打包子进程被拦的问题。

### 2.3 Rust 依赖（src-tauri/Cargo.toml，edition 2021）

| crate | 用途 |
|---|---|
| tauri 2.11（features: `macos-private-api`, `tray-icon`） | 应用框架；macOS 私有 API 用于光标隐藏等 |
| tauri-plugin-log / updater / process / autostart / global-shortcut | 日志、更新、重启、开机自启、全局快捷键 |
| quinn 0.11 + rustls（quinn 内置）+ ring 0.17 | QUIC/TLS 1.3 传输 |
| rcgen 0.13 | 首启生成自签名证书（传输身份） |
| arboard 3.6 | 跨平台剪贴板（文本+图片） |
| rmp-serde 1 | MessagePack 序列化（输入命令帧/发现报文） |
| socket2 / if-addrs | SO_REUSEADDR 套接字绑定 / 本机网卡枚举 |
| tokio 1.52（rt-multi-thread, io-util, sync, time） | QUIC 侧异步运行时 |
| windows-sys 0.61（大量 Win32 features）/ windows-service 0.8 | Win32 原生调用 / 服务框架 |
| **windows 0.61 + windows-core 0.61**（bate 新增） | COM 对象模型（IDataObject/IDropSource/IStream + `#[implement]`），用于被控端**原生 OLE 拖放**（windows-sys 只把 COM 建模为裸指针） |
| image 0.25（仅 bmp） | Windows CF_DIB 剪贴板位图解码 |
| core-foundation / core-graphics（macOS） | CGEventTap 输入捕获 |

`rust-version = "1.89"`（bate 提升，macOS 单实例锁用了 `std::fs::File::try_lock`）。Workspace 成员：`input-helper`（Windows 专用子 crate，产出锁屏输入辅助 exe sidecar）。另有 `src-tauri/examples/single_instance.rs` 示例。

## 3. 架构与数据流

### 3.1 端口与协议常量

| 常量 | 值 | 定义位置（bate） |
|---|---|---|
| 发现端口（transportPort） | 47833（`auto` 模式被占用时漂移，跨度 8） | `defaultLayout.ts:30`；lib.rs `DISCOVERY_PORT`/`DISCOVERY_PORT_SPAN` |
| QUIC 端口（quicPort） | 47834，占用时向后扫描最多 64 个（`PORT_SCAN_COUNT`）后回退随机 0 | `quic_transport.rs:36`、`candidate_ports()` |
| 协议版本 | `PROTOCOL_VERSION = 1`（不匹配直接拒绝连接） | `quic_transport.rs:27` |
| 数据报上限 | 16 KiB（输入事件走数据报，latest-wins） | `quic_transport.rs:30` |
| 流上限 | 48 MiB（剪贴板图片/文件走流 + "ok"/"reject" ACK） | `quic_transport.rs:33` |
| TLS SNI | 固定 `mykvm.local`（自签证书 CN） | `quic_transport.rs:29` |

### 3.2 设备发现（UDP 47833）

- 服务端角色在本机绑定发现端口，周期性广播**本机通告**（peer 信息：id/name/platform/clusterId/配对要求/屏幕列表/QUIC 端口/base64 证书公钥/协议版本，对应前端 `LanPeer` 类型 `runtime.ts:8`）。
- 同时监听其他设备的通告，维护 `peers` 列表（`lastSeenMs` 超时剔除）。
- 通告中携带 `transportPublicKey`（= 本机证书 DER 的 base64，`quic_transport.rs:488-496` 注释明确：**该键必须跨重启稳定**，否则对端证书固定失败、配对授权失效）。因此身份文件持久化在应用数据目录：`quic-transport-cert.der` / `quic-transport-key.der`（`load_or_create_identity()`，`quic_transport.rs:501`）。bate 修复：TUN/代理隧道下本机 IP 选择规避（见 4.4.1 IP 选择链），设备身份（主机名+主 IPv4）不再随代理漂移。
- 支持"单端口模式"（transportPortMode: auto/fixed）：auto 下端口被占则漂移，fixed 下尝试复用。

### 3.3 配对与授权

- 机器分 `server`（服务端，管理布局、捕获输入）与 `client`（客户端，被控）两种角色（`MachineRole`，`types.ts:3`）；同集群由 `clusterId` 标识。
- 配对码流程（两段式传输，敏感信息不走明文 UDP）：请求方 `request_lan_pairing` → UDP 发现包 `pair-request`（lib.rs 处理于 :1044-1063）→ 被请求方 `begin_pairing_challenge`（:8682，生成 6 位码并弹窗，状态机见前端 `PairingStatus` idle/available/requested/paired）回 `pair-challenge` → 请求方 `confirm_lan_pairing(host, code)` → **把 `pair-confirm`（code+clusterId+pairSecret）经 QUIC 加密流发送并等待 ACK**（`confirm_pairing_for_peer` :8285-8324）→ 被请求方 `handle_pairing_stream_packet`（:8581）→ `complete_pairing_from_confirm`（:8748，错码累计 5 次销毁挑战）验码并写入配对列表。bate 还支持"证书轮换修复"（已配对控制器换证书后可重配，:8553）。
- 成功后双方把对方记入 `pairedControllers`（含对方证书公钥，`types.ts:26-35`）；QUIC 握手时证书固定校验 + 应用层校验对方是否在配对列表。`reset_pairing` 命令清空。

### 3.4 输入事件流（控制端 → 被控端）

```
控制端 input.rs 钩子捕获
  → 判定"正在控制远端"（鼠标越界/热键）
  → InputEvent（MouseMove{screen_id,x,y}/MouseButton/Scroll/Key{key_code:u16}）
     —— shared_input.rs:18-26（InputEvent）/:38-57（InputCommand），serde camelCase
  → rmp-serde 编码 → TransportHandle::send_datagram（16KiB 内，UDP 数据报，无 ACK、latest-wins）
  → 被控端 on_datagram 回调 → 解码 → 授权校验 → 注入路径（bate 分支）：
     · 常规（GUI 应用运行中）：input.rs 直接注入（Windows SendInput / macOS CGEventPost）；
       Windows 注入被拒（UIPI/ERROR_ACCESS_DENIED）时**经 SYSTEM helper 管道重试**（d28a72b）
     · 锁屏/未登录（Windows）：Windows 服务内的 **headless_client 只收运行时**收到数据报
       → 经命名管道 \\.\pipe\mykvm-input-s{session} 发 InputCommand（4 字节 LE 长度帧）
       → input-helper 服务 worker 注入（OpenInputDesktop + DESKTOP_JOURNALPLAYBACK）
     · 单个 send 失败不断流、丢 "up" 数据报的键/钮自动释放（见第 10 节）
```

关键可靠性设计（`quic_transport.rs` 注释详述）：
- **命令循环永不 await 网络**：数据报在已建连接上同步入队；连接建立在 spawn 任务里做，避免一只死-peer 的 2s 握手把所有鼠标事件卡住（历史 bug"周期性输入冻结"）。
- **peer 健康快速失败**：连续 ≥3 次失败进入 3s 静默重试窗（`DATAGRAM_FAIL_THRESHOLD/RETRY_WINDOW`），让输入层立刻释放光标而不是把 move 流灌进黑洞。
- **连接槽去重 + `forget_connection`**：`ConnectionSlot::Connecting` 防连接风暴；发送失败时从连接缓存剔除坏连接（bate）。
- **keep-alive 3s / idle 超时 10s**：空闲不掉线，死连接几秒内自愈。
- 剪贴板/文件等大块数据走**双向流 + ACK**（`send_stream_expect_ack`，5s 超时；最多 8 个并发流）。
- bate：`start_datagram_only`（无流处理）服务无头接收专用入口；updater 清单拉取 20s 超时（PR #22）。

### 3.5 剪贴板流

- 双向轮询检测：签名去重（文本直接比对；图片 FNV-1a 哈希 + 尺寸 + 长度，clipboard.rs:56）防止"刚从对端收到的内容又被发回去"（回声抑制）。
- 尺寸上限：文本 256 KiB、图片 32 MiB RGBA（线上 base64 约 43 MiB，流上限 48 MiB 与之对齐，quic_transport.rs:33）。
- 平台差异：Windows 用 arboard + `IsClipboardFormatAvailable` 判型（PNG/DIBV5/DIB/BMP 优先判图）+ CF_DIB 原生回退解码（`image` crate 的 `BmpDecoder::new_without_file_header`，clipboard.rs:383）；macOS 走 `pbcopy/pbpaste` 子进程并强制 `LANG/LC_CTYPE=en_US.UTF-8`（修中文乱码）；Linux 走 `wl-copy/wl-paste`，失败回退 `xclip`。
- bate 传输可靠性：发送侧 `send_stream_expect_ack`；对端无响应按 2s→4s→8s…封顶 60s 指数退避（lib.rs `clipboard_retry_delay` :5912），对端明确拒绝（STREAM_REJECTED）不重试；接收写回重试 10×50ms。

### 3.6 文件传输与拖拽（bate）

- 通道一（API 发送）：`send_files_to_device(deviceId, paths)` 命令（desktopApi.ts:297）返回 `FileTransferSummary{targetName,fileCount,byteCount}`，经 QUIC 流分块发送，接收端写入 `fileTransferReceiveRoot` 并做文件名/ID 消毒与授权校验。
- 通道二（拖拽，bate 新增）：从控制端把文件**拖到远端屏幕**上完成传输——`windows_drag.rs`（OLE 数据对象/拖拽源）+ `windows_drag_overlay.rs`（拖拽覆盖层）+ `windows_drop_catcher.rs`（被控端接住系统拖放），拖拽控制消息**按序**发送（39f6e5a），前端有拖放目标高亮与摘要展示（App.tsx `NativeFileDragPayload` :163、`fileTransferTargetIdAtPosition` :3904）。

### 3.7 Windows 服务化接收（bate 新架构）

被控 Windows 机器现在的输入接收分两级：

1. **服务级（登录前/锁屏也可用）**：`MyKVMInputService`（SYSTEM）除管理会话 worker 外，自身运行 `headless_client.rs` 的**只收输入网络运行时**（`headless_client::start`，input-helper/main.rs:349）：datagram-only QUIC（`start_datagram_only`，quic_transport.rs:262）+ 发现通告线程（250ms 轮询/3s 通告），收到输入数据报 → `handle_input_datagram_with_sink` → `dispatch_input_command_to_windows_helper` → 会话 worker 管道注入。启动条件：client 角色 + receive 模式 + 已配对（`headless_receive_enabled`，headless_client.rs:185）。
2. **GUI 接管**：用户登录、MyKVM 应用启动后，应用经**服务控制管道** `\\.\pipe\mykvm-service-control`（shared_input.rs:12）命令服务停掉 headless receiver（"GUI takeover"，input-helper/main.rs:282），由应用自身运行完整运行时（含剪贴板/文件流/配对）。

配套机制：`start_held_input_watchdog`（按住的键在管道断开时自动释放）、服务配置参数解析（legacy worker-only 参数兼容，`parse`）、控制管道 ACK（`write_control_ack`）、按 owner SID 的管道 SDDL。

## 4. 源码模块清单

### 4.1 Rust 后端（src-tauri/src/，bate 分支行数）

| 文件 | 体量 | 职责 |
|---|---|---|
| `lib.rs` | 10756 行（最大） | Tauri 入口 `run()`、全部 34 个 #[tauri::command]、应用状态、UDP 发现、配对、剪贴板同步循环、托盘/自启/更新/窗口、Windows 服务控制管道客户端、文件传输编排、对端日志拉取（函数级地图见 4.4） |
| `input.rs` | 9478 行 | 输入捕获与注入，平台分支实现（地图见 4.4） |
| `quic_transport.rs` | 1288 行 | QUIC 端点绑定（SO_REUSEADDR）、身份持久化、证书固定、连接缓存、数据报/流收发、peer 健康度、`start_datagram_only`（服务无头接收）与 `forget_connection`（坏连接剔除）；含单元测试 |
| `clipboard.rs` | ~14 KB | 剪贴板内容抽象（文本/图片）、签名去重、平台读写、bate 增加对无响应 peer 的重试退避（见 3.5） |
| `windows_input.rs` | ~16 KB | Windows 注入原语：`inject_mouse_move`（虚拟屏 65535 归一化 + SetCursorPos 兜底）、`inject_mouse_button`（含 XBUTTON1/2 侧键）、`inject_scroll`（×120）、`inject_key`（VK→扫描码 + EXTENDEDKEY）、`DesktopAttachment`（OpenInputDesktop，JOURNALPLAYBACK 权限是锁屏注入的关键）、`send_secure_attention`（sas.dll!SendSAS）、注入被拒诊断（UIPI ERROR_ACCESS_DENIED，10s 节流告警） |
| `shared_input.rs` | ~4 KB | 跨进程/跨端共享协议：`InputEvent`（网络）、`InputCommand`（管道）、按钮位掩码、MessagePack + 4 字节长度帧编解码、管道名/`SERVICE_CONTROL_PIPE` 服务控制管道/状态文件路径常量 |
| `headless_client.rs` | 232 行（bate 新增） | Windows 服务内的只收输入网络运行时（datagram-only QUIC + 发现线程 + held-input 看门狗），见 3.7 |
| `performance.rs` | ~7.5 KB | 进程 CPU%（Windows GetProcessTimes 差分 / Unix ps 差分）、内存（WorkingSet / phys footprint / rss） |
| `main.rs` | ~20 行 | `handle_process_control_args()` → `acquire_single_instance()`（autostart 重复启动不抢窗，bate）→ `run()` |
| `windows_drag.rs` / `windows_drag_overlay.rs` / `windows_drop_catcher.rs` | 1044/344/546 行（bate 新增） | 原生 OLE 拖拽传输族（地图见 4.4） |

### 4.2 Windows 输入服务（src-tauri/input-helper/，独立 crate，bate 大幅增强）

双模式二进制（`--service` / `--worker`），bate 上从"纯 worker 管理器"升级为**服务化接收平台**（+548 行）：

- `--service`：注册为 Windows 服务 `MyKVMInputService`。职责三件套：
  1. **worker 管理**：按会话事件（ConsoleConnect/Logon/Logoff 等）与 worker 退出检测，在活动会话重建注入 worker（`WTSGetActiveConsoleSessionId` → `WTSQueryUserToken` → `DuplicateTokenEx` → `CreateProcessAsUserW` 进 WinSta0\Default 桌面）；
  2. **headless 接收**：`headless_client::start()`（main.rs:349）启动服务级只收运行时（见 3.7），收到 GUI 应用经控制管道发来的接管/交还命令时启停（`start_receiver`/`stop_receiver`）；
  3. **服务控制管道**：`run_control_pipe`（main.rs:365）监听 `\\.\pipe\mykvm-service-control`（按 owner SID 定制 SDDL），解析 GUI 命令并 ACK。
  服务配置参数经 `parse` 解析（兼容 legacy worker-only 参数，拒绝残缺的 headless 参数）。
- `--worker`：会话内注入进程。创建入站管道 `\\.\pipe\mykvm-input-s{session}`（SDDL：SYSTEM + 会话用户 GA）→ 读 4 字节长度帧 → MessagePack 解码 `InputCommand` → `DesktopAttachment` 附加输入桌面 → `windows_input::inject_*`。状态写 `%ProgramData%\MyKVM\input-helper-status-s{session}.txt`。
- 容错设计：长度帧损坏→重建管道而非退出；未知命令→跳过保持连接（新旧版本混布兼容）；桌面/错误状态变化立即写状态文件但写入频次有界。

### 4.3 前端（src/，bate 分支）

| 文件 | 体量 | 职责 |
|---|---|---|
| `App.tsx` | 4135 行 | 全部 UI（`App()` :182-~3426，辅助函数 :3428-4135；bate 新增对端日志拉取按钮与文件拖拽目标高亮，地图见 4.4） |
| `desktopApi.ts` | 437 行 | 唯一的后端桥：32 个 `invoke` 包装（bate 新增 `fetchClientLog`）+ 浏览器 stub（`BROWSER_RUNTIME`，使 `npm run dev` 纯浏览器可跑布局编辑器）；updater `check({ timeout: 20_000 })` 防清单拉取卡死（PR #22）；updater/relaunch 按需动态 import |
| `types.ts` | 1.8 KB | `LayoutState`/`Device`/`Screen`/`PairedController`/`ModifierMap` 等核心类型 |
| `runtime.ts` | 2.2 KB | `RuntimeStatus`/`DiscoveryStatus`/`PairingStatus`/`InputServiceStatus`/`PerformanceSample` 等运行时快照类型 |
| `layout.ts` | 5.5 KB | 布局编辑器几何：屏幕展平、碰撞检测、重叠吸附（80px 容差 SNAP_TOLERANCE）、边界计算 |
| `defaultLayout.ts` | 1.9 KB | 默认布局（含浏览器回退设备），默认端口 47833/47834、默认修饰键映射 control↔meta、默认热键 |
| `hotkeyInput.ts` | 4.3 KB | 热键录制/规范化（KeyX→字母、F1-F24、方向键等；Backspace 清除） |
| `i18n.ts` | ~19 KB | `TEXT = { cn: {...}, en: {...} }` 静态字符串表（bate 增补日志拉取等文案） |
| `main.tsx` | 1.2 KB | StrictMode 挂载 + 禁用 WebView 缩放手势（Ctrl/Meta+滚轮、捏合） |
| `constants.ts` | 3 行 | APP_VERSION/仓库地址 |

### 4.4 大文件的函数级地图（bate 分支口径）

#### 4.4.1 `src-tauri/src/lib.rs`（bate：10756 行，34 个命令，60 个单元测试）

| 行号区间 | 内容 |
|---|---|
| 43-106 | 常量：`DISCOVERY_PORT=47833`、漂移跨度 `DISCOVERY_PORT_SPAN=8`、协议名（`mykvm.discovery.v1` / clipboard / file-transfer / drag-control / **log-request** 均 v1）、TTL 90s、剪贴板轮询 150ms/回声宽限 1200ms/退避上限 60s、文件分块 256KiB/单文件上限 2GiB/进度事件 100ms、日志 tail 512KiB、事件名 `runtime-state-changed`（:93）、单实例互斥体名 |
| 124-586 | 后端类型镜像：`SingleInstanceGuard` :124、`Screen`/`Device` :151/:165、`ScreenSwitchHotkeys` :197、`LayoutState`/`ModifierMap`/`PairedController` :234/:281/:292、`LanPeer` :312、`DiscoveryStatus`/`PairingStatus`/`RuntimeStatus` :358/:368/:379、`PairingChallenge` :450、`FileTransferTarget` :464、`TransferFile`/`IncomingFileTransfer` :475/:482、`FileTransferSummary`/`FileTransferProgress(Reporter)` :499-556（emit 进度事件 530-552）、`FileTransferPacket` :557（kind=start/chunk/finish + `drop_to_desktop`/`drag_drop`/`client_log` 标志）、**`AppRuntime` :587（含 Windows 网络租约字段 :619）** |
| 622-1370 | `impl AppRuntime`：`start_quic_transport` **758-886**（幂等；`on_datagram` **786-800** → `input::handle_input_datagram`；`on_stream` **802-871** 按优先级分发：配对流 → 拖放控制 → **日志请求** → 文件传输 → 剪贴板；先克隆布局快照再处理避免锁卡输入热路径，注释 :815-821）；`start_discovery` **888-1120**（Windows 先取**服务网络租约** :890 + 防火墙规则；UDP 线程 :968-1116：3s 通告、pair-request 处理 :1044、可见 peer 合并+warm、prune）；**`acquire_input_service_network_lease` :1123-1173**（服务独占网络端口，GUI 发现前必须拿租约）；`start_input` :1217；`start_clipboard` :1245-1294 |
| 1372-2757 | 全部 34 个 `#[tauri::command]`（`load_app_state` :1372 … `is_portable_mode` :2748； bate 新增 `fetch_client_log` **2367-2407**；`install_input_service` :2168 含非提权重启等待租约 :2256-2280；`invoke_handler` 注册表 :3512-3547） |
| 2758-3243 | 进程控制与单实例：`handle_process_control_args` :2758、三平台 `acquire_single_instance` :2805/:2834/:2871、命名事件激活/退出 :2896-3005、`launched_from_autostart` :3029、退出策略 :3011-3028 |
| 3244-3566 | `run()`：插件装配（autostart/global-shortcut :3251-3261、updater :3280、log 1MB 轮转 :3286）；**WebView 就绪前急启动运行时** :3316-3348（消除管理员重启后的不可见窗口期）；macOS 光标/显示器监视与**边缘拖拽 worker** :3371-3471；Windows drop catcher :3477-3504；`ExitRequested` 拦截改隐藏 :3550 |
| 3568-3726 | `setup_tray` :3568（菜单/左键显示）、窗口 show/hide/ensure :3639-3725 |
| 3891-4891 | Windows 输入服务管理：状态查询 :3891、**网络租约 `acquire_windows_input_service_network_lease` :4009-4089**、服务控制管道客户端（`SERVICE_CONTROL_PIPE` :4022）、`install_windows_input_service` **4151-4300**（停旧→helper 复制+ACL 加固 :4321-4436→`CreateServiceW` 自启动）、卸载 :4662、SAS 检测 :4739、提权重启 :4817 |
| 4892-5441 | `apply_windows_window_chrome` :4892、本地屏幕刷新 :5130、**本机 IP 选择链（TUN 修复）**：`local_ip_address` :5442（5s 缓存）→ `probe_local_ip_address` :5463 → `preferred_lan_ipv4` :5486（192.168>10>172.16-31 启发式）→ `point_to_point_ipv4` :5501（掩码 ≥/30 = TUN）→ `usable_discovery_ipv4` :5553（排除回环/组播/链路本地/**198.18.0.0/15**） |
| 5442-5817 | 剪贴板同步：**`run_clipboard_sync` :5917-6091**（仅剪贴板目标在远端时活动；`clipboard_retry_delay` :5912 指数退避 2s→4s→8s 封顶 60s、`STREAM_REJECTED` 不重试；回声保护 :6113-6129；`clipboard_packet_from_content` :5860 formats 信封+递增 sequence）、接收 `handle_clipboard_packet` :6163-6252（按 **origin_transport_public_key** 授权 :6334，id 漂移不影响；写回重试 10×50ms :6139） |
| 5818-6333 | `ClipboardPacket`/`ClipboardFormat` :5818/:5852 |
| 6362-7157 | 文件传输：目标解析 :6362、收集 :6455、**`send_transfer_file_bytes` :6546-6651**（256KiB 分块、2GiB 上限、100ms 进度）、包构造/发送 :6652-6736、**OLE 拖放链路** `send_ole_drag_start` :6737 / `stream_ole_drag_files` :6800 / `handle_drag_control_packet` :6910 / `mod drag_place`（ShareMouse 式落点暂存/释放）**6992-7130**、`enum DropMode`（TransfersFolder/Desktop/DragDrop/ClientLog）:6639 |
| 7158-7362 | 接收：`handle_file_transfer_packet` :7158（授权 :7598 + 目标校验 :7617；活动拖放会话喂内存流 :7389）、接收根 Downloads/MyKVM Transfers :7196、**日志拉取**：`client_log_dir` :7208、`tail_of_file` :7219（512KiB）、`write_own_log_tail` :7233、`LogRequestPacket` :7276、`handle_log_request_packet` **7288-7347**（复用文件传输信任检查，回传走 `DropMode::ClientLog`） |
| 7363-7664 | 接收分块/收尾 :7363-7597、文件名/ID 消毒 :7632-7664（非法字符替换、180 字节截断、拒绝 `.`/`..`、防覆盖） |
| 7665-8123 | peer 存在感与发现协议：`apply_peer_presence` :7737、`update_device_from_peer` :7862（客户端用对端**当前通告 id** 寻址）、`local_peer_from_layout` :8070 附近、`warm_quic_peer` :8044（空数据报预热）、`local_peer_id` :8102（主机名+主 IPv4） |
| 8124-8324 | 扫描与配对：`scan_for_peers` :8124、`probe_for_peer` :8198、`request_pairing_for_peer` :8245（pair-request，1.8s 等 pair-challenge）、**`confirm_pairing_for_peer` :8285-8324**（code+cluster_id+pair_secret 走 QUIC 加密流） |
| 8325-9105 | 发现收发与编解码（`DiscoveryPacket` :7967，MessagePack；报文 kind：announce/probe/reply/pair-request/pair-challenge/pair-confirm）、可见性规则 :8526、配对挑战 `begin_pairing_challenge` **8682-8746**（TTL 60s/5 次尝试/失败换码/证书轮换可修复重配）、`handle_pairing_stream_packet` :8581、`complete_pairing_from_confirm` **8748-8816**、prune :8818、广播地址 :8854、端口探测 :8902 |
| 9105-10756 | **60 个单元测试**：peer presence/合并（:9493）、配对挑战（:9642）、pair-confirm 正反例（:9871）、剪贴板授权/sequence/退避（:9996-10598）、文件传输（:10411）、发现目标与 TUN IP 选择（:10599） |

**任务拓扑**：lib.rs **无 tokio**（唯一异步点 `spawn_blocking` :2521，scan_lan_peers 的同步 UDP 扫描）；并发模型 = `std::thread` + `Arc<Mutex/AtomicBool>`。lib.rs 内线程：UDP 发现循环 :968-1116、剪贴板循环 :1277、输入运行时 :1217、服务租约等待 :2260、单实例事件监听 :2985、macOS 光标/可见性/显示器监视 :3143/:3163/:3191、macOS 边缘拖拽 worker :3383、OLE 拖放信号 :6885、日志请求应答 :7326。QUIC 模块内部自持 2-worker tokio runtime。前端只收 `runtime-state-changed` 一个事件（emit :1825），其余状态靠 `load_app_state`/`read_runtime_status` 轮询兜底。

#### 4.4.2 `src-tauri/src/input.rs`（bate：9478 行，60 个单元测试）

| 行号区间 | 内容 |
|---|---|
| 48-190 | 平台 FFI 导入 + **输入来源授权缓存**（`credential_send_due` :123、`authorized_input_origins` :166、缓存 TTL 5s/全量凭据刷新 2s：稳态数据报从 ~0.8KB 降到 ~0.15KB） |
| 192-345 | 核心类型：`Edge`、`InputTarget`（**随 target 携带 modifier_map** :222-224，bate）、`ActiveTarget`、`ClipboardTarget`、`InputPacket`/`InputControlPacket`、**`HeldInputs` 发送端 :309-318 + `SenderHeld` :335（bate：按住状态心跳，HELD_REFRESH 250ms）**、`ReceivedHeld` :384（`heartbeats` 门控：旧控制器不发 held 就永不误放）+ `reconcile_received_held` :427 + `start_held_input_watchdog` :455-487 |
| 518-848 | 切换方向 `SwitchDirection` :518、热键→VK 匹配 :560-650（`try_lock` 防阻塞）、`request_screen_switch` :666、回程落点记忆 `remembered_local_screen_point` :834 |
| 850-988 | 运行时入口：`start_input_runtime` **:850**、macOS 辅助功能/安全键盘检查 :951 |
| 1019-1473 | **平台捕获**：macOS :1019-1240（CGEventTap :1101）；Windows :1242-1439（**`SetWindowsHookExW(WH_MOUSE_LL)` :1295 / `WH_KEYBOARD_LL` :1311**，消息泵主循环 :1335-1424）；**钩子被 Windows 静默移除的自愈**：`windows_hooks_look_removed` :3275（GetLastInputInfo 差值 >2s）+ 主循环每 1s 检查/5s 冷却重装 :1336-1371；其他平台 no-op :1441-1459 |
| 1717-1800 | **发送 `send_packet`**（bate：附加 HeldInputs :1775；`input_send_failure_persistent` :1710——首次失败只丢事件，持续 ≥1s 才交还控制；`INPUT_SEND_FAILING_SINCE_MS` :1699） |
| 1972-2073 | **数据报接收入口**：`handle_input_datagram` :1972（GUI 用，先起 watchdog）→ `handle_input_datagram_with_sink` **:1995-2073**（headless 服务用 helper-only sink；带凭据包全量校验+缓存来源 :2022-2037，无凭据包按来源缓存放行） |
| 1867-1938 | 包上下文与重映射：`input_packet_context` :1867、`remap_event_for_target` :1896（**上下行同规则**：重映射随 target 携带而非查 live layout）、`mark_target_offline` :1939 |
| 2178-2322 | 授权：`packet_authorized` :2178、`packet_authorized_fields` :2198-2222（cluster_id + pair_secret + 公钥/设备 id 命中配对表） |
| 2426-2758 | **注入分发与 helper 路由**：`dispatch_input_command` **:2426-2470**（本地 SendInput 被拒 ERROR_ACCESS_DENIED → 经 SYSTEM helper 管道重试）；`should_route_to_windows_helper` :2526（仅安全桌面/锁屏走 SYSTEM——普通桌面 SYSTEM 注入会被 Medium 完整性窗口拒绝）；**`WindowsInputDispatcher` :2616-2758**（单写句柄缓存 + 指数退避重开 :2670 + 存活探测 PeekNamedPipe :2731） |
| 2950-3133 | 捕获上下文：`WINDOWS_CAPTURE_CONTEXT` :2950、鼠标 8ms 节流 `should_send_mouse_move` :3002、远端按钮状态 :3050-3081、macOS 释放 :3134 |
| 3275-3468 | Windows 捕获处理：`windows_mouse_proc` :3290 / `windows_keyboard_proc` :3333（远控激活时 keydown 匹配回程热键 → 直接返回本地 :3360）、按键失败交还 :3871-3895、`release_windows_remote_control` :3469 |
| 3557-3960 | **Windows 鼠标移动主处理 `handle_windows_mouse_move` :3557-3811**（crossing 判定 :3718；**拖拽边缘暂存** :3752-3773：左键按住且 `!hold_consumed` → `pending_drag_cross` + `windows_drop_catcher::arm` :3763，500ms 放弃窗；拖尾位置存 `move_pending` :3697；节流测试 :7698-7839） |
| 4230-4475 | macOS 热键匹配 :4230、`drain_switch_request_macos` :5602、**macOS 边缘拖拽**：`EdgeDragEvent` 枚举（StartOle/DropOle/CancelOle/Transfer）:4459、`set_edge_drag_sender` :4483（lib.rs 单 worker 通道）、`capture_edge_drag_files` :4578（**changeCount 快照防误判** :4624-4646）、`fire_pending_edge_drop` :4666、**受控端反向交接 `receiver_handoff_drag` :4743**（StartOle + Escape 撤销 + 补发左键抬起） |
| 4966-5458 | **跨屏交接核心算法**：`crossing_target` :4966（把进入速度带进远端坐标）、`crossing_layout_point` :5018（原生/布局双判）、`is_crossing_screen` **:5060-5112**（速度 ≥1.0、轴向占优 ≥0.5、160px 激活带、贴边 1px；拒绝屏幕中间大跳变）、active target 状态机 `update_active_remote_screen` **:5113-5165**（多屏漫游，`try_lock` 永不阻塞钩子线程）、`exited_entry_edge` :5191（仅入口边缘交还）、回环点 :5238（贴边落点 + 150ms 冷却防回弹）、**拖尾补发 `flush_pending_mouse_move` :5294-5317**、心跳 :5318、光标停靠 `send_remote_cursor_park` :5365（PARK_CORNER_CLEARANCE=64 避热角）、macOS 进入/返回序列 :5459/:5512 |
| 5602-5739 | 切换请求排空：macOS :5602、Windows `drain_switch_request_windows` :5654-5739（Enter 钉锚/跨本机多屏） |
| 6434-6668 | **键码映射表**：`mac_key_to_windows_vk` :6434、`windows_vk_to_mac_key` :6547、唯一数据源静态表 `mac_key_to_windows_vk_pairs` :6555-6668 |
| 6670-7155 | macOS 注入原语（move :6670、双击跟踪 `MacClickTracker` :6721、button :7156、scroll :7218、key :7298 含 `windows_vk_to_mac_flag` :7285）、**macOS 拖拽覆盖层 `mod macos_drag_overlay` :6906-7155**（原生 AppKit，对称于 Windows overlay） |
| 7156-7618 | macOS button/scroll/key 注入、输入源切换 `mod macos_input_source` :7415、Windows 注入委托 :7589-7606、**其他平台 no-op :7608-7618** |
| 7620-9478 | **60 个单元测试**（发送失败宽限 :7625、节流 :7698、上下行同规则重映射 :8834 等） |

#### 4.4.3 `src/App.tsx`（bate：4135 行，React 19 单文件巨石）+ `src/desktopApi.ts`（437 行）

**desktopApi.ts**：32 个 `invoke` 包装（bate 新增 `fetchClientLog` :~379 → 命令 `fetch_client_log`）；`checkForAppUpdate`/`installAppUpdate` 内 updater `check({ timeout: 20_000 })`（防清单拉取卡死，PR #22）；`BROWSER_RUNTIME` stub（:36-84）使纯浏览器 `npm run dev` 可跑布局编辑器；`relaunch`/updater 按需动态 import。

**App.tsx**：

| 行号区间 | 内容 |
|---|---|
| 1-181 | 常量与类型：设备色板、`PLATFORM_LABELS`、**`WORKSPACE_TABS = [layout, devices, settings]` :110**、`CLIENT_TABS = ["settings"]` :118（客户端角色只显示设置页）、`DragState`、`NativeFileDragPayload` :163 |
| **App() 182-~3426** | 单组件承载全部 UI（~3200 行）： |
| — 182-300 | 状态声明（`activeTab`、snapshot、`fileDragTargetId` :198 等） |
| — ~276-295 | 原生文件拖放窗口监听（dragover/drop，`fileTransferTargetIdAtPosition` :3904 决定 dropEffect） |
| — 375-405 | 订阅 `runtime-state-changed` 事件 :381（即时同步）+ 注释明确"轮询仍是兜底" |
| — ~405-640 | 启动加载 effect：`loadAppState`、便携模式检测、自启状态、主题 media query 跟踪 |
| — ~660-740 | 运行时轮询兜底（runtime+layout 一起刷新）；客户端 Tab 过滤 :718/:723 |
| — ~800-960 | 主题/窗口 chrome 同步、启动时更新检查 :841、性能采样轮询 :883、诊断加载 :913/:938 |
| — ~1000-1010 | **bate 新增：对端日志拉取**（`fetchClientLog(deviceId)` :1007，无目标/已发送提示 :1000/:1009，设置页按钮 :3251） |
| — ~1060-1110 | **bate 新增：Tauri onDragDrop 事件处理**（`NativeFileDragPayload` :1071、拖放目标命中 :1079/:1089、拖拽目标高亮 :2377） |
| — ~1150-2000 | 配对弹窗、重新配对、更新安装（:1904 手动检查、:1947 安装+relaunch） |
| — 2174-2210 | **角色未设置 → Onboarding 页**（选择服务端/客户端） |
| — 2210-3426 | 主界面 shell + 三个 Tab 的 JSX（布局编辑器/设备列表/设置） |
| 3428-4135 | 纯函数与图标：`fallbackScreen` :3428、主题解析 :3442/:3454、格式化（百分比/内存/速率）:3465-3504、热键标签渲染 :3518-3579、缩放 :3580、图标组件 :3594-3734、`screenStatusKind` :3735、peer→设备映射 `applyPeerPresence` :3754 / `upsertPeerDevice` :3797、文件传输辅助（`fileTransferTargetIdAtPosition` :3904 等） |

#### 4.4.4 拖拽传输族（bate 新增）：`windows_drag.rs`（1044）/ `windows_drag_overlay.rs`（344）/ `windows_drop_catcher.rs`（546）

三文件均不经过 Tauri 窗口/前端事件，是纯原生 Win32 实现（前端唯一相关 UI 是 overlay 本身；拖拽字节被 `windows_drag::session_wants` 拦截喂内存，不走前端进度条）：

| 文件 | 角色 | 关键函数（行号） |
|---|---|---|
| `windows_drag.rs` | **受控端拖拽发起方**：收到 "start"+文件字节后跑真 `DoDragDrop`，以 `IDataObject`（FileGroupDescriptorW + FileContents/IStream）暴露虚拟文件，任何 Explorer/IM/邮件 drop target 可正常接收 | 会话槽 `start_drag_session` :314（60s 空闲看门狗；残留会话"结束旧的"而非拒绝 :346）；字节喂入 `session_wants`/`feed_chunk`/`finish_file` :257/:261/:273；`signal_drop` :286 / `cancel_session` :300（合成左键抬起唤醒 `QueryContinueDrag`）；**核心 workaround `drag_from_window_under_cursor` :494-561**（DoDragDrop 需光标下有窗口：注册 "MyKVMDragSource" 类、光标处建 8×8 NOACTIVATE 弹窗、注入左键按下泵 WM_LBUTTONDOWN）；`FileReadStream`（IStream，`read_at` 阻塞读网络流、30s 超时）:598-759；`DragDataObject`（GetAsyncMode 恒 true 强制目标异步提取）:761-947；`DragDropSource`（IDropSource，注入模式看 session 状态）:985-1020 |
| `windows_drag_overlay.rs` | 跟随光标的文件图标+进度覆盖层（原生分层窗口，15ms 定时器，进度差 ≥0.5% 才重绘；drop 后停靠任务栏右上直到传完） | `start` :64、`run` :72-143、`paint` :191-227（32×32 shell 图标 + 5px 进度条）、`file_icon_pixels` :229 |
| `windows_drop_catcher.rs` | **边缘 OLE 接收窗口（双向）**：控制端向贴边拖文件时，钩子不跨界，在边缘布 36×220 隐形 `IDropTarget` 窗口接住 `CF_HDROP` → sink → 文件传输（DropMode::DragDrop），注入 Escape+左键抬起结束源拖拽，置 HANDOFF 让钩子跨界；受控端响应 "pull" 反向交还拖拽 | `init`（注册 DropSink）:132、`arm` :143（坐标经 WM_ARM 投递到窗口线程，跨线程 SetWindowPos 不可靠）、`hold_consumed`/`reset_hold` :91/:96（**每按住一次只捕获一次**，防边缘往返重复拷贝）、`take_handoff` :115（钩子每 move 消费：非 None 立即跨界）、`handoff_to_controller` :171-197（reset_hold + 置位 + arm 光标下 + `nudge_cursor` 1px 强制 OLE 重新命中测试）、`DragEnter` :320-379（读 HDROP、`inject_end_drag` :472） |

**QUIC 传输编排**（lib.rs）：控制面 `DragControlPacket`（start/drop/cancel/pull，协议 `mykvm.drag-control.v1`）走**可靠流且 start 先 ACK**，单 worker 串行化保证 drop/cancel 不超车（lib.rs :3376-3381、`send_ole_drag_start` :6737）；数据面文件字节走普通 `FileTransferPacket` 独立线程流式发送，受控端 `handle_file_transfer_packet`（lib.rs :7390-7396）把属于拖拽会话的 chunk 重定向到内存 FileBuffer。与 `send_files_to_device` 同一引擎，仅 `DropMode` 不同（TransfersFolder/Desktop/DragDrop/ClientLog）。

## 5. 前后端接口清单

### 5.1 Tauri 命令（desktopApi.ts 梳理，实现在 lib.rs）

| 命令 | 签名/返回 | 用途 |
|---|---|---|
| `load_app_state` | → `AppStateSnapshot{layout, runtime}` | 启动加载布局+运行时快照 |
| `save_layout` | `{layout}` → snapshot | 保存布局/设置 |
| `reset_pairing` | → snapshot | 清空配对 |
| `start_runtime` / `stop_runtime` / `read_runtime_status` | → `RuntimeStatus` | 运行时生命周期 |
| `scan_lan_peers` | → `DiscoveryStatus` | 手动刷新发现 |
| `probe_lan_peer` | `{host}` → LanPeer | 直连探测 |
| `request_lan_pairing` / `confirm_lan_pairing` / `dismiss_pairing_request` | 配对流程 | 配对码确认/拒绝 |
| `write_clipboard_text` | `{text}` | 手写剪贴板 |
| `read_performance_sample` | → `PerformanceSample` | 性能采样 |
| `read_diagnostic_info` | → `DiagnosticInfo` | 诊断（日志目录/网络/防火墙提示） |
| `open_log_directory` | | 打开日志目录 |
| `is_autostart_enabled` / `set_autostart` | 开机自启（tauri-plugin-autostart） | |
| `restart_as_admin` | Windows UAC 重启 | 控制提权窗口（UIPI 限制） |
| `read_input_service_status` / `install_input_service` / `uninstall_input_service` | → `InputServiceStatus` | Windows 锁屏服务管理 |
| `send_files_to_device` | `{deviceId, paths}` → `FileTransferSummary` | 文件发送 |
| `fetch_client_log`（bate 新增） | `{deviceId}` → string | 从对端机器拉取运行日志（Settings → Diagnostics，commit 6744b32） |
| `is_portable_mode` | 便携模式判定 | |
| `sync_window_chrome` / `minimize_main_window` / `hide_main_window` / `toggle_maximize_main_window` / `start_window_drag` | 自绘标题栏窗口控制 | |
| `open_repository_url` / `open_releases_url` | | |
| `set_app_upgrading` | `{enabled}` | 更新期间防运行时干扰 |

前端动态 import 的插件 API：`check/downloadAndInstall`（updater）、`relaunch`（process）。

### 5.2 Tauri 事件

前后端事件只有**一个**：`runtime-state-changed`（lib.rs:93，emit 于 lib.rs:1825，App.tsx:381 订阅），携带完整 `RuntimeStatus` 快照；其余状态变化靠前端轮询 `read_runtime_status`/`load_app_state` 兜底（App.tsx 注释明示）。二次开发若需要更低延迟的 UI 推送（如逐设备事件），需要新增 emit+listen 对。

### 5.3 capabilities（src-tauri/capabilities/default.json）

仅 `core:default`、`updater:default`、`process:default`——窗口能力最小化，自绘窗口控制走自定义命令实现。

## 6. 平台差异速查

| 维度 | Windows | macOS | Linux |
|---|---|---|---|
| 输入捕获 | 低级钩子 `WH_MOUSE_LL`/`WH_KEYBOARD_LL`（input.rs:1295/1311）+ 消息泵；钩子被系统移除可自愈重装（:3275/:1336） | CGEventTap（需辅助功能授权）+ 原始手势 tap | **bate 仍为 no-op**（input.rs:1441-1459 返回 unsupported，属原型阶段，README 自述 Linux 为蓝图） |
| 输入注入 | SendInput（虚拟屏归一化；UIPI 限制→被拒时经 SYSTEM helper 重试，windows_input.rs + input.rs:2426） | CGEventPost（含双击跟踪、修饰键映射表） | **bate 仍为空函数**（input.rs:7608-7618） |
| 剪贴板 | arboard + CF_DIB/PNG 原生回退 | arboard + pbcopy/pbpaste（强制 UTF-8） | wl-copy/wl-paste → xclip |
| 锁屏控制 | MyKVMInputService 服务 + 会话 worker + 管道 + sas.dll SendSAS | 不适用 | 不适用 |
| 打包 | NSIS（安装钩子：停服务/关实例/改名被锁 helper/防火墙 UDP-In 规则） | app+dmg，签名+可选公证，min 12.0 | AppImage/deb/rpm |
| 前端 Platform 类型 | `'windows'` | `'macos'` | `'unknown'`（types.ts:1 —— Linux 支持存在但前端类型未单列，二次开发注意） |

## 7. 安全模型

1. **传输身份**：首启 `rcgen` 生成自签证书（CN=mykvm.local,localhost），DER 持久化于应用数据目录；`public_key` = 证书 DER base64，随发现通告广播。
2. **证书固定而非 WebPKI**：`PinnedCertVerifier`（quic_transport.rs:606-640）逐字节比对对端证书与通告公钥；握手签名仍用 ring 校验（证明对方持有私钥）。修复了历史跨平台握手失败（CHANGELOG Unreleased：`invalid peer certificate: BadSignature`）。
3. **协议版本闸门**：`PROTOCOL_VERSION` 不一致即拒绝（`client_config()`，quic_transport.rs:661）。
4. **配对授权**：clusterId + 配对码 + pairedControllers 白名单；`pairSecret` 存于布局状态。
5. **Windows 管道安全**：SDDL `D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})` 仅 SYSTEM 与会话用户可连，`PIPE_REJECT_REMOTE_CLIENTS`。
6. **信任边界提示**：所有流量限局域网；发现广播与 QUIC 端口对局域网开放（NSIS 自动加防火墙 UDP-In 规则）。**任何知道你 IP 的局域网主机都能发起 QUIC 连接与配对请求**——安全完全依赖证书固定 + 配对白名单 + 配对码人肉确认。二次开发若扩展协议，务必延续"先固定证书、再验授权"的次序。
7. **更新链**：updater 用 minisign 公钥（tauri.conf.json:53）验证 latest.json 签名；CI 校验签名私钥与配置公钥的 key id 一致（release.yml:268-324）。

## 8. 构建、开发与发布流程

### 8.1 本地开发

```bash
# 前置：Node 22+、Rust stable（Windows 还需 MSVC Build Tools + WebView2）
scripts/check-dev-env.ps1   # 一键体检（含 Smart App Control / CodeIntegrity 排查）
npm install
npm run tauri:dev           # scripts/run-tauri-dev.ps1 会把 CARGO_TARGET_DIR 挪到 %LOCALAPPDATA%\CargoTarget\mykvm
npm run dev                 # 纯浏览器 UI 开发（BROWSER_RUNTIME stub）
npm run lint && npm run build
cargo check --manifest-path src-tauri/Cargo.toml
```

### 8.1.1 测试与打包速查（二次开发常用）

```bash
# —— 测试 ——
npm run test                       # 前端单元测试（vitest，src/*.test.ts）
cargo test --manifest-path src-tauri/Cargo.toml --workspace        # Rust 全量（lib + input-helper）
cargo test --manifest-path src-tauri/Cargo.toml --lib clipboard    # 只跑某个关键字相关的测试
# —— 打包（Windows）——
npm run tauri:build                # 只出可执行（不打安装包）：src-tauri/target/release/mykvm.exe
npm run tauri:bundle               # 完整打包：自动先构建 input-helper 侧车（build-tauri-assets.mjs），
                                   #   产出 NSIS 安装器 src-tauri/target/release/bundle/nsis/mykvm_*.exe
# —— 打包（其他平台）——
npm run tauri:build:mac-arm        # macOS aarch64 app+dmg（不签名）
scripts/install-mac-app.sh         # macOS 本地安装 + 自签
```

注意：仓库内版本号恒为 `0.1.0`，`tauri:bundle` 本地打的包不会改版本号；正式带版本号的安装包由 CI `release.yml` 在 push 到 `main`/`bate` 时自动产出（详见 8.4）。

### 8.2 Windows 侧车（input-helper）构建链

`tauri.conf.json` 的 `beforeBuildCommand = node scripts/build-tauri-assets.mjs`：
`npm run build`（网页）→ Windows 上额外 `cargo build -p mykvm-input-helper --release --target x86_64-pc-windows-msvc` → 拷贝到 `src-tauri/binaries/mykvm-input-helper-{target}.exe`。
`build.rs` 在缺侧车时写一个**空占位文件**防止 tauri-build 失败（本地 cargo check 不会被卡）。

### 8.3 CI（ci.yml）

ubuntu-22.04 单 job：Node 22 → Linux 桌面依赖（libwebkit2gtk-4.1-dev 等）→ `npm ci` → build → lint → `cargo check`。

### 8.4 发布（release.yml，三阶段）

1. **prepare**（ubuntu）：按最新 `v*.*.*` tag + 提交主题计算版本——`feat:`→minor、`fix:`→patch、bate 分支→`x.y.z-beta.{RUN_NUMBER}`；产出 tag + draft release + 发布说明（CHANGELOG `[Unreleased]` 优先）。
2. **build**（矩阵 macos-14 universal / windows / ubuntu）：校验 updater 签名 key id、临时把版本号写进 package.json/tauri.conf.json/Cargo.toml（beta 频道还改 updater endpoint 指到 `releases/download/beta/latest.json`）、macOS 钥匙串签名 + 可选公证，`tauri-action` 构建上传，**保持 draft**。
3. **assemble-latest-json**（ubuntu）：下载全部资产、汇总各平台签名产物生成 `latest.json`（windows 资产统一改名为固定名 `mykvm-windows-x64-setup.exe`）、上传后**才把 draft 转正**——防止发布中途 in-app updater 拿到 404 的 latest。

## 9. 二次开发切入点与注意事项

> **后续功能/优化的排期与分步实施计划见 [UPGRADE_ROADMAP.zh-CN.md](./UPGRADE_ROADMAP.zh-CN.md)**（待实现功能、待优化项、四阶段路线 + 工程债专项，含验收标准与风险清单）。

### 9.1 常见修改场景 → 代码路径

| 想做的事 | 主要改哪里 |
|---|---|
| 新增一种输入事件（如触摸/笔） | `shared_input.rs` InputEvent/InputCommand → `input.rs` 捕获+注入 → 三平台注入原语；**记得升 PROTOCOL_VERSION 并兼容旧帧**（worker 已有"未知命令跳过"容忍；held 心跳有 `heartbeats` 门控兼容旧端） |
| 新增设置项 | `types.ts` LayoutState → `defaultLayout.ts` → App.tsx 设置面板 → lib.rs save_layout/持久化 → i18n.ts 双语文案 |
| 新增 Tauri 命令 | lib.rs `#[tauri::command]`（注册表 :3512-3547）→ desktopApi.ts 包装（记得 isTauri stub） |
| 改发现/通告报文 | lib.rs 发现段 :888-1120 与编解码 :7967+；注意 `LanPeer` 前后端类型同步（runtime.ts） |
| 改传输行为 | quic_transport.rs（端口扫描/健康度/流上限常量在文件头部） |
| 剪贴板新格式（如文件） | clipboard.rs 内容抽象 + lib.rs `run_clipboard_sync` :5917 |
| 文件拖拽/传输 | 拖拽族见 4.4.4；`DropMode`（lib.rs :6639）决定落点；控制消息协议 `mykvm.drag-control.v1` |
| Windows 服务行为 | input-helper/main.rs + headless_client.rs（见 3.7）；GUI↔服务协议 = `SERVICE_CONTROL_PIPE` |
| 新平台快捷键 | hotkeyInput.ts 规范化 + lib.rs 全局快捷键注册 |

### 9.2 技术债与风险提示

1. **三大巨石文件**：lib.rs 10756 行、input.rs 9478 行、App.tsx 4135 行（bate）——单文件承载全部命令/全部输入逻辑/全部 UI。二次开发建议：新增逻辑放新模块（上游 bate 也是这么做的：windows_drag 三文件、headless_client.rs），避免继续膨胀；修改前先按 4.4 节地图定位。
2. **注释即文档**：这个代码库的 Rust 注释密度高、质量高，大量"为什么"（历史 bug 根因）写在注释里。**改动前通读目标函数附近注释**，很多坑已踩过（SO_REUSEADDR 端口漂移、JOURNALPLAYBACK 权限、桌面同名不同对象、命令循环禁 await 等）。
3. **前端 Platform 类型无 linux**（types.ts:1），Linux 分支用 `'unknown'` 兜底；图标/文案按平台分支时注意。
4. **CHANGELOG 滞后**于 tag；若沿用其发布说明机制，发版前记得维护 `[Unreleased]`。
5. **版本号在仓库内恒为 0.1.0**：不要手工改它，发布由 CI 计算；本地调试若依赖版本号需注意。
6. **单实例与进程控制参数**（main.rs）：`--mykvm-quit-existing` 被 NSIS 钩子依赖，改进程名/标识需同步 nsis-hooks.nsh。
7. **安全编码约束**（本仓库贡献守则）：任何服务端 URL 请求仅允许 http/https；必须校验目标 host；拒绝 localhost/回环/私有网段/链路本地/保留地址——防止把内网服务暴露为 SSRF 跳板。若给 updater/诊断/日志回传等功能加网络请求，须遵守。
8. **测试现状**：Rust 侧 quic_transport/clipboard/shared_input/performance 有单元测试；前端无测试框架。cargo test 可跑（Linux/Win 全平台无 UI 依赖）。

### 9.3 bate 分支与上游协作

- 上游模式：日常开发在 `bate`，阶段性 `Merge bate into main`；main push 自动触发 stable 发布，bate push 触发 beta 发布。
- 二次开发建议：基于 `bate` 起步（包含全部近期修复），或至少先读完第 10 节差异再决定基线；保持 `origin` 远端可用 `git fetch upstream` 同步。

## 10. bate（基线）相对 main 的差异

工作区当前检出的 bate 分支领先 main **68 个提交，27+ 文件 +8623/-849 行**。已并入本文档各节；此处保留差异主题清单供回溯（main 口径的历史行号地图如需可 `git switch main` 对照）：

- **文件拖拽传输（最大新特性）**：新增 `windows_drag.rs`（OLE 数据对象/拖拽源，Cargo为此引入 `windows`/`windows-core` COM 依赖）、`windows_drag_overlay.rs`、`windows_drop_catcher.rs`；拖拽控制消息按序发送（39f6e5a）、拖放落点修复（78cf6e6）。
- **Windows 服务化接收（第二大变化）**：input-helper +548 行——服务内跑 `headless_client.rs` 只收运行时（登录前/锁屏即可被控）、新增服务控制管道 `\\.\pipe\mykvm-service-control` 与 GUI 接管协议、服务参数解析。
- **输入可靠性批次**：单个 send 失败不断流（140e4ac）、丢 "up" 数据报的键/钮自动释放（758a979）、上下行同规则修饰键重映射（60dfbec）、Windows 拒绝的输入经 SYSTEM helper 重试（d28a72b）、Windows 擅自移除钩子的重装与回程热键（3641d2f）、拖尾位置补发（cff5929）。
- **macOS 体验**：过屏时立刻藏光标（7786ea5）、隐藏窗口时退出 Dock/Cmd+Tab（1cdb489）、单实例锁改用 `File::try_lock`（rust-version 升至 1.89）。
- **网络/身份**：TUN 代理下设备身份保持稳定（676f5d3）、剪贴板对端无响应退避（d6abb6e）、quic_transport `start_datagram_only`/`forget_connection`/`start_inner` 重构。
- **诊断**：Settings → Diagnostics 一键拉取对端日志（6744b32，命令 `fetch_client_log`）；配对码在已配对客户端重复配对时也显示（e58baaa）。
- **其他**：updater 清单拉取 20s 超时（PR #22）；CHANGELOG 补录（fe938ee）。

## 10.5 二次开发第一轮实施记录（2026-10-02，工作区未提交）
在 bate 基线上完成的四项优化 + 对等双向控制（Peer 模式）。改动文件：`src-tauri/Cargo.toml`、`src-tauri/src/{clipboard,lib,input,windows_drag,headless_client}.rs`、`src/{types,i18n,App}.tsx|ts`、`package.json`、`vitest.config.ts`（新）、`src/{layout,hotkeyInput}.test.ts`（新）、两个 README、CHANGELOG、两个 workflow。

### A. 四项优化

1. **剪贴板图片 PNG 上线**：`ClipboardImage` 增加 `png_base64` 字段（serde default，旧包兼容）；新线格式 kind `"imagePng"`（`clipboard_content_from_format` 双 kind 解析，lib.rs）；发送端 `clipboard_packet_from_content` 先 `clipboard::encode_png`、仅在编码成功且小于原始 RGBA 时采用，否则回退 legacy `"imageRgba"`；`decode_png`（clipboard.rs）先读 PNG 头校验尺寸并按 32MB RGBA 预算拒绝解压炸弹；`ClipboardContent::signature` 改为对 rgba+png 拼接哈希——两表示同像素同签名，回声抑制不受影响。测试：PNG 往返/维度不符拒绝/预算上限/4K legacy 预算（改用非法 base64 保持最坏情况断言）。
2. **文档同步**：README×2 修正"无配对 PIN"过时描述（配对码已实现）、补文件传输与拖拽矩阵；CHANGELOG `[Unreleased]` 记录全部用户可见变更。
3. **前端测试基建**：vitest + `vitest.config.ts`（node 环境）；`npm run test`；首批 `layout.test.ts`（重叠/吸附/展平/边界）与 `hotkeyInput.test.ts`（规范化/meta 标签/格式化）24 用例；ci.yml 与 release.yml 各加 Unit tests 步骤。
4. **文件传输增强**：`send_file_transfer_packet` 每包重试 ≤3 次（250ms 退避）——重发安全的前提是接收端容忍重复分块；`FileTransferPacket.file_sha256`（serde default）finish 包携带全文件 SHA-256（ring），接收端流式哈希比对，不一致删 .part 拒绝落地；乱序/重复容忍：`append_incoming_file_transfer_chunk` 对"范围已被前缀覆盖"的重发幂等成功，OLE 拖拽内存流 `FileBuffer::append_at` 同样按 offset 去重（windows_drag.rs）；接收并发上限 4（`MAX_CONCURRENT_INCOMING_TRANSFERS`）。测试：SHA 校验/重复分块/并发上限/缓冲去重。

### B. Peer 模式设计（双向控制）

**语义**：`machineRole` 新增 `'peer'`，`inputMode` 新增 `'both'`（捕获+接收同开）。server/client 单向路径行为完全不变。

| 改动面 | 位置 | 内容 |
|---|---|---|
| 角色归一 | lib.rs `normalize_machine_role`/`normalize_input_mode`/`pairing_required` | peer 合法角色；`pairing_required` 扩为 client‖peer 且 controllers 空 |
| 配对对称 | lib.rs `complete_pairing_from_confirm`/`confirm_lan_pairing`/`begin_pairing_challenge`/`pair_challenge_usable_for_local_peer` | 接收端允许 peer 且 `input_mode="both"`；**两端互写**：接收端 `append_paired_controller`（幂等，按 id/公钥去重）+ `upsert_paired_peer_device`（插入对端设备条目含 screens——反向切换目标的来源）；发起端 confirm 成功后同样互写并落盘；挑战/发起门控放宽到 peer |
| 授权对称 | lib.rs 五处（clipboard :6449、file、log、drag-control、discovery reply）+ `role_receives_from_peers` 帮助函数 | "仅 client 校验来源"统一改为"client‖peer 严格校验（origin ∈ pairedControllers）"；server 保持 cluster+secret 历史行为；输入包授权本就角色无关（controllers 键匹配） |
| 热键/服务 | `screen_switch_shortcuts_for_layout`/`runtime_toggle_shortcut_for_layout`/`sync_global_shortcuts`（lib.rs）+ input.rs :570 + headless_client.rs `headless_receive_enabled` | 注册与热键门控放开到 peer；headless 锁屏接收条件加 peer（`receive|both` 均可） |
| 冲突仲裁 | input.rs `LAST_CONTROLLED_MS`/`mark_controlled_activity`/`controlled_active()`（1.5s 窗口） | 收到授权输入数据报即刷新时间戳；Windows 双钩子回调：注入事件（`LLMHF_INJECTED`/`LLKHF_INJECTED`）直接放行不回捕，`controlled_active()` 期间物理输入保持本地并强制 `release_windows_remote_control`（后到方向接管）；捕获循环亦做接管检查；macOS 对称：注入事件统一打 `MACOS_SELF_EVENT_MARKER`（`tag_injected_macos_event`，此前仅本地消费事件有标记），tap 回调与 run loop 门控 + `release_in_flight_local_control_macos` |
| UI/i18n | App.tsx `setMachineRole`/Onboarding 三选一/角色三态切换器/Tab 与页面门控（`machineRole !== "client"` 显示布局与设备页）/配对弹窗/日志拉取按钮/`upsertPeerDevice` 保留 both + i18n.ts 双语 `roles.peer`、`onboarding.peer*`、文案 | peer 显示全部 Tab；`setMachineRole("peer")` 写 `inputMode:"both"` 并自动启动运行时 |

**已知边界**：MVP 按 2 台机器设计；peer 需两端同版本（旧端不识别 peer 角色/`both` 模式，退化为单向互通）；`SERVER_HELP` 场景未验证多 peer 混合 server 的会话互斥（依赖 controllers 数量约束未强制）。

**真机验证清单**（Windows↔Windows 或 Win↔Mac，两台实机）：
1. 两端选 Peer → 一端设备页添加对端 IP → 配对码确认 → 两端各自出现对方设备与屏幕
2. A 滑到 B 屏幕控制 → 键盘/滚轮/剪贴板（文本+截图）→ 回程热键返回
3. 不返回直接在 B 上操作物理键鼠 → B 立即接管、A 会话收回
4. B 反向滑到 A 屏幕控制 → 全链路反向
5. 文件拖拽跨屏（若 Win→Win）与手动发送 → Downloads/MyKVM Transfers
6. 锁屏 B（或登出）→ A 仍可控制 B（headless 服务接管）→ 登录后 GUI 接管
7. 任一端切回 server/client → 行为退回单向，无残留状态

### 追加功能：四角防误穿越（Corner Guard，同日实现）

**需求**：鼠标移到屏幕四角（如右上角点窗口关闭按钮）时误触发跨机切换。

- **语义**：`LayoutState` 新增 `cornerGuard: bool`（默认 **true**）+ `cornerGuardSize: u32`（默认 32，clamp 0..=200，0=关闭）；设置页开关 + 数字输入；`runtime_relevant_layout_changed` 包含两字段 → 改设置自动重启输入运行时。
- **实现**：`point_in_corner_zone(screen, edge, x, y, size)`（input.rs）——沿穿越 Edge 判垂直向贴角（穿越 Left/Right 看 y 距上下角，Top/Bottom 看 x 距左右角）；`is_crossing_screen` 开头命中即拒绝。**显式参数传递**（`crossing_target` ← 两平台调用点传 `context.corner_guard_size`，capture 启动时 `effective_corner_guard_size()` 从 layout 快照），不用全局静态——避免 cargo test 并行污染。native 与 layout 两次坐标尝试都过同一门控，Windows/macOS 双平台自动生效。
- **不触碰的路径**：远端漫游（`update_active_remote_screen` 用 `point_in_screen` 独立判定）、回程（`exited_entry_edge`）、热键切换（`screen_switch_request`）——防把光标困在远端；角落区内沿边滑动移出后正常穿越。
- **测试**：`corner_guard_blocks_right_edge_crossing_at_top_and_bottom_corners`、`corner_guard_blocks_top_edge_crossing_at_left_and_right_corners`、`corner_guard_disabled_allows_corner_crossing`、`effective_corner_guard_size_clamps_and_honors_the_toggle`（input.rs；既有 7 处穿越测试调用点补 `0` 参数保持原语义）。

### 第三轮：自动互配 + WoL + 锁屏 + 签名 + 批量化 + 上限（2026-10-02）

| 项 | 设计与落点 |
|---|---|
| **P0 发现即自动互配** | `LayoutState.autoPairing`（默认 true，前端开关）。`auto_pair_discovered_peers()`（lib.rs，调用点：发现循环 merge 块 / scan_lan_peers / probe_lan_peer）：policy 开 && peer 带公钥 && 不在白名单 → `append_paired_controller` + `upsert_paired_peer_device` + 落盘；cluster 收敛：本机 controllers 空 && 对端有 cluster → 采纳（双方均未配对取字典序较小者）；`advertised_cluster_id` 在 policy 开时未配对也广播。pair-request 分支 policy on 不弹窗、直接互配回 challenge；`complete_pairing_from_confirm` 跳码；对端 policy off 仍走码流程（原代码保留）。 |
| **授权谓词统一** | `cluster 匹配 && (origin ∈ 白名单 ‖ pair_secret 匹配)`：clipboard（:6466 重写）、file（`file_transfer_packet_authorized`）、log/drag 两处 reject 块、input.rs `packet_authorized_fields`——白名单（证书公钥）为主锚，secret 为码流程时代旧对端的后备；`server` 保持 cluster+secret；headless 条件去 secret。 |
| **P1.1 WoL** | 依赖 `mac_address = "1"`；`local_mac_address()`（OnceLock 缓存，跳多播/全零）→ `LanPeer.mac`/`Device.mac` 持久化（离线保留）；`build_magic_packet`（6×FF+16×MAC，lib.rs）+ `wake_device` 命令（广播 255.255.255.255 端口 9/7/0）；设备页「唤醒」按钮（无 MAC 禁用）。 |
| **P1.2 离开锁屏** | `LayoutState.lockOnLeave`（默认 **false**）；`maybe_lock_on_leave()` 仅在两平台「本机主动穿越成功」分支调用（Windows `LockWorkStation`@`Win32_System_Shutdown`；macOS 本地注入 Ctrl+Cmd+Q 六事件序列打 MYKV 自标记）；peer 被控/回程/漫游不触发。 |
| **P1.3 发现签名** | 新模块 `discovery_signing.rs`：HMAC-SHA256，密钥文件 `discovery-signing.key` 持久化于 QUIC 身份同目录（KEY_IO_LOCK 防并行竞态重生成），签名绑定 (kind, peer.id, transport_public_key)；`DiscoveryPacket.signature`（serde default 空兼容旧端）；`decode_discovery_packet` 验签失败丢弃；run() setup 接线 identity dir。测试断言绑定性与篡改拒绝（IDENTITY_DIR 为进程级 OnceLock，测试共享单一目录）。 |
| **P2.1 键盘批量化** | `InputPacket.events: Vec<InputEvent>`（wire 尾追加，serde default 兼容旧端；`InputPacketRef` 同步镜像 + `input_event_slice_is_empty` 谓词保持字节一致）；接收端 `handle_input_datagram_with_sink` 一次处理 event+events（保序，held/心跳只对首事件）；发送端 `KEY_BATCH` 队列（cap 16 / 12ms 窗口）+ `queue_key_event`/`flush_key_batch`/`send_packet_batched`，两平台捕获循环每 tick flush，返回本地即排空。鼠标 move 不批（已有 8ms 节流）。 |
| **P2.5 控制器上限** | `normalize_paired_controllers` 上限 `MAX_PAIRED_CONTROLLERS=8`（按 pairedAtMs 保留最新）；auto_pair 达限裁剪并 warn。 |

**待续（下一轮，设计已定稿）**：传输管理器面板+取消（前端 fileTransfers 状态现成，后端加 per-transfer cancel AtomicBool）、剪贴板历史+快捷键（ring buffer + global-shortcut 扩展）、文件断点续传（.part 保留 + resume_from + ACK 扩展 + SHA 续算）；以及大项：远程屏幕预览、Linux 支持、ARM64、多语言包。

### 第四轮：真机测试回归修复（2026-10-02 晚，A 机日志取证驱动）

两台真机实测暴露的问题，根因由 A 机日志坐实（`%LOCALAPPDATA%\com.xzhpl.mykvm\logs\mykvm.log`，UTC 时间戳）：

| 问题 | 根因（日志证据） | 修复 |
|---|---|---|
| **钩子误判风暴**（被远控时每 2-5 秒掉控制会话，打断 Alt+Q 组合键、清剪贴板目标；今天 18 次 WARN，旧版 0 次） | 键盘钩子的 injected/controlled_active 早退分支**跳过了 `LAST_HOOK_EVENT_TICK` 更新**——被远端纯键盘控制时注入键持续刷新 `GetLastInputInfo` 而 tick 冻结，2 秒后 `windows_hooks_look_removed` 误判 → 释放 + unhook/reinstall 循环 | 键盘钩子重构为与鼠标钩子一致：`KBDLLHOOKSTRUCT` 读取 + tick store 提到所有早退分支之前（任何到达钩子的事件都证明钩子链活着） |
| **三功能最后实测失败** | B 机 16:40 后零广播（不在网络）；19:03 配对无挑战、QUIC ack 超时 | 部署清单（见下） |
| **B 机新便携 exe"双击没反应"** | 单实例互斥体 `Local\MyKVM_SingleInstance` 被旧实例持有 → 新二进制静默退出 | `acquire_single_instance` 失败路径加明确 warn 日志；部署清单要求先从托盘退出旧实例 |
| **双广播源 / input_ready 抖动** | 旧 `MyKVMInputService`（旧目录旧构建）与新 GUI 并存——服务 headless 端点 datagram-only，截胡配对/剪贴板/文件流 | 部署清单：卸载旧服务或设置页重装（安装流程自动停旧服务并替换 helper） |
| **WoL 用虚拟网卡 MAC**（日志 mac=0a0027000010，Hyper-V/VPN 类地址点不亮真机） | `mac_address` crate 返回第一个适配器 | Windows 版 `local_mac_address` 改用 `GetAdaptersAddresses`（features 增 `Win32_NetworkManagement_IpHelper/Ndis` + `Win32_Networking_WinSock`）：**选取拥有发现 IPv4 的适配器**的物理地址，回退 crate 结果 |

**文件剪贴板与拖拽 Win 受端落地**（同日早前）：
- 文件剪贴板（Ctrl+C 文件 → 跨机 Ctrl+V）：`ClipboardContent::Files` + wire kind `"fileList"`（`ClipboardFormat.files: Vec<ClipboardFileEntry{name, data_base64}>`，serde default 兼容旧端）；读取走 arboard `file_list`（含文件夹/超限 24MB 整体跳过）；接收端写 `下载\MyKVM Transfers\Clipboard\`（静态 FILES_DIR 由 run() setup 注入）+ arboard `Set::file_list` 回填本机剪贴板；回显抑制 = `FILES_LAST_RECEIVED_SIG`（name+size 哈希，轮询零重读）。**前提：复制时本机剪贴板目标指向对端（正在互控/最近互控）——没有活跃目标不轮询发送（设计如此）。**
- 拖拽 Win→Win：控制端边缘拖拽（catcher 抓取 → DragDrop 模式传输）本来就有，但 Windows 受端无 OLE 会话消费时文件永久落在隐藏目录 `.mykvm-drag-staging\`（staged 处理原为 macOS-only）；修复：Windows 上 finish 时自动移到可见的 `MyKVM Transfers\` 根（`unique_transfer_destination` 防覆盖）。

**便携部署清单（B 机每次换包必读）**：
1. 托盘退出旧 MyKVM 实例（否则单实例互斥体让新 exe 静默退出；日志有 warn）
2. `sc stop MyKVMInputService && sc delete MyKVMInputService`（或装好后在新版设置页重装输入服务）——旧目录的 helper/服务必须清理
3. 覆盖 mykvm.exe + mykvm-input-helper.exe（或直接解压新 zip）
4. 启动后确认设备页对端 online 且 inputReady（未 ready 时文件目标/拖拽不可用）
5. 文件剪贴板与拖拽只在"控制会话存在"时工作：先从控制端滑到被控机一次

### 第五轮：`.setup()` 覆盖事故修复 + 全屏暂停穿越（2026-10-03，日志取证驱动）

| 问题 | 根因（日志坐实） | 修复 |
|---|---|---|
| **文件剪贴板接收 22 次 "landing directory is not configured"**（B 发文件过来 A 全部写入失败） | **Builder 上注册了第二个 `.setup()`，Tauri 2 中后注册的覆盖先注册的**——discovery 签名目录与文件剪贴板落地目录两处接线全部被原 setup 静默顶掉（原 setup 里 tray/急启动等照常运行，故应用表现正常，极难察觉） | 两处接线合并进唯一的 setup 闭包（并加注释说明只允许一个）；启动时打 `discovery signing identity dir:` / `file-clipboard landing dir:` 两行日志——**这两行的出现与否就是接线是否生效的判据**。注意接线日志必须放在 setup 内 log 插件注册之后（此前打在注册前会被吞掉） |
| **拖拽 Win→Win "没实现"** | 日志证明拖拽链路实际工作（4 次 DragEnter → 交付 72 字节文件成功），但 Windows 受端把 DragDrop 模式文件落在隐藏目录 `.mykvm-drag-staging\`（staged 后处理原为 macOS-only），用户看不到 | （上一轮已修）Windows 受端 finish 时自动移到可见的 MyKVM Transfers 根——**需 B 机换新包** |
| **语音快捷键 Alt+Q 仍回落 A** | 当天 33 次钩子误判重装（tick 不更新的误报）打断控制会话；B 端 input_ready 抖动（B 反复重启） | （上轮已修 tick）本轮补控制会话日志：`control session started -> device` / `control session ended`——下次测试按时间轴即可判定按键时 A 是否处于控制状态 |
| **新功能：全屏暂停穿越** | — | Windows：捕获循环每 500ms 轮询前台窗口（`GetForegroundWindow`+`GetWindowRect` vs `MonitorFromWindow`+`GetMonitorInfoW`，±2px 容差，排除桌面 shell）→ `FULLSCREEN_CROSSING_PAUSED`；穿越发起前检查（回程/漫游/热键不受影响）；`LayoutState.fullscreenGuard`（默认 true）+ 设置页开关；状态切换打 info 日志。macOS 后续 |

**本机实证记录**：修复后 A 机启动日志出现 `discovery signing identity dir: C:\Users\Administrator\AppData\Roaming` 与 `file-clipboard landing dir: C:\Users\Administrator\Downloads\MyKVM Transfers\Clipboard` ✅。

**重测清单**（两台机器都换 10-03 02:21 之后的 zip）：
1. 启动后两边日志都应出现 `landing dir:` 行（没出现=跑的旧包）
2. 语音 Alt+Q：A 控 B 按住 → B 触发；若仍回落 A，两边拉日志看 `control session started/ended` 时间轴
3. 文件剪贴板双向：A 控 B 时 Ctrl+C 文件 → B 的 Clipboard 目录 + Ctrl+V；反向同理（B 端剪贴板同步开关必须开）
4. 拖拽：A 拖文件贴边 → B 的 MyKVM Transfers 根目录
5. 全屏：B 开全屏游戏 → A 的鼠标推 B 屏边缘不再穿越（设置可关）

### 第五轮收尾：剩余功能一次性落地（2026-10-03 凌晨）

| 功能 | 实现 |
|---|---|
| **文件断点续传** | 接收端 start 时按 `-{file_name}.part` 后缀+总字节匹配旧分片（`find_resumable_part` 取最大者收编：rename→临时名、SHA 预喂已有字节、`received_bytes=.part 长度`、`next_chunk_index=len/CHUNK`）；start ACK 从 `ok` 扩展为 `ok:<offset>`（`FILE_RESUME_OFFER` 槽位，每次接受的 start 都刷新防残留）；发送端解析 offset → `seek`+源前缀 [0,offset) 重喂 SHA 续算；QUIC 流回执改为透传原始回复（`verify_stream_ack` 兼容 `ok:` 前缀）。完整性：收编错内容只会在 finish 时 SHA 不匹配→删 .part 报错，不会静默落坏文件 |
| **传输取消** | `AppRuntime.transfer_cancels: HashMap<transferId, Arc<AtomicBool>>`；`send_file_transfer_packet/start` 重试循环与分片间检查取消位；新命令 `cancel_file_transfer`；前端进度 toast 加取消按钮 |
| **剪贴板历史** | `CLIPBOARD_HISTORY`（cap 20 条/总 64MB，文本截断 8KB）；记入点=发送成功+接收写入成功；命令 `read_clipboard_history`/`restore_clipboard_history`；全局快捷键 Ctrl+Shift+V → `clipboard-history-toggle` 事件 → 前端弹层选择回填本机剪贴板（随后被常规同步推到对端） |
| **稳定性** | `write_layout_to_disk` 忽略返回值改日志告警；清理 clipboard.rs/input.rs 多处未用变量与死赋值 |

**新增测试**（cargo test 170 全绿，其中 lib 165）：`file_transfer_resumes_from_adopted_part`（跨 transfer_id 收编+偏移对齐+finish SHA）、`file_transfer_ignores_part_larger_than_the_transfer`（尺寸不符不收编、全新 start 从 0 起）、`find_resumable_part_prefers_largest_within_budget`（同名取最大/超预算跳过/空文件跳过/他文件名排除）。

**协议注意**：续传要求两端同版本才有收益（start ACK 语义从 `ok` 变 `ok:<offset>`；`verify_stream_ack` 向后兼容旧端——旧接收端回 `ok` 时发送端按 offset=0 全量重发）。

**便携包**：`portable/MyKVM-Portable-win-x64.zip`（10-03 04:15 重建），exe 内已含 `file-clipboard landing dir`/`discovery signing identity dir`/`file transfer resuming` 日志串。

### 第六轮：路线图阶段一 + 阶段二落地（2026-10-03 下午）

> 排期与步骤详见 [UPGRADE_ROADMAP.zh-CN.md](./UPGRADE_ROADMAP.zh-CN.md)；本轮按其阶段一/二全部实现，**按用户要求整条路线未完前不打包**（便携包仍是 04:15 版，不含本轮内容）。

| 项 | 实现 |
|---|---|
| **1.3 白名单 LRU 淘汰** | `PairedController.last_used_ms`（serde default 兼容旧配置）+ 进程内 `PAIRED_CONTROLLER_LAST_USED` 使用表（授权热路径零磁盘写）；命中点：input.rs `packet_authorized_fields` 白名单命中、`file_transfer_packet_authorized`、`append_paired_controller`（重配对即刷新）；`normalize_paired_controllers` 按 `max(last_used, paired_at)` 降序淘汰，`write_layout_to_disk` 序列化前把使用表折叠进快照（重启后 LRU 不退化）；测试 2 个 |
| **1.4 全屏窗口期收窄** | `FULLSCREEN_GUARD_POLL_MS` 500→200ms（常量化），进全屏后的穿越盲区 <1/3 秒 |
| **1.2 历史持久化 + 快捷键录制** | 历史落盘 `clipboard-history.bin`（MessagePack 单文件含二进制、tmp+rename 原子写；后台线程 Condvar 去抖 2s 合并突发）；启动异步恢复（next_id 同步恢复防撞号）；`clear_clipboard_history` 命令（弹层加清空按钮）；快捷键改为 `LayoutState.clipboard_history_shortcut`（默认 ctrl+shift+v，空值=停用）+ `sync_clipboard_history_shortcut`（随布局保存即重注册）+ 设置页录制框（复用 hotkeyInput 录制模式） |
| **1.1 macOS 全屏检测** | `macos_appkit::foreground_is_fullscreen()`：`CGWindowListCopyWindowInfo`（OnScreenOnly|ExcludeDesktopElements）取 layer-0 窗口 bounds vs `CGGetActiveDisplayList`+`CGDisplayBounds` 显示器 bounds，复用 ±2px `rect_covers_monitor`；纯 FFI（无 core-foundation Rust API 依赖，字典键用 NSString 经 CFEqual 匹配，避开 extern static）；任何失败路径返回 false（只少杀不误杀）；`update_fullscreen_guard` 去掉平台 cfg，mac 端在 `handle_macos_mouse_move` 穿越评估前调用（限频内置）；**mac 真机运行时行为待验证**（本机已过类型检查） |
| **2.3 剪贴板事件驱动** | Windows：隐藏 message-only 窗口 + `AddClipboardFormatListener`（`Win32::System::DataExchange` 模块）→ `WM_CLIPBOARDUPDATE` 置位 `CLIPBOARD_EVENT_PENDING` + Condvar 唤醒；`run_clipboard_sync` 的 sleep 换 `wait_for_clipboard_wake`（无监听平台退化为普通 sleep）；事件唤醒绕过 150ms 节流（change_count 去重仍在，防重复读）；macOS 保持 changeCount 轮询 |
| **2.1 传输管理面板** | `TransferHistoryEntry`（cap 50，`transfer-history.json` 同步落盘，收/发双端记录：发送端逐文件成功/失败、接收端 finalize 成功/SHA 失败/finalize 失败）；命令 `list/clear/resend_transfer_history_entry`（重发=同目标同路径走正常发送管线）；设备页新增「传输历史」卡片（方向/文件/对端/大小/时间/错误/重发按钮）；进度事件 done 时自动刷新 |
| **2.2 队列级恢复** | `pending-queue.json`（input paths + completed 展开文件路径；仅 TransfersFolder 模式的用户发送纳入跟踪，拖拽/Desktop/ClientLog 不跟踪）；逐文件成功即更新落盘，全部完成删档；启动时发现残留队列 → `transfer-queue-resume` 事件 + 启动查询 → 底部横幅「继续/忽略」（忽略=清档不再提示）；`resume_pending_transfer_queue` 走正常发送管线（重写更小的队列、.part 续传自动生效）；测试 2 个 |

**验证**：cargo test 169 lib + 5 helper 全绿；前端 eslint/tsc/vitest（24）全绿。**未打包**（按用户要求，待整条路线完成）。

**下一批**（路线图阶段三/四 + 贯穿项）：Win→Win 拖拽落点（依赖 5.1 拆分 file_transfer.rs）、Linux 输入、ARM64、Peer 验证码 UI、屏幕预览、多语言、前端测试扩充、打包脚本化。

### 第七轮：路线图阶段三 + 阶段四（2026-10-03 晚，本地锚点未推送）

| 项 | 实现 | 锚点 |
|---|---|---|
| **5.1 file_transfer.rs 拆分** | 纯移动 -1191 行：wire 包/收发路径/续传/目标解析/清洗器 → `file_transfer.rs`；lib.rs 保留命令胶水、进度上报、历史与队列持久化；glob 导入使调用点零改动 | f208975 |
| **3.1 Win→Win 拖拽落点跟随光标** | 受端合成 OLE 会话（windows_drag.rs）本为 Mac 控制端服务；本轮解除 send_ole_drag_start/stream/signal 的 macOS 门控（补缺失的 cancel 参数），edge-drop 交接改为「先开原生会话再流式喂字节」，拒收/旧端自动回落 stage 路径；input.rs 转发 left-up 时发 drop（本地拖拽已在贴边被 inject_end_drag 终结）、回程发 cancel；流式中断发 cancel 防半喂挂死；设置开关 dragNativeDrop 默认开 | 278c2d1 |
| **4.3 Peer 验证码** | `pairing_status` 放开 peer 角色（原为非 client 一律 idle，验证码生成了但前端永远查不到），文案角色感知；前端本已 peer-ready | a8f0e1d |
| **4.2 ARM64** | release.yml 矩阵加 `windows-arm64`（aarch64-pc-windows-msvc 交叉构建），latest.json 防止 arm64 资产被误当 x64 updater 并注册 windows-aarch64 | 23de1f4 |
| **4.4 屏幕预览** | `screen_preview.rs`：GDI BitBlt 虚拟屏 → JPEG q60 长边 480px → base64，借 QUIC stream ack 通道以 `ok:<base64>` 回传（新协议 mykvm.preview.v1）；按需单帧（偏离路线图的 1~2fps 连续流，属刻意 MVP 取舍）；`previewEnabled` 双端默认关，服务端校验配对+开关；设备卡预览按钮+弹窗 | 287b7e4 |
| **4.5 多语言** | `src/locales/{zh-CN,en}.ts` 拆分，i18n.ts 变注册表并附扩展配方；纯移动 | ffa7185 |
| **4.1 Linux** | ⏸ 延期：无法交叉类型检查（ring C 构建限制）更无法运行验证，盲写会危及 Linux 端「诚实提示不可用」语义；按路线图建议独立立项 | — |

**验证**：cargo test 170+5 全绿；tsc/eslint/vitest（24）全绿。**全部锚点仅本地提交，未推送**（等用户命令）。便携包仍为 10-03 04:15 版（按约定整条路线完成前不打包）。

## 11. 本机构建验证记录（Windows 10 x64，2026-10-02）

**bate 分支（当前基线）**：

| 项 | 结果 |
|---|---|
| Node.js | v22.22.3 ✅（要求 22+） |
| npm 依赖安装 | ✅（npm install） |
| 前端 lint | ✅（eslint 0 错误） |
| 前端 build（tsc -b && vite build） | ✅ 347ms，产出 dist/ |
| cargo check（src-tauri workspace，增量） | ✅ 49.6s（14 个警告，多为平台 `#[cfg]` 分支下的 dead-code，正常） |
| Rust 工具链 | rustc/cargo 1.99.0 ≥ bate 要求的 1.89 ✅ |
| MSVC Build Tools | 2022 BuildTools + VC.Tools.x86.x64 + Win11 SDK 22621 ✅ |
| WebView2 运行时 | 运行 tauri dev 前需确认（scripts/check-dev-env.ps1 可查） |

**main 分支（切换前）**：npm install/lint/build ✅、cargo check 全量 16m05s ✅（6 警告）。

环境搭建说明：本机最初无 Rust 与 MSVC，验证过程中经 `rustup`（stable-x86_64-pc-windows-msvc）与 `winget install Microsoft.VisualStudio.2022.BuildTools`（VC.Tools.x86.x64 + Windows11SDK.22621）安装。
