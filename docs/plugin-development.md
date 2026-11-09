# virt 插件开发协议 0.1

状态：实验性、版本锁定。`virt:plugin@0.1.0` WIT、manifest `api: =0.1.0`、UI IPC `apiVersion: 1` 必须匹配。修改公共协议需同步 WIT、Rust、TypeScript SDK 和合同测试。Rust 内置插件静态链接并可信；第三方使用 Component Model，没有普通 Rust 动态库 ABI。

## 最小包和开发循环

参考 `examples/wasm-hello/`，包含 `manifest.json`、`package.json`、`component.wasm` 和可选 `ui.html`。package 元数据只接受 schema_version 1、component 和 ui 相对路径；目录穿越、链接文件、保留内置命名空间被拒绝。组件最多 16 MiB，UI 1 MiB，清单 64 KiB。

```sh
rustup target add wasm32-unknown-unknown
pnpm --filter @virtade/plugin-sdk build
node examples/wasm-hello/build-ui.mjs
cargo build --manifest-path examples/wasm-hello/Cargo.toml --release --target wasm32-unknown-unknown
cargo run -p virtade-plugin-runtime --features tooling --bin virt-componentize -- examples/wasm-hello/target/wasm32-unknown-unknown/release/virt_hello.wasm examples/wasm-hello/component.wasm
```

桌面 → 插件 → 输入包目录 → 检查 → 逐项选择授权 → 安装（保持禁用）→ 启用 → Hello 面板。修改源码后重新构建；卸载旧包、检查并安装新包。重载重建当前已安装且哈希一致的组件，不从开发目录静默导入更新。调试信息显示在插件卡片，错误包括 fuel/epoch、内存、权限、版本、依赖、忙碌或重入。可通过 cargo test -p virtade-plugin-runtime 验证真实组件。

安装复制内容到应用数据目录；安装哈希绑定清单、元数据、组件、UI。启动和启用校验哈希，篡改包被隔离。启用状态持久化；禁用撤销 Context 和旧 UI generation；卸载删除已管理的包目录。新版本升级需要显式重新安装；自动更新、签名分发和市场不属于当前接口。

## 权限与生命周期

manifest 的 permissions 只是请求。有效权限为请求与用户授权交集。服务调用、注册、覆盖、事件发布/订阅、扩展点、读写配置、文件根目录、网络 origin、可执行程序和 Agent 范围分别授权。服务覆盖还要求默认实现声明 replaceable。host-private 服务即使声明权限也拒绝第三方调用。

- WASM 没有 WASI、环境变量、任意 IPC、原生指针；32 MiB 内存、每次 1000 万 fuel、约 2 秒 epoch/调用超时。
- 同一组件一次进入一个导出；并发或重入返回明确 busy 错误。把长任务拆成启动及轮询，避免同步自调用服务或在事件回调内调用自身组件。
- Context 最多 1024 项注册、128 个活跃托管任务；每个插件最多 64 个 UI contribution、总 spec 4 MiB。
- 配置以 plugin:<id> 隔离，单值 1 MiB、每命名空间 1000 键及 4 MiB。UI 事件队列最多 128 条；轮询返回 dropped，不能将其视为可靠审计存储。
- 文件通过 cap-std 根目录操作，只接受普通相对路径，拒绝链接越界。资源 API 文件 1 MiB，编辑器文件 4 MiB。
- 网络授权精确 origin；禁止跳转和 URL 内凭据，1 秒超时、响应 1 MiB。允许的 origin 包括其 DNS 解析地址；授权内网地址意味着显式内网访问，当前不做 DNS 地址固定。
- 进程授权是启动该原生程序的能力，不能把它解释为进程操作系统沙箱。参数有上限、清空继承环境、1 秒运行超时，Unix 进程组/Windows Job 对象清理。只有可信用户能授予这项较强能力。

卸载先撤销注册和 Context，再终止托管异步任务并调用 stop。服务动态解析栈顶，撤销覆盖后恢复此前仍活跃实现。通知 observer 的失败不会取消 Agent 的原生任务；拦截式 hook 只用于明确选择拦截语义的 emit。原生线程及进程必须由内置插件自行追踪和关闭。

## UI SDK

