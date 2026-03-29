# control-plane-shell-agent

这是一个面向 Linux 的最小化 Phantun `v2` 控制面 agent。

它监听一个本地 UNIX domain socket，接收 Phantun 发来的控制面 `request`
消息，把请求上下文导出为环境变量，执行一个固定的 `sh` 脚本，再根据脚本
执行结果返回 `response`。

## 行为概览

1. 传输层：`AF_UNIX + SOCK_STREAM`
2. 分帧方式：NDJSON，也就是每行一个 JSON 对象
3. 协议版本：Phantun control-plane `v2`
4. 脚本执行方式：`/bin/sh <script>`
5. 请求上下文传递方式：仅环境变量
6. 成功判定：脚本退出码为 `0`
7. 失败判定：非零退出码、超时、脚本启动失败
8. 并发策略：agent 内部对脚本执行做全局串行化

agent 不会把 phase 或 payload 当作命令行参数传给脚本。  
脚本只需要读取环境变量。

## 构建

```bash
go build -o phantun-agent ./tools/control-plane-shell-agent
```

## 启动

```bash
./phantun-agent \
  --socket /run/phantun/agent.sock \
  --script /opt/phantun-agent/handler.sh
```

可用参数：

- `--socket`：监听的 UNIX socket 路径
- `--script`：每次请求都会执行的脚本路径
- `--max-script-timeout`：脚本执行的本地硬超时上限，默认 `10s`

## Phantun 侧配置

让 Phantun 指向同一个 socket：

```bash
phantun-server ... --control-target /run/phantun/agent.sock
```

或者：

```bash
phantun-client ... --control-target /run/phantun/agent.sock
```

## 环境变量

agent 会先清掉继承环境中的旧 `PHANTUN_*` 变量，然后再导出当前请求的上下文：

- `PHANTUN_PROTOCOL_VERSION`
- `PHANTUN_KIND`
- `PHANTUN_SESSION_ID`
- `PHANTUN_REQUEST_ID`
- `PHANTUN_PHASE`
- `PHANTUN_DEADLINE_MS`
- `PHANTUN_STATE`
- `PHANTUN_MODE`
- `PHANTUN_REASON`
- `PHANTUN_LOCAL`
- `PHANTUN_REMOTE`
- `PHANTUN_DEV`
- `PHANTUN_MTU`
- `PHANTUN_ADDR4`
- `PHANTUN_ADDR6`
- `PHANTUN_PEER4`
- `PHANTUN_PEER6`

其中可选字段只有在请求里存在时才会导出。

语义区分：

- `PHANTUN_ADDR4` / `PHANTUN_ADDR6`：内核实际看到的接口本地地址
- `PHANTUN_PEER4` / `PHANTUN_PEER6`：内核实际看到的 point-to-point peer/destination 地址

## 脚本约定

agent 每次都执行同一个脚本：

```bash
/bin/sh /opt/phantun-agent/handler.sh
```

不会传任何额外命令行参数。

脚本只需要遵守这几条：

1. 从环境变量读取 phase 和 payload
2. 成功时 `exit 0`
3. 失败时返回非零退出码
4. 失败原因尽量写到 `stderr`

仓库里已经附带了一个可直接修改的模板：

```bash
tools/control-plane-shell-agent/handler.template.sh
```

另外还补了三份更偏“直接改防火墙规则”的版本：

```bash
tools/control-plane-shell-agent/handler.iptables.template.sh
tools/control-plane-shell-agent/handler.nftables.template.sh
tools/control-plane-shell-agent/handler.openwrt.template.sh
```

建议这样选：

- 普通 Linux + `iptables`/`ip6tables`：`handler.iptables.template.sh`
- 普通 Linux + `nftables`：`handler.nftables.template.sh`
- OpenWrt `firewall4`（现代 OpenWrt，基于 nftables）：`handler.openwrt.template.sh`

如果是老的 OpenWrt `firewall3`/`iptables`，用 `handler.iptables.template.sh` 改。

这三份不是“只放行转发”的示例，而是按 Phantun 官方 NAT 语义写的：

- `client`：对 TUN 地址做 `SNAT/MASQUERADE`
- `server`：把监听的 TCP 端口 `DNAT` 到 TUN 地址
- `FORWARD` 放行只是配套规则，不是核心逻辑

它已经把环境变量收敛成普通 shell 变量，并按 phase 拆成了独立函数，
同时提供了：

- `log`：普通日志输出
- `fail`：输出错误并退出
- `run`：统一执行命令，方便后面包 `nft` / `iptables`
- `dump_context`：打印当前上下文，便于联调

你后续基本只需要把各个 phase 里的 `TODO` 替换成真正的防火墙命令。

示例：

```sh
#!/bin/sh
set -eu

PHASE="${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
DEV="${PHANTUN_DEV:-}"

run() {
  printf '+ %s\n' "$*"
  "$@"
}

case "${PHASE}" in
  pre_start)
    :
    ;;
  post_start)
    [ -n "${DEV}" ] || {
      echo "missing PHANTUN_DEV" >&2
      exit 1
    }
    # run nft add rule inet phantun input iifname "${DEV}" accept
    ;;
  sync_state)
    :
    ;;
  pre_stop)
    :
    ;;
  post_stop)
    :
    ;;
  *)
    echo "unknown phase: ${PHASE}" >&2
    exit 2
    ;;
esac
```

## 超时规则

每次请求的实际脚本超时时间为：

```text
min(request.deadline_ms, --max-script-timeout)
```

如果 `deadline_ms` 缺失或者为 `0`，就只使用
`--max-script-timeout`。

超时时：

1. agent 会杀掉整个脚本进程组
2. 返回 `success=false`
3. 响应消息会是 `script timed out after ...`

如果是 agent 自己正在退出导致脚本被取消，返回消息会是：

```text
script canceled by agent shutdown
```

## 响应规则

脚本执行结束后：

- 退出码为 `0` -> `success=true`
- 非零退出码 -> `success=false`

失败消息优先级：

1. `stderr`
2. `stdout`
3. 兜底消息，例如 `script exited with code 1`

响应里的消息会先做截断，避免脚本输出过大。

## 说明

1. 这个 agent 面向 Linux，依赖 UNIX socket 和进程组 kill 语义。
2. 因为脚本执行是全局串行的，所以多个 Phantun 实例共用一个 agent 时，会按顺序执行脚本。
3. 这是一个刻意保持最小化的实现，没有插件系统、自动重试，也没有内部 hook 框架。
