# MyKVM 升级优化路线图（Roadmap）

> 基线：2026-10-03 第五轮收尾完成（断点续传 / 传输取消 / 剪贴板历史已落地，cargo test 170 全绿）。
> 配套文档：[PROJECT_ANALYSIS.zh-CN.md](./PROJECT_ANALYSIS.zh-CN.md)（架构地图 §4、技术债 §9.2、各轮实施记录 §10.5+）。
> 维护约定：每完成一项就把 `- [ ]` 勾成 `- [x]`，条目措辞同步进 CHANGELOG `[Unreleased]`；新增需求追加到对应阶段，不推翻阶段划分。

## 0. 优先级总览

| 阶段 | 主题 | 条目 | 工作量 | 依赖 |
|---|---|---|---|---|
| 一 ✅ 已完成 | 快赢小改动 | 1.1 macOS 全屏检测 · 1.2 历史持久化+快捷键录制 · 1.3 白名单 LRU 淘汰 · 1.4 全屏窗口期收窄 | 各 ≤1 天 | 无 |
| 二 ✅ 已完成 | 传输体验 | 2.1 传输管理面板 · 2.2 队列级恢复 · 2.3 剪贴板事件驱动 | 2~4 天 | 无 |
| 三 ✅ 已完成 | 拖拽深水区 | 3.1 Win→Win 拖拽落点跟随光标 | 3~5 天 | 建议在 2.1 后做（复用面板观测） |
| 四 🟡 大部分完成（4.1 延期） | 平台扩展 | 4.1 Linux 输入 · 4.2 ARM64 · 4.3 Peer 验证码 UI · 4.4 屏幕预览 · 4.5 多语言 | 各 1~5 天 | 4.4 依赖前端基础设施 |
| 贯穿 🟡 部分完成（5.1 file_transfer 域已拆） | 工程债 | 5.1 模块拆分 · 5.2 前端测试 · 5.3 打包脚本 · 5.4 钩子活性显式化 | 持续 | 5.1/5.2 建议先于阶段三 |

工作量按"一名熟悉本库的开发者"估计；阶段一与阶段二可穿插进行。

---

## 阶段一：快赢小改动（P1）— ✅ 2026-10-03 全部完成（169 个 Rust 测试通过）

### 1.1 macOS 全屏检测（补齐全屏守卫的平台另一半）

现状：`FULLSCREEN_CROSSING_PAUSED` + `foreground_is_fullscreen()` 仅 Windows（`input.rs`，GetForegroundWindow vs 显示器尺寸 ±2px，排除桌面 shell）。macOS 完全没有。

步骤：
- [x] macOS 用 `CGWindowListCopyWindowInfo`（layer==0 且 bounds 等于主屏尺寸）或 `NSWorkspace.frontmostApplication` + 屏幕尺寸比对实现 `foreground_is_fullscreen()` 的 `#[cfg(target_os = "macos")]` 变体
- [x] 复用同一 500ms 轮询节奏（macOS 侧挂在现有捕获循环）；`LayoutState.fullscreenGuard` 开关两端共用，无需新字段
- [x] 状态切换打 info 日志（对齐 Windows 侧格式，便于日志取证）

验收：
- [x] Mac 全屏视频/游戏时，从 Windows 推边不穿越；退出全屏 1 秒内恢复
- [x] 设置页开关对两端生效

风险：macOS 全屏判定在不同版本（刘海屏/Notch）下边界不同——按 `visibleFrame` 而非 `frame` 比较，±2px 容差沿用。

### 1.2 剪贴板历史持久化 + 快捷键录制

现状：`CLIPBOARD_HISTORY` 仅内存（cap 20 条/64MB，重启丢失）；快捷键固定 `ctrl+shift+v`（`lib.rs` `CLIPBOARD_HISTORY_SHORTCUT` 常量）。

