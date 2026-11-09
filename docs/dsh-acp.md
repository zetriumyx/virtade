# DeepSeek Harness 原生 ACP 接入

调研日期：2026-11-09。官方项目处于 developer preview，接口可能发生破坏变更。

依据官方 [ACP 包说明](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/acp/acp/README.md) 与 [实现](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/acp/acp/src/index.ts)：`dsh --profile acp` 提供 JSON-RPC 2.0 stdio ACP v1。它保留 Harness 原生 Agent、工具、上下文、持久化及子任务行为。virtade 只实现协议客户端。

当前支持 initialize、session/new、服务器声明支持时的 session/resume、长请求 session/prompt、session/update 语义事件、session/cancel 和 session/request_permission。配置以原生 ACP profile 为准；不在 virtade 中偷偷替换模型或 Agent Loop。可在设置中指定 dsh 程序。启动需要用户事先安装和配置对应版本 DSH；若开发 checkout 只能用 pnpm 启动，可提供本地可执行启动脚本转发到官方命令，stdout 必须仅有协议帧。

DSH ACP 提供已提交语义消息，不提供底层 token/retry 原始流、DSH 专有展示卡片、终端或历史转录重放。UI 显示收到的消息，不能将未提供的原始流伪造出来。会话恢复不重放旧事件，既有历史来自 virtade SQLite。任务 UUID 为宿主请求关联标识，不能当作 DSH 原生可恢复 turn ID。

只映射 allow_once/reject_once；不将永久授权伪装成逐次审批。没有适用选项时返回 cancelled。审批绑定会话/任务、120 秒有效期、单次使用，桌面再包装为 90 秒 token。立即取消须等待 prompt 已进入发送队列，防止 cancel 超前。原生响应和已排队语义事件由单事件泵排序后交给持久化层；未知终态保留 unknown，不能重新提交。

自动化协议测试验证了正常审批、重复/错误身份拒绝、事件先于终态、恢复及立即取消。当前环境未安装并配置 DSH，也未对真实 DeepSeek 服务提交修改代码任务；这些合同测试不构成真实 DSH 端到端验收。发布前应固定 DSH 版本并记录 initialize 的 agentInfo.version、能力和真实任务结果。
