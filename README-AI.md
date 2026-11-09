# PuTTY MCP v4.4.0

基于 PuTTY 0.85 的 Windows x64 集成，让 AI 操作你已经连接并明确开启观察的 SSH 终端。使用入口见 [START_HERE.md](START_HERE.md)。

免安装包是 `dist/putty-ai-v4.4.0-x64.zip`。运行程序为 `putty.exe` 和 `putty-ai-mcp.exe`。MCP 宿主按本地 stdio 启动后者，启动参数留空。同一程序按需启动独立 Broker，多个 MCP 客户端和 PuTTY 窗口可以共用。PuTTY 与 MCP 可按任意顺序启动。

PuTTY 默认逐条批准。审批展示客户端来源、连接编号、目标会话和完整脚本。用户可从左上角启用多窗口 **AUTO RUN**，在当前主窗口及其 AI 子终端直接执行支持的操作。AI 宿主的工具确认仍按自身配置执行。运行中的任务可以请求发送一次 Ctrl+C，按键发送与远端退出分开核对。

PuTTY 标题栏菜单可开启 AI 和切换普通或多子终端模式。`submit_command` 直接发送命令正文和 Enter，终端显示远端自身的回显。任务根据提示符判断是否完成，命令任务的退出码为空。v4.4.0 使用独立本机管道。升级时请同时换用两个 v4.4.0 程序并更新宿主配置。

`attach_terminal` 可附加到主窗口或子窗口已经打开的前台终端。AI 可以读取画面、发送单键或组合键、输入文本和执行括号粘贴。H3C 分页时可以发送一个空格翻页。`detach` 或用户在窗口输入会交回控制，原 SSH 连接继续运行。

多子终端模式可在同一条已认证的 SSH 连接上打开最多四个子终端。每个子终端有自己的黑色窗口和会话 ID。新窗口优先放在与现有窗口重叠较少的位置，主窗口和已有子窗口保持原位。切回普通模式后主窗口恢复逐条批准，现有子窗口转为手动使用。

`top` 和 `vi` 在主 PuTTY 窗口的原终端区域显示，任务通道结束后自动恢复原 Shell 画面。vi 支持临时缓冲区，也可指定远端已有文件。导航、插入 UTF-8 文本、撤销、重做、保存和退出均可通过 MCP 请求。输出分页会保留敏感行的脱敏，任务退出结果和输出是否丢失分别报告。

- [Codex 注册和启动](docs/installation.md)
- [QwenPaw 接入和连接自检](docs/qwenpaw.md)
- [DeepSeek Harness 接入](docs/deepseek-harness.md)
- [工具接口和调用顺序](docs/ai-integration.md)
- [MCP 联调结果](docs/mcp-smoke-results.json)和[命令字节验证](docs/custom-shell-results.json)
- [架构与 IPC](docs/architecture.md)

## 构建

准备 Windows x64、Visual Studio 2022 C++ 工具、Windows SDK、CMake 和 Rust MSVC 工具链。测试需要 Node.js。安装 Git Bash 时也可验证直接发送的命令在本地 Shell 中的解释结果。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1 -RunTests
```

测试使用独立的 v4.4.0 Broker 和合成终端。运行隔离测试时应先退出自己启动的同版 Broker。发布脚本会生成运行包和源码包。

运行程序构建后，可用 WiX 4.0.6 生成 MSI。

```powershell
dotnet tool install wix --version 4.0.6 --tool-path .\.build-tools\wix4 --allow-roll-forward
.\scripts\build-installer.ps1 -WixPath .\.build-tools\wix4\wix.exe
```
