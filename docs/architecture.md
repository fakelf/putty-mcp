# 架构、协议与状态机

本文为 v2：任意支持 Windows 本机 stdio MCP 的 AI 客户端可使用。Codex 仅是示例宿主；v2 增加 custom 命令授权，不再只支持模板。

## 数据和线程

```
Codex -- MCP JSON-RPC stdio --> rmcp Adapter (Rust/Tokio)
       -- 本地当前用户 Named Pipe --> 独立 Broker (Rust/Tokio)
       -- 同一 Named Pipe 上独立连接 --> 每个 PuTTY 的 C 工作线程
       -- SPSC 输出副本 / PostMessage + 有界单槽控制投递 --> PuTTY GUI
       -- 现有 Ldisc / SSH Backend --> 原堡垒机 --> Linux
```

PuTTY GUI 仅执行预分配有界复制、Interlocked 操作和必要的事件通知；不执行 Named Pipe I/O、JSON、模型调用、网络请求、日志扫描或线程 join。原始终端仍收到原字节并按原逻辑显示，AI 缓冲耗尽只丢弃副本。工作线程只持有自己的 Bridge 内存和 HWND，不持有或调用 Terminal、Ldisc、Backend。

每个启用观察的窗口创建 128 个 16 KiB 槽，约 2 MiB；`PUTTY_AI_RING_SLOTS` 可在启动程序前设置为 16..1024，仅影响后续显式启用操作。每次输出回调最多复制 64 KiB，超出或溢出的字节推进原始序号并计入丢弃。SPSC 的单生产者是 PuTTY GUI，消费者仅一个 IPC 线程；Interlocked 的发布/消费顺序防止读取未完成写入的槽。

工作线程每批最多排空 16 个数据块，使用事件唤醒，空闲时 250 ms 心跳。I/O 使用 Overlapped；每次收发截止 2 秒，等待集合含停止事件。Broker 离线重连退避为 250 ms 至 5 秒。退出通过 GUI/工作线程引用计数异步回收，GUI 不等待网络或工作线程。新增的观察和执行批准窗口全部为**非模态**：用户不回应时也不会接管或暂停 PuTTY 的事件循环。批准回调仍在实际发送前重新校验在线状态、授权代次、截止时间和 Backend 状态；重连、认证和退出销毁旧批准窗口。

Broker 仅参与 AI 副本和受控输入，不参与原 SSH 传输。Adapter 与 Broker 生命周期分离；Adapter 可按需启动 Broker，退出时不会终止它。连接异常不会自动重试原命令；后续新请求可以重新连接 Broker。

## 身份与权限

- Named Pipe 名称 `putty-ai-v2-<SID>`；与旧 v1 隔离。DACL 只赋予该 SID 访问权限，设置禁止远程 Named Pipe 客户端。
- 双方查询真实 Named Pipe 对端 PID，并打开实际进程 Token 比较用户 SID。Native 注册还核对真实 PID 和 GetProcessTimes 创建标识。
- 第一个管道实例使用 FILE_FLAG_FIRST_PIPE_INSTANCE 原子地实现同用户唯一 Broker。最多 64 管道实例、60 个已接受连接、32 个注册窗口。
- Session ID 是 CNG 生成的 128 位随机值，不使用 PID/HWND/窗口标题作为身份或路由。重新启用、重连新 Backend 时旧 Bridge 注销；新 Bridge 重新随机生成 ID。Broker 崩溃恢复时仍在运行的同一 Bridge 重新注册同一 ID，但没有恢复旧租约或任务执行状态。
- Broker 为每条 Adapter 连接自行生成 client_id，不信任 RPC 提供的客户端身份。租约和任务均绑定该连接。重连是新客户端，旧租约不能复用。
- 默认没有任何 Bridge、采集缓冲、AI 工作线程或 IPC。用户在已认证的目标 SSH 窗口中明确同意观察后才创建；关闭立即禁止本地新增采集/输入，并异步关闭管道、清理 Broker 数据。已经送达客户端的数据不能撤回，跨进程权限传播有 IPC 延迟。
- 普通 SSH 配置 Host 不用作最终目标。标题固定前缀显示 Session ID 前 8 位；MCP 返回完整 ID 和“目标需本地确认”标签。
- 控制请求需要本地菜单确认；diagnostic 与 custom 不能相互继承，升级需先释放再本地批准。最长 60 秒，不因轮询续约；到期仅拒绝新输入，仍跟踪已发出的长任务。每条命令/脚本还需本地再次确认完整文本、空输入行、空闲 POSIX Shell、非密码提示和目标身份。
- GUI 实际发送前再次确认认证后状态、SSH、Backend sendok、Backend 输入缓冲为空、非备用屏幕、非原始鼠标模式、无粘贴、授权代次、同一客户端、在线状态和期限。不能自动判定的交互状态必须由本地用户明确确认；没有确认就不发送。
- 人工键盘、终端鼠标和粘贴事件先撤销控制、递增授权代次，再继续原事件处理，不吞人工输入。主线程一次发送小于 4000 字节的包装，其他请求不允许交错写入；超限明确拒绝。单次脚本文本最多 2048 UTF-8 字节，校验也有界。