`packages/plugin-sdk` 的 connectVirt() 只在扩展 iframe 内工作。宿主以 MessageChannel 绑定插件 ID、generation 和 view；每次请求带固定宿主身份，不能以插件 payload 冒充别的插件。iframe sandbox 只有 allow-scripts，使用 opaque origin；禁止直接网络和任意嵌套 frame。主窗口 IPC 另有只存在顶层窗口的随机 nonce。

SDK 包含 services.call、config.get/set、resources.request、contribute、on/dispose、agents.providers/openSession/submit/cancel/history。`input` 是宿主给 renderer 的只读初始化 JSON。SDK 不提供批准其他 Agent 工具请求的 API。事件监听在组件 Context 中随插件禁用回收；UI dispose 关闭端口及轮询。

贡献定义：`{id: '<plugin-id>.<name>', kind, title, schema_version: 1, spec}`。

| kind | spec 与行为 |
| --- | --- |
| panel / sidebar / workspace_view | `{type:'iframe'}` 使用包内 UI；`{type:'text',text}` 为安全文本。工作台导航打开独立视图。 |
| settings | 同上，出现在设置页的折叠区。 |
| status | `{text}` 显示状态栏；更新需重新注册生命周期或提供面板查看动态状态。 |
| chat_renderer / tool_renderer | `{type:'iframe',provider?,method?}` 精确筛选原生事件；input 保留完整 event。未匹配内容保留内置渲染。 |
| command / workflow | `{service:{name,major},request}` 调用授权服务；或 `{view:'<owned-view-id>'}` 打开同插件视图。 |
| menu | `{command:'<owned-command-id>'}` 添加主工具栏命令入口。 |
| keybinding | `{keys:'mod+shift+k',command:'<owned-command-id>'}`；同键冲突按 ID 排序取首个，编辑控件内及系统编辑键不被覆盖。 |
| editor | 当前支持独立编辑器扩展视图，与 panel 相同；不加载未隔离脚本到 Monaco 主进程。语言服务器、装饰及诊断桥接留作后续协议。 |
| agent_provider | 以下版本化 Provider 服务协议。 |

## 第三方 Agent Provider ABI

贡献 ID 本身就是 Provider ID，必须以插件 ID 开头，不能冒充 codex/claude/dsh。spec 为 `{api_version:1,service:{name:'my.provider',major:1}}`。插件需要 service_provide、service_call 和 contribution agent_provider 权限。桌面自动发现贡献，无需改核心源码。

每次调用为 `{api_version:1, connection_id?, method, params}`。所有 DTO 使用 crates/agent-core/src/types.rs 的 serde 表示。服务接口是有界的即时请求；插件自行持有会话及工作状态，通过公开 SDK/CLI/协议运行真正的 Agent。

| method | params → result |
| --- | --- |
| provider/connect | `{cwd}` → `{connection_id,info:ProviderInfo}` |
| session/create | SessionConfig → Session |
| session/resume | `{session_id,config:SessionConfig}` → Session |
| task/submit | `{session_id,text}` → Task |
| task/cancel | Task → null（仅取消请求确认） |
| approval/respond | ApprovalResponse → null |
| events/poll | `{after,limit:64}` → AgentEvent[]，严格递增的连接内 sequence，schema_version=1 |
| provider/disconnect | null → null，释放该连接资源 |

宿主将 event provider 和 native_version 绑定到实际贡献及 connect 信息，不信任插件自报其他 Provider 的身份。事件原始 payload 保留，终态由 outcome 声明。工具审批在主 UI 用新的宿主 opaque token 再包装，身份/有效期/单次响应验证与内置 Provider 相同。插件自己的声明不构成对其质量或原生功能完整性的担保，必须诚实声明 capabilities。

## Rust 内置插件

实现 virt::Plugin 的 manifest/start/stop。start 得到作用域 Context，通过 provide/on/hook/contribute/spawn/defer 注册；无需独立取消注册句柄。stop 在作用域撤销后执行，不能再注册资源。Service 使用 JSON DTO，不把 Trait 对象暴露到 WIT/TS。Caller 来自内核，敏感公共服务应在 call_scoped 验证 caller.grants；严禁以请求 JSON 的 owner 字段代替内核身份。

## 清单结构校验

