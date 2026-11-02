# DeepSeek Harness 接入 PuTTY MCP

本说明针对官方 `deepseek-ai/deepseek-harness`（`dsh`），按 2026-11-02 的上游文档核对。自定义同名宿主请按它实际支持的 MCP 设置适配。本机未安装 dsh，因此尚未做 Harness 端到端联调；本包的 stdio 握手、七工具发现及实际 PuTTY/Linux 执行已分别验证。后续版本如改变配置格式，应核对 `dsh --help` 和文末来源。

## 1. 解压和运行位置

将整个运行包解压，例如得到 `C:\PuTTY-AI\putty.exe`。三个 exe 保持同目录；部署电脑需要 VC++ 2015–2022 x64 Runtime。无需编译器、Rust 或 Python。

**运行 dsh 的后端进程须位于同一 Windows 本机、同一用户下。** 本包通过 Windows Named Pipe 连接现有 PuTTY。只在 Windows 浏览器打开远端 Harness 网页并不满足这个条件；WSL、Linux、Docker 后端也不能按本文直接连接。MCP 没有 HTTP 地址。Harness 自身的 DeepSeek 模型/API 配置仍由你现有的 Harness 管理。

## 2. 生成当前路径的 overlay

在解压目录打开 Windows PowerShell，执行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-mcp.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-client-config.ps1 -Format deepseek -OutputPath .\putty.cordis.yml
```

自检不发送远程命令，也不读取终端内容；应返回 `passed: true` 和七个工具。未开启观察时零会话正常。生成器只写你指定的 overlay 文件，不改 Harness 的现有配置。移动软件包后重新生成路径。

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
        env: {}
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

成功发现后，应出现以下七个工具（命名空间由 serverName 决定）：

| Harness 工具名 | 用途 |
|---|---|
| `mcp__putty__list_sessions` | 列出已授权会话 |
| `mcp__putty__read_terminal` | 增量读取输出 |
| `mcp__putty__acquire_control` | 申请本地控制批准 |
| `mcp__putty__submit_command` | 提交完整命令/脚本 |
| `mcp__putty__get_task_status` | 轮询任务及输出 |
| `mcp__putty__cancel_task` | 请求取消，不能保证停止已发远端进程 |
| `mcp__putty__release_control` | 释放控制 |

用户用本包 PuTTY 手动连接服务器，到目标 Shell 后在标题栏菜单开启 AI 观察。第一句测试可说：

> 请使用 mcp__putty__list_sessions 列出当前授权的 PuTTY 会话，告诉我窗口编号，先不发送命令。

核对目标后，可说“使用这个会话执行 `df -h`”。AI 的调用顺序、精确参数及错误处理见 `ai-integration.md` 和 `mcp-tools.json`。当前版本先批准 custom 控制，再逐条批准脚本；本包没有一次授权后永久自动执行的功能。

## 5. 常见情况

- 找不到七个工具：检查 overlay 是否加载、command 是否存在、MCP 插件是否报错；实际发现成功前不能声称安装完成。
- 插件模块不存在：检查是否为官方完整 profile、版本是否匹配；根据上游版本文档修复插件依赖，不要只反复重启 PuTTY。
- 会话为空：确认运行本包 PuTTY，已开启 AI 观察，后端同机同用户；普通版或旧 v1 窗口不适用。
- 批准项目灰色：待定请求会自动弹出批准窗口；灰色菜单可能表示没有待批准内容或批准窗口已打开，详见 `approval-fix.md`。
- 断开/重载：Adapter 新连接不能继承旧控制与任务。未知结果需核查，不能自动再次提交原脚本。
- PuTTY 出现很长的 printf/eval 命令：这是实际 Shell 包装，用随机 PAI 标记分隔输出并取退出码；不是额外 SSH 连接或后台上传。原始 transcript 保留这些内容。

## 上游依据

- [官方项目与 npm 启动入口](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)
- [官方 MCP 客户端插件：字段、工具命名、重连行为](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/mcp/mcp-client/README.md)
- [官方第三方 MCP 指南：insert overlay、--patch、持久层](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/user/guide/mcp-memory.zh.md)
- [官方 CLI 配置层参考](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.zh.md)
