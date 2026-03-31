# control-plane 同步重设计方案（历史草案）

## 状态说明

这个文档保留为重设计讨论记录，**不是当前实现的规范文档**。

当前代码已经落地为 `v2` 同步控制面，但和这份草案相比，有几处应以代码和正式协议文档为准：

1. 规范文档是 [design/control-plane-protocol.md](control-plane-protocol.md)
2. 当前 `payload` 已包含 `peer4` / `peer6`
3. 实际停机路径是“有界等待后继续退出”，不是停机阶段失败就强制非零退出
4. bundled shell agent 与模板以 [tools/control-plane-shell-agent/README.md](../tools/control-plane-shell-agent/README.md) 为准

如果你是为了理解当前行为或实现细节，请优先看：

1. [design/control-plane-protocol.md](control-plane-protocol.md)
2. [README.md](../README.md)
3. [tools/control-plane-shell-agent/README.md](../tools/control-plane-shell-agent/README.md)

## 1. 目标

这次重设计的目标，不再是“异步状态广播”，而是把 control-plane 改成一套**同步闸门协议**。

目标行为如下：

1. 只要配置了 `--control-target`，这些 target 对应的 agent 就全部是**强依赖**。
2. 启动时，任何一个 agent 连不上，整体启动失败。
3. 启动、停机的各阶段都必须等所有 agent 明确确认后，才能进入下一阶段。
4. 运行过程中，只要任一 agent 断连，就视为 quorum 失效。
5. quorum 失效后进入有界恢复；恢复失败则受控停机。

这套语义本质上更接近 `wg-quick` 的 `PreUp/PostUp/PreDown/PostDown`：

1. `pre_*` 控制是否允许继续
2. `post_*` 控制该阶段是否真正完成
3. 任一 hook 失败，就是主流程失败，不再是 best-effort

## 2. 为什么现有异步方案不够

当前实现的核心语义是：

1. Phantun 先继续自己的主流程
2. control-plane 只是把状态异步推送出去
3. ACK/投递只是旁路同步
4. 超时后允许继续

这套模式适合“可观测性”，但不适合你现在要的业务约束：

- 每个 agent 都必须参与生命周期控制
- 主流程必须被 agent 的确认结果直接约束

也就是说，协议模型已经不是“事件流”，而是“本地生命周期编排协议”。

## 3. 总体设计

### 3.1 传输层

建议继续使用本地 UNIX socket，不要切 HTTP。

原因：

1. 需求本质是本机进程协作，不是通用服务接口
2. 你真正需要的是 `request -> response`，不需要 HTTP 那整层语义
3. 本地长连接更适合表达 quorum 与断连检测
4. HTTP 会带来额外实现重量，但不会解决关键问题

建议传输：

1. `AF_UNIX + SOCK_STREAM`
2. UTF-8 JSON
3. 分帧继续用 NDJSON，或者改成 length-prefix 都可以

这里真正重要的是**同步请求/响应语义**，不是分帧形式。

### 3.2 连接模型

每个配置的 target 都是一个必需 agent。

启动流程的第一步不是发状态，而是先建齐连接：

1. Phantun 主动连接所有配置的 target
2. 每个 target 都必须在 `connect_timeout` 内连接成功
3. 任一 target 失败，整体启动失败

不存在“连上一部分也先跑起来”的模式。

### 3.3 协议模型

建议把现有“push event”改成“长连接上的轻量 RPC”。

每个请求至少包含：

1. `session_id`
2. `id`
3. `phase`
4. `deadline_ms`
5. `payload`

每个响应至少包含：

1. `session_id`
2. `id`
3. `success`
4. `message`

说明：

1. `id` 仍然需要保留，但语义改成**请求关联号**
2. `session_id` 用来区分不同 Phantun 进程实例，避免旧连接/旧响应串进来
3. 协议字段保持最小化，不引入额外包装层或冗余字段

### 3.4 `phase` 枚举

