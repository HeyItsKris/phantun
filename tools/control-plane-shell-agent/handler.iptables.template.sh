#!/bin/sh
set -eu

# Linux iptables/ip6tables template.
# This follows Phantun's documented NAT model:
# - client: SNAT/MASQUERADE traffic from the tunnel to the physical uplink
# - server: DNAT the listening TCP port to the tunnel address
# FORWARD accept rules are added as supporting rules, not the main feature.

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

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
}

ipt() {
  tool="$1"
  table="$2"
  shift 2
  "$tool" -w -t "$table" "$@"
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

ensure_forward_rules() {
  tool="$1"

  ipt_ensure_rule "$tool" filter FORWARD -i "$DEV" -j ACCEPT
  ipt_ensure_rule "$tool" filter FORWARD -o "$DEV" -j ACCEPT
}

remove_forward_rules() {
  tool="$1"

  ipt_delete_rule "$tool" filter FORWARD -i "$DEV" -j ACCEPT
  ipt_delete_rule "$tool" filter FORWARD -o "$DEV" -j ACCEPT
}

apply_client_ipv4() {
  nat_addr="${PEER4:-$ADDR4}"
  [ -n "$nat_addr" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || fail "client IPv4 requires a default uplink interface"

  tun_addr="$(cidr_addr "$nat_addr")"
  ensure_forward_rules "$IPT"
  ipt_ensure_rule "$IPT" nat POSTROUTING -s "$tun_addr" -o "$iface" -j MASQUERADE
}

remove_client_ipv4() {
  nat_addr="${PEER4:-$ADDR4}"
  [ -n "$nat_addr" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || return 0

  tun_addr="$(cidr_addr "$nat_addr")"
  ipt_delete_rule "$IPT" nat POSTROUTING -s "$tun_addr" -o "$iface" -j MASQUERADE
  remove_forward_rules "$IPT"
}

apply_client_ipv6() {
  nat_addr="${PEER6:-$ADDR6}"
  [ -n "$nat_addr" ] || return 0
  tool_exists "$IP6T" || return 0

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || fail "client IPv6 requires a default uplink interface"

  tun_addr="$(cidr_addr "$nat_addr")"
  ensure_forward_rules "$IP6T"
  ipt_ensure_rule "$IP6T" nat POSTROUTING -s "$tun_addr" -o "$iface" -j MASQUERADE
}

remove_client_ipv6() {
  nat_addr="${PEER6:-$ADDR6}"
  [ -n "$nat_addr" ] || return 0
  tool_exists "$IP6T" || return 0

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || return 0

  tun_addr="$(cidr_addr "$nat_addr")"
  ipt_delete_rule "$IP6T" nat POSTROUTING -s "$tun_addr" -o "$iface" -j MASQUERADE
  remove_forward_rules "$IP6T"
}

apply_server_ipv4() {
  nat_addr="${PEER4:-$ADDR4}"
  [ -n "$nat_addr" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || fail "server IPv4 requires a default uplink interface"

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  tun_addr="$(cidr_addr "$nat_addr")"
  ensure_forward_rules "$IPT"
  ipt_ensure_rule "$IPT" nat PREROUTING -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
}

remove_server_ipv4() {
  nat_addr="${PEER4:-$ADDR4}"
  [ -n "$nat_addr" ] || return 0

  iface="$(default_iface_v4)"
  [ -n "$iface" ] || return 0

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || return 0

  tun_addr="$(cidr_addr "$nat_addr")"
  ipt_delete_rule "$IPT" nat PREROUTING -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
  remove_forward_rules "$IPT"
}

apply_server_ipv6() {
  nat_addr="${PEER6:-$ADDR6}"
  [ -n "$nat_addr" ] || return 0
  tool_exists "$IP6T" || return 0

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || fail "server IPv6 requires a default uplink interface"

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  tun_addr="$(cidr_addr "$nat_addr")"
  ensure_forward_rules "$IP6T"
  ipt_ensure_rule "$IP6T" nat PREROUTING -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
}

remove_server_ipv6() {
  nat_addr="${PEER6:-$ADDR6}"
  [ -n "$nat_addr" ] || return 0
  tool_exists "$IP6T" || return 0

  iface="$(default_iface_v6)"
  [ -n "$iface" ] || return 0

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || return 0

  tun_addr="$(cidr_addr "$nat_addr")"
  ipt_delete_rule "$IP6T" nat PREROUTING -p tcp -i "$iface" --dport "$port" -j DNAT --to-destination "$tun_addr"
  remove_forward_rules "$IP6T"
}

pre_start() {
  log "pre_start"
  dump_context

  tool_exists "$IPT" || fail "iptables not found: ${IPT}"

  # TODO:
  # Add any pre-flight checks here, such as verifying sysctl forwarding state.
  :
}

post_start() {
  log "post_start"
  dump_context

  [ -n "$DEV" ] || fail "post_start requires PHANTUN_DEV"

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

sync_state() {
  log "sync_state"
  dump_context

  case "$STATE" in
    post_start|running)
      post_start
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

  case "$MODE" in
    client)
      remove_client_ipv4
      remove_client_ipv6
      ;;
    server)
      remove_server_ipv4
      remove_server_ipv6
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
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
