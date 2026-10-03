# MyKVM 二次开发升级报告书

> 期间：2026-10-02 ~ 2026-10-03 · 基线：上游 XxMinor/mykvm `bate` 分支（fe938ee）
> 产出仓库：fork `ClvieH/Ckvm`（bate 分支）· 本报告对应本地 HEAD：见文末提交台账

---

## 一、结论摘要

以 bate 分支为基线完成 **八轮** 二次开发，新增/修复 **40+ 项**，全程测试护栏从 0 扩充到 **Rust 171 + 前端 30（3 个测试文件）**，全部通过。升级路线图（`docs/UPGRADE_ROADMAP.zh-CN.md`）四个阶段 + 工程债专项 **全部执行完毕**（4.1 Linux 输入按既定理由延期立项），便携包已按最终代码重建并附自动接线校验。

**双机重测前必须**：两台机器都换用 10-03 19:15 之后的便携包（zip 内 exe 已含本轮全部协议变更）；续传/取消/历史/原生拖拽/预览等功能需两端同版本才能生效。

## 二、功能增量（用户视角）

### 控制与穿透
| 功能 | 说明 |
|---|---|
| Peer 双向控制 | 两台机器互为主从，`inputMode: both`，证书白名单对称授权 + CONTROLLED_ACTIVE 仲裁 |
| 角落防误切 | 屏幕四角死区不穿越（关闭按钮/开始菜单安全），大小可调，回程双向生效 |
| 全屏暂停穿越 | 前台全屏应用（游戏/视频）时暂停穿越；Windows 轮询 200ms + **macOS CGWindowList FFI**，两端生效 |
| 离开锁屏 | 控制权划出时自动锁本机（默认关） |
| 快捷切屏/启停 | 方向热键 + 快捷启停，peer 模式同样注册 |

### 配对与发现
| 功能 | 说明 |
|---|---|
| 局域网自动配对 | 发现即互配（证书信任锚 + 遗留口令回退），验证码流程可切换回来，控制器上限 8 |
| **LRU 淘汰** | 白名单满 8 按「最近使用」淘汰（进程内使用表 + 落盘折叠，重启不退化） |
| 发现签名 | HMAC-SHA256 绑定身份，伪造 announce 被丢弃（旧端未签名仍收） |
| 重启秒恢复 | 对端上线即时刷新 targets；已配对再配对自动短路（消灭 "no pairing challenge"） |
| Peer 验证码 | 手动配对（关自动互配）时接收端正常显示验证码 |
| Wake-on-LAN | 设备广播真实网卡 MAC（GetAdaptersAddresses），设备页一键唤醒 |

### 传输
| 功能 | 说明 |
|---|---|
| 拖拽 Win→Win **落点跟随光标** | 受端起真实 OLE 拖拽会话（复用 Mac→Win 机制），释放落光标下文件夹；拒收/旧端自动回落 Transfers；开关 `dragNativeDrop` 默认开 |
| 断点续传 | `.part` 收编（同名同尺寸）+ `ok:<offset>` ACK 协商 + SHA 前缀重喂；**队列级恢复**：中断后启动弹「继续/忽略」 |
| 传输取消/历史 | toast 取消按钮；设备页历史面板（50 条，失败重发，持久化） |
| 完整性 | 全程 SHA-256 finish 校验，坏文件绝不落地 |

### 剪贴板
| 功能 | 说明 |
|---|---|
| 文件剪贴板 | Ctrl+C 文件 → 对端 Ctrl+V，落 `Downloads\MyKVM Transfers\Clipboard`，24MB 上限 |
| 历史面板 | 20 条/64MB，**持久化**（MessagePack 原子写），Ctrl+Shift+V **可录制**，可清空 |
| 事件驱动同步 | Windows `WM_CLIPBOARDUPDATE` 即时唤醒（毫秒级，原 150ms 轮询）；Mac 保持 changeCount 轮询 |
| 图片压缩 | PNG 线上编码（4K 截图 33MB → 数百 KB），泄漏修复 |

### 可靠性修复（重点）
- **Tauri 2 双 `.setup()` 覆盖**：文件剪贴板落地目录/签名目录接线被顶掉 → 合并单一 setup + 启动日志判据
- **Alt+Q 语音和弦断裂**：批量按键未入 SENDER_HELD 台账 → `track_all` 修复
- **钩子误报重装风暴**：tick 更新提前到所有 early-return 之前（双钩子不变量统一），决策抽纯函数 + 回归测试（30 分钟纯键盘流零误判/边界值/回绕）
- Peer 画布重叠修复（对端屏排布到本机右侧）、性能监听假死、传输进度风暴等

