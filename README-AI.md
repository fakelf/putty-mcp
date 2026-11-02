# PuTTY AI Bridge / Session Broker / 通用 MCP v2

**首次安装或交给其他 AI 使用，请先读同目录 `START_HERE.md`。** 完整工具参数/返回/调用顺序见 `docs/ai-integration.md`，机器可读接口见 `docs/mcp-tools.json`。运行包自带不发送远程输入的 `scripts/check-mcp.ps1`。

最终交付产物为 `dist/putty-ai-final-x64/` 和同名 zip，完整源码为 `dist/putty-ai-final-source.zip`。程序与实测的 v2 修复版一致。已修复灰色批准菜单误导：控制与命令自动弹出本地确认窗口，拒绝后及时清理 Broker 待定状态，灰色菜单明确显示无申请/已有批准窗口。DeepSeek Harness 接入见 `docs/deepseek-harness.md`；可复制的 AI 指令在 `FEED_TO_DEEPSEEK.txt`。

这是在随附 PuTTY 0.85 C 源码上实现的 Windows x64 集成。源码、实际构建产物和自动化测试均在本目录内。默认 AI 关闭；SSH、认证、加密、Web 启动参数和堡垒机连接路径保持原实现。

## 使用已有构建产物

运行文件位于 `dist/putty-ai-final-x64/`，压缩包为 `dist/putty-ai-final-x64.zip`。三个运行程序是 `putty.exe`、`putty-ai-broker.exe`、`putty-ai-mcp.exe`。三个程序保持同目录；Adapter 会按需启动同用户唯一 Broker。没有 HTTP 服务或 TCP 监听端口。安装不依赖 Codex，可在支持 Windows 本地 stdio MCP 的其他 AI 客户端中配置。

1. 由企业管理员把 Web 启动流程使用的 PuTTY 指向本次构建的 `putty.exe`，保留原参数、协议关联、堡垒机策略。此项目不会自动修改浏览器、注册表、堡垒机或用户 Codex 配置。
2. 正常经 Web 堡垒机打开会话，完成认证和 MFA。在目标 Linux Shell 中，鼠标点击 PuTTY 标题栏左上角图标，打开系统菜单，选择 **AI OFF - enable observation...**，阅读并确认数据授权。
3. 标题出现 `[AI READ ONLY xxxxxxxx]`。前 8 位 ID 对应 `list_sessions` 返回的会话。只共享启用后的新增输出，不读取既有屏幕、历史、配置、启动参数或密钥。
4. 按 `docs/installation.md` 注册 MCP。先让 Codex 调用 `list_sessions`、`read_terminal`，核对窗口和真实目标。
5. 执行排障或部署时，AI 调用 `acquire_control(requested_mode=custom)`。自动弹出与返回 `client_id` 匹配的 CUSTOM SHELL 批准窗口，由用户批准。控制租约最长 60 秒；到期不停止已发出任务的观察。
6. AI 调用 `submit_command` 后立即取得任务 ID；自动弹出可滚动命令确认窗口。核对完整脚本、空输入行、空闲 POSIX Shell 和目标身份，不能确认时拒绝。随后通过 `get_task_status` 查询。custom 支持命令、脚本、管道和重定向；每次上限 2048 UTF-8 字节，观察期限最长一小时。
7. 点击 **AI: revoke control** 随时撤销；键盘、终端鼠标和粘贴操作也会撤销。点击 **AI READ ONLY/CONTROL - disable AI** 完全关闭该窗口的采集线程和 IPC。

请使用标题栏图标打开系统菜单。终端区右键、Alt/键盘输入会触发人工优先撤销，适合紧急停止，不适合在待执行命令期间打开批准菜单。

普通未改造 PuTTY 进程无法事后注入 Bridge。先部署新程序，再经原 Web 流程新开窗口；旧版已建立的窗口不在 MCP 列表中。已运行本次程序且启用观察的窗口可以随时被新启动的 Codex 发现，无需重建 SSH 连接。

## 从源码构建和测试

需要 Windows x64、Visual Studio 2022 C++ 工具及 Windows SDK、CMake、Rust MSVC 工具链；运行自动化测试另需 Node.js。所有命令在 PowerShell 执行：

部署电脑还需 Microsoft Visual C++ 2015–2022 x64 Runtime（实际 dumpbin 检查依赖 VCRUNTIME140.dll 和系统 UCRT）。源码包 `dist/putty-ai-final-source.zip` 包含完整改造源码，不含 Cargo target 和构建缓存。若已在解压源码包根目录，下面命令去掉 `putty\` 前缀。Shell 引号测试需要 Git Bash，或通过 PUTTY_TEST_BASH 指向本机 bash。

```powershell
& .\putty\scripts\build.ps1 -RunTests
```

只运行已有构建的测试：

```powershell
& .\putty\scripts\test.ps1
```

隔离测试会创建临时模拟会话并重启它自己启动的 Broker。请先关闭正在使用的 Broker；测试检测到已有 Broker 会失败，不应连接生产终端。`test_ai_native.exe` 是会自动批准**合成命令**的独立测试程序，完全没有 SSH Backend，未放入运行包。

构建不含 Bridge 的 PuTTY：

```powershell
cmake -S .\putty\putty-0.85 -B .\putty\build-off -G 'Visual Studio 17 2022' -A x64 -DPUTTY_AI_BRIDGE=OFF
cmake --build .\putty\build-off --config Release --target putty -j 8
```

## 文档与验证范围

- `docs/source-investigation.md`：原始源码调查、构建基线及原文件备份。
- `docs/architecture.md`：线程、权限、消息、序号、任务状态及资源上限。
- `docs/installation.md`：Codex 配置、部署、环境限制和卸载。
- `docs/test-report.md`：实际测试、测量结果和未验证内容。
- `docs/known-limitations.md`：必须在试点部署前了解的限制。
- `docs/mcp-tools.json`：从实际运行 MCP 获取的工具 Schema。

v2 是面向排障与部署的受控 Shell 实现；diagnostic 模式仍保留五个模板，custom 需单独本地批准。旧包位于 putty-ai-x64，新旧管道隔离，升级需要使用新 PuTTY 打开会话。没有真实企业堡垒机和生产 Linux 的验收结果；不能据模拟测试宣称生产兼容性或可靠的任意交互程序识别。