同 SID 的恶意软件、管理员、调试器或能代替用户点击本地批准窗口的程序不在隔离边界内。这里验证 Windows 用户和进程身份，不验证可执行文件签名或阻止同用户恶意进程。

## IPC v1

所有整数为 Windows x64 小端。每帧固定 64 字节头：

| 偏移 | 字段 | 长度 |
|---|---|---|
| 0 | PAI1 | 4 |
| 4 | version=1 | 2 |
| 6 | kind | 2 |
| 8 | payload_length | 4 |
| 12 | flags：READ=1、CONNECTED=2、CONTROL=8 | 4 |
| 16 | session_id 原始 UUID 字节 | 16 |
| 32 | request_id/task_id | 16 |
| 48 | 原始输出字节序号 | 8 |
| 56 | 本地授权代次 epoch | 8 |

Payload 全局上限 128 KiB；读取前先验证头和长度。Native 接收控制负载另限制为最多 2068 字节（custom client+timeout+script），固定槽尾端留 NUL。无 Base64 终端传输。

| kind | 用途 | Payload |
|---|---|---|
| 0 | 无控制请求/确认 | 空 |
| 1 | Native HELLO | PID u32、进程创建 FILETIME u64、HWND u64，共 20 字节 |
| 2 | OUTPUT | 至多 16 KiB 原字节；sequence 为第一字节偏移 |
| 3 | POLL/状态 | client_id 16 字节、ack u32、native_dropped u64，共 28 字节；兼容测试用 20 字节状态 |
| 10 | 请求本地控制批准 | client_id 16 字节 |
| 11 | 请求本地诊断命令批准 | client_id 16 字节、命令枚举 u32、超时 u32；任务 ID 在头中 |
| 12 | 撤销 | 空 |
| 13 | 单独请求本地 custom 批准 | client_id 16 字节 |
| 14 | 本地逐条批准 custom 脚本 | client_id 16 字节、timeout u32、最多 2048 字节 UTF-8 脚本 |
| 100/101 | Adapter RPC/结果 | 有界 JSON；结果有 ok 或明确 error |

v2 新增 flags：CUSTOM_CAPABLE=16、CUSTOM_CONTROL=32。custom 租约既需 Broker 连接归属匹配，也需 Native 明确报告同模式批准。原 framing version 1 保持，进程代际通过 v2 管道名隔离。Native 拒绝非法 UTF-8、NUL/CR/ESC/C0/C1 以及隐形方向字符（LF/Tab 允许）。

单个 Native 连接只接受其注册 Session ID；不匹配或非法消息导致断开 AI 通道。跨会话不能通过 HWND/PID 路由。Broker 控制通道最多 8 条，Native 控制投递只有一个固定槽；满时拒绝或断开该 AI 通道，绝不等待 PuTTY GUI。

## 输出语义

