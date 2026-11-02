# 安装、通用 AI 客户端接入和故障排查（v2）

本项目不依赖 Codex。对于任何支持 **Windows 本地 stdio MCP** 的 AI 宿主，新增 Local/STDIO 服务，Command 为同包 `putty-ai-mcp.exe` 的绝对路径、Args 为空即可。常见 JSON 形式见 `examples/mcp-client.json`；完整接口、请求返回与调用顺序见 `docs/ai-integration.md`。只支持 HTTP URL 的网页客户端不能直接使用本包。

v2 支持单独授权的 custom 模式，可提交故障排查/部署命令及多行脚本，并在目标窗口逐条批准完整文本。v2 三个程序要配套使用，管道为 putty-ai-v2，与旧版隔离；旧窗口不能热升级，需用新版 PuTTY 新开连接。旧包保留，不自动中断旧 SSH。

当前修复版为 `putty-ai-v2-fixed-x64`：申请和命令会自动弹出批准窗口，无需手动寻找批准菜单。灰色菜单若提示已有批准窗口，切到该独立小窗口；提示 no pending 表示当前无申请。rejected_locally 要停止轮询，用户明确重试后才能释放并重申。

## 环境与部署

仅支持 Windows 本机 x64。已使用 MSVC、Rust MSVC 和本机 `codex-cli 0.166.0-alpha.5.1` 验证。Linux/WSL/容器/远端 Codex 无法直接访问本机当前用户 Named Pipe；本版不提供转发、远程管道、TCP 或 HTTP 后门。应在与 PuTTY 相同的 Windows 用户下运行本机 Adapter；不能把 Windows Server 路径当作 WSL 可执行配置。

保留 `putty.exe` 的原 Web 堡垒机启动参数、协议关联、授权和审计链路。不要用另一个 SSH 客户端重连来代替已有 PuTTY 会话。新程序没有数字签名；如企业启动器检查签名/哈希，先由管理员完成测试和签名/白名单部署。不要直接覆盖正在使用的原程序，先保留可回滚版本。

## 注册 MCP

运行包的三个 exe 放在同一个目录。例如 `C:\PuTTY-AI`：

```powershell
codex mcp add putty -- 'C:\PuTTY-AI\putty-ai-mcp.exe'
codex mcp list
codex mcp list --json
```

`mcp list` 验证配置存在，不代表已完成协议或 SSH 验证。此 stdio 服务没有 OAuth，列表中的 `auth_status: unsupported` 是正常结果，真正授权在 PuTTY 本地执行。使用 Codex `/mcp` 检查初始化，再调用 `list_sessions`。未启用观察时返回空列表是正常行为。

源码和运行包都有 `scripts/install-mcp.ps1`：不带 `-Register` 只显示命令，用户自行明确加该开关才修改真实配置。写入前备份原配置；同路径已启用时保留已有策略。用户在本聊天明确要求安装后，已在本机注册 putty 并核验，现场只读检查结果见 `installed-mcp-check.json`。本机配置备份留在用户配置目录，不随包分发。

将 `examples/config.toml` 中的 command 改为绝对路径，人工合并到用户配置；运行包根目录的 `config.toml` 已填入本工作区实际构建路径，移动目录后应改写。不要覆盖现有整份配置。

示例包括 enabled、required、startup_timeout_sec、tool_timeout_sec、enabled_tools、default_tools_approval_mode 和 tools.<tool>.approval_mode。写入相关工具使用独立 prompt 策略，PuTTY 本地授权仍是实际执行边界。若只需观察，把 enabled_tools 改成 `["list_sessions", "read_terminal"]`。不要通过全局关闭审批来绕过本地授权。

