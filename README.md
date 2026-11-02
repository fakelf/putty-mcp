# PuTTY MCP

基于 PuTTY 0.85 的 Windows x64 MCP 集成，让 AI 在用户已登录的 PuTTY SSH 会话中读取输出、执行排障命令和部署脚本。

用户继续手动连接服务器和完成认证。MCP 复用已授权的终端，不另建 SSH 连接，不向远端安装 Agent。项目独立于 Codex，也提供官方 DeepSeek Harness 的接入模板。

## 下载与开始使用

从 [Releases](https://github.com/fakelf/putty-mcp/releases) 下载 `putty-ai-final-x64.zip`，完整解压后阅读 `START_HERE.md`。三个程序须保持同目录：

- `putty.exe`：包含 Native Bridge 的 PuTTY。
- `putty-ai-mcp.exe`：由 AI 宿主启动的本地 stdio MCP 服务。
- `putty-ai-broker.exe`：由 MCP 服务按需启动的同用户会话 Broker。

运行需要 Windows x64 和 Microsoft Visual C++ 2015–2022 x64 Runtime。宿主后端必须与 PuTTY 同机、同一 Windows 用户。没有 HTTP MCP 端点。

在解压目录验证协议连接：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-mcp.ps1
```

这一步不发送远端命令、不读取终端内容。随后用本包 PuTTY 连接服务器，在标题栏系统菜单开启 AI 观察；核对窗口中的会话编号后再让 AI 使用。

## DeepSeek Harness

在运行包解压目录生成当前路径的 Cordis overlay，再启动官方 `dsh`：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-client-config.ps1 -Format deepseek -OutputPath .\putty.cordis.yml
dsh web --patch "$PWD\putty.cordis.yml"
```

将 `FEED_TO_DEEPSEEK.txt` 的目录占位符替换成实际解压路径，再把全文交给 AI。详细步骤和官方依据见 [DeepSeek Harness 接入文档](docs/deepseek-harness.md)。本地 MCP 和真实 SSH 执行已验证，Harness 尚未端到端联调。

## 工具与授权

提供 `list_sessions`、`read_terminal`、`acquire_control`、`submit_command`、`get_task_status`、`cancel_task`、`release_control` 七个工具。

默认不观察终端。用户开启观察后，只捕获之后的新输出。执行任意 Shell 命令需要单独批准 `custom` 控制模式，并在 PuTTY 本地逐条批准完整脚本。任务异步返回 task_id，通过增量轮询取得结果；超时、断线或未知结果不能自动重试。

每次脚本最多 2048 UTF-8 字节；不提供交互按键或密码/MFA 自动输入。终端保留 printf/eval 包装及随机 PAI 完成标记回显。输出是 transcript，非屏幕快照；丢失范围会显式报告。取消不保证停止已发到远端的进程。

- [安装与使用入口](START_HERE.md)
- [完整接口与调用顺序](docs/ai-integration.md)
- [机器可读工具 Schema](docs/mcp-tools.json)
- [架构](docs/architecture.md)
- [验证报告](docs/test-report.md) / [真实 SSH 测试范围](docs/live-validation.md)
- [限制](docs/known-limitations.md)

## 从源码构建

需要 Windows x64、Visual Studio 2022 C++ 工具与 Windows SDK、CMake、Rust MSVC 工具链。测试还需要 Node.js，Shell 引号测试需要 Git Bash。请在隔离环境运行完整测试，避免已有生产 Broker 或 SSH 会话参与故障注入测试。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build.ps1 -RunTests
```

运行包及源码包生成于 `dist/`。源码主体在 `putty-0.85/`，Rust 工作区在 `putty-ai-rs/`。关闭原生集成可使用 CMake 参数 `-DPUTTY_AI_BRIDGE=OFF`。

## 许可与来源

本项目包含上游 PuTTY 0.85 源码及修改，并保留 [PuTTY 许可证](LICENCE)。Rust 依赖许可证见 [third-party-licenses.txt](docs/third-party-licenses.txt)。本项目不是 PuTTY 或 DeepSeek 的官方发行版。

公开副本将测试报告中的开发机路径替换为示例路径；测试结果与程序二进制保持不变。不包含账号配置、认证凭据、实际终端记录或构建缓存。