原始字节只保存在有界内存中。MCP 返回 `sanitized_utf8_transcript_not_screen`：C0/C1 控制字符转为可见转义，保留换行和 Tab，按 UTF-8 解码并处理页尾不完整码点，疑似凭据行做尽力脱敏。没有导出二进制 raw 工具，没有读取真实屏幕快照。ANSI、回车覆盖、退格、进度条语义不会被伪装成屏幕结果。

cursor 是每会话原始字节偏移。`next_cursor` 用于增量续读；`missing_ranges` 和 `lost_bytes` 明确说明捕获丢弃、断线传输丢失或 Broker 历史淘汰；`truncated` 也表示还有后续数据。首次不传 cursor 从最早仍保留的数据开始；要审计从启用起的全部缺口，传 cursor=0。list_sessions 提供累计捕获缺口、Native 环形缓冲丢弃和已收字节计数。

Broker 每会话原始历史上限 2 MiB 且最多 1024 块；每任务历史 256 KiB。MCP 原始读取页为 4..8192 字节，最终转义 JSON 文本硬上限 64 KiB，超限明确要求减小页长。Adapter stdio 入站行上限 128 KiB。

## 任务和完成检测

diagnostic 支持精确的五个模板：`uname -a`、`uptime`、`df -h`、`free -m`、`iostat -x 1 5`。custom 必须单独批准，允许合法文本的 POSIX 命令/脚本、管道和重定向，不承诺只读；Broker 和 C GUI 均校验边界及授权模式。Schema 的 command 为有长度描述的字符串，requested_mode 为 diagnostic/custom 枚举。

新增 `command.c` 安全单引号包装完整脚本，交给 POSIX eval；前后随机 RS/US 标记传回状态。自定义脚本最多 2048 UTF-8 字节，包装发送小于 4000 字节；窗口完整展示所有脚本行，Unicode 可滚动只读。权限取决于原会话账号，不自动 sudo，不输入凭据；交互程序需要人工接管。观察 deadline 为 1 秒至一小时，与输入租约的 60 秒独立。

每会话最多一个未解决命令，控制消息队列最多 8 条；全局任务上限 256，终态任务保留最多 10 分钟。相同客户端、Session ID 和 request_id 重复提交相同模板返回原 task_id，改换命令则拒绝。

`queued` 表示等待本地批准；Native 确认已交给 Ldisc 后为 `running`。Shell 包装使用随机任务 ID 的 RS/US 边界和退出码。命令回显中只有反斜杠文字，不会被当作真正边界。扫描跨块且有界，支持大输出。只有观测到合法 BEGIN 后的 END、合法 0..255 状态码且无捕获间隙，才返回 completed/failed 与 exit_code；没有可靠证据始终是 null。

本地明确拒绝且 Native 确认没发出时为 cancelled。取消请求与本地批准可能竞态，因此对已投递或已发送的取消保守返回 unknown；不发送 Ctrl+C，不宣称远端被停止。超时为 timeout，也不停止远端；任务不因等待超时自动释放不确定 Shell 的执行隔离，需用户再次操作并核对状态。断线、人工干预、输出缺口为 unknown 或明确终态，不重试。

终端 transcript 包含命令回显和可能相邻的终端文本，不是经过服务器 Exec API 隔离的 stdout/stderr。恶意目标可伪造输出，随机文本标记只适用于协作的 POSIX Shell，不构成对抗性完成证明。

## 本地审计

Broker 独立有界审计线程，队列 128；`%LOCALAPPDATA%\PuTTY-AI-v2\audit.jsonl` 和一个轮转文件各约 1 MiB。目录和文件 DACL 限制为当前用户。只保存时间、经过 UUID 验证的 Session/Task/Client ID、工具名、RPC 结果和授权状态，不保存命令文本、终端内容或凭据。记录请求前队列已满/存储已故障时拒绝新的写入类工具，不阻塞 PuTTY。崩溃可能丢失尚未落盘的审计队列；这不是耐久事务审计，也不代替堡垒机审计。