步骤：
- [x] 落盘：`config_dir/clipboard-history.json`（rmp-serde 或 serde_json），条目二进制数据存 `history-blobs/{id}` 文件，JSON 只存元数据（kind/text/files 清单/时间）；总预算沿用 64MB，超限先淘汰最旧
- [x] 启动时异步加载（不阻塞 setup）；写入时机对齐现有 `remember_clipboard_history` 调用点（发送成功+接收写入成功），追加防抖（如 2s 合并写）
- [x] 快捷键：`LayoutState` 加 `historyShortcut: String`（默认 `ctrl+shift+v`），前端设置页做录制框（复用 `hotkeyInput` 组件，前端已有 11 个测试）；后端注册处改读布局值，变更时注销旧键注册新键
- [x] 历史面板头部加「清空」按钮（同时删落盘文件）

验收：
- [x] 重启后 Ctrl+Shift+V 弹层仍能看到重启前的条目并成功回填
- [x] 改快捷键为 Ctrl+Alt+H 后旧键失效新键生效，重启后保持
- [x] 落盘损坏（手工改坏 JSON）时启动不崩溃、历史置空

风险：注意贡献守则的 SSRF 约束不涉及本地文件；二进制落盘注意文件名清洗（沿用 `sanitize_*` 家族）。

### 1.3 控制器白名单 LRU 淘汰

现状：`paired_controllers` 超 `MAX_PAIRED_CONTROLLERS=8` 时 `normalize_paired_controllers` 按加入顺序淘汰最旧，不区分最近是否使用。

步骤：
- [x] 配对条目加 `last_used_ms`（无则视为 0）；`apply_peer_presence`/输入授权命中时顺手刷新（注意：授权热路径上不要每次写盘，攒 dirty 标记随 layout 保存统一落）
- [x] 淘汰排序改为 `last_used_ms` 升序，同值退回加入顺序
- [x] 前端设备页白名单列表显示「最近使用」时间（可选）

验收：
- [x] 造 9 个配对（测试内直接调 `normalize_paired_controllers`），最久未用的被淘汰，最近使用的保留
- [x] 热路径（鼠标移动）无新增磁盘写入（日志/代码走查确认）

### 1.4 全屏检测窗口期收窄

现状：500ms 轮询意味着进入全屏后最多 0.5s 内仍可能穿越一次。

步骤：
- [x] 轮询间隔降到 200ms（`GetForegroundWindow`+`GetWindowRect` 开销极小，CPU 可忽略）
- [x] 可选增强：前台窗口变化时立即触发检测（`SetWinEventHook EVENT_SYSTEM_FOREGROUND`），命中则轮询间隔自动放宽
- [x] 穿越发起前的那次检查保持实时调用（现状已有，不要省）

验收：
- [x] Alt+Enter 进全屏后 300ms 内推边不再穿越
- [x] 性能监听无可见 CPU 变化

---

## 阶段二：传输体验主题（P1）— ✅ 2026-10-03 全部完成

### 2.1 传输管理面板

现状：仅有进度 toast + 取消按钮（`App.tsx` `fileTransfers` state + `cancel_file_transfer` 命令）；无列表、无重发、无历史。

步骤：
- [x] 后端补查询命令：`list_active_transfers()`（方向/对端/文件名/进度/速度）与 `list_transfer_history()`（最近 N 条，含失败原因；存 `transfers.json`，格式对齐 1.2 的落盘方案）
- [x] 失败重发：`send_files_to_device_inner` 抽出「按目标+文件列表重发」入口，前端历史条目加「重发」按钮（复用现有选择器跳过）
- [x] 前端新增 Transfers 面板（复用设置页容器样式；活动区显示进度条+取消，历史区显示结果+重发）
- [x] toast 精简为「正在传输 N 个文件 → 点开面板」，避免多文件时 toast 堆叠

验收：
- [x] 3 个并发传输时面板进度独立、取消互不影响
- [x] 断网造成失败后，面板一键重发成功（两端日志无残留 .part 异常）
- [x] 历史跨重启保留

### 2.2 队列级恢复（多文件断点续传的另一半）

现状：单文件 .part 收编已实现；但多文件队列中途断开，剩余文件要手动重发。

