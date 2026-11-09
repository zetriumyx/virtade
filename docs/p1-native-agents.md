# P1-01：virtade 原生 Agent 接入基础

这是第一批增量的历史记录。当前 Claude Rust Provider、SQLite 和统一 CLI 的实现与命令请读 [P1-02](p1-persistence.md)，下文“尚未实现”描述的是 P1-01 当时的边界。

2026-11-08。项目从 `/workspace/ade` 迁移到 `/workspace/virtade`；Cargo 内核包由 ade-kernel 改为 `virt`，协议包为 `virt-protocol`。尚未发布包，因此没有维护旧 import 别名；不代表 crates.io 名称已注册。

## 本次完成的闭环

`virt` 插件 → 版本化 Provider service → Codex app-server 子进程 → 原生通知/审批 → Provider event → `virt` Event Bus。Claude 使用独立官方 SDK bridge，当前通过协议测试与真实 SDK 加载握手，尚未包装为 Rust AgentProvider 或 virt 插件。

| 层 | 实现 |
| --- | --- |
| agent-core | AgentProvider、SessionConfig/Session/Task、能力声明、结构化错误、保留原始载荷的事件、一次性审批响应 |
| stdio RPC | 无 shell argv 启动、cwd、显式环境继承名单、双向请求、响应关联、deadline、取消等待清理、超大/损坏帧拒绝 |
| process supervisor | 采用 [process-wrap](https://github.com/watchexec/process-wrap) 10.0.1，Unix ProcessGroup、Windows JobObject、KillOnDrop、主动关闭及等待退出；复用成熟实现，不自写 Win32 FFI |
| CodexProvider | initialize/initialized、thread/start/resume、turn/start/interrupt、流式原生事件、command/file approval 映射 |
| CodexPlugin | `agent-codex` 静态内置插件；服务 `virtade.agent.codex@1`，事件 `virtade.agent.codex.event.v1`；卸载回收服务、事件泵及子进程 |
| Claude bridge | 锁定 `@anthropic-ai/claude-agent-sdk` 0.3.319，原生 query、sessionId/resume、canUseTool、interrupt、完整 SDK 消息；不编写模型/工具循环 |
| CLI | `virtade doctor codex/claude`；`virtade codex <cwd> <prompt> [--resume <id>]` |

Rust AgentProvider 是同构建的宿主接口，不是公共 Rust ABI。Node bridge 使用有版本号的私有 NDJSON 协议，不宣称兼容 Codex app-server 协议。服务 JSON 只是调用入口，审批绝不通过通用 ServiceCall 暴露。

## 生命周期与错误语义

每个原生进程建立独立进程组/Job Object。stdout 专用于协议；stderr 独立持续排空，最多保留 8 KiB，只有显式读取诊断才可访问，不自动写日志。入站帧默认上限 1 MiB，transport 队列默认 128，request 并发数量有界。Claude bridge 入站 1 MiB、待写出 4 MiB、并发请求/审批 128、本地预留 session 1024。

慢消费者导致有界队列耗尽时主动关闭连接并返回 Backpressure，不能默默漏掉审批或终态。当前策略是 fail-closed；后续通过 SQLite 持久化后推送、文本 delta 合并来避免正常 UI 暂停触发关闭。事件 sequence 当前为 connection-local，尚未实现数据库支持的跨重连 per-session 游标，不能据此断点续流。

请求超时和 Future 被取消只取消宿主等待，不表示原生操作没执行。Codex 的未知 submit 结果保留 Unknown，阻止同一 session 自动重提；重新建立 provider 并恢复/核对原生 session 后才能继续。事件 turn/completed 可解除明确终态。cancel 的成功返回只表示原生 interrupt 请求被接受，不提前标记取消完成。

进程 shutdown 是强制终结进程树，未完成的模型操作结果为 unknown；正常任务取消优先使用原生 interrupt。当前进程 supervisor 尚未增加独立的优雅 SIGTERM/Windows 控制事件升级策略。Drop 发出关闭，只有 await shutdown 才等待 supervisor 收尾。进程退出前尽量排空最终 stdout 帧；异常退出绝不自动重放提交。

Codex 并行单位是不同 session；同 session 同时只允许一个 submit。插件管理器仍串行启动/停止；无需 UI 驱动 Tokio 任务执行。

## 审批边界

- Codex 只映射原生 commandExecution/fileChange approval，允许 AllowOnce/Deny；其他原生请求返回明确 unsupported，并保留 ProviderEvent，不能假装兼容权限批量请求或用户问答。
- 宿主产生随机审批 token，绑定 native request/session/task、有限有效期；错 session/task、过期、重复响应均拒绝，超时向原生协议返回 decline。
- CodexPlugin 的公共服务支持 info、session/create/resume、task/submit/cancel；不提供 approval/respond。可信宿主通过 `CodexPlugin::respond_approval` 进入审批路径。Event Bus hook 不能通过返回 Continue/Intercept 批准工具。
- CLI 暂未接入人工审批交互，收到已映射的审批默认 Deny；不使用 bypassPermissions 或 danger-full-access。Codex 默认 untrusted + workspace-write。
- Claude canUseTool 一次性 approval ID 通过私有 sidecar 端口传递，失联、原生 signal abort、任务结束或超时默认拒绝；不写 updatedPermissions，不持久授权。未来 Rust adapter 还需验证宿主提交的 session/task 绑定后才能开放 UI。

## Claude bridge 的能力限制

官方 SDK 的接口依据锁定包的 sdk.d.ts 编译验证，并查阅 [TypeScript API](https://code.claude.com/docs/en/agent-sdk/typescript)、[权限](https://code.claude.com/docs/en/agent-sdk/permissions)、[会话](https://code.claude.com/docs/en/agent-sdk/sessions)。

新 session 在 SDK 首次 query 前仅是宿主预留 UUID，返回 `reserved: true`；不会声称磁盘上已有原生 session。首次 query 使用 sessionId，收到 init/result 后后续 query 使用 resume；显式 session/resume 通过 getSessionInfo 校验原生记录，找不到就报错。中断后没有原生 result 的流结束标记 unknown，阻止直接重提，绝不伪装成 cancelled。每条 SDKMessage 完整保留，工具状态细粒度规范化留待 Rust adapter。

暂不提供 Claude CLI 命令、Rust Provider、认证探测、多工作区索引或安全 UI。doctor 只加载真实 SDK 并检查 bridge handshake，没有启动 query 或执行模型任务。可通过宿主配置 `VIRTADE_CLAUDE_EXECUTABLE` 指定已有二进制；打包 sidecar、Node 和原生二进制的再分发政策尚未确定。

SDK package 的 license 字段为 `SEE LICENSE IN README.md`；virtade 自有 MIT 代码不改变 Anthropic SDK 或 Claude Code 的使用/分发条款。P1 本地开发使用官方依赖，不宣称第三方 SDK 也是 MIT，不将 node_modules 纳入源码交付。

## 实际验证

Linux x86_64，Rust 1.99.1，Node 24.22.1，pnpm 11.25.0。自动化协议用例使用明确命名的 synthetic fixture，不调用模型或消耗账号额度。

| 验证 | 结果 |
| --- | --- |
| virt 内核回归 | 原有 27 项测试通过 |
| native transport | 9 项测试通过：乱序关联、deadline/晚响应、错误帧、退出、背压、stderr、argv/env、子进程树 heartbeat 回收 |
| Codex Provider / virt 集成 | 6 项测试通过：session/stream/approval、到期拒绝、原生取消终态、Unknown 禁止重提、不支持配置显式拒绝、插件卸载与审批入口限制 |
| Claude bridge | 4 项测试通过：帧限制、SDK adapter 会话与完整事件/一次性审批、到期拒绝与不存在的恢复、interrupt 后 Unknown |
| 真实 Codex 入口 | 安装版本 `codex-cli 0.167.0-alpha.4`，真实 initialize/initialized 握手、ProviderInfo 与 shutdown 通过；依据该二进制生成 schema 核对已用端点 |
| 真实 Claude SDK 入口 | 0.3.319 bridge 加载和 initialize 握手通过；未执行原生 query |
| Windows 编译 | `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --locked` 通过；没有 Windows 原生运行结论 |

Rust fmt/Clippy、测试、Node 类型检查/构建/测试进入三平台 CI。远程 CI 尚未执行；macOS 未运行。Codex 文档滚动更新：[官方 app-server 文档](https://developers.openai.com/codex/app-server/)。当前只验证上述 alpha 二进制，未承诺跨所有 Codex 版本兼容；userAgent 原样记录到 native_version，尚未实现兼容范围拒绝策略。

## 下一步与 P1 剩余门禁

1. 把 Claude bridge 接入 Rust AgentProvider 与 virt 插件，补 host 审批身份校验和 Node 进程退出/取消测试；完成两个 Provider 的统一调用闭环。
2. SQLite workspace/session/task 元数据与原生 ID 映射、Unknown 恢复、事件游标；实现重启后恢复，避免自动重复提交。
3. Tauri 2 + React 工作台与 Chat、原生流式展示、人工审批和取消；把当前 CLI 流程迁入受控 IPC。
4. Windows 原生进程/Job Object 运行测试、Claude 与 Codex 的真实修改仓库任务及恢复验收，记录具体版本和证据。

本次未执行真实代码修改任务，P1 整体未验收；P2 第三方 WASM/UI 插件隔离和安装管理仍未实施。
