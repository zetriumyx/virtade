# virtade 开发规范（适用于整个仓库）

本文件供接手项目的 AI Agent 和开发者使用。先读本文件，再读 [HANDOFF.md](HANDOFF.md)。用户在当前任务中的明确指令优先；不要把历史计划当成已经实现或已经验收的功能。

## 产品与架构约束

- 项目名 **virtade**，Rust 插件内核名 **virt**。目标是通用 Agent Development Environment，Windows 优先，同时兼容 Linux/macOS。
- ADE 是原生 Agent 的宿主。通过官方 SDK、CLI、公开协议接入 Claude Code、Codex、DeepSeek Harness；不得改其内部 Harness，不得用自研 Agent Loop 假装保留原生能力。
- 不直接依赖 Cordis 或 DeepSeek Harness 内部库；成熟开源组件优先。当前技术栈为 Rust/Tokio、Tauri 2、React/TypeScript/Vite、Tailwind/Radix、Zustand、Wasmtime Component/WIT、SQLite、Monaco、xterm.js/portable-pty、Git CLI。
- 保持 Cargo Workspace 和前端 Monorepo。根据实际依赖拆包，不引入无必要的抽象和循环依赖。
- 优先完善当前闭环。云端 Agent、远程工作区、插件市场、自动更新不是当前交付范围，不擅自扩展。

## 开始工作

1. `git status --short --branch`：确认分支、未提交修改和远端；保留用户已有工作。
2. 阅读 [HANDOFF.md](HANDOFF.md) 中的优先任务和 CI 状态，再读相关源码与测试。历史架构文档描述目标，当前行为以源码和可复现证据为准。
3. 明确本次要解决的问题及验收方式；选择一个可运行、可复核的增量。无需为常规可逆实现反复请求确认。
4. 实现、验证、更新相应文档。遇到认证或目标平台缺失，记录具体阻塞，继续不依赖该条件的工作。

## 内核、插件与权限

- 内置 Rust 插件可静态链接且可信；第三方逻辑默认用 WASM Component，不把普通 Rust 动态库作为公共插件格式。
- 所有服务、事件、贡献、任务及资源必须有明确 owner 和生命周期。通过 Context 注册；卸载须撤销访问并清理所属资源。不要脱离作用域启动后台任务或遗留子进程。
- 保留服务栈的显式覆盖规则；撤销覆盖后恢复仍有效的上一实现。拒绝普通重复注册、越权覆盖和替换 host-private 服务。
- manifest 权限是请求，不是自动授权；有效权限取请求与授权交集。敏感服务必须使用内核提供的 Caller 身份，不能相信请求 JSON 中的 owner。
- 跨语言边界使用版本化 WIT/JSON DTO，不暴露 Rust Trait 对象、裸指针、任意 Tauri IPC 或不受控内核句柄。
- 改公共接口时同步涉及的 Rust 类型、WIT、TypeScript SDK、清单 schema、示例及合同测试；说明兼容性和版本变化。当前公共协议仍为实验版 0.1。
- 保留 WASM fuel/epoch/内存限额、队列/消息/任务配额、超时及取消；不得为让测试通过而移除保护或吞掉错误。
- 第三方 UI 通过受控 MessageChannel 通信，校验 owner/generation；不能访问宿主 nonce、私有服务或通用审批入口。保持 CSP、opaque iframe sandbox、来源及导航校验。
- **iframe 访问隔离不等于 CPU/内存故障隔离。** 当前缺口见交接文档，未完成独立执行与恢复验证前，不得宣称恶意 UI 代码不能卡住工作台。
- 原生进程授权是强能力，不是操作系统沙箱；不擅自扩大文件、网络、环境变量或进程访问范围。

## Agent、持久化与工作台

