# virtade 开发交接

更新时间：2026-11-09。本文件是接手 Agent 的主入口，与 [AGENTS.md](AGENTS.md) 一起阅读；无需依赖前任聊天记录。

## 先从这里开始

1. 读取根目录开发规范，检查工作区和 GitHub 最新 CI。
2. 先处理下文任务 1 的 Windows 回归失败；不要将当前状态报告成全绿。
3. 再推进 UI 扩展故障隔离的架构验证。真实账号或目标平台缺失时，继续可独立完成的工作。
4. 每次交付同步本文件和验收证据，保持下一位 Agent 可直接接续。

仓库：<https://github.com/zetriumyx/virtade>，默认分支 `main`。初始实现提交 `0d0afd7bbf3a9a08af248c8608dcd82b3d9f41d0`。本次交接为文档增量，不改变运行时代码。

## 项目目标与当前阶段

virtade 是以 **virt** Rust 插件内核为基础的跨平台 ADE，不是单模型聊天客户端。以原生 Agent 为执行者，允许插件扩展服务、事件、Provider、UI 和工作流；Windows 为首要目标。

P0 内核原型已完成。P1/P2/P3 多项实现已落地，但真实 Agent 修改代码验收、不可信 UI 故障隔离、Windows 全套测试与多平台交互/分发仍未完成。不能把“已实现”“合同测试通过”“平台构建通过”“真实任务验收通过”合并为一个完成状态。

### 已有实现

| 模块 | 实现及关键入口 |
| --- | --- |
| virt 内核 | `crates/virt/src/`：Context、依赖拓扑、加载/启动/暂停/卸载/重载、资源作用域、服务覆盖栈、事件/hook、权限及贡献；主要回归在 `crates/virt/tests/kernel.rs` |
| 公共内核协议 | `crates/protocol/src/lib.rs`：Manifest、Capability、ServiceKey、错误和 API 版本；Rust trait 仅在可信原生宿主内使用 |
| 原生 Agent/RPC | `crates/agent-core/src/types.rs`、`rpc.rs`：能力与事件 DTO、有界双向 JSON-RPC、Unix 进程组/Windows Job、帧与队列上限 |
| 原生 Provider | `plugins/agent-codex`：app-server；`plugins/agent-claude` 与 `packages/claude-bridge`：官方 SDK；`plugins/agent-dsh`：官方 ACP v1 stdio |
| 持久化 | `crates/storage`：SQLite schema v2、单实例锁、任务意图/原生 ID/事件游标/配置配额；`crates/workspace`：Provider 无关协调和 Unknown 防重放 |
| WASM 宿主 | `crates/plugin-runtime/src/runtime.rs`、`wit/plugin.wit`：Wasmtime 50.0.0、无 WASI、fuel/epoch/内存及 WIT 导入 |
| 插件管理/权限 | `packages.rs`：检查、复制安装、摘要校验、启停/重载/卸载、篡改隔离、UI generation；`broker.rs`：文件/网络/短时进程；`transport.rs`：长期双向 RPC 资源 |
| 桌面宿主 | `crates/desktop-host/src/host.rs`：版本化 DesktopRequest、会话事件泵、持久化、审批、诊断；`agents.rs`：AgentControl 作用域服务；`contributed.rs`：外部 Provider ABI 适配 |
| 原生工作台 | `crates/desktop-host/src/workbench.rs`：私有内置服务、cap-std 文件、Git/worktree、portable-pty；集成测试在同 crate 的 `tests/` |
| Tauri 边界 | `apps/desktop/src-tauri/src/desktop.rs`、`security.rs`：主窗口 nonce、来源/导航校验、退出清理；`native-smoke.js` 只在测试 feature 中编译 |
| 前端/SDK | `apps/desktop/src/components/`：Chat、Monaco 文件与 Diff、终端、Git、插件/命令/渲染扩展；`packages/plugin-sdk/src/index.ts`：隔离端口、服务/事件/配置/资源/Agent/RPC API |
| 示例/CLI | `examples/wasm-hello`：可安装 UI + Component 和独立测试变体；`apps/cli`：Claude/Codex 诊断、任务、会话和历史；CLI 不提供桌面式人工审批 |

