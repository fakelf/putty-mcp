# QwenPaw 接入和连接自检

解压 `putty-ai-v4.4.0-x64.zip` 后可直接打开 `putty.exe`，也可用 MSI 安装到当前 Windows 用户的 `%LOCALAPPDATA%\Programs\PuTTY MCP`。安装后在开始菜单搜索「PuTTY MCP 连接助手」。运行需要 Windows x64 和 VC++ 2015–2022 x64 Runtime。

1. 在助手里点 **打开 PuTTY**。登录服务器后，从 PuTTY 标题栏左上角菜单开启 **AI 观察**。
2. 点 **复制 QwenPaw 配置**。到 QwenPaw 的 **智能体 → MCP**，为当前使用的智能体导入并启用。已有同名 `putty` 条目时，更新它的配置。QwenPaw 后端和 PuTTY 需要在同一台 Windows 电脑、同一个 Windows 用户下运行。[QwenPaw 官方配置说明](https://github.com/agentscope-ai/QwenPaw/blob/main/website/public/docs/mcp.zh.md)
3. 在 QwenPaw 对话中说「请调用 putty 的 list_sessions，告诉我授权窗口编号」。再点助手的 **连接自检**。

助手生成的 JSON 使用本机安装路径和 `stdio`，`args` 为空。可用脚本生成同一配置。

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-client-config.ps1 -Format qwenpaw
```

| 看到的阶段 | 含义 |
|---|---|
| MCP 程序位置或进程启动失败 | 先检查是否从开始菜单打开了已安装的助手，再根据报错检查 exe 路径和运行环境 |
| MCP 握手失败 | 本机子进程已启动，但初始化未完成。查看该行的错误 |
| Broker 后台失败 | 初始化已到达 MCP，连接本机后台时出错 |
| 十二个工具通过，PuTTY 会话为零 | MCP 已接通。检查是否使用助手打开的 PuTTY，SSH 是否登录并已启用观察 |
| 本机自检通过，QwenPaw 未收到启动记录 | 检查当前智能体是否启用新配置、QwenPaw 后端是否在同一台 Windows 和同一用户下，以及 QwenPaw 的启动日志 |
| QwenPaw 完成握手和工具发现，尚未查询会话 | 请让它调用一次 `list_sessions`，再刷新自检 |

自检分开显示本机探测与 QwenPaw 最近一次实际连接。QwenPaw 的状态记录由复制出的配置启用，保存在 `%LOCALAPPDATA%\PuTTY-AI-v4.4.0\qwenpaw-connection.json`。显示历史记录时，对应进程已经退出。多个 QwenPaw 连接共用最近一条记录。

先在包目录运行 `.\putty-ai-mcp.exe --connection-report off --text` 查看本机连接摘要。这条命令不会启动 Broker，适合排查宿主自动启动失败。若授权会话数大于零，但 QwenPaw 最后查到零个会话，请核对 QwenPaw 是否已使用 v4.4.0 适配器，然后让它重新调用 `list_sessions`。

本机自检可能按需启动 Broker。排查 QwenPaw 冷启动时，先在 QwenPaw 重试，记录报错，再运行自检。自检只调用初始化、工具发现和会话枚举。

如果 QwenPaw 后端在 Docker、WSL 或另一台机器上，控制台所在的浏览器无法替后端访问同用户 Windows 管道。使用 Windows 原生 QwenPaw 可直接用这份配置。

## 多子终端排查磁盘 I/O

在目标 PuTTY 主窗口左上角勾选 **AI multi-window mode (auto-run)**。让 QwenPaw 先用 `list_sessions` 核对窗口编号、`mode: multi_child`、`approval_mode: automatic` 和 `child_open` 能力。

> 请在这个主窗口打开两个子终端。确认两者都关联这个主窗口后，第一个采样设备 I/O，第二个采样进程 I/O。只发送实际采样命令。通过提示符判断任务完成，读取实际输出后按采样时间对齐结果。确认窗口空闲后关闭两个子终端。

设备侧可用 `TZ=UTC LC_ALL=C iostat -y -x -t 2 5`，进程侧可用 `TZ=UTC LC_ALL=C pidstat -d 2 5`。工具缺失时报告缺项。QwenPaw 负责分配任务和汇总，PuTTY MCP 负责打开窗口、发送命令和读取结果。**AUTO RUN** 下无需在 PuTTY 逐次批准，QwenPaw 的工具确认仍按自身设置生效。接口参数见 [AI 接入手册](ai-integration.md#多子终端模式)。

连接通过 SSH 提供终端的网络设备时，`submit_command.command` 只填设备本身的命令。若设备命令会切换模式并且已知新提示符，可提供 `expected_prompt`。遇到 H3C 分页时，QwenPaw 可读取同一任务的 `read_task_screen`，用 `request_task_action` 发送一个 `space`，再读下一帧。若分页是在 PuTTY 手动启动的，可以先 `attach_terminal`。看到 `completed` 后结合设备输出判断结果。命令任务没有退出码。
