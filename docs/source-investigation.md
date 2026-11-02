# 源码调查与构建基线

调查时间：2026-11-02（Asia/Shanghai）。工作区不是 Git 仓库，无法提供 Git 提交号或未提交 diff。没有覆盖其他项目文件、账号表格、数据库、Tauri/前端代码或用户 Codex 配置。

## 原始状态

- 源码：`putty/putty-0.85/`；版本头 `version.h` 为 Release 0.85。
- 主语言为 C99，Windows GUI 也为 C。CMake 顶层 `project(putty LANGUAGES C)`。
- 这是同时包含 `unix/` 与 `windows/` 的跨平台完整源码包，并非只能用于 Unix。Windows 窗口、资源和构建目标齐全；实际产物是 Windows x64 PE 可执行程序。
- 在修改前，递归对随附 `putty-0.85.tar.gz` 的每个普通文件计算 SHA-256，与源码对应文件比较：差异 0。布局、版本、README 与官方 PuTTY 源码一致；没有发现相对于该压缩包的企业定制。此比对不等于验证下载来源或数字签名。
- 原有三个修改文件已保存在 `docs/originals/`，原压缩包保持不变；`baseline-source/putty-0.85/` 是隔离提取的原始源码。
- 原始构建：MSVC 19.36.32534、VS 2022、Windows SDK 10.0.18362.0、x64 Release。`build-baseline/Release/putty.exe` 在任何修改前构建成功。
- 原始构建有 LNK4075（原有 MANIFESTUAC/MANIFEST:NO 配置）警告，未因此修改原构建策略。未安装 Perl，不影响本次 PuTTY 目标构建。

## 实际入口与结构

| 关注点 | 原始源码位置与行为 |
|---|---|
| WinGuiSeat、窗口和事件循环 | `windows/window.c` 创建 WinGuiSeat、HWND、Backend，随后 Windows 消息与顶层回调循环；`WndProc` 处理事件，`wgs_cleanup` 清理 |
| GUI 会话状态 | `windows/win-gui-seat.h` 的 WinGuiSeat：Backend、Ldisc、Terminal、HWND、session_closed |
| Backend 生命周期 | `start_backend` → `backend_init` → `ldisc_create`；关闭/重连经 `close_session` 和 `start_backend` |
| 认证后通知 | `ssh/mainchan.c` 调用 `seat_notify_session_started`；此版本原 GUI 使用空回调，改为仅设置 AI 可启用状态 |
| 原始远端输出 | Seat vtable 的 `win_seat_output` 调用 `term_data`；新增有界旁路复制后仍调用原函数并返回原值 |
| 人工输入 | `WndProc`/TranslateKey → `term_keyinput` 或 `term_keyinputw` → 原 Ldisc → Backend |
| AI 输入 | 自定义 `PostMessage` 只通知 GUI；用户确认后在 GUI 调用 `ldisc_send(...,false)`，Backend 未改造 |
| 屏幕与历史 | `terminal/terminal.h`、`terminal/terminal.c` 的 screen、alt_screen、scrollback、tree234；本版不读取这些缓冲 |
| 配置与启动 | `windows/putty.c`、`cmdline.c`；保留命令行、已保存会话 `@session`/`-load`、文件映射传参、SSH/端口/代理选项 |
| 构建 | `CMakeLists.txt` 与 `windows/CMakeLists.txt`；仅 Windows putty 目标链接 Bridge，不给 PuTTYtel、Plink 等添加 AI |

`win_seat_output` 是 Seat 送入 Terminal 的字节入口，也可能包含原线路规程已有的本地回显；它不是纯 SSH 协议载荷。采集侧不直接记录键盘事件，MCP 始终标注为终端 transcript。

未读取或记录实际启动参数、私钥、密码、Token、认证配置。无法从普通源代码确认企业 Web 启动器实际传入的敏感参数或最终目标，因此没有自动推断和展示这些信息。

## 原有文件修改清单

1. `windows/window.c`：包含 Bridge 接口、输出旁路、认证后状态、本地菜单、标题 ID/权限指示、人工输入撤销、异步通知、断线/重连/认证/退出清理；原认证和 Backend 算法未改。
2. `windows/win-gui-seat.h`：增加 Bridge 指针、认证后标志、标题状态缓存，按现有 memset 初始化。
3. `windows/CMakeLists.txt`：增加 `PUTTY_AI_BRIDGE` 编译选项、Bridge 源文件/系统库以及独立测试目标。

新增源码集中在 `ai_bridge/`，Rust 服务位于独立 `putty-ai-rs/` Workspace。可以用 `git diff --no-index docs/originals/window.c putty-0.85/windows/window.c` 等命令审阅三个原文件差异。