首版只保留 5 个 `phase`，不再继续扩张：

1. `pre_start`
2. `post_start`
3. `sync_state`
4. `pre_stop`
5. `post_stop`

语义：

1. `pre_start`：启动前同步闸门，成功后才允许初始化数据面
2. `post_start`：数据面初始化完成后的同步闸门，成功后才进入 `running`
3. `sync_state`：重连恢复时的状态快照回灌，不单独推进生命周期
4. `pre_stop`：停机前同步闸门，成功后才允许停止数据面
5. `post_stop`：数据面停止后的最终清理确认

不额外定义 `resume`、`starting`、`running` 这类 phase。

原因：

1. 内部生命周期由 Phantun 自己维护
2. 对 agent 只暴露真正需要参与的同步点
3. 避免协议枚举不断膨胀

### 3.5 `payload` 最小字段集

`payload` 只保留必要上下文：

1. `state`
2. `mode`
3. `local`
4. `remote`
5. `dev`
6. `mtu`
7. `addr4`
8. `addr6`
9. `reason`

字段说明：

1. `state`：当前生命周期位置，不等同于 `phase`
2. `mode`：`client | server`
3. `local`：本地 endpoint
4. `remote`：远端 endpoint
5. `dev`：TUN 设备名
6. `mtu`：TUN MTU
7. `addr4`：IPv4 CIDR
8. `addr6`：IPv6 CIDR
9. `reason`：停机原因，仅停机相关阶段需要

说明：

1. `phase` 表示“这次请求要 agent 做什么”
2. `payload.state` 表示“Phantun 当前已经推进到哪里”
3. 这样在 `sync_state` 场景里，agent 可以仅凭快照判断此前阶段已经发生过

### 3.6 `payload` 必填规则

按阶段固定约束如下。

#### `pre_start`

必填：

1. `state`
2. `mode`
3. `local`
4. `remote`

可空：

1. `dev`
2. `mtu`
3. `addr4`
4. `addr6`
5. `reason`

约束：

1. `state` 必须为 `pre_start`
2. 这时数据面尚未初始化，接口相关字段允许为空

#### `post_start`

必填：

1. `state`
2. `mode`
3. `local`
4. `remote`
5. `dev`
6. `mtu`

条件必填：

1. 若启用 IPv4，则 `addr4` 必填
2. 若启用 IPv6，则 `addr6` 必填

可空：

1. `reason`

约束：

1. `state` 必须为 `post_start` 或 `running`
2. agent 应将其视为“此前启动阶段已经完成”

#### `sync_state`

规则：

1. `sync_state` 本身不引入新的字段要求
2. 必填性由当前 `payload.state` 决定
3. 若 `state` 为 `running`，按 `post_start` 规则校验
4. 若 `state` 为 `pre_stop` 或 `stopping`，`reason` 必填
5. 若 `state` 为 `post_stop` 或 `stopped`，`dev`、`mtu`、`addr4`、`addr6` 可以为空

#### `pre_stop`

必填：

1. `state`
2. `mode`
3. `reason`

建议携带：

1. `local`
2. `remote`
3. `dev`
4. `mtu`
5. `addr4`
6. `addr6`

约束：

1. `state` 必须为 `pre_stop`
2. `reason` 不能为空
3. agent 应按完整上下文执行停机前动作

#### `post_stop`

必填：

1. `state`
2. `mode`

可空：

1. `local`
2. `remote`
3. `dev`
4. `mtu`
5. `addr4`
6. `addr6`
7. `reason`

约束：

1. `state` 必须为 `post_stop` 或 `stopped`
2. 这时数据面已停止，不要求接口信息仍然存在

统一校验规则：

1. agent 收到缺失必填字段的请求，必须返回 `success=false`
2. agent 不应尝试猜测缺失字段的默认值
3. Phantun 端构造请求时，应按当前 phase 保证字段完整性

## 4. 生命周期状态机

内部状态机建议从现在的：

