# control-plane 实施方案（草案 v2：push fan-out + 解耦）

## 1. 范围与确认结论

已按你的确认更新目标边界：

1. 控制面默认关闭。
2. `up` 语义是“数据面就绪”，不探测对端可达性。
3. `reason` 放在 `stopping`，首版使用：`process_exit` / `main_loop_error` / `signal`。
4. `README.md` 只加一个简短段落，详细协议放单独文档并引用。
5. 连接方向改为 **push fan-out**：Phantun 主动连接多个控制面目标并推送状态。

## 2. 总体设计（解耦优先）

### 2.1 设计原则

1. 数据面代码不感知传输细节（UDS、重连、序列化都不进主循环）。
2. 控制面实现集中在独立模块，后续替换传输方式不改数据面。
3. binary (`client.rs` / `server.rs`) 仅保留“初始化 + 3 个状态上报点”。

### 2.2 模块拆分

新增 `phantun/src/control_plane/`：

1. `model.rs`：协议模型（`snapshot/event`、state、reason、metadata）。
2. `reporter.rs`：数据面可调用的上报句柄（`publish_starting/up/stopping/down`）。
3. `runtime.rs`：控制面运行时（状态总线 + 多目标 fan-out 调度）。
4. `sink_unix.rs`：单目标 Unix socket 推送与重连逻辑。

`phantun/src/lib.rs` 只导出 `pub mod control_plane;`。

## 3. 协议与事件语义

传输保持：UTF-8 NDJSON（每行一条 JSON）。

字段（v1）：

1. `v`：`1`
2. `type`：`snapshot | event`
3. `ts`：毫秒时间戳
4. `id`：消息 id（字符串）
5. `data`：
   - `state`: `starting | up | stopping | down`
   - `reason`: `null | process_exit | main_loop_error | signal`（仅 `stopping` 有值）
   - `mode`: `client | server`
   - `local`, `remote`, `dev`, `mtu`, `addr4`, `addr6`（可空）

语义：

1. 每个 target 每次重连成功后先发 `snapshot`（全量当前状态）。
2. 后续状态变化发 `event`（增量语义，但 payload 仍为完整状态快照，便于消费端无状态处理）。
3. `stopping` 需要 consumer ACK（`{"type":"ack","id":"m<state_seq>-<message_seq>"}`），`down` 不需要 ACK。
4. 连接中断期间不补历史事件；重连后的 `snapshot` 负责状态收敛。

## 4. push fan-out 运行机制

### 4.1 CLI

新增可重复参数：

1. `--control-target <UNIX_PATH>`（可多次传入）

规则：

1. 未传入则控制面完全不启动（零开销路径）。
2. 传入一个或多个目标则启用控制面。

### 4.2 每个 target 的独立任务

每个目标一个异步任务，互相隔离：

1. 循环 `connect()`。
2. 成功后立即发送 `snapshot`。
3. 订阅状态总线，状态变化就发送 `event`。
4. 写失败/对端断开则退出当前连接，进入重连。

### 4.3 重连策略

1. 指数退避：`200ms -> ... -> 10s` 上限。
2. 加 `jitter` 防止多目标同时抖动重连。
3. 不中断数据面，不因控制面失败退出进程（降级运行 + 日志告警）。
4. `snapshot` 发送失败也统一走退避，避免重连风暴。

## 5. 数据面接入点（最小改动）

`client.rs` / `server.rs` 只做：

1. 参数解析后，构造 `ControlReporter`（如果启用）。
2. TUN 创建并关键参数确定后上报 `up`。
3. 退出路径改为 `stopping(reason) -> down`。

约束：

1. 不在 UDP/TCP 快路径内做序列化/IO。
2. 不在主循环散落大量控制面判断分支。

## 6. 文件改动清单（预计）

1. `phantun/Cargo.toml`
   - 新增 `serde`, `serde_json`
2. `phantun/src/lib.rs`
   - 导出 `control_plane`
3. `phantun/src/control_plane/mod.rs`
4. `phantun/src/control_plane/model.rs`
5. `phantun/src/control_plane/reporter.rs`
6. `phantun/src/control_plane/runtime.rs`
7. `phantun/src/control_plane/sink_unix.rs`
8. `phantun/src/bin/client.rs`
   - 新增 `--control-target` + 三个上报点
9. `phantun/src/bin/server.rs`
   - 新增 `--control-target` + 三个上报点
10. `README.md`
   - 增加简短控制面段落 + 指向协议文档
11. `design/control-plane-protocol.md`
   - 协议详细说明（单独文档）

## 7. 验收标准

1. 未配置 `--control-target`：行为与当前版本一致。
2. 配置多个 target：多个 consumer 都能收到同样状态序列。
3. 某个 consumer 挂掉：其余 consumer 不受影响，数据面不受影响。
4. consumer 重启后：首条 `snapshot` 能把状态拉齐。
5. 关键状态顺序正确：`starting -> up -> stopping -> down`。

## 8. 风险与对策

1. 风险：目标路径未监听导致持续重连刷日志。
   对策：退避 + 限频日志（同类错误按窗口聚合）。
2. 风险：消息字段后续扩展影响兼容。
   对策：协议文档明确“新增字段可忽略，不删除既有字段”。
3. 风险：状态上报遗漏导致宿主机误判。
   对策：统一封装上报入口，避免在多个地方手写 JSON。

## 9. 开工顺序（实施步骤）

1. 先建 `control_plane` 模块骨架与单元测试（不接入数据面）。
2. 接入 `client/server` 的 CLI 与 3 个生命周期 hook。
3. 增加协议文档 `design/control-plane-protocol.md`。
4. 在 `README.md` 增加简短入口说明与文档链接。
5. 做本地验证（单 target、多 target、断连重连）。
