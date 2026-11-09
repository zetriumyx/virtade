# 分阶段任务及验收

执行原则：先完成 architecture.md，再实施 P0。每阶段先通过对应门禁，再扩展范围。以下工时为执行顺序，不承诺未经团队规模确认的日期。


## 2026-11-09 实施状态

P0 已验收。P1/P2/P3 的主体实现已落地；P1 的真实账号修改任务和 P3 的 Windows/macOS 实机验收仍未通过，不能宣布所有阶段完成。[当前证据与缺口](acceptance.md)、[桌面运行](desktop.md)、[第三方插件协议](plugin-development.md)、[DSH ACP](dsh-acp.md)。下方复选框表示对应整项是否完成，含额外子项的未完成框保留原样。

已落地：Tauri/React、Chat/审批/恢复、WASM 资源隔离、插件安装/启停/卸载/篡改隔离、UI 贡献与 Provider ABI、Monaco、Git/worktree、真实 PTY、多会话并行、插件诊断及 CI 配置。未包含自动升级、完整编辑器 LSP 桥、三平台性能门槛或已签名发行。

## P0：内核原型

1. protocol：manifest、API/依赖 SemVer、服务键、错误码；验证不兼容 API/依赖时没有启动副作用。
2. ResourceScope：注册撤销、托管异步任务、关闭 Context；验证关闭竞态、取消、等待清理。
3. 服务栈：默认注册、显式覆盖授权、动态解析、撤销恢复；验证普通冲突、拒绝越权、多层覆盖和非栈顶撤销。
4. 事件：sync/async、确定性优先级、拦截、panic/timeout；验证卸载后不再回调。
5. 生命周期：拓扑、缺失依赖、循环、事务式启动回滚、暂停/恢复、卸载/cascade/reload；验证失败注册和后台任务被回收。
6. 三插件演示：base 提供 greeting，consumer 依赖 base 并订阅事件，override 替换 greeting；逆序注册后依赖启动，卸载 override 恢复 base，最后全部清零。
7. CI：fmt、clippy、cargo test，Windows/Linux/macOS matrix；实际执行结果独立记录。

退出门禁：上述演示可运行且断言通过；边界自动化测试通过；不对 WASM、安全隔离、桌面或真实 Agent 可用性作 P0 验收。P0 可信内置插件使用宿主 Rust trait，不发布稳定 SDK。

## P1：最小真实 virtade

依赖 P0；先做两个 Provider 的协议 spike，避免 UI 先行造成接口返工。

早期记录：Claude 与 Codex 都已接入 Rust AgentProvider 和 virt 插件；SQLite 工作区/会话/任务/事件落库、Unknown 恢复保护和跨进程 CLI 会话恢复已通过协议测试，见 [P1-02 记录](p1-persistence.md)。原始 spike 见 [P1-01 记录](p1-native-agents.md)。以下整项复选框保留未完成状态，避免把自动化协议测试误标为真实 Agent 与 Windows 桌面验收通过。

- [x] agent-core：Provider DTO、能力声明、事件 envelope、审批与错误、实例/session/task 状态机。
- [x] Windows process supervisor：Job Objects、无 shell argv 启动、cwd/env、取消/退出；Unix 对应实现。
- [x] Claude SDK sidecar：锁定 SDK/原生二进制版本，发现/流/审批/interrupt/resume；记录缺失能力。
- [x] Codex app-server：初始化、双向请求、thread/turn、审批、异常重连；不重复提交任务。
- [x] storage：SQLite migrations、WAL、事务和崩溃恢复；会话 ID 映射，unknown task reconciliation。
- [x] Tauri 2 + pnpm monorepo：React/Vite、Tailwind/shadcn、Zustand、Chat Markdown/代码块/工具卡、workspace/session/settings、深浅色和布局恢复。
- [ ] 诊断：tracing、脱敏、Provider 检测页、取消耗时与 UI 流式批处理。
- [ ] 测试：fake transport 合同测试、损坏帧/大帧/慢流、进程异常退出、审批过期/重复响应；真实 Agent 测试独立 opt-in，不在普通 CI 请求付费服务。