- `starting -> up -> stopping -> down`

升级成：

1. `connecting_agents`
2. `pre_start`
3. `starting`
4. `post_start`
5. `running`
6. `reconnecting_agents`
7. `pre_stop`
8. `stopping`
9. `post_stop`
10. `stopped`

对外如果还要保留简化态，后面可以再映射；但内部实现必须按上面的完整生命周期来写。

## 5. 各阶段语义

### 5.1 `connecting_agents`

目的：

- 在数据面初始化前，把所有必需 agent 全部连上

规则：

1. 所有 target 都必须连接成功
2. 任一 target 超时或失败，启动失败

### 5.2 `pre_start`

目的：

- 询问所有 agent：是否允许启动

规则：

1. 向所有已连接 agent 发送 `pre_start`
2. 等待全部响应
3. 任一 `success=false`、超时、断连、响应格式错误，整体启动失败
4. 在该 barrier 成功之前，数据面初始化不得开始

### 5.3 `starting`

目的：

- 真正初始化数据面

内容：

1. 创建/配置 TUN
2. 启动转发主循环
3. 采集运行时元数据

规则：

1. 只有 `pre_start` 全部成功后，才能进入这里
2. 若初始化失败，进入失败路径

### 5.4 `post_start`

目的：

- 数据面起来后，让每个 agent 完成后置动作

典型动作：

1. 应用 nftables / iptables
2. 切换策略路由
3. 标记服务健康
4. 放行业务流量

规则：

1. 向所有 agent 发送 `post_start`
2. 所有 agent 都必须成功返回
3. 任一 agent 失败，整次启动算失败
4. 成功之前不能进入稳定 `running`

### 5.5 `running`

目的：

- 正常运行

规则：

1. 所有 agent 连接必须持续健康
2. agent 不再是旁路观察者，而是运行前提的一部分

### 5.6 `reconnecting_agents`

目的：

- 在运行过程中某个 agent 断连后，做一次有界恢复

规则：

1. `post_start` 成功后，只要任一必需 agent 断连，立刻进入该状态
2. 启动 `reconnect_grace_timeout`
3. 在宽限期内，所有缺失 agent 都必须重新连回
4. 全部恢复后，必须重新做一次确认，才能回到 `running`
5. 若宽限期内恢复失败，则进入 `pre_stop`

恢复后的重新确认，首版可以直接复用 `post_start` 语义，不一定要新造一个 `resume` 阶段。

### 5.7 `pre_stop`

目的：

- 在数据面停机前，先让 agent 做停机前准备

规则：

1. 向所有必需 agent 发送 `pre_stop`
2. 等待全部确认
3. 任一失败、超时、断连，都算停机阶段失败
4. 收到 `SIGINT/SIGTERM` 后，也必须先过这个 barrier，再继续停数据面

这部分就是显式学习 `wg-quick` 的同步语义。

### 5.8 `stopping`

目的：

- 真正关闭数据面

内容：

1. 取消 worker
2. 停止接收/转发流量
3. 在有界超时内 drain 或 abort 任务

### 5.9 `post_stop`

目的：

- 数据面彻底停下后，让 agent 完成最终清理

规则：

1. 向所有必需 agent 发送 `post_stop`
2. 等待全部响应
3. 任一失败，最终退出记为失败

## 6. 运行期断连策略

`post_start` 成功之后，只要有任一必需 agent 断连，就不再只是记日志，而是视为**失去运行前提**。

建议策略：

1. 检测到断连，立即进入 `reconnecting_agents`
2. 给一个很短的恢复窗口，例如 `1s` 到 `3s`
3. 宽限期内必须所有 agent 全部恢复
4. 恢复后重新做一次确认
5. 若恢复失败，则进入受控停机

这个策略比“只告警不处理”更符合你的业务目标，也比“零容忍立刻自杀”更稳。

## 7. 错误语义

这次重设计后，以下都属于**控制路径错误**：

