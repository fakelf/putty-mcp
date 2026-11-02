# 实际验证报告

**最终交付补充：** `live-validation.md` 记录随后由用户本地批准完成的真实 Linux SSH 测试；本文件下面的自动化与性能数据仍保持其原有范围。最终打包使用同一组已测程序，另补 DeepSeek Harness 配置与文档；Harness 本身尚未做端到端联调。

**v2 修复版增补：** 当前版本增加单独批准的 custom 命令、管道/脚本、最长一小时观察和通用 AI 接入文档，并修复批准菜单变灰后的状态同步和自动弹窗，详见 `v2-change-report.md`、`approval-fix.md`。下方原始基线/性能表是 v1 留存结果；当前源码的最新测试原始输出在 `rust-tests.txt`、`mcp-smoke-results.json` 和 `custom-shell-results.json`。当前 Rust 测试为 17 项，并增加了本机 POSIX Shell 包装执行测试；不能将旧性能数据视为当前版的新增性能证明。

验证日期：2026-11-02，Windows x64，Asia/Shanghai。测试过程不使用生产服务器、Web Token、账号密码或企业 SSH 会话。

## 构建与静态检查

| 项目 | 结果 |
|---|---|
| 未修改 PuTTY 0.85 x64 Release | MSVC 实际编译通过，`build-baseline/Release/putty.exe` |
| 带 Bridge 的 PuTTY x64 Release | 实际编译通过，`build-ai/Release/putty.exe` |
| `PUTTY_AI_BRIDGE=OFF` | 独立配置和完整 x64 Release 编译通过，`build-off/Release/putty.exe` |
| Rust Broker / MCP Adapter | 实际 release 构建通过，Cargo.lock 固定依赖，官方 rmcp 3.8.0 |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，无 Rust 警告 |
| C 原始构建警告 | 原有 LNK4075 MANIFEST 配置警告，AI 和原版相同；未伪装为零警告 |
| 运行依赖检查 | dumpbin 确认 VCRUNTIME140.dll、系统 UCRT 及 Windows 系统库；没有额外 SSH 客户端依赖 |

原始可执行文件 SHA-256：`349DFA079845FAC4849294B19D267FBDD5A1CA7DCC0AF9078EDEFCA7B6BD4B89`。交付的三个程序哈希另在运行包 `SHA256.json` 中。

## 自动化测试

`scripts/test.ps1` 整体退出码 0。原始输出见 `rust-tests.txt`、`native-ring-results.json`、`mcp-smoke-results.json`；Schema 由实际 MCP 的 tools/list 导出。

Rust 共 13 项：8 项 Broker 状态/权限测试，4 项公共协议/文本/缓冲测试，1 项真实 Windows Named Pipe 集成测试。后者包含 20 个并发模拟 Native 会话、两个独立客户端、完整 80 MiB 数据验证，以及真实子进程故障。

- 验证未授权输出不可读、无效/关闭 Session ID 拒绝、不同会话内容不混淆、独立游标和重复读取不产生历史重放。
- 验证精确模板白名单；多命令、管道、重定向、sudo、命令替换和换行均拒绝；重复 request_id 幂等、不同命令复用 ID 拒绝。
- 验证同会话写入互斥、任务归属、租约过期/人工撤销、MCP 连接断开清理。
- 验证大输出和跨块完成标记、回显中的文字转义不是标记、丢失输出导致 unknown、超时不伪造退出码、取消不宣称停止远端。
- 验证 UTF-8 跨回调、ANSI/C0 安全转义、缓冲历史淘汰、丢失范围、非法帧长度。
- 实际管道拒绝伪造注册 PID；重复 Broker 启动失败，不能替换已有管道。
- 实际 Broker 被 kill 后旧 RPC 失效，重启后不恢复旧权限或任务。

C `test_ai_ring` 验证确定性溢出、跳号、槽位消费、并发生产/消费完整性并执行微基准。