当前重要版本：Rust 1.99.1、Node 24、pnpm 11.25.0、Wasmtime 50.0.0、WIT `virt:plugin@0.1.0`、UI IPC v1、Claude SDK 0.3.319。版本最终以各清单和锁文件为准。

### 必须保留的语义

- 插件请求权限与实际授权分离；host-private 服务即使声明 ServiceCall 也拒绝第三方访问。
- 服务覆盖按明确规则入栈，卸载恢复上一有效实现；旧 Context/旧 UI generation 不得复活。
- 提交先落库，确认原生 ID 后绑定；异常退出/未知结果保持 Unknown，禁止自动重提。
- 桌面审批使用新的宿主 token，绑定会话/任务、90 秒期限、单次使用。不能直接暴露或复用原生审批身份。
- Claude 保留 session，但 bridge task UUID 不是原生可恢复 turn ID；DSH 保留 ACP 提供的语义事件，不伪造 token 级原始流。
- 外部 Provider 的贡献 ID 有 owner 命名空间；宿主绑定实际 Provider/native version，不接受冒充 Codex 的事件身份。
- 长 RPC 请求在 Context 托管任务中执行，排入发送队列后才确认；operation_id 只是宿主关联号。过载断开并报告不确定性，不静默丢帧。
- 文件保存校验预期版本；Git 不强制移除脏 worktree；旧工作区/文件异步结果不能污染新选择。

## 已有验证及当前 CI（需要首先更新）

### 本地记录

初始提交前，Linux 本地通过 90 项 Rust 测试、5 项 Node 测试、6 项浏览器回归、8 项实际 WebKit/Tauri 冒烟断言，以及 fmt/clippy/TypeScript/生产构建。正常桌面构建不含 `native-smoke` 测试 IPC。

证据见 [验收文档](docs/acceptance.md)、[Linux 摘要](docs/validation/linux-summary.json)、[原生冒烟](docs/validation/linux-native-smoke.json)、[截图](docs/images/native-workbench.png) 和 [性能基线](docs/benchmarks/README.md)。这些是指定版本的历史证据，不能代表未来改动。

### 首轮远端结果

运行：<https://github.com/zetriumyx/virtade/actions/runs/42571331814>，对应初始提交 `0d0afd7`；完整 workflow 结论 **failure**。

| Job | 结果 |
| --- | --- |
| core (ubuntu-latest) | 通过 |
| core (macos-latest) | 通过 |
| core (windows-latest) | **失败：外部 Provider 集成测试** |
| web | 通过：格式、类型、构建、Node/浏览器测试 |
| desktop (ubuntu-latest) | 通过：严格 clippy、Tauri 构建、原生冒烟 |
| desktop (macos-latest) | 通过：严格 clippy、Tauri 构建 |
| desktop (windows-latest) | 通过：严格 clippy、Tauri 构建 |

三平台能编译不代表 Windows 测试全绿，也不代表 Windows/macOS 已人工验证真实 Agent 和桌面交互。机器可读摘要见 [remote-ci-initial.json](docs/validation/remote-ci-initial.json)。下一次接手先查询最新 run，不要沿用旧结论。

## 后续任务（按优先顺序）

### 1. 修复 Windows 外部 Provider 回归（最先处理）

**已观察证据**：首轮 CI 的 `third_party_provider_is_discovered_without_core_registration_and_cannot_forge_identity` 在 `crates/desktop-host/tests/contributed.rs:95` 失败；读取 `history["events"][0]["event"]["provider"]` 得到 Null，预期 `hello.provider`。任务等待条件已经看到了 Completed。当前只能确认这些现象，不能把假设写成已经复现的根因。

**优先调查**：`DesktopRequest::History` 分别异步查询 events 和 tasks，是否可能先读到旧 events、后读到新 Completed 状态；判断需要修正一致性契约还是测试等待条件。核对 Store 事件/终态事务、宿主事件泵和原生提交确认顺序，排除真正的事件丢失或身份绑定错误。

**重现入口**：

```sh
cargo test -p virtade-desktop-host --test contributed --locked -- --nocapture
```