- Provider 如实声明 Supported/Experimental/Unsupported；保留专有事件、原始 payload 和真实 native version，不伪造兼容或成功结果。
- 区分本地 session/task ID、原生 session/turn ID、宿主 RPC operation_id 和审批 token，不能混用。
- 任务提交前先持久化意图。中断或不确定结果保留 Unknown，不能自动重放有副作用的请求；取消确认不代表任务已取消。
- 审批绑定正确会话和任务，单次使用且有期限；不把永久授权伪装成单次审批，不允许插件批准其他 Agent 的请求。
- SQLite 修改须考虑迁移、事务、外键、应用标识、已有数据及崩溃恢复。不要通过删数据库掩盖迁移或状态问题。
- 文件操作限定工作区，保留路径/链接逃逸防护、大小限制和保存版本校验；外部修改不能被静默覆盖。
- Git/worktree 操作不得默认 force 或丢弃用户未提交内容。并行 Agent 共享目录可能冲突，应保留提醒并优先使用独立 worktree。
- 流式 UI 与执行解耦，异步结果须校验当前工作区/会话，避免旧响应污染新视图。Mock/fixture 只用于明确的测试边界，不能混入生产宿主或当作真实 Agent 验收。

## 工具链与常用命令

工具链以 `rust-toolchain.toml`、`package.json` 和锁文件为准：当前 Rust 1.99.1、Node 24、pnpm 11.25.0。不要无理由升级或修改锁文件。

在仓库根目录执行：

```sh
pnpm install --frozen-lockfile
pnpm format:check
pnpm check
pnpm build
pnpm --filter @virtade/desktop exec playwright install chromium
pnpm test

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo run -p virt --example three_plugins --locked
```

- 修改 Rust 后用 `cargo fmt --all`；独立示例另用 `cargo fmt --manifest-path examples/wasm-hello/Cargo.toml`。
- 修改前端/SDK 后用 `pnpm format`。`pnpm build` 同时构建 SDK、Claude bridge、桌面前端，并重新生成示例 UI。
- 桌面运行：`pnpm --filter @virtade/desktop tauri dev`。普通 `pnpm --filter @virtade/desktop dev` 仅为浏览器预览。
- 原生编译：`pnpm --filter @virtade/desktop tauri build --no-bundle`；单独 Cargo 使用 `desktop,custom-protocol` features。默认 Cargo 测试不依赖 GUI 系统开发包。
- 系统依赖、原生冒烟和 Windows 前置条件见 [docs/desktop.md](docs/desktop.md)。`native-smoke` 只用于测试，不能作为正常发行 feature；只使用一次性数据目录。
- 测试选择应与改动相关：核心/权限/状态机变化必须有有意义的自动化回归；纯文档或格式变更不要求重复昂贵全套测试。未运行的检查要明确标注。
- 不用 sleep、降低断言、跳过平台或关闭安全检查掩盖竞态和失败。性能优化先有可重复基准，不能混比 debug/release 数据。

## Git、文档与交付

- 文档默认中文，代码标识符和提交信息可用英文。报告明确写出改了什么、验证了什么、还缺什么。
- 禁止提交 token、账号登录文件、`.env`、会话数据库、原始敏感日志、依赖目录、编译缓存或 Tauri 生成的 schema 目录。保留有意提交的示例 Component、图标和验证证据。
- GitHub 授权不等于模型/Agent 授权；认证信息必须经环境或官方登录流程提供，不能让用户把密码/令牌发到聊天或写进仓库。
- 按用户已授权范围提交和推送；不强推、覆盖他人工作或擅自发布发行版。提交前检查 diff；不得声称本地测试、模拟测试或旧 CI 结果代表当前版本的全部平台验收。
- 功能/接口变化同步更新相关文档；阶段结论更新 [docs/acceptance.md](docs/acceptance.md)，后续工作更新 [HANDOFF.md](HANDOFF.md) 和 [docs/roadmap.md](docs/roadmap.md)。历史验证 JSON 保留原版本语义，不倒改成新版本验证结果。
- 阶段结束留下：提交号、变更摘要、验证命令和结果、失败/未运行项、下一项可执行任务。不得只把交接信息留在聊天里。
- 自有代码 MIT；遵守第三方许可证。`THIRD_PARTY_NOTICES.md` 是依赖声明清单，不能替代分发所需的上游 LICENSE/NOTICE 文本。
