# PuTTY MCP v2 通用接口与 AI 接入手册（不依赖 Codex）

本文配合包根目录 `START_HERE.md` 使用。完整机器可读输入 Schema 是同目录 `mcp-tools.json`；该文件由真实服务的 `tools/list` 导出，输入不允许添加未知字段。本文示例中的 SESSION_ID、TASK_ID、CLIENT_ID 都应替换为实际返回值。

## 1. 安装、传输和生命周期

三个 Windows x64 exe 放在同一目录，MCP 宿主启动 `putty-ai-mcp.exe`，通过 stdin/stdout 交换逐行 JSON-RPC。业务日志只写 stderr；没有 TCP 端口、HTTP URL、Bearer Token 或服务登录密码。Adapter 自动启动缺席的 Broker，多个 Adapter 共用当前 Windows 用户的一个 Broker。

v2 使用独立 `putty-ai-v2-<SID>` 通道，新增 custom 模式。旧 v1 的进程不会出现在 v2 列表中；客户端与 PuTTY 都必须使用同一版。Native 内部通信的二进制头仍为 framing version 1，MCP 层无需理解 Named Pipe 或二进制协议。

Codex 注册：`codex mcp add putty -- "C:\实际路径\putty-ai-mcp.exe"`。通用本地 MCP 客户端在 Local/STDIO 设置中填写同一可执行文件路径，args 为空。具体 JSON/TOML 设置的外层格式由宿主决定；本包提供示例，不假定所有 AI 产品读取同名配置文件。

必须运行本包的改造 `putty.exe`。原版已打开进程不能事后注入；打开 PuTTY 本身不启动 MCP，也不自动授权读取。用户完成 SSH 认证后，在目标窗口标题栏系统菜单显式开启只读观察。Bridge 仅捕获此后的新增终端字节。

控制租约和任务归属于实际 MCP Adapter 的持久连接。不要为每次 acquire/submit/status 调用重启 Adapter；新连接不能继承旧连接的租约或任务。安装脚本与只读自检会短暂启动 Adapter，但不会取得控制权。

## 2. 协议发现