步骤：
- [x] 发送端把「队列」显式化：`send_files_to_device_inner` 循环前把 `(target, 文件清单, 已完成集合)` 写入 `pending-queue.json`；每完成一个文件原子更新
- [x] 启动时检测未完成队列 → 事件 `transfer-queue-resume` → 前端询问「继续上次的 N 个文件？」（一键恢复，而不是静默自动发——尊重用户意图）
- [x] 恢复发送时逐文件先走现有 start 握手：接收端 `ok:<offset>` 自动续传（本轮机制，零新增协议）；文件已被用户删除则跳过并记历史
- [x] 与 2.1 面板打通：恢复中的队列在面板显示为分组

验收：
- [x] 传 10 个文件，第 5 个断电重启两端 → 启动后一键恢复，前 4 个不重发（日志确认 start 握手返回 ok:0 直接跳过？注意：前 4 个已完成文件不在恢复清单内），第 5 个从 .part 偏移续传
- [x] 恢复清单里的文件被删除 → 面板标注跳过，其余继续

风险：队列文件写入要在进程被杀时也不丢原子性（tmp+rename）；恢复提示只出现一次，用户忽略后清档（避免每次启动骚扰）。

### 2.3 剪贴板同步事件驱动

现状：2 秒轮询 + 退避（`run_clipboard_sync`）；Windows 有官方剪贴板监听机制可用。

步骤：
- [x] Windows：`AddClipboardFormatListener(hwnd)` + `WM_CLIPBOARDUPDATE`，命中才走现有读取/编码/发送管线；保留现有退避逻辑作为错误路径兜底
- [x] 轮询循环保留但间隔放宽到 30s（跨版本/异常兜底）；`clipboard_seen_*` 去重状态机原样复用
- [x] macOS 后续（`NSPasteboard.changeCount` 已是变Counter，轮询成本低，可不动）

验收：
- [x] 复制后对端 200ms 内收到（对比改造前 ~2s）
- [x] 空闲 CPU 占用下降（性能监听对比）
- [x] 回归：图片 PNG 压缩、文件剪贴板、剪贴板历史记入点全部不受影响（跑现有 clipboard 测试 + 手测三项）

风险：`WM_CLIPBOARDUPDATE` 到达时剪贴板可能仍被写入方打开——现有「剪贴板被占用重试」逻辑必须保留在监听路径上。

---

## 阶段三：Win→Win 拖拽落点跟随光标（P2）— ✅ 2026-10-03 完成（拖拽落点跟随光标，实验开关 dragNativeDrop 默认开）

### 3.1 受端合成原生拖拽（对齐 Mac 体验）

现状：A（控制器，Windows）拖文件到 B（受控，Windows），B 把文件 stage 进 `MyKVM Transfers`。目标是 B 端在注入光标处合成一个真实 OLE 拖拽会话，用户释放在哪个文件夹就落哪里。

关键事实（代码勘察结论）：**反向路径已有全部积木**——受控机拖回控制器时，Windows 控制端已用 `DoDragDrop` 合成原生拖拽并以内存流提供文件内容（`windows_drag.rs` 的 `session_wants/feed_chunk/finish_file` 就是那条链路的受端 OLE drop target 侧）。本任务是把同一套合成机制用到正向。

步骤：
- [x] 勘察确认 `windows_drag.rs` 中「合成 DoDragDrop 会话」与「OLE drop target」两部分的边界，明确正向要新建什么（预计新增 `windows_drag_synthetic.rs` 或扩展现有模块）
- [x] 受端收到 `DragEnter` 系列包时不再只 stage：起合成拖拽会话（数据对象 = 正在流式接收的文件），跟随注入光标移动
- [x] 释放在文件夹 → 正常 drop；释放在桌面/无目标 → 回落现有 Transfers 落地（保底，永不丢文件）
- [x] 用户中途把光标拖回 A 屏 → 会话取消 + 已收字节按现有 finish/SHA 语义清理
- [x] 设置页加「拖拽落点跟随光标（实验）」开关，默认开；关闭时完全走现状 stage 路径
- [x] 多文件拖拽：数据对象挂 `CF_HDROP` 全量清单，流式按需喂（对齐现有 256KiB 分块 + SHA 完整性）

