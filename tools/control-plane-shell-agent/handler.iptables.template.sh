#!/bin/sh
set -eu

# Linux iptables/ip6tables template.
# This follows Phantun's documented NAT model:
# - client: SNAT/MASQUERADE traffic from the tunnel peer address to the physical uplink
# - server: DNAT the listening TCP port to the tunnel peer address
# - pre_stop: remove NAT entry rules so new flows stop entering
# - post_stop: remove the remaining FORWARD rules and custom chains

: "${PHANTUN_PROTOCOL_VERSION:?missing PHANTUN_PROTOCOL_VERSION}"
: "${PHANTUN_KIND:?missing PHANTUN_KIND}"
: "${PHANTUN_SESSION_ID:?missing PHANTUN_SESSION_ID}"
: "${PHANTUN_REQUEST_ID:?missing PHANTUN_REQUEST_ID}"
: "${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
: "${PHANTUN_DEADLINE_MS:?missing PHANTUN_DEADLINE_MS}"
: "${PHANTUN_STATE:?missing PHANTUN_STATE}"
: "${PHANTUN_MODE:?missing PHANTUN_MODE}"

IPT="${IPTABLES_BIN:-iptables}"
IP6T="${IP6TABLES_BIN:-ip6tables}"

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
  printf '[phantun-agent][iptables] %s\n' "$*"
}

fail() {
  printf '[phantun-agent][iptables] %s\n' "$*" >&2
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
  value=$(printf '%s' "$1" | tr '[:upper:]/:-.' '[:lower:]_____' | tr -cd 'a-z0-9_' | cut -c1-18)
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

RESOURCE_ID="$(sanitize_id "${MODE}_${DEV:-${SESSION_ID}}_${LOCAL:-na}")"
FILTER_CHAIN="PHTF_${RESOURCE_ID}"
NAT_CHAIN="PHTN_${RESOURCE_ID}"

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
  log "filter_chain=${FILTER_CHAIN} nat_chain=${NAT_CHAIN}"
}

ipt() {
  tool="$1"
  table="$2"
  shift 2
  "$tool" -w -t "$table" "$@"
}

ipt_chain_exists() {
  tool="$1"
  table="$2"
  chain="$3"
  ipt "$tool" "$table" -nL "$chain" >/dev/null 2>&1
}

ipt_ensure_chain() {
  tool="$1"
  table="$2"
  chain="$3"

  if ! ipt_chain_exists "$tool" "$table" "$chain"; then
    run "$tool" -w -t "$table" -N "$chain"
  fi
}

ipt_flush_chain() {
  tool="$1"
  table="$2"
  chain="$3"

  if ipt_chain_exists "$tool" "$table" "$chain"; then
    run "$tool" -w -t "$table" -F "$chain"
  fi
}

ipt_delete_chain() {
  tool="$1"
  table="$2"
  chain="$3"

  if ipt_chain_exists "$tool" "$table" "$chain"; then
    ipt_flush_chain "$tool" "$table" "$chain"
    run "$tool" -w -t "$table" -X "$chain"
  fi
}

ipt_ensure_rule() {
  tool="$1"
  table="$2"
  chain="$3"
  shift 3

  if ! ipt "$tool" "$table" -C "$chain" "$@" >/dev/null 2>&1; then
    run "$tool" -w -t "$table" -A "$chain" "$@"
  fi
}

ipt_delete_rule() {
  tool="$1"
  table="$2"
  chain="$3"
  shift 3

  while ipt "$tool" "$table" -C "$chain" "$@" >/dev/null 2>&1; do
    run "$tool" -w -t "$table" -D "$chain" "$@"
  done
}

require_any_peer() {
  if [ -z "$PEER4" ] && [ -z "$PEER6" ]; then
    fail "requires PHANTUN_PEER4 or PHANTUN_PEER6"
  fi
}

require_ipv6_tool() {
  [ -n "$PEER6" ] || return 0
  tool_exists "$IP6T" || fail "ip6tables not found: ${IP6T}"
}

reset_filter_chain() {
  tool="$1"

  ipt_ensure_chain "$tool" filter "$FILTER_CHAIN"
  ipt_flush_chain "$tool" filter "$FILTER_CHAIN"
  ipt_ensure_rule "$tool" filter "$FILTER_CHAIN" -j ACCEPT
}

ensure_filter_jumps() {
  tool="$1"

  ipt_ensure_rule "$tool" filter FORWARD -i "$DEV" -j "$FILTER_CHAIN"
  ipt_ensure_rule "$tool" filter FORWARD -o "$DEV" -j "$FILTER_CHAIN"
}

remove_filter_jumps() {
  tool="$1"

  ipt_delete_rule "$tool" filter FORWARD -i "$DEV" -j "$FILTER_CHAIN"
  ipt_delete_rule "$tool" filter FORWARD -o "$DEV" -j "$FILTER_CHAIN"
}