退出门禁：在 Windows 桌面选工作区，分别让原生 Claude Code 与 Codex 修改真实测试仓库的文件并通过该仓库测试；观察工具过程与审批，取消长任务，重启后恢复同一原生 session；保存版本、录屏与脱敏日志。Linux/macOS 同时构建验证，不能以模拟流代替真实 Agent 门禁。

## P2：开放插件

- [x] Wasmtime Component/WIT spike：host↔guest 服务与 callback，memory/fuel/epoch/host I/O timeout，Windows 构建由 CI 门禁验证（本地为 Linux）。
- [ ] 定稿 manifest JSON schema、API SemVer、WIT 绑定、TS typed IPC；生成 types，跨语言 golden fixtures。
- [x] permission broker：请求与授予分离、撤销、fs/network/process/sensitive-service；路径和链接逃逸测试。
- [ ] Contribution Registry：全部 UI 扩展点、唯一 ID、owner、撤销事件、用户快捷键冲突规则。
- [x] 隔离 UI 宿主：端口身份与来源验证、限流、CSP；不向第三方暴露 Tauri IPC。
- [ ] 插件安装事务：包大小/路径穿越/哈希、清单验证、隔离目录、版本回滚，禁用/卸载/故障熔断；本地包先行，不做市场。
- [ ] SDK 和模板：Rust 内置、WASM、TS UI，服务/事件/Provider/配置/存储/权限/释放文档、日志与调试入口。

退出门禁：外部目录中的插件无需改 virtade 源码即可添加面板、提供服务、订阅 Agent 事件；禁用后 UI、监听和任务清零；越权 import、假端口消息、无限循环、超额内存与故障均有拒绝/限时证据，主 UI 保持可交互。

## P3：工作台

- [x] DSH 官方协议 spike，按能力发布 Provider 或 terminal-only；缺公开接口形成 upstream issue/文档，禁止修改 Harness 绕过。
- [x] 文件树 + Monaco、保存冲突/外部修改提示、文件访问路径控制。
- [x] xterm.js + portable-pty，Windows ConPTY、resize、编码、进程回收。
- [x] Git status/diff/stage/commit/worktree，操作取消和未提交改动保护；使用 Git 原有认证链。
- [ ] 多 Agent 并行队列、资源上限、worktree 隔离与冲突提醒；不承诺共享目录自动无冲突。
- [ ] 插件开发重载、事件检查器、任务与资源面板、基准数据。
- [ ] 三平台构建签名/分发与升级恢复、SBOM、许可证通知、端到端回归。

退出门禁：每平台完成编辑、终端、Git/worktree 和至少两 Agent 并行代码任务；插件故障后可定位并恢复；记录性能 baseline，回归阈值按实测设置。云 Agent、远程工作区和插件市场保持接口余地，不在这些阶段实施。


## 尚需推进的门禁（按依赖排序）

接手 Agent 先读根目录 [HANDOFF.md](../HANDOFF.md) 和 [AGENTS.md](../AGENTS.md)。首轮远端 CI 三平台桌面构建已通过，但 Windows core 有一项回归失败；优先修复并记录根因与新 CI，再推进下列阶段门禁。最新任务入口和完成标准统一维护在 HANDOFF.md。

1. 完成 UI 扩展的独立执行及故障恢复门禁；iframe sandbox 仅作为权限边界，不能替代 CPU / 内存故障隔离。
2. 用户原生 Agent 的有效认证到位后执行真实修改/取消/恢复；Codex 当前原生 unauthorized 不应由宿主绕过。
3. 在真实 Windows / macOS 或远端 CI 执行现有构建与合同测试，修复平台差异，再做桌面交互验收。
4. 安装升级的版本回滚与故障熔断策略、跨语言 golden fixtures、可发布 API 文档；当前只支持卸载后显式重装，不自动更新。
5. 根据本地 baseline 和三平台实测决定性能门槛；补足签名与第三方许可文本物料，之后评估发行。
