# virtade

以 **virt** Rust 插件内核构建的跨平台 Agent Development Environment。项目自有代码采用 MIT。当前已实现插件内核、Wasmtime/WIT 隔离插件、TypeScript UI SDK、Tauri/React 工作台，以及 Claude Code、Codex、DeepSeek Harness 的原生协议适配。包含 Chat、SQLite 会话恢复、Monaco、PTY 终端、Git/worktree、插件管理与多会话并行。

**实现不等于全部验收完成**：Linux 原生 WebView/IPC 冒烟、Rust 合同测试和浏览器回归已通过；真实 Codex 代码任务因原生登录失效而失败，Claude/DSH 真实任务及 Windows/macOS 交互仍待对应环境验证，UI 插件的 CPU/内存故障隔离仍需补齐。详见[验收记录](docs/acceptance.md)。
- [架构设计与组件调研](docs/architecture.md)
- [分阶段任务与验收标准](docs/roadmap.md)
- [P0 验证记录与边界](docs/p0-validation.md)
- [P1 接入设计与验证记录](docs/p1-native-agents.md)
- [P1 Claude Provider、持久化与恢复](docs/p1-persistence.md)
- [桌面开发、系统前置条件与 CI](docs/desktop.md)
- [插件协议、SDK、模板和权限](docs/plugin-development.md)
- [DSH 原生 ACP 接入](docs/dsh-acp.md)
- [当前验收记录与未完成门禁](docs/acceptance.md)

## 运行

安装 [Rustup](https://rustup.rs/)，在此目录执行（PowerShell、bash 均可）：

```sh
cargo run -p virt --example three_plugins --locked
cargo test --workspace --all-targets --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

`rust-toolchain.toml` 锁定 Rust 1.99.1，`Cargo.lock` 锁定依赖。以上 Rust 测试不需要 Node、Agent 账号、Tauri 或系统 WebView 开发包。

## 启动桌面

先按[桌面开发文档](docs/desktop.md)安装 Windows WebView2 / Linux WebKitGTK / macOS Xcode 前置条件：

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm --filter @virtade/desktop tauri dev
```

`pnpm --filter @virtade/desktop dev` 是浏览器预览，不能直接访问本地工作区。第三方示例插件位于 `examples/wasm-hello`；在桌面插件页检查目录、逐项授权后安装并启用。插件运行于 Wasmtime，UI 运行于 opaque iframe，所有调用经过权限与 generation 验证。

## 原生接入诊断

```sh
# 需要 PATH 中有 Codex CLI，只执行本地 app-server 握手，不提交模型任务
cargo run -p virtade-cli -- doctor codex

# Claude SDK bridge 需要 Node 24、pnpm 11.25.0
pnpm install --frozen-lockfile
pnpm check
pnpm build
pnpm test
cargo run -p virtade-cli -- doctor claude
```

诊断通过只证明通信入口可用。真实任务可用以下命令显式运行，需要对应 Agent 认证，可能使用模型额度并修改指定工作目录。数据库的父目录需已存在，建议将数据库放在源码目录之外：

```sh
cargo run -p virtade-cli -- run codex <cwd> <database> "任务说明"
cargo run -p virtade-cli -- run claude <cwd> <database> "任务说明"
cargo run -p virtade-cli -- sessions <database> <cwd>
cargo run -p virtade-cli -- run codex <cwd> <database> "继续任务" --resume <local-session-id>
cargo run -p virtade-cli -- tasks <database> <local-session-id>
cargo run -p virtade-cli -- history <database> <local-session-id>
```

`--resume` 使用 `sessions` 输出的本地会话 ID，由宿主映射到原生 session。CLI 将事件落库后输出 JSON 和持久化游标，审批请求默认拒绝；桌面支持单次人工批准/拒绝。Ctrl+C 请求原生取消，等待终态；超时保留 unknown，禁止自动重提任务。`history` 当前返回前 100 条，存储 API 支持游标分页。

SQLite 存储当前为明文，包含提示词和原生事件；不保存 API key 或批准令牌作为凭据。新数据库在 Unix 下以 0600 创建，Windows 使用父目录 ACL。一个数据库同时由一个 virtade 实例持有，避免错误的崩溃恢复；不支持运行中从第二个 CLI 实例查看同一数据库。

## 当前结构

```text
crates/protocol/          virt-protocol：内核清单、权限、错误
crates/virt/              virt：Context、服务栈、事件、依赖与生命周期
crates/agent-core/        Provider DTO、受控进程、双向 stdio RPC
crates/storage/           SQLite 工作线程、迁移、事务、恢复和事件游标
crates/workspace/         与 Provider 无关的会话/任务持久化协调
crates/plugin-runtime/    Wasmtime Component、权限代理、包管理与 UI 消息桥
crates/desktop-host/      桌面命令、Agent 事件泵和工作台内置插件
plugins/agent-codex/      Codex Provider 与 virt 内置插件
plugins/agent-claude/     Claude Rust Provider 与 virt 内置插件
plugins/agent-dsh/        DeepSeek Harness 官方 ACP 客户端
packages/claude-bridge/   官方 Claude SDK TypeScript sidecar
packages/plugin-sdk/      版本化 TypeScript 隔离 UI SDK
apps/desktop/            React 工作台与 Tauri 2 原生壳
apps/cli/                统一诊断、持久化任务、会话与历史入口
docs/                    技术方案、路线图、验证记录
```

宿主通过 `Kernel::install(plugin, grants)` 授予插件权限，请求与授予取交集；`start` 按需启动依赖，`start_all` 完整预检后启动。所有注册从 Context 进入，卸载自动撤销并等待托管任务释放。卸载有依赖者时默认拒绝，显式 Cascade 才级联。`shutdown().await` 是完成资源回收的正常退出路径，Drop 仅同步撤销并发出 abort。

Rust 内置插件静态链接且可信；第三方逻辑默认使用无 WASI 的 WebAssembly Component，不能使用普通 Rust 动态库获取宿主权限。进程执行授权允许启动原生程序，不构成操作系统沙箱。virtade 托管原生 Agent，不重新实现其 Harness。公共 SDK 0.1 为实验版本，发行与真实任务限制见验收文档。
