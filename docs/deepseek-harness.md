# DeepSeek Harness 接入 PuTTY MCP

本说明针对官方 `deepseek-ai/deepseek-harness`（`dsh`），配置格式按 2026-11-02 的上游文档核对。实际 Harness 聊天联调待现场验证。

## 1. 解压和运行位置

将整个运行包解压，例如得到 `C:\PuTTY-AI\putty.exe`。两个 exe 保持同目录。部署电脑需要 VC++ 2015–2022 x64 Runtime。

**运行 dsh 的后端进程须位于同一 Windows 本机、同一用户下。** 本包通过 Windows Named Pipe 连接现有 PuTTY。只在 Windows 浏览器打开远端 Harness 网页并不满足这个条件；WSL、Linux、Docker 后端也不能按本文直接连接。MCP 没有 HTTP 地址。Harness 自身的 DeepSeek 模型/API 配置仍由你现有的 Harness 管理。

## 2. 生成当前路径的 overlay

在解压目录打开 Windows PowerShell，执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-mcp.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-client-config.ps1 -Format deepseek -OutputPath .\putty.cordis.yml
```

自检通过时会返回 `passed: true` 和十二个工具。未开启观察时零会话正常。移动软件包后重新生成 overlay。

`examples/deepseek-harness.cordis.yml` 是模板；生成后的结构如下，command 是你本机的实际路径：

```yaml
- insert:
    - id: putty-mcp-local
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: putty
        transport: stdio
        command: 'C:\PuTTY-AI\putty-ai-mcp.exe'
        args: []
        env:
          PUTTY_MCP_SOURCE: 'DeepSeek Harness'
        toolCallTimeoutMs: 10000
```

这是 Harness 的 `--patch` 覆盖层，包含 `insert` 外层；不能把通用 `mcpServers` JSON 当作此文件。`toolCallTimeoutMs` 是单次工具调用超时，远程长任务由本包 task_id + 轮询观察。

## 3. 启动 Harness 并加载

已有官方 dsh 命令时，在解压目录执行：

```powershell
dsh --version
dsh web --patch "$PWD\putty.cordis.yml"
```

已有 Web 实例需要关闭并使用上面的参数重新启动；更换 overlay 不会自动给一个未加载它的进程增加工具。若使用官方 npm 入口，则对应命令为：

```powershell
npx @deepseek-ai/dsh web --patch "$PWD\putty.cordis.yml"
```

首次使用 npm 入口需要 Node.js 及网络。本文不自动安装/升级 Harness，也不更换已有模型配置。使用源码版时在 Harness 仓库按其说明构建后，执行 `pnpm dsh web --patch '本机生成的putty.cordis.yml绝对路径'`。

若想持久化，将这个单独的 insert patch 合并到实际 `$DSH_HOME/profiles/web/cordis.patch.yml`，仅影响 web profile；保留原有 patch。不要同时用持久配置和 `--patch` 重复注册同一个 `serverName`。可先使用 `dsh --profile web --patch 'overlay绝对路径' --dump-config` 检查组合配置；不要把含其他私密配置的完整 dump 上传给他人。

## 4. 给 AI 的内容

把包根目录 `FEED_TO_DEEPSEEK.txt` 中的目录占位符换成实际路径，粘贴给 Harness。若 AI 可读取本地文件，也可直接让它读取该文件。**提供本地路径和文档即可，不需要把 exe 上传到模型对话。** 仅上传文档不会创建工具；宿主必须先按以上步骤加载 MCP。

成功发现后，命名空间 `putty` 下有十二个工具。

| Harness 工具名 | 用途 |
|---|---|
| `mcp__putty__list_sessions` | 列出已授权会话 |
| `mcp__putty__read_terminal` | 增量读取输出 |
| `mcp__putty__attach_terminal` | 附加到已打开的前台终端 |
| `mcp__putty__submit_command` | 提交完整命令/脚本 |
| `mcp__putty__get_task_status` | 轮询任务及输出 |
| `mcp__putty__cancel_task` | 请求撤销待批命令 |
| `mcp__putty__interrupt_task` | 为当前运行任务请求一次 Ctrl+C |
| `mcp__putty__start_interactive` | 申请启动 `top` 或 `vi`，vi 可选择打开已有远端文件 |
| `mcp__putty__read_task_screen` | 读取前台终端或交互任务的文字帧 |
| `mcp__putty__request_task_action` | 请求按键、文本、释放前台控制或 vi 操作 |
| `mcp__putty__open_child_session` | 在处于多子终端模式的主窗口申请打开子终端 |
| `mcp__putty__close_child_session` | 申请关闭空闲子终端 |

用户用本包 PuTTY 手动连接服务器，到目标 Shell 后在标题栏菜单开启 AI 观察。第一句测试可说：

> 请使用 mcp__putty__list_sessions 列出当前授权的 PuTTY 会话，告诉我窗口编号，先不发送命令。

核对目标后，可说“使用这个会话执行 `df -h`”。普通模式的 PuTTY 会弹窗显示来源和完整命令。用户开启多窗口 **AUTO RUN** 后，PuTTY 校验请求并直接发送，Harness 的确认策略仍独立生效。中断当前运行任务时，AI 使用原 `task_id` 调用 `interrupt_task`，再用 `get_task_status` 查按键和任务结果。详细接口见 [AI 接入手册](ai-integration.md)。

## 5. 常见情况

- 工具缺失时，检查 overlay 是否加载、command 是否存在及 MCP 插件日志。
- 插件模块不存在：检查是否为官方完整 profile、版本是否匹配；根据上游版本文档修复插件依赖，不要只反复重启 PuTTY。
- 会话为空时，确认两边都来自本包，PuTTY 已开启 AI 观察，后端同机同用户。
- 普通模式的待批请求会弹窗。自动模式可用标题上的 **AUTO RUN** 和 `list_sessions.approval_mode` 核对。
- 断开/重载：Adapter 新连接不能继承旧控制与任务。未知结果需核查，不能自动再次提交原脚本。
- `submit_command` 直接发送命令正文和 Enter。设备自己的回显照常显示。任务以提示符判断完成，命令任务的 `exit_code` 为 `null`。

## 上游依据

- [官方项目与 npm 启动入口](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)
- [官方 MCP 客户端插件：字段、工具命名、重连行为](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/mcp/mcp-client/README.md)
- [官方第三方 MCP 指南：insert overlay、--patch、持久层](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/user/guide/mcp-memory.zh.md)
- [官方 CLI 配置层参考](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.zh.md)
