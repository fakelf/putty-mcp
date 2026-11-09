# Windows 安装和 Codex 接入

使用 MSI 安装或解压 `putty-ai-v4.4.0-x64.zip`，两个 exe 保持同目录。打开本包的 PuTTY 手动登录 SSH。AI 宿主配置为 Windows 本地 stdio，`command` 指向 `putty-ai-mcp.exe` 的绝对路径，`args` 留空。客户端和终端在同一用户下运行。其他宿主的示例在 `examples/`。

在解压目录执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-mcp.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-mcp.ps1 -Register
```

第一条验证协议和十二个工具。第二条将此路径注册给 Codex，随后重启 MCP 客户端使工具重新加载。脚本默认只预览，明确加 `-Register` 才写 Codex 配置。升级时会识别并更新旧包的单行默认工具清单。若使用自定义 `enabled_tools`，按需要添加 `attach_terminal`、`request_task_action` 和 `read_task_screen`。示例把 `PUTTY_MCP_SOURCE` 设置为 Codex，也包含 Codex 自己的工具审批策略。PuTTY 的 **AUTO RUN** 仅影响 PuTTY 弹窗，宿主确认策略独立生效。

如果旧 Codex CLI 在读取配置时报 `unknown variant writes`，使用桌面应用当前配套的 CLI 或升级独立 CLI，再执行注册。新版示例采用 `auto` 和逐工具 `prompt`。配置字段见 [OpenAI 官方文档](https://learn.chatgpt.com/docs/config-file/config-reference)。

用本包 PuTTY 连好后开启观察，按窗口标题中的编号核对 `list_sessions`。先开 PuTTY 或先开 MCP 均可。升级时更新宿主中 `putty-ai-mcp.exe` 的路径并重启宿主，再用同包 v4.4.0 PuTTY 打开 SSH 会话。v4.4.0 使用独立管道，旧版进程可以继续运行原会话。

DeepSeek Harness 参见 [专用接入说明](deepseek-harness.md)。完整工具参数见 [AI 接入手册](ai-integration.md)。