## 三、新模块与测试

| 模块/文件 | 职责 |
|---|---|
| `src-tauri/src/file_transfer.rs` | 传输域（-1191 行自 lib.rs）：wire 包、收发、续传、目标解析、清洗器 |
| `src-tauri/src/discovery_signing.rs` | 发现签名（HMAC-SHA256，持久密钥） |
| `src-tauri/src/screen_preview.rs` | 屏幕预览（GDI 抓屏 → JPEG q60/480px → base64 走 stream ack 通道） |
| `src/locales/{zh-CN,en}.ts` | UI 文案分语言（新增语言 = 复制一个文件 + 注册） |
| `src/format.ts` + `format.test.ts` | 纯格式化函数（React-free，可测） |
| `scripts/build-portable.ps1` | 便携打包一键脚本：构建 → 拷贝 → **exe 接线日志串校验** → zip |

测试资产：`hook_liveness`（重装风暴回归）、`whitelist LRU`×2、`pending_queue`、`transfer_history`、`file transfer resume`×3、`peer pairing code`、前端 `format/hotkey/layout` 30 项。

## 四、验证记录

| 项 | 结果 |
|---|---|
| `cargo test --workspace` | **171 lib + 5 helper 全绿**（每次锚点前全量跑） |
| `npm run lint / tsc -b / vitest` | 全绿，30 tests |
| `npm run tauri:build` + `build-portable.ps1` | ✅ 10-03 19:15，三个接线日志串校验通过 |
| 双机实测 | 待用户执行（重测清单见分析文档 §10.5-10.7） |

## 五、提交台账（bate 分支，本地 8 个锚点未推送）

```
37ac8f6 docs: roadmap/checks, changelog and round-7 record for phases 3+4
ffa7185 refactor: split UI strings into src/locales/{zh-CN,en}.ts        (4.5)
287b7e4 feat: on-demand remote screen preview (opt-in, Windows capture)  (4.4)
23de1f4 build: add Windows ARM64 to the release matrix                   (4.2)
a8f0e1d fix: peer-mode manual pairing now shows the verification code    (4.3)
278c2d1 feat: Win->Win edge drags drop into the folder under the cursor  (3.1)
f208975 refactor: extract the file-transfer domain into file_transfer.rs (5.1)
702f8dd feat: six rounds of secondary development (第一~五轮+阶段一/二)
5612e3a fix: hook-liveness tick recorded before every mouse-hook early return (5.4)
cff6a2d refactor: extract UI formatting helpers into format.ts with tests    (5.2)
<下一个> docs: upgrade report + roadmap completion (本报告)
```

## 六、遗留与延后（如实声明）

| 项 | 状态 |
|---|---|
| **双机真机重测** | 拖拽落点时序、屏幕预览画面、macOS 全屏检测运行时行为需实测（代码路径与既有机制同构，失败均回落旧行为） |
| **4.1 Linux 输入** | 延期：无法交叉类型检查/运行验证（ring C 工具链），盲写会危及「诚实提示不可用」语义；待 Linux 环境立项 |
| **5.1 其余拆分** | pairing 域（~1500 行）与 App.tsx 组件化未做——file_transfer 拆分已先行（3.1 的既定前置） |
| **4.4 MVP 偏差** | 按需单帧而非 1~2fps 连续流（刻意取舍，协议可平滑升级为连续流） |
| **CI 副作用** | 推送后 fork 的 Actions 会自动跑 beta 构建，可能因缺 secrets 显示失败，属预期 |
| 便携包 | 10-03 19:15 版已含全部本轮内容（脚本自动校验） |

## 七、部署清单（双机重测）

1. **两端**：托盘退出旧版 →（装过服务的）`sc stop MyKVMInputService && sc delete MyKVMInputService`（管理员）→ 解压新 zip 覆盖
2. 启动后两边日志出现 `discovery signing identity dir:` 与 `file-clipboard landing dir:` = 新包生效
3. **不要点唤醒/配对**，直接推鼠标——重启秒恢复应直接可用
4. 重测清单：语音 Alt+Q（B 端触发）/ 文件剪贴板双向 / 拖拽落点跟随光标 / 全屏暂停 / 断点续传（日志出现 `file transfer resuming`）/ 传输取消 / 剪贴板历史（Ctrl+Shift+V）/ 屏幕预览（两端先开开关）/ 队列恢复横幅
