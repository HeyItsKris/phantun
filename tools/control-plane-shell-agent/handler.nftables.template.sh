#!/bin/sh
set -eu

# Linux nftables template.
# This follows Phantun's documented NAT model:
# - client: srcnat/masquerade the tunnel peer address on the uplink
# - server: dstnat the listening TCP port to the tunnel peer address
# A forward chain is added as supporting policy for tunnel traffic.

: "${PHANTUN_PROTOCOL_VERSION:?missing PHANTUN_PROTOCOL_VERSION}"
: "${PHANTUN_KIND:?missing PHANTUN_KIND}"
: "${PHANTUN_SESSION_ID:?missing PHANTUN_SESSION_ID}"
: "${PHANTUN_REQUEST_ID:?missing PHANTUN_REQUEST_ID}"
: "${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
: "${PHANTUN_DEADLINE_MS:?missing PHANTUN_DEADLINE_MS}"
: "${PHANTUN_STATE:?missing PHANTUN_STATE}"
: "${PHANTUN_MODE:?missing PHANTUN_MODE}"

NFT="${NFT_BIN:-nft}"

SESSION_ID="${PHANTUN_SESSION_ID}"
REQUEST_ID="${PHANTUN_REQUEST_ID}"
PHASE="${PHANTUN_PHASE}"
STATE="${PHANTUN_STATE}"
MODE="${PHANTUN_MODE}"
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
  printf '[phantun-agent][nftables] %s\n' "$*"
}

fail() {
  printf '[phantun-agent][nftables] %s\n' "$*" >&2
  exit 1
}

run() {
  log "+ $*"
  "$@"
}

tool_exists() {
  command -v "$1" >/dev/null 2>&1
}

sanitize_id() {
  value=$(printf '%s' "$1" | tr '[:upper:]-.' '[:lower:]__' | tr -cd 'a-z0-9_' | cut -c1-24)
  [ -n "$value" ] || value="default"
  printf '%s' "$value"
}

cidr_addr() {
  printf '%s' "$1" | awk -F/ '{print $1}'
}

default_iface_v4() {
  ip -4 route show default 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") {print $(i + 1); exit}}'
}

default_iface_v6() {
  ip -6 route show default 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") {print $(i + 1); exit}}'
}

parse_port() {
  value="$1"
  if [ -z "$value" ]; then
    return 1
  fi
  printf '%s' "${value##*:}"
}

TABLE_NAME="phantun_$(sanitize_id "${DEV:-${SESSION_ID}}")"

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
  log "table=${TABLE_NAME}"
}

table_exists() {
  "$NFT" list table inet "$TABLE_NAME" >/dev/null 2>&1
}

delete_table() {
  if table_exists; then
    run "$NFT" delete table inet "$TABLE_NAME"
  fi
}

render_client_ruleset() {
  iface4="$(default_iface_v4)"
  iface6="$(default_iface_v6)"
  addr4=""
  addr6=""
  [ -n "$PEER4" ] && addr4="$(cidr_addr "$PEER4")"
  [ -n "$PEER6" ] && addr6="$(cidr_addr "$PEER6")"

  cat <<EOF
table inet ${TABLE_NAME} {
  chain forward {
    type filter hook forward priority 0; policy accept;
    iifname "${DEV}" accept
    oifname "${DEV}" accept
  }

  chain postrouting {
    type nat hook postrouting priority srcnat; policy accept;
EOF
  if [ -n "$addr4" ] && [ -n "$iface4" ]; then
    cat <<EOF
    ip saddr ${addr4} oifname "${iface4}" masquerade
EOF
  fi
  if [ -n "$addr6" ] && [ -n "$iface6" ]; then
    cat <<EOF
    ip6 saddr ${addr6} oifname "${iface6}" masquerade
EOF
  fi
  cat <<EOF
  }

  # TODO:
  # Add your real client-side nftables policy here if the sample rules are not enough.
}
EOF
}

render_server_ruleset() {
  iface4="$(default_iface_v4)"
  iface6="$(default_iface_v6)"
  addr4=""
  addr6=""
  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  [ -n "$PEER4" ] && addr4="$(cidr_addr "$PEER4")"
  [ -n "$PEER6" ] && addr6="$(cidr_addr "$PEER6")"

  cat <<EOF
table inet ${TABLE_NAME} {
  chain forward {
    type filter hook forward priority 0; policy accept;
    iifname "${DEV}" accept
    oifname "${DEV}" accept
  }

  chain prerouting {
    type nat hook prerouting priority dstnat; policy accept;
EOF
  if [ -n "$addr4" ] && [ -n "$iface4" ]; then
    cat <<EOF
    iifname "${iface4}" tcp dport ${port} dnat ip to ${addr4}
EOF
  fi
  if [ -n "$addr6" ] && [ -n "$iface6" ]; then
    cat <<EOF
    iifname "${iface6}" tcp dport ${port} dnat ip6 to ${addr6}
EOF
  fi
  cat <<EOF
  }

  # TODO:
  # Add your real server-side nftables policy here if the sample rules are not enough.
}
EOF
}

apply_ruleset() {
  [ -n "$DEV" ] || fail "post_start requires PHANTUN_DEV"

  delete_table
  case "$MODE" in
    client)
      render_client_ruleset | run "$NFT" -f -
      ;;
    server)
      render_server_ruleset | run "$NFT" -f -
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
}

pre_start() {
  log "pre_start"
  dump_context

  tool_exists "$NFT" || fail "nft not found: ${NFT}"

  # TODO:
  # Add any pre-flight checks here, such as verifying sysctl forwarding state.
  :
}

post_start() {
  log "post_start"
  dump_context
  apply_ruleset
}

sync_state() {
  log "sync_state"
  dump_context

  case "$STATE" in
    post_start|running)
      apply_ruleset
      ;;
    post_stop|stopped)
      post_stop
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
  # Add drain/quarantine logic here if needed.
  :
}

post_stop() {
  log "post_stop"
  dump_context
  delete_table
}

main() {
  case "$PHASE" in
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