**完成标准**：在 Windows 复现并给出因果证据；若修复生产读取竞态，增加有确定性时序的测试；若修复测试，必须明确并保留 API 允许的并发语义。保留身份/native version 断言，不用 sleep 或跳过 Windows 掩盖问题；重新通过三个 core job 和相关宿主测试，记录修复提交与 CI 链接。

### 2. 补齐 UI 扩展故障隔离

**已知缺口**：sandbox iframe/opaque origin/MessageChannel/CSP/nonce 解决访问控制，不能保证恶意 UI JS 不阻塞或耗尽 WebView 渲染资源。WASM 逻辑隔离已测试，不能拿它替代 UI 验收。

**实施顺序**：先在 `docs/` 新增 ADR，明确 UI 执行单元、信任边界、跨平台进程行为、权限撤销、主 UI 恢复和性能代价；做最小 spike，再决定生产实现。独立进程、远程/声明式 UI 等可作为候选，当前未选定具体方案。不要未经验证就声称浏览器 Worker 或新窗口具有独立内存/进程隔离，也不要用字符串扫描“禁止 while”代替运行时隔离。

**完成标准**：自动测试插件 UI 死循环、异常、消息洪泛、超额内存及故障恢复；宿主拒绝越权、撤销旧身份，主 UI 仍能操作或按清晰策略恢复，原生 Agent 不受牵连；至少 Windows/Linux 验证，补足 macOS 证据。同步更新 SDK、模板、权限说明和 P2 退出门禁。

### 3. 完成真实原生 Agent 的 P1 验收

**阻塞**：历史真实 Codex 0.167.0-alpha.4 修改任务收到 unauthorized（登录刷新失败），已按 Failed 保存；Claude/DSH 无真实修改成功证据。GitHub 已授权与模型认证是不同事项，不能借此假装 Agent 可用。

**步骤**：用户通过官方流程配置有效原生认证；在一次性小仓库中分别让 Claude/Codex 修复失败测试，核对磁盘 diff 和仓库测试；验证工具审批、取消长任务、退出恢复同一原生会话，再验证两个独立 worktree 并行。DSH 另按官方 ACP profile 配置和固定版本验证。

**完成标准**：保存 Agent/SDK/OS 版本、脱敏结果和必要录屏，证明是真实原生能力。认证不可用时记录阻塞，继续其他任务；不得重写 Harness、跳过权限或用合成事件替代真实验收。

### 4. 完善公开插件系统的工程门禁

- 补充启用/禁用/卸载及持久化失败的事务一致性，当前已覆盖安装落库失败回滚；设计显式升级/版本回滚与故障熔断，不把“重载当前缓存包”当作升级。
- 为 manifest/WIT/TypeScript/Provider ABI 建立跨语言 golden fixtures 和一致性检查，逐步收紧/共享 DTO，明确兼容策略；当前 JSON schema 只是结构辅助。
- 增加可复现的插件开发/重建/重载工具，梳理贡献更新/释放和快捷键冲突规则。状态贡献当前为静态文本；editor 扩展目前为独立隔离视图，完整 LSP/Monaco 装饰桥尚未实现。
- 增加诊断事件检查器、可观测的资源/取消指标和系统性故障恢复验证。现有脱敏诊断导出和微基准不是完整监控系统。

**完成标准**：第三方无需改核心即可添加 UI/服务/Provider，禁用后权限及资源按明确时限回收；失败/回滚/滞后有可测试语义；更新版本化文档与示例。

### 5. 目标平台、工作台与发行收尾

- 完成 Windows ConPTY、Unicode/路径/进程树、Git/worktree、编辑保存、退出/恢复等实际交互；macOS 同步验证。
- 多 Agent 当前支持独立会话并行、资源上限和共享目录冲突提醒；自动队列调度/资源策略、自动 worktree 分配尚非完整实现。
- Claude bridge 依赖 Node 和相邻 SDK 依赖；可执行程序移动后可能要手工设置 bridge 路径。设计安装/分发方案，避免把开发 checkout 路径当作已打包资源。
- 收集发行必需的第三方 LICENSE/NOTICE、完善依赖物料；当前清单有 UNVERIFIED 项。签名/安装包和升级策略完成前，不声称可签名发布。
- 在固定三平台环境采样，再制定性能回归阈值；不要以共享 Linux 容器的 debug/release 微基准代替整机性能或资源占用指标。