1. 连接超时
2. agent 主动拒绝
3. 响应超时
4. 连接断开
5. 响应格式非法
6. `session_id` 不匹配
7. `id` 不匹配

默认处理：

1. 启动阶段出错：启动失败
2. 运行阶段出错：进入恢复；恢复失败则 fail-stop
3. 停机阶段出错：停机流程失败，进程非 0 退出

## 8. 超时模型

即便改成同步，也必须是**有界同步**，不能无限等待。

建议拆成独立超时项：

1. `connect_timeout`
2. `pre_start_timeout`
3. `post_start_timeout`
4. `reconnect_grace_timeout`
5. `pre_stop_timeout`
6. `task_drain_timeout`
7. `post_stop_timeout`

统一规则：

- 任一阶段超时，都算该阶段失败

可以继续做清理，但绝不能把超时当成成功。

## 9. 报文草案

请求示例：

```json
{
  "v": 2,
  "kind": "request",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "phase": "pre_start",
  "deadline_ms": 3000,
  "payload": {
    "state": "pre_start",
    "mode": "client",
    "local": "127.0.0.1:1234",
    "remote": "1.2.3.4:4567",
    "dev": null,
    "mtu": null,
    "addr4": null,
    "addr6": null,
    "reason": null
  }
}
```

成功响应：

```json
{
  "v": 2,
  "kind": "response",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "success": true,
  "message": "accepted"
}
```

失败响应：

```json
{
  "v": 2,
  "kind": "response",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "success": false,
  "message": "nft apply failed"
}
```

## 10. 为什么 `id` 还要保留

`id` 仍然有必要，原因是：

1. 用来把响应关联回具体请求
2. 用来识别超时后迟到的响应
3. 未来即便有并发请求，也不会协议歧义
4. 日志和排障会清晰很多

但现有的事件流格式：

- `m<state_seq>-<message_seq>`

不适合继续沿用。

同步版建议改成：

- 每个 `session_id` 内单调递增的请求号，例如 `1`、`2`、`3`

## 11. agent 侧要求

每个 agent 必须做到：

1. 在 Phantun 启动前就已经准备好监听对应 UDS
2. 收到请求后，在 deadline 内返回明确结果
3. 原样带回 `session_id` 和 `id`
4. 明确返回成功或失败
5. 在 `post_start` 成功后持续保持连接

也就是说，agent 已经不是“被动消费者”，而是主生命周期参与者。

## 12. 兼容与迁移

这不是一个小修小补，而是协议语义重写。

因此不建议让新旧模式共用一个协议版本。

当前这次重构的决定是：

1. 当前同步闸门协议定义为 `v2`
2. 继续沿用现有 `--control-target`
3. 一旦配置 `--control-target`，就按强依赖 agent 语义执行

这意味着：

1. CLI 入口保持不变
2. 但 control-plane 的运行语义发生了明确切换
3. 这是一次有意的兼容性破坏，用来换取更清晰的生命周期模型

## 13. 对现有实现的影响

这次改动不是局部补丁，基本属于重构：

1. 旧的异步 event/ACK 抽象不够用
2. 连接管理要改成面向 agent 的长连接 request/response 模型
3. 启动与停机流程要改成显式 barrier
4. 运行期断连要成为一等状态转换，而不是日志事件
5. 二进制侧只保留少量生命周期 hook，控制逻辑尽量收敛到独立模块

结论上，这是“重设计”，不是“在原设计上再补几个 ACK”。

## 14. 编码前需要冻结的决策

如果下面这些点不再变化，就可以进入实现：

1. 所有配置的 agent 都是强依赖
2. 所有阶段都要求全量确认
3. 运行期断连进入有界恢复，恢复失败则受控停机
4. 传输继续使用本地 UDS，不上 HTTP
5. `id` 保留，但改成请求关联号
6. 协议版本升级，不与当前异步版混用

这些点一旦冻结，后续实现就可以按稳定规格推进，而不是边写边改语义。
