#!/bin/sh
set -eu

# Copy this file to your real handler path and replace the TODO blocks with
# nftables/iptables commands.

: "${PHANTUN_PROTOCOL_VERSION:?missing PHANTUN_PROTOCOL_VERSION}"
: "${PHANTUN_KIND:?missing PHANTUN_KIND}"
: "${PHANTUN_SESSION_ID:?missing PHANTUN_SESSION_ID}"
: "${PHANTUN_REQUEST_ID:?missing PHANTUN_REQUEST_ID}"
: "${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
: "${PHANTUN_DEADLINE_MS:?missing PHANTUN_DEADLINE_MS}"
: "${PHANTUN_STATE:?missing PHANTUN_STATE}"
: "${PHANTUN_MODE:?missing PHANTUN_MODE}"

SESSION_ID="${PHANTUN_SESSION_ID}"
REQUEST_ID="${PHANTUN_REQUEST_ID}"
PHASE="${PHANTUN_PHASE}"
STATE="${PHANTUN_STATE}"
MODE="${PHANTUN_MODE}"
DEADLINE_MS="${PHANTUN_DEADLINE_MS}"
REASON="${PHANTUN_REASON:-}"
LOCAL="${PHANTUN_LOCAL:-}"
REMOTE="${PHANTUN_REMOTE:-}"
DEV="${PHANTUN_DEV:-}"
MTU="${PHANTUN_MTU:-}"
ADDR4="${PHANTUN_ADDR4:-}"
ADDR6="${PHANTUN_ADDR6:-}"
PEER4="${PHANTUN_PEER4:-}"
PEER6="${PHANTUN_PEER6:-}"

log() {
  printf '[phantun-agent] %s\n' "$*"
}

fail() {
  printf '[phantun-agent] %s\n' "$*" >&2
  exit 1
}

run() {
  log "+ $*"
  "$@"
}

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
}

pre_start() {
  log "pre_start"
  dump_context

  # TODO:
  # 在这里放启动前就必须准备好的规则，例如：
  # run nft add table inet phantun
  # run nft add chain inet phantun input '{ type filter hook input priority 0; }'
  :
}

post_start() {
  log "post_start"
  dump_context

  [ -n "${DEV}" ] || fail "post_start requires PHANTUN_DEV"

  # TODO:
  # 在这里放依赖真实接口信息的规则，例如：
  # run nft add set inet phantun peers '{ type ipv4_addr; }'
  # run nft add rule inet phantun input iifname "${DEV}" accept
  :
}

sync_state() {
  log "sync_state"
  dump_context

  case "${STATE}" in
    running|post_start)
      # TODO:
      # agent 重连后建议把这里写成“校准/补齐规则”的逻辑，而不是简单重复追加。
      # 例如先检查 table/chain/set 是否存在，不存在则补上。
      :
      ;;
    pre_stop|stopping|post_stop|stopped)
      # TODO:
      # 如果你的规则需要在停止阶段切状态，也可以在这里做 reconcile。
      :
      ;;
    *)
      log "sync_state no-op for state=${STATE}"
      ;;
  esac
}

pre_stop() {
  log "pre_stop"
  dump_context

  # TODO:
  # 在这里放停止前的准备动作，例如先摘流量、打标记、切换策略等。
  :
}

post_stop() {
  log "post_stop"
  dump_context

  # TODO:
  # 在这里做最终清理，例如删除规则、chain、set、table。
  # 如果你担心重复执行，尽量写成幂等删除。
  :
}

main() {
  case "${PHASE}" in
    pre_start)
      pre_start
      ;;
    post_start)
      post_start
      ;;
    sync_state)
      sync_state
      ;;
    pre_stop)
      pre_stop
      ;;
    post_stop)
      post_stop
      ;;
    *)
      fail "unknown phase: ${PHASE}"
      ;;
  esac
}

main "$@"