**完成标准**：每平台通过编辑/终端/Git/worktree/并行 Agent 真实场景，构建物可按文档安装运行，并有版本化、可审查的验证及许可物料。

## 环境、运行与测试

常规命令见 [AGENTS.md](AGENTS.md)，系统依赖及原生冒烟见 [docs/desktop.md](docs/desktop.md)。GitHub CI 定义在 `.github/workflows/ci.yml`。

注意：

- 所有命令从仓库根目录运行；仓库 clone 到任何路径都应能开发，不应依赖 `/workspace`。
- 旧云容器曾在 `/workspace/scratch` 解包 Linux 系统库并临时挂载 WebKit 子进程目录；这些不是项目文件、不会随 GitHub clone 提供，也不是正式安装方案。正常环境按桌面文档安装系统依赖。
- 不将测试时的 `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` 带入产品、发布配置或普通平台 CI。
- 磁盘紧张时可清理可重建的 Cargo 缓存、限制 `-j`；不要删除用户源码/数据库或降低测试门禁。默认 dev/test 已关闭 debug symbols/incremental 以控制缓存体积。
- `examples/wasm-hello` 是独立 Cargo workspace，不在根 `cargo fmt --all` 范围内。提交的 Component 是测试 fixture/示例产物，改 guest/WIT 后必须重建相关变体，不能只改源码保留旧二进制。
- 新环境需要重新确认 GitHub 和 Agent 授权；不要搬运前任登录文件、环境令牌或本地会话数据库。

### 重建 WASM 示例及测试变体

以下命令会生成三个有意提交的 Component。普通 hello 用于可安装示例，probes/provider 仅用于明确的合同测试。不要把测试 Provider 的合成事件宣称为真实 Agent 输出。

```sh
rustup target add wasm32-unknown-unknown
pnpm build

cargo build --manifest-path examples/wasm-hello/Cargo.toml --target wasm32-unknown-unknown --release --no-default-features
cargo run -p virtade-plugin-runtime --features tooling --bin virt-componentize -- examples/wasm-hello/target/wasm32-unknown-unknown/release/virt_hello.wasm examples/wasm-hello/component.wasm

cargo build --manifest-path examples/wasm-hello/Cargo.toml --target wasm32-unknown-unknown --release --no-default-features --features test-probes
cargo run -p virtade-plugin-runtime --features tooling --bin virt-componentize -- examples/wasm-hello/target/wasm32-unknown-unknown/release/virt_hello.wasm examples/wasm-hello/test-probes.component.wasm

cargo build --manifest-path examples/wasm-hello/Cargo.toml --target wasm32-unknown-unknown --release --no-default-features --features test-provider
cargo run -p virtade-plugin-runtime --features tooling --bin virt-componentize -- examples/wasm-hello/target/wasm32-unknown-unknown/release/virt_hello.wasm examples/wasm-hello/provider-test.component.wasm

cargo test -p virtade-plugin-runtime --test components --locked
cargo test -p virtade-desktop-host --test contributed --locked
```

## 文档索引及交接更新模板

- [开发规范](AGENTS.md)：持续生效的工程与架构规则。
- [架构与调研](docs/architecture.md)：原始设计目标，部分属于后续计划。
- [路线图](docs/roadmap.md)：P0–P3 分阶段门禁；复选框含整项子要求，不能仅凭勾选断言整阶段完成。
- [验收记录](docs/acceptance.md)：实现与验证边界。
- [插件协议/SDK](docs/plugin-development.md)：API、权限、资源限额、Provider ABI 和 RPC。
- [桌面指南](docs/desktop.md)、[DSH](docs/dsh-acp.md)、[性能](docs/benchmarks/README.md)：专项入口。
- `docs/p0-validation.md`、`docs/p1-*.md`：早期里程碑记录，不代表当前所有边界。

下一位 Agent 交付后至少更新：

```text
日期 / 提交 / 任务：
最终改动与接口变化：
实际运行的检查及结果（含平台/CI 链接）：
失败、未运行与真实阻塞：
是否改变阶段验收结论：
下一项任务、入口文件与完成标准：
```