编辑器可关联 [manifest.schema.json](schemas/manifest.schema.json) 辅助编写清单；它只描述 JSON 结构，SemVer/依赖拓扑/授权与包内容由 Rust 宿主实际校验。不要向 manifest 注入 `$schema` 字段，manifest 拒绝未定义的顶层字段。依赖当前要求每项 `id/version`；权限使用 `{kind,target}`，其中 Agent 范围为 `{kind:'agent_control',target:{provider:'codex',root:'绝对规范目录'}}`。服务键为 `{name,major}`。配置权限无 target。

## 调试与性能

先运行 `cargo run -p virt --example three_plugins` 验证完整内核闭环；`cargo run -p virt --example baseline` 输出 JSON，测量 10,000 次服务调用和单监听事件的 median/p95/p99，以及启动/清理耗时。它是微基准，不代表 GUI 或 Agent 吞吐。比较前固定编译 profile、Rust/CPU/系统与负载，优化版需使用 `--release`，不能混比 debug/release 数据。

隔离边界：WASM fuel/epoch/内存限额约束逻辑组件；iframe sandbox、CSP 和端口令牌约束 UI 访问权限。浏览器 iframe 本身不保证独立的 CPU 或内存进程配额，恶意 UI JavaScript 仍可能卡住 WebView 渲染线程。Rust 内核与原生 Agent 位于 WebView 之外，但不能据此宣称任意 UI 插件故障后主 UI 仍可交互；独立 UI 进程及其恢复策略是开放不可信 UI 的后续门禁。已授权的原生进程和已开始的文件写入可能产生不可撤回的外部副作用；撤销 Context 阻止后续调用，不回滚已提交的系统操作。

## 原生双向协议连接

外部 Provider 可使用 `resources.request` 的持久化 RPC 操作，复用宿主进程组 / Windows Job 监督器，不必修改核心代码。它不是通用 shell；需要同时授予绝对可执行路径的 `process_command` 和规范 cwd 的 `filesystem_read`。原生程序保留自身 Harness。宿主仅继承 PATH、HOME、USERPROFILE、APPDATA、LOCALAPPDATA、SystemRoot、TEMP、TMP、LANG；不自动继承模型密钥或代理凭据。

- `rpc_open {program,cwd,args,json_rpc_2}` → `{id}`。连接 ID 与插件 owner/Context 绑定，最多每插件 4 个、全局 16 个。
- `rpc_start {id,method,params,timeout_ms}` → `{operation_id}`。支持 50ms–15 分钟期限，进入有序发送队列后才确认；长请求在宿主管理任务中等待，避免阻塞 WASM 导出。每连接最多 16 个待响应请求。
- `rpc_notify {id,method,params}` 发送通知；`rpc_respond {id,request_id,result?,error?}` 回答该连接收到的服务器请求。result 可为 null，不能与非空 error 同时出现。这不提供批准其他内置 Agent 请求的权限。
- `rpc_poll {id,after}` → `{events:[{sequence,frame}],closed}`。原生通知/服务器请求保持原始 frame，异步响应为 `{operation_id,result}` 或 `{operation_id,error}`。after 确认已消费的连接内序号，每批最多 64 条。帧最多 256 KiB、底层队列 16 帧；外层待消费队列最多 128 条 / 2 MiB，超额断开并显式标记结果可能 unknown，禁止静默丢事件。
- `rpc_close {id}` 显式关闭。禁用/卸载先撤销访问并关闭所有所属连接，取消 Context 托管请求；原生监督器杀死并回收进程树，最多等待 5 秒。重新启用不能复用旧连接 ID。真实 subprocess 测试验证双向帧、异步结果、权限、撤销和 Linux 进程退出。

RPC 是传输协议，不自动解释某个 Agent 的业务能力。第三方 Provider 必须实现会话、任务与审批映射，保留专有事件、真实 capabilities 和错误；公开 ABI 的合成测试不等于其真实 Agent 可用性验收。

TypeScript SDK 为这些请求提供 `context.rpc.open/start/notify/respond/poll/close` 类型化封装；例如 `const {id}=await context.rpc.open({program,cwd,args:['--profile','acp'],json_rpc_2:true})`。`start` 的 operation_id 是宿主关联号，不是 Agent 原生 turn ID。收到响应前如连接中断，应按 unknown 处理，不能自动重发有副作用的请求。