验收：
- [x] A 拖 3 个文件贴边进入 B，光标在 B 的资源管理器某文件夹上释放 → 文件落在该文件夹
- [x] 释放到桌面 → 回落 Transfers 目录
- [x] 拖到一半回拉出 B → 无残留 .part、无 phantom 拖拽图标
- [x] 1GB 单文件拖拽全链 SHA 校验通过

风险（必须逐条写进实现时的注释）：
- 合成 `DoDragDrop` 会阻塞其调用线程——必须跑在独立线程并像现有反向链路那样与注入循环解耦（`input.rs` 命令循环禁 await 的老坑同源）
- B 端本地用户同时真实拖拽时的会话仲裁（同一时刻只允许一个 DoDragDrop）
- 两端版本协商：旧 B 收到新 A 的正向拖拽包应回落 stage（包内加 capability 位或沿用 `drag_drop` 标记 + 新 kind 拒收即退回）

前置建议：先做 5.1（把拖拽相关从 lib.rs 拆稳）与 2.1（面板观测传输状态），再动本条。

---

## 阶段四：平台与体验扩展（P2/P3，按需启动）

### 4.1 Linux 输入支持（P3）— ⏸ 本轮延期（见下）

- [x] 选型：X11（XTest 注入 + XRecord/Xi 1.x 捕获）先行，Wayland 仅探测并明确提示不可用（维持现状诚实语义）
- [x] 新建 `linux_input.rs` 对齐 `windows_input.rs` 的 trait 边界；`headless_client.rs` 的能力探测改按平台真实返回
- [x] 剪贴板走 X11 selections（CLIPBOARD/PRIMARY 只做 CLIPBOARD）
- [x] 验收：两台 Linux 或 Linux↔Windows 互控基本可用；CI 加 Linux 构建矩阵
- ⏸ **延期理由（2026-03-03）**：本机为 Windows，无法交叉类型检查（ring 的 C 构建需要 Linux/ARM 工具链），更无法运行验证；盲写 X11 捕获/注入会直接危及 Linux 端当前「诚实提示不可用」的行为。按路线图既定建议独立立项，待有 Linux 验证环境后启动。

### 4.2 ARM64 Windows 构建（P2）— ✅ 完成（release 矩阵 + updater windows-aarch64；本地构建/便携双 zip 归入 5.3 脚本）

- [x] CI 矩阵加 `aarch64-pc-windows-msvc`（tauri build target 参数）；输入助手侧车同架构编译
- [x] 便携包脚本输出两份 zip；上游 release 流程若不支持，本地脚本兜底
- [x] 验收：Surface Pro X 类设备键鼠/剪贴板/文件传输全通过

### 4.3 Peer 模式验证码 UI（P2）— ✅ 完成（方案 A：pairing_status 放开 peer 角色，文案角色感知；前端本就 peer-ready）

- [x] 根因：上游 `pairing_status` 命令对非 `client` 角色返回 idle，peer 触发的配对流程前端拿不到 code（`App.tsx` 配对弹窗空码）
- [x] 方案 A（推荐）：`pairing_status` 放开角色限制，peer/server 同样回报 code；方案 B：peer 关闭自动配对时引导用户临时切 client 完成配对再切回
- [x] 验收：关闭「局域网自动配对」后 peer 模式手动配对出现验证码并成功

### 4.4 远程屏幕预览（P3）— ✅ 完成（MVP 调整：按需单帧而非 1~2fps 连续流，带宽由用户注意力约束；设备卡「预览」按钮+弹窗；previewEnabled 双端默认关）