这些字段已核对 [官方 MCP 文档](https://learn.chatgpt.com/docs/extend/mcp) 与 [官方配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)。本机 `codex mcp add --help` 确认支持 `codex mcp add <NAME> -- <COMMAND>`；完整示例另以隔离 CODEX_HOME 执行 `codex mcp list --json` 验证。不同版本应重新核对字段，不能假定任意旧 CLI 都支持逐工具审批。

把 `examples/operations/AGENTS.md` 放在专门运维工作目录，让 Codex 在该目录中运行。规则强调目标核对、输出不可信、敏感信息、模板限制、未知结果不可重试和人工优先。

## 日常操作

在堡垒机认证后、确认目标且没有秘密内容时，鼠标点击目标窗口**标题栏左上图标**进入系统菜单，显式开启观察。标题上的 8 位 ID 与 list_sessions 对应；不要根据同样的主机名或当前焦点猜目标。

控制申请返回 pending_local_approval 后核对 client_id 及模式，批准对应本地菜单项，再次 acquire_control 查询 granted。诊断用 requested_mode=diagnostic，排障/部署自定义脚本用 requested_mode=custom。submit_command 会返回 queued；用户再批准完整脚本。脚本不超过 2048 UTF-8 字节，包装行小于 4000 字节；大部署拆步骤。观察期最长一小时，租约 60 秒仅控制新输入，不停止已发任务。只允许空闲、空输入行的 POSIX Shell。若已有输入、密码/sudo/MFA 提示或运行 vim/top/less，先拒绝；确认恢复到正确状态后才重新申请。

终端右键、键盘、粘贴、恢复连接或认证会撤销/关闭 AI。请用标题栏图标访问批准菜单，避免 Alt 或终端右键被作为人工接管。人工需要接管时直接正常操作即可，不会吞掉输入。

## 验证与测试

源码工作区运行 `scripts/build.ps1 -RunTests`。`tests/mcp-smoke.mjs` 会启动真实 C 测试 Bridge、两个真实 MCP Adapter、真实 Named Pipe Broker，并导出实际工具 Schema。测试前停止自己正在使用的 Broker；已有 Broker 时隔离测试会拒绝继续。测试程序没有 SSH Backend，不连接任何生产服务器。

真实环境验收应使用企业批准的非生产主机，依次验证 Web 自动启动、原认证/审计、20 个窗口的 ID 对应、只读观察、逐条批准、人工接管、Broker/Adapter 故障及输出压力。逐项记录真实终端响应与 CPU/内存，不能用本项目模拟数字替代。

## 常见问题

| 现象 | 检查 |
|---|---|
| 空会话列表 | 是否启动本次 putty.exe、SSH 已认证、本地明确开启观察、Adapter 与 PuTTY 同用户 |
| 本地观察菜单不能启用 | 是否仍在认证、非 SSH、Backend 不可发送、备用屏幕/原始鼠标、粘贴中或已断线 |
| acquire_control 一直 pending | 用目标窗口标题栏图标打开菜单，核对 client_id；不是在 Codex 中回复“同意”就能授予本地控制 |
| 命令一直 queued | 还需逐条本地批准；60 秒控制租约可能已过期；重新核对目标和 Shell 状态 |
| unknown / timeout | 远端可能仍运行；不要自动重试；人工核查并接管后再重新申请 |
| IPC 断线 | 检查 Broker 是否存在、SID/ACL、是否有重复 Broker；Native 退避最多 5 秒；旧租约和任务不会恢复 |
| 审计故障拒绝写入 | 检查当前用户 LocalAppData 空间、权限和审计目录；修复后重启 Broker，仅影响 AI，PuTTY SSH 不需重启 |
| 文本含可见 ANSI 转义 | 此为安全 transcript，非屏幕快照；不解释控制码，不保证进度条布局 |

## 禁用与卸载

每个 PuTTY 窗口点击 AI 状态菜单关闭采集；AI 资源由工作线程异步退出回收，GUI 不等待。此后原 SSH 继续。`PUTTY_AI_BRIDGE=OFF` 可编译完全不含 Bridge 的程序。

在用户配置将 `[mcp_servers.putty] enabled=false` 或由用户执行 `codex mcp remove putty`，然后重启 Codex 会话。Broker 独立运行，可在任务管理器确认路径后结束对应 `putty-ai-broker.exe`；不会结束 PuTTY SSH。卸载运行目录并按需删除本工具自己的 LocalAppData 审计目录即可。程序没有安装服务、计划任务、驱动、服务器 Agent 或修改远端 SSH 配置。

