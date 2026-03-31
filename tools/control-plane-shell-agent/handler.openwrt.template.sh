#!/bin/sh
set -eu

# OpenWrt template for firewall4 (nftables-based, typical on OpenWrt 22+).
# This follows Phantun's documented NAT model:
# - client: srcnat/masquerade tunnel peer traffic on WAN
# - server: dstnat the listening TCP port to the tunnel peer address
# - pre_stop: remove NAT entry rules so new flows stop entering
# - post_stop: remove the remaining forward rules
#
# The generated include file appends rules to fw4's existing chains instead of
# creating a parallel forward base chain.

: "${PHANTUN_PROTOCOL_VERSION:?missing PHANTUN_PROTOCOL_VERSION}"
: "${PHANTUN_KIND:?missing PHANTUN_KIND}"
: "${PHANTUN_SESSION_ID:?missing PHANTUN_SESSION_ID}"
: "${PHANTUN_REQUEST_ID:?missing PHANTUN_REQUEST_ID}"
: "${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
: "${PHANTUN_DEADLINE_MS:?missing PHANTUN_DEADLINE_MS}"
: "${PHANTUN_STATE:?missing PHANTUN_STATE}"
: "${PHANTUN_MODE:?missing PHANTUN_MODE}"

FW4="${FW4_BIN:-fw4}"
RULESET_DIR="${RULESET_DIR:-/usr/share/nftables.d/ruleset-post}"

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
  printf '[phantun-agent][openwrt] %s\n' "$*"
}

fail() {
  printf '[phantun-agent][openwrt] %s\n' "$*" >&2
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
  value=$(printf '%s' "$1" | tr '[:upper:]/:-.' '[:lower:]_____' | tr -cd 'a-z0-9_' | cut -c1-24)
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

RULE_TAG="phantun_$(sanitize_id "${MODE}_${DEV:-${SESSION_ID}}_${LOCAL:-na}")"
RULESET_FILE="${RULESET_DIR}/90-${RULE_TAG}.nft"

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
  log "rule_tag=${RULE_TAG} include=${RULESET_FILE}"
}

require_any_peer() {
  if [ -z "$PEER4" ] && [ -z "$PEER6" ]; then
    fail "requires PHANTUN_PEER4 or PHANTUN_PEER6"
  fi
}

reload_firewall() {
  tool_exists "$FW4" || fail "fw4 not found: ${FW4}"
  run "$FW4" reload
}

render_forward_rules() {
  cat <<EOF
add rule inet fw4 forward iifname "${DEV}" counter accept comment "${RULE_TAG}"
add rule inet fw4 forward oifname "${DEV}" counter accept comment "${RULE_TAG}"
EOF
}

render_client_nat_rules() {
  if [ -n "$PEER4" ]; then
    iface4="$(default_iface_v4)"
    [ -n "$iface4" ] || fail "client IPv4 requires a default uplink interface"
    addr4="$(cidr_addr "$PEER4")"
    cat <<EOF
add rule inet fw4 srcnat ip saddr ${addr4} oifname "${iface4}" counter masquerade comment "${RULE_TAG}"
EOF
  fi

  if [ -n "$PEER6" ]; then
    iface6="$(default_iface_v6)"
    [ -n "$iface6" ] || fail "client IPv6 requires a default uplink interface"
    addr6="$(cidr_addr "$PEER6")"
    cat <<EOF
add rule inet fw4 srcnat ip6 saddr ${addr6} oifname "${iface6}" counter masquerade comment "${RULE_TAG}"
EOF
  fi
}

render_server_nat_rules() {
  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  if [ -n "$PEER4" ]; then
    iface4="$(default_iface_v4)"
    [ -n "$iface4" ] || fail "server IPv4 requires a default uplink interface"
    addr4="$(cidr_addr "$PEER4")"
    cat <<EOF
add rule inet fw4 dstnat iifname "${iface4}" tcp dport ${port} counter dnat ip to ${addr4} comment "${RULE_TAG}"
EOF
  fi

  if [ -n "$PEER6" ]; then
    iface6="$(default_iface_v6)"
    [ -n "$iface6" ] || fail "server IPv6 requires a default uplink interface"
    addr6="$(cidr_addr "$PEER6")"
    cat <<EOF
add rule inet fw4 dstnat iifname "${iface6}" tcp dport ${port} counter dnat ip6 to ${addr6} comment "${RULE_TAG}"
EOF
  fi
}

write_ruleset() {
  mode="$1"

  [ -n "$DEV" ] || fail "post_start requires PHANTUN_DEV"
  mkdir -p "$RULESET_DIR"

  tmp_file="${RULESET_FILE}.tmp.$$"
  {
    render_forward_rules
    if [ "$mode" = "full" ]; then
      case "$MODE" in
        client)
          render_client_nat_rules
          ;;
        server)
          render_server_nat_rules
          ;;
        *)
          fail "unsupported PHANTUN_MODE: ${MODE}"
          ;;
      esac
    fi
  } >"$tmp_file"

  mv "$tmp_file" "$RULESET_FILE"
  reload_firewall
}

remove_ruleset() {
  if [ -f "$RULESET_FILE" ]; then
    run rm -f "$RULESET_FILE"
    reload_firewall
  fi
}

apply_rules() {
  require_any_peer
  write_ruleset full
}

quiesce_rules() {
  write_ruleset forward-only
}

cleanup_rules() {
  remove_ruleset
}

pre_start() {
  log "pre_start"
  dump_context

  tool_exists "$FW4" || fail "fw4 not found: ${FW4}"
  tool_exists ip || fail "ip not found"

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