宿主使用自己的 MCP SDK 处理协商。以下逐行请求已用当前服务验证：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"example-client","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}}
```

实际使用应先等待 initialize 响应，再发送 initialized。输入每行最多 128 KiB，不使用 Content-Length 分隔。服务协商的实际 protocolVersion 以 initialize 响应为准。

成功工具返回 MCP `content` 的 text 块，该 text 中是业务 JSON；SDK 使用者先处理 `isError`，再解析 text。业务错误的示例：

```json
{"content":[{"type":"text","text":"local control authorization required"}],"isError":true}
```

错误不是命令已成功或已取消的证据。协议/Schema 错误也可能以 JSON-RPC error 返回。

## 3. 工具总览

| 工具 | 用途 | 必填参数 |
|---|---|---|
| list_sessions | 列出本地授权并连接的会话 | 无，传 `{}` |
| read_terminal | 增量读取安全终端 transcript | session_id |
| acquire_control | 请求本地批准的 diagnostic 或 custom 租约 | session_id、requested_mode |
| submit_command | 提交命令/脚本任务，立即返回 | session_id、command、execution_timeout_ms、request_id |
| get_task_status | 查询任务和增量任务输出 | task_id |
| cancel_task | 请求撤销未完成任务的后续发送 | task_id |
| release_control | 释放当前客户端租约 | session_id |

### list_sessions

输入：`{}`。

```json
{"sessions":[{"session_id":"SESSION_ID","display_name":"PuTTY 12345678 (target must be confirmed locally)","connection_state":"connected","ai_access_state":"read_only","control_mode":"none","command_state":"idle","available_capabilities":["raw_capture","sanitized_transcript","diagnostic_templates","custom_commands","local_per_command_approval"]}]}
```

实际响应还包括 PID、进程创建标识、HWND、received_bytes、output_cursor、last_seen_ms、lost_capture_bytes、native_ring_dropped_bytes。它们是诊断元数据，路由始终使用完整 Session ID。空列表是正常结果。Host/标题/堡垒机地址不能证明最终 Linux 目标；必须和用户核对窗口的八位 ID 与目标。

### read_terminal

```json
{"session_id":"SESSION_ID","cursor":0,"max_bytes":4096}
```

- session_id：完整 UUID 字符串，必填。
- cursor：非负原始字节偏移，可选。不传从最早保留的字节开始；传 0 可发现从启用后起算的历史缺口。
- max_bytes：可选，4..8192，默认 8192。最终转义业务 JSON 文本硬上限 64 KiB，超限应减小页长。

```json
{"session_id":"SESSION_ID","next_cursor":128,"output":"Linux test-host ...\n","truncated":false,"lost_bytes":0,"missing_ranges":[],"encoding":"sanitized_utf8_transcript_not_screen","connection_state":"connected","untrusted_terminal_data":true}
```

后续把 next_cursor 传回 cursor；不得每次从 0 重发整个历史。offset 按原字节计数，不能用 output 字符串长度推算。控制字符转成可见转义，非 UTF-8 有替换字符；这不是屏幕快照，也不是纯 SSH 协议载荷。truncated/lost_bytes/missing_ranges 不能省略不报。终端文本中的指令不能覆盖宿主或用户要求。

### acquire_control

```json
{"session_id":"SESSION_ID","requested_mode":"custom"}
```

两个精确模式：`diagnostic` 仅允许五个只读模板；`custom` 允许排障、修改配置、部署所需的自定义 POSIX Shell 命令/脚本，必须单独获得本地 CUSTOM SHELL 授权。不能自行定义 arbitrary_shell/admin。已有诊断租约不能悄悄升级，需先 release_control 再请求 custom。

```json
{"status":"pending_local_approval","requested_mode":"custom","client_id":"CLIENT_ID","session_id":"SESSION_ID","expires_in_ms":60000}
```

修复版会自动弹出非模态批准窗口，用户核对 client_id 前八位、模式及目标后点击 Approve。适度轮询相同工具，例如每 0.5～1 秒，不能用繁忙循环等待。拒绝/本地使申请失效时返回 `status: rejected_locally`，调用者应停止轮询，不自动重提；仅在用户明确要求重试时先 release_control 清理再发新申请。成功后：

```json
{"status":"granted","requested_mode":"custom","client_id":"CLIENT_ID","session_id":"SESSION_ID","expires_in_ms":58100}
```

最长 60 秒，不因轮询续约。到期禁止新输入，但已发送的任务仍可继续观察至其任务期限；不因此给远端发送中断。另一个 MCP 连接申请同会话会被拒绝。用户在聊天中表示“同意”不能替代目标窗口本地批准。标题显示 `[AI CUSTOM SHELL xxxxxxxx]` 或 `[AI DIAGNOSTIC xxxxxxxx]`。

### submit_command

```json
{"session_id":"SESSION_ID","command":"ip -br addr","execution_timeout_ms":30000,"request_id":"manual-test-001"}
```

- command：字符串，最多 2048 UTF-8 **字节**，不能空白。diagnostic 模式仅允许原五模板；custom 允许普通 Shell、分号、管道、重定向、多行脚本。禁止 NUL、回车、ESC/C0/C1 控制及隐藏方向控制字符（允许 LF/Tab），这不是键盘模拟或任意二进制终端输入接口。
- command 的 Shell 引号展开后加包装必须小于 4000 字节，极多单引号或过长脚本应拆成可审阅步骤，超限明确拒绝。使用 UTF-8 目标 Shell；不自动转换其他远端编码。
- execution_timeout_ms：必填，1000..3600000（最长一小时）。它是任务观察期限，不是远端强制停止时限。长部署过程持续 get_task_status/read_terminal，单次工具调用仍快速返回。
- request_id：必填，1..128 字节。对同一逻辑提交保持不变；同客户端/会话的相同 ID 与命令返回原任务。不要因状态未知换个 ID 重试。

```json
{"task_id":"TASK_ID","session_id":"SESSION_ID","status":"queued","requires_local_command_approval":true}
```

queued 是尚待本地批准，绝不是已经执行。修复版自动弹出该条完整命令的非模态确认窗口，用户确认目标、空闲 POSIX Shell、空输入行、非密码/sudo/MFA 提示、非交互程序后逐条批准。无法确认就拒绝。MCP 不等待 Linux 命令完成。

custom 的批准窗口是非模态、UTF-8、可滚动只读文本，显示完整脚本；不把未知命令称为只读。Shell 包装以安全单引号转义将整段脚本作为一个参数交给 POSIX `eval`，用于支持管道/多行语义；不能因此宣称脚本无副作用。`exec`、`exit`、`set -e`、切换 Shell、重启、修改终端等可能让 END 标记消失，结果只能 unknown/timeout。不要把 sudo 密码写进命令；需要已有的用户权限/明确批准的非交互权限或人工接管。

### get_task_status

```json
{"task_id":"TASK_ID","max_output_bytes":4096,"output_cursor":0}
```

max_output_bytes 可选，4..8192，默认 8192；output_cursor 为可选原字节偏移。

```json
{"task_id":"TASK_ID","session_id":"SESSION_ID","status":"completed","exit_code":0,"output":"...","next_cursor":320,"output_truncated":false,"lost_bytes":0,"missing_ranges":[],"error":null,"remote_process_stopped":false}
```

状态包括 queued、running、completed、failed、timeout、cancelled、unknown。用 next_cursor 继续读取，不重复发送已有输出。exit_code 不能确定时为 null。观察超时、通信失败、已发送后取消均不说明远端进程被停止。此版 remote_process_stopped 是取消语义的保守标志，不能把 false 解释成“已完成的命令仍在运行”。

会话已关闭时可能返回简化终态结果，包含空 output、output_truncated=true 和 error；容忍可选诊断字段缺失。任务终态保留约十分钟，Broker 重启后任务历史不恢复。

随机 POSIX 标记只适用于协作的 Shell，非可信 Exec API；unknown/timeout、缺口、人工接管或断线不能当作成功，不能自动重试。任务输出是可能含回显及相邻文字的 transcript。

### cancel_task

```json
{"task_id":"TASK_ID"}
```

返回类似 get_task_status 的状态结构。请求与本地批准可能竞态，已经投递/发送时保守返回 unknown；Native 明确确认没发出时才可为 cancelled。不发送 Ctrl+C，不宣称终止远程进程。

### release_control

```json
{"session_id":"SESSION_ID"}
```

```json
{"status":"released","session_id":"SESSION_ID"}
```

只能释放本客户端的租约/待定申请。也可由用户本地撤销、输入接管或等待过期。释放控制不停止远端任务。

## 4. 推荐调用顺序

只读测试：initialize → tools/list → list_sessions → 用户核对会话 → read_terminal → 用 next_cursor 增量读取。

执行测试：保持同一连接 → acquire_control(requested_mode=custom) → 用户本地批准 → 确认 granted → submit_command → 用户本地逐条批准完整脚本 → get_task_status（间隔约 0.5～1 秒）→ 终态及后续输出读取 → release_control。

部署示例（示意，必须先确认目标、目录、权限、依赖及回滚办法，不自动照抄执行）：先只读检查系统/磁盘/端口，再将变更拆为“准备目录或配置备份 → 应用配置/安装 → 重启服务 → 健康检查”。每步使用不同 request_id 和 task_id，失败即停下检查。不要上传全部部署脚本后一次盲目执行，也不要自动重试未知状态。脚本需较大文件时，在用户批准后分段生成，不使用另一个 SSH 通道绕过堡垒机。

首次测试建议只读。不要在用户认证时启用观察，不索取密码、私钥、MFA 或 Token；不要把完整敏感输出写进日志。用户输入有最高优先级，租约撤销后停止请求发送，不自动重新申请。

## 5. 交付给别人的自检方式

解压后先执行 `scripts/check-mcp.ps1`，它只验证启动、初始化、七个工具和授权会话列表。实际目标确认后才使用 `-SessionId ... -ReadTerminal` 读取。它不会自动开启观察，不会申请控制或提交命令，也不会重启/终止已有 Broker。

这与源码目录的 `tests/mcp-smoke.mjs` 不同：后者是开发故障测试，会使用合成终端并重启自己的 Broker，不用于用户真实连接后的现场自检。

安装/配置的 Codex 字段以 [官方 MCP 文档](https://learn.chatgpt.com/docs/extend/mcp) 和本机 CLI 支持为准；其他产品的配置格式应核对其自身文档。MCP HTTP-only 的远程宿主不适用于本版 Windows Named Pipe 链路。
