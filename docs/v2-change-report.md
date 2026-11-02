# v2 自定义命令、部署用途与通用 AI 接入

2026-11-02，按用户明确提出的“AI 通过 PuTTY 排障或部署系统”需求实现。原五个诊断模板仍保留，但不再是唯一执行方式。默认关闭/只读、本地授权、原 SSH/堡垒机路径、多会话隔离保持。

## 实际变化

- `acquire_control.requested_mode` 为 diagnostic/custom。custom 模式单独本地批准；不能从诊断权限自动升级。
- `submit_command.command` 为字符串。custom 支持命令、管道、重定向及多行 POSIX 脚本；两端都检查授权、长度及控制字符。
- 命令最大 2048 UTF-8 字节，单引号扩展后的包装行小于 4000 字节，保护终端常见 canonical 行限制。大部署按明确步骤提交，用户逐条批准。
- 任务观察超时 1000..3600000 ms（最长一小时）。输入控制租约仍最长 60 秒，到期禁止新的输入但不丢弃运行任务的输出观察。超时/断线不代表进程停止。
- 批准窗口使用 Unicode 可滚动只读区，展示完整脚本，非模态。人工接管使待批准任务失效。
- 新增 `ai_bridge/command.c`/`.h`，用 POSIX 单引号转义将整段脚本作为单个 eval 参数；Shell 标记仍只提供协作环境的完成观测。脚本可修改系统，不称作只读。exit/exec、交互密码、切换 Shell、断线会导致未知结果。
- v2 使用独立 Named Pipe `putty-ai-v2-<SID>` 和独立 LocalAppData 审计目录 PuTTY-AI-v2，避免旧版混用及轮转冲突。
- Broker 自动启动改用 CreateProcessW 且 bInheritHandles=FALSE，避免继承启动它的 PowerShell/.NET 管道使外层命令不退出；只创建隐藏后台进程，不等待或关闭 PuTTY。
- 新程序和源码分别交付为 `putty-ai-v2-x64.zip`、`putty-ai-v2-source.zip`；旧目录/旧包没有覆盖，原有已打开窗口不被强制退出。

## 通用 MCP 客户端

不是 Codex 专用系统。任意能启动本机 Windows stdio MCP 的宿主都可用：Command=同包 `putty-ai-mcp.exe` 绝对路径，Args=[]，无 Token。先 `initialize` / `notifications/initialized` / `tools/list`，再调用七个自描述工具。宿主需保持进程连接，因为租约与任务绑定实际客户端连接。

包根 `START_HERE.md` 是给用户/其他 AI 的入口；`docs/ai-integration.md` 列出完整参数、返回和顺序；`docs/mcp-tools.json` 是真实工具 Schema；`examples/mcp-client.json` 是通用 JSON 形式示例；`scripts/generate-client-config.ps1` 打印当前解压路径配置，绝不自行修改宿主配置。安装步骤不要求接收方拥有本聊天历史。

## 验证范围

v2 Windows PuTTY、Broker、Adapter 已实际构建。当前修复版新增控制拒绝清理测试，连同 custom 权限分离、控制字符/长度限制、长任务租约到期保持观察测试，整体 17 项（12 Broker、4 common、1 Windows 管道压力/故障）。批准菜单问题及自动弹窗修复另见 approval-fix.md。

新增 C `test_ai_command` 校验 UTF-8、控制字符、单引号展开和长度；`tests/shell-quoting.mjs` 把实际 C 生成的包装交给本机 Git Bash 执行，验证单引号、管道、多行 UTF-8、临时目录内脚本重定向和非零退出码。它只改测试临时目录，不连接任何服务器。

扩展真实 C Bridge → 真实 Windows Broker → 两个真实 MCP Adapter 测试，验证 diagnostic 不能执行 ip、custom 单独批准、ip/管道/Unicode 脚本入队完成、客户端互斥、控制字符拒绝、人工接管待批准命令以及旧故障隔离测试。结果见 mcp-smoke-results.json、custom-shell-results.json 和 rust-tests.txt。

`tests/broker-autostart.mjs` 另验证首次自动启动 Broker 后 Windows PowerShell 能有界退出，并可由第二个 Adapter 继续访问独立 Broker；结果见 broker-autostart-results.json。该回归在隔离测试之后运行，会留下它启动的空闲 Broker。

**尚未实测**：用户目标 Linux、企业堡垒机、真实 sudo、真实服务部署及长达一小时的真实运行。自动化用合成终端及本机 POSIX Shell，不能据此宣称已在用户服务器部署成功。真实 ip 等命令需用户用新版 PuTTY 登录并开启观察，核对 Session ID 后批准测试。