cleanup_filter() {
  tool="$1"

  remove_filter_jumps "$tool"
  ipt_delete_chain "$tool" filter "$FILTER_CHAIN"
}

reset_nat_chain() {
  tool="$1"
  shift

  ipt_ensure_chain "$tool" nat "$NAT_CHAIN"
  ipt_flush_chain "$tool" nat "$NAT_CHAIN"
  ipt_ensure_rule "$tool" nat "$NAT_CHAIN" "$@"
}

ensure_client_nat_jump() {
  tool="$1"
  ipt_ensure_rule "$tool" nat POSTROUTING -j "$NAT_CHAIN"
}

ensure_server_nat_jump() {
  tool="$1"
  ipt_ensure_rule "$tool" nat PREROUTING -j "$NAT_CHAIN"
}

remove_client_nat_jump() {
  tool="$1"
  ipt_delete_rule "$tool" nat POSTROUTING -j "$NAT_CHAIN"
}

remove_server_nat_jump() {
  tool="$1"
  ipt_delete_rule "$tool" nat PREROUTING -j "$NAT_CHAIN"
}

cleanup_nat_chain() {
  tool="$1"

  remove_client_nat_jump "$tool"
  remove_server_nat_jump "$tool"
  ipt_delete_chain "$tool" nat "$NAT_CHAIN"
}

apply_client_ipv4() {
  [ -n "$PEER4" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || fail "client IPv4 requires a default uplink interface"

  tun_addr="$(cidr_addr "$PEER4")"
  reset_filter_chain "$IPT"
  ensure_filter_jumps "$IPT"
  reset_nat_chain "$IPT" -s "$tun_addr" -o "$iface" -j MASQUERADE
  ensure_client_nat_jump "$IPT"
}

apply_client_ipv6() {
  [ -n "$PEER6" ] || return 0
  require_ipv6_tool

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || fail "client IPv6 requires a default uplink interface"

  tun_addr="$(cidr_addr "$PEER6")"
  reset_filter_chain "$IP6T"
  ensure_filter_jumps "$IP6T"
  reset_nat_chain "$IP6T" -s "$tun_addr" -o "$iface" -j MASQUERADE
  ensure_client_nat_jump "$IP6T"
}

apply_server_ipv4() {
  [ -n "$PEER4" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || fail "server IPv4 requires a default uplink interface"

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  tun_addr="$(cidr_addr "$PEER4")"
  reset_filter_chain "$IPT"
  ensure_filter_jumps "$IPT"
  reset_nat_chain "$IPT" -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
  ensure_server_nat_jump "$IPT"
}

apply_server_ipv6() {
  [ -n "$PEER6" ] || return 0
  require_ipv6_tool

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || fail "server IPv6 requires a default uplink interface"

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  tun_addr="$(cidr_addr "$PEER6")"
  reset_filter_chain "$IP6T"
  ensure_filter_jumps "$IP6T"
  reset_nat_chain "$IP6T" -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
  ensure_server_nat_jump "$IP6T"
}

apply_rules() {
  [ -n "$DEV" ] || fail "post_start requires PHANTUN_DEV"
  require_any_peer
  require_ipv6_tool

  case "$MODE" in
    client)
      apply_client_ipv4
      apply_client_ipv6
      ;;
    server)
      apply_server_ipv4
      apply_server_ipv6
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
}

quiesce_rules() {
  case "$MODE" in
    client|server)
      cleanup_nat_chain "$IPT"
      if [ -n "$PEER6" ]; then
        cleanup_nat_chain "$IP6T"
      fi
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
}

cleanup_rules() {
  quiesce_rules
  cleanup_filter "$IPT"
  if [ -n "$PEER6" ]; then
    cleanup_filter "$IP6T"
  fi
}

pre_start() {
  log "pre_start"
  dump_context

  tool_exists "$IPT" || fail "iptables not found: ${IPT}"
  tool_exists ip || fail "ip not found"
  require_ipv6_tool

  # TODO:
  # Add any pre-flight checks here, such as verifying sysctl forwarding state.
  :
}

post_start() {
  log "post_start"
  dump_context
  apply_rules
}

sync_state() {
  log "sync_state"
  dump_context

  case "$STATE" in
    post_start|running)
      apply_rules
      ;;
    pre_stop|stopping)
      quiesce_rules
      ;;
    post_stop|stopped)
      cleanup_rules
      ;;
    *)
      log "sync_state no-op for state=${STATE}"
      ;;
  esac
}

pre_stop() {
  log "pre_stop"
  dump_context
  quiesce_rules
}

post_stop() {
  log "post_stop"
  dump_context
  cleanup_rules
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
