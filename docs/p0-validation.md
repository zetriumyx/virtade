# P0 验证记录

日期：2026-11-08。工作目录：`/workspace/virtade`。

## 环境与结果

本地执行环境为 Linux x86_64，Rust 1.99.1；依赖由 Cargo.lock 锁定。环境起初没有 Rust，已通过官方 Rustup 安装；项目不需要系统 WebView 或 Node 依赖。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过，无警告 |
| `cargo test --workspace --all-targets --locked` | 27 个集成测试通过，0 失败 |
| `cargo run -p virt --example three_plugins --locked` | PASS，全部验收断言通过 |

三平台 GitHub Actions matrix 已配置，**尚未在远程 CI 执行**；Windows/macOS 结果未知。当前没有真实 Agent、Tauri GUI、WASM 插件或性能基准的执行结果。

## 三插件验收

示例代码：`crates/virt/examples/support/mod.rs`；入口：`three_plugins.rs`；自动化用例：`three_plugin_acceptance`。

1. 按 override、consumer、base 的逆序安装，解析得到 base → consumer → override。
2. base 注册允许覆盖的 greeting 服务，consumer 声明硬依赖，override 获取 provide/replace 权限后替换服务。
3. consumer 调用得到 override 结果，发送事件后异步监听器收到 payload。
4. 动态卸载 override，再次调用恢复 base 结果。
5. 级联卸载 base 和 consumer，验证服务实现、监听器、托管任务计数全部为零。
6. consumer 后台任务捕获资源的 Drop 计数为 1，证明不只是删注册记录；旧 Context 的 emit 被拒绝。

## 关键假设与证据

| 假设 | 自动化证据 |
| --- | --- |
| 预检能避免依赖错误造成部分启动 | 缺依赖启动计数保持 0，环给出路径，版本/API 不兼容拒绝 |
| 服务覆盖可撤销且不会复活已卸载实现 | 多层覆盖、中间层卸载、底层先卸载、最后一层消失 |
| 替换需要明确规则 | 重复 new 冲突、base 拒绝覆盖、manifest 请求未获 grant 被拒绝 |
| Context 生命周期可被撤销 | pause/reload 后旧 Context 不能注册/调用/启动任务；注册与卸载竞态无残留 |
| 调用双方任一卸载都可取消调用 | provider 卸载中止 in-flight task，caller 卸载取消其等待及 provider 调用任务 |
| 事件可排序、拦截、重入和撤销 | 同优先级顺序、Intercept、嵌套派发、快照中已卸载监听器跳过 |
| 可恢复的插件故障不会摧毁内核 | start/service/event/stop panic 捕获、deadline、后台 panic 诊断、健康插件继续运行 |
| 生命周期失败有明确收尾 | 新启动集合回滚但已有插件保留；start timeout 清理；Failed 插件新 scope 重启；stop 失败仍清理级联集合 |
| 管理操作可预测 | 活跃依赖者阻止直接卸载、cascade 逆序释放、慢 stop 前已撤销、无关无效清单不阻止活跃插件清理 |
| 重复重载不累积注册 | 100 次 reload 每次保持 1 个服务实现，shutdown 后为 0；不是 RSS/长期泄漏基准 |
| 外层 start Future 被取消不会留下可调用服务 | guard 撤销部分注册，保留中间状态并拒绝直接重启，显式 unload 收尾 |

## 公共 API 的当前语义

- `Kernel::install` 校验清单并保存工厂；不是从磁盘安装第三方包。
- `start(id)` 按需启动传递依赖；`start_all` 完整图预检后启动。生命周期操作通过 `&mut Kernel` 串行。
- `pause/unload` 必须显式选择 RefuseDependents 或 Cascade；暂停全部释放，start 恢复。
- `reload` 使用原工厂的新作用域；失败没有自动重试或旧版本热更新回滚。
- `Context::provide/call` 使用 `(name, major)`；调用重新解析服务，不返回可绕过撤销的实现引用。
- `on` 为异步 handler，`hook` 为无 I/O 的同步回调适配器；`emit` 为异步顺序派发，总 deadline 包括全部监听器。
- `Context::spawn` 托管 cooperative async Future；后台 panic 保留最多 32 条 generation 内诊断。诊断不是持久化日志。
- 正常退出须 await `shutdown`；Kernel Drop 只能同步撤销与发出 abort，不能保证异步析构完成。

## 还未验证或实现

P0 的权限仅限制 service provide/call/replace 与 event subscribe/publish 的 Context API，**不能沙箱化 Rust 内置插件**；文件、网络、进程与敏感服务权限 broker 在 P2。无 WASM 执行，无 UI Contribution Registry，无 TypeScript SDK，也没有伪造 Agent Provider 或流式模型响应。

panic 隔离只对 unwind 生效；native abort、进程崩溃、不让出的 CPU 循环、阻塞析构/系统调用仍会影响宿主。注册表使用短临界区 Mutex，不宣称无锁或已完成高吞吐验证。事件目前无广播队列、重试、跨 session 总序或持久化。服务小版本协商、稳定 WIT、IPC schema 生成、配置和数据库属于后续阶段。

下一阶段应先完成 Windows 进程管理与 Claude SDK / Codex app-server 协议 spike，再引入 Tauri Chat、存储和真实代码修改任务验收；完整任务见 roadmap.md。