`mcp-smoke.mjs` 使用两个实际 rmcp stdio Adapter、实际 Rust Broker 和实际 C Bridge 工作线程。它验证初始化、七个工具与 Schema、C 数据旁路、增量游标、客户端租约互斥、立即返回任务、合成任务完成、人为接管、Native 缓冲溢出、Broker 崩溃/恢复、Adapter 崩溃隔离、关闭注销和新 ID。测试还保持实际非模态批准窗口处于待定状态，确认终端事件计数继续增加，再通过真实批准回调和输入包装完成合成命令。合成 C 前端**没有 SSH Backend**，本地批准由测试程序模拟；生产 putty.exe 不含自动批准代码。

## 性能测量

以下是本机单次采样，不构成性能提升或真实网络吞吐证明。

| 测量 | 结果与范围 |
|---|---|
| 20 模拟会话 → 真实 Windows Broker | 80 MiB，0.1955 s，409.22 MiB/s；所有会话达到指定收取字节数且读取内容逐一匹配，历史溢出均披露 |
| 同次 Broker CPU | 0.203125 CPU s；多线程累计 CPU 时间，不能直接当作整机 CPU 百分比 |
| 同次 Broker 峰值 Working Set | 52,178,944 bytes（约 49.76 MiB），包含 20 个会话有界历史；不含测试进程 |
| C SPSC 缓冲微基准 | 1 GiB，0.039652 s，25,824.41 MiB/s，平均每次 16 KiB push+pop 0.605 µs；缓存内复制，不是 SSH/IPC 吞吐 |
| 同次 Native 缓冲占用 | 2,099,200 bytes（2 MiB 数据加槽元数据） |
| 实际 MCP submit 返回 | 约 0.400 ms；远端任务没有在工具调用中等待完成 |
| Broker 离线时合成 C 前端事件响应 | ping 往返约 0.225 ms；证明该次 IPC 故障未阻塞测试 GUI，不代替真实 SSH GUI 的全部故障测量 |
| Native 故意灌入 16 MiB | 丢弃约 14,581,760 bytes，明确计数并报告输出缺口；原模拟事件循环仍响应 |

原版与改造版 AI 关闭的实际 PuTTY GUI 对比见 `ui-benchmark-results.json`。本地回环 Raw Backend 发送合成文字，分别 100 次 WM_NULL 探测；窗口隐藏，不涉及堡垒机：

| 指标 | 原版 0.85 | 改造版，AI 关闭 |
|---|---:|---:|
| 测量时长 | 3.262 s | 3.219 s |
| 累计 CPU | 2.031 s | 1.625 s |
| 峰值 Working Set | 20,721,664 B | 20,914,176 B |
| 事件响应 p50 | 0.120 ms | 0.178 ms |
| 事件响应 p95 | 6.141 ms | 7.369 ms |
| 事件响应最大值 | 11.669 ms | 11.816 ms |
| 回环 TCP 提供速率 | 0.948 MiB/s | 0.963 MiB/s |

提供速率只计测试端成功提交的 TCP 字节，不能当作已经渲染的吞吐。两次负载、调度、缓存存在差异；这些数字不能证明改造使 CPU 更低。复测命令 `node tests/ui-benchmark.mjs`（在本目录执行）需要先有原始基线程序。

## 验证边界

- **已通过：** Windows 真编译、公共/状态单元测试、模拟终端状态、真实本机 Named Pipe、真实 C 工作线程、真实 rmcp stdio、20 模拟会话压力、原版和 AI 关闭 GUI 的本机探测。
- **尚未验证：** 企业 Web 启动器、真实堡垒机认证/审计、真实 Linux 模板执行和真实 POSIX Shell 标记、真实 20 个 SSH 窗口、真实 SSH 开启 AI 时 CPU/内存/吞吐/输入延迟、跨用户账户 ACL 拒绝测试、针对恶意同 SID 进程的隔离（后者不在设计边界）。
- 不把模拟批准当作用户批准 UI 的自动化验收；GUI 本地菜单、对话和实际 Shell 状态需企业非生产试点中人工验证。
- 首次源码验证使用隔离 CODEX_HOME，配置解析结果存于 `codex-config-validation.json`。之后按用户明确安装指令注册了本机 putty，新增协议初始化、七工具发现及只读会话列表检查，结果见 `installed-mcp-check.json`。注册前备份了原配置，不向交付包复制用户配置或备份。