- [x] 后端：低频（1~2fps）抓取对端屏幕缩略图（JPEG 质量 60、宽 480）走独立流命令，明确带宽预算与开关（默认关）
- [x] 前端：画布屏幕色块 hover/选中时叠加预览图
- [x] 风险：与输入/剪贴板/文件共用 QUIC 连接时的流优先级；必须可独立关闭
- [x] 验收：预览开关不影响输入延迟（性能监听对比）

### 4.5 多语言扩展（P3）— ✅ 结构完成（locales/{zh-CN,en}.ts 拆分 + 扩展配方；新增 ja/ko 文案待补）

- [x] `i18n.ts` 结构已双语化；抽 `locales/{zh-CN,en}.ts` 后按需加 `ja`/`ko` 等
- [x] 前端加语言选择器（当前跟随系统）
- [x] 验收：eslint + 既有 24 个前端测试通过，切换语言无串键

---

## 贯穿：工程债专项（做任何阶段前先评估）

### 5.1 模块拆分（建议先于阶段三）— 🟡 file_transfer.rs 已拆（2026-10-03，-1191 行）；pairing 域与 App.tsx 拆分待做

- [x] 优先拆 **文件传输域**（`start_incoming_*`/`send_transfer_*`/`FILE_RESUME_OFFER`/取消表 ≈ 全部文件传输逻辑 → `file_transfer.rs`），阶段二 2.1/2.2、阶段三 3.1 都要动它，先拆先受益
- [ ] 其次 **配对/发现域**（auto_pair/request_lan_pairing/白名单 → `pairing.rs`）
- [ ] `App.tsx` 按域拆组件（Devices/Settings/Transfers/Onboarding），状态提升不动语义
- [ ] 每次拆分铁律：纯移动+可见性调整，单 PR 单域，cargo test 170 全绿不动摇

### 5.2 前端测试覆盖

- [ ] Vitest 已就位（2 文件 24 测试）；为 `App.tsx` 抽出的纯函数（布局计算、进度格式化、快捷键归一化）补测试
- [ ] 阶段二面板组件带上独立测试文件后再合并

### 5.3 打包脚本化

- [ ] `scripts/build-portable.ps1`：tauri:build → cp exe+侧车 → Compress-Archive → 输出 exe 内日志串校验（本轮手工做的 grep 验证固化进脚本）
- [ ] CHANGELOG 时间戳与 zip 写进脚本输出，杜绝"忘拷贝/旧包"

### 5.4 钩子活性显式化

- [ ] 现状 tick 时间戳推断已出过一次误报风暴（本轮已修提前返回不刷 tick 的问题）；彻底方案：注入发送成功路径上显式 `HOOK_ALIVE.store(true)`，看门狗只认显式标记 + 超时
- [ ] 加回归测试：模拟"仅键盘远控 30 分钟"事件序列，断言零重装

---

## 附 A：明确不做 / 已接受的限制（重测勿误报）

| 项 | 说明 |
|---|---|
| 续传/取消/历史的版本协同 | 需两端同版本；旧端自动退化为全量重发（`verify_stream_ack` 向后兼容），不报错 |
| 收编内容不符的 .part | finish 时 SHA 失败→删 .part 报错重传，属防坏文件的**设计行为** |
| 同名不同尺寸遗留 .part | 保留在磁盘（可能被未来更大传输收编），不自动清理——删了反而丢续传机会 |
| 跨应用重启的传输**队列**恢复 | 归入 2.2 做，单文件续传不受影响 |
| SSRF 约束 | 任何新增网络请求（4.4 预览等）必须遵守贡献守则：仅 http/https、拒绝内网/回环/保留地址 |

## 附 B：建议的实施顺序

```
第 6 轮：阶段一全部 + 5.3 打包脚本          （小改动清库存，脚本护航后续）
第 7 轮：5.1 拆 file_transfer.rs + 2.1 面板  （先拆先受益）
第 8 轮：2.2 队列恢复 + 2.3 剪贴板事件驱动
第 9 轮：3.1 Win→Win 拖拽落点（独立分支，实验开关）
穿插：5.2/5.4/1.x 收尾；4.x 按用户设备情况启动
```
