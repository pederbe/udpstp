#!/bin/sh
# Checks the example systemd unit (contrib/systemd/udpstp-server.service):
# systemd-analyze verify, and its security exposure at most
# $UDPSTP_UNIT_EXPOSURE tenths (default 20, an exposure of 2.0). With a
# udpstp binary as argument and a systemd user manager running, it also
# runs the unit as a user service, with its sandboxing, and a Test
# against it over loopback. A user service can't take DynamicUser= (it
# runs as you) or hide your home, so those two are left out; the system
# unit's own run is CI's (ci.yml).
#
# Usage: scripts/systemd-check.sh [UDPSTP]
set -eu

cd "$(dirname "$0")/.."
unit=contrib/systemd/udpstp-server.service
limit=${UDPSTP_UNIT_EXPOSURE:-20}
user_unit="$HOME/.config/systemd/user/udpstp-check.service"
# Not in /tmp: the unit's PrivateTmp= hides it from the service.
mkdir -p "$HOME/.cache"
work=$(mktemp -d -p "$HOME/.cache" udpstp-unit.XXXXXX)
cleanup() {
    if [ -f "$user_unit" ]; then
        systemctl --user stop udpstp-check.service 2>/dev/null || true
        rm -f "$user_unit"
        systemctl --user daemon-reload
    fi
    rm -rf "$work"
}
trap cleanup EXIT

# verify wants ExecStart's program to exist.
sed "s|/usr/local/bin/udpstp|/bin/true|" "$unit" >"$work/udpstp-server.service"
systemd-analyze verify "$work/udpstp-server.service"
if ! systemd-analyze security --offline=true --threshold="$limit" --no-pager \
    "$work/udpstp-server.service" >"$work/security.txt"; then
    cat "$work/security.txt"
    exit 1
fi
tail -n 1 "$work/security.txt"

binary=${1:-}
if [ -z "$binary" ]; then
    exit 0
fi
if ! systemctl --user show-environment >/dev/null 2>&1; then
    echo "No systemd user manager: the unit's run is skipped"
    exit 0
fi
cp "$binary" "$work/udpstp"
port=24611
printf '[[key]]\nadmin_key_name = "check"\nkey_id = 1\nkey_text = "systemd-check-key"\n' >"$work/keys.toml"
chmod 600 "$work/keys.toml"
printf 'port = %s\nephemeral_ports = "50000-50383"\nmax_hops = 8\n' "$port" >"$work/server.toml"
mkdir -p "$(dirname "$user_unit")"
sed -e "s|/usr/local/bin/udpstp|$work/udpstp|" \
    -e "s|/etc/udpstp/keys.toml|$work/keys.toml|" \
    -e "s|/etc/udpstp/server.toml|$work/server.toml|" \
    -e "/^DynamicUser=/d" \
    -e "s|^RuntimeDirectory=udpstp$|RuntimeDirectory=udpstp-check|" \
    -e "s|%t/udpstp/|%t/udpstp-check/|g" \
    -e "s|^ProtectHome=yes|ProtectHome=read-only|" \
    "$unit" >"$user_unit"
systemctl --user daemon-reload
systemctl --user start udpstp-check.service
sleep 1
if ! systemctl --user is-active --quiet udpstp-check.service; then
    journalctl --user -u udpstp-check.service --no-pager | tail -n 20
    exit 1
fi
if "$work/udpstp" client --key-file "$work/keys.toml" --test-interval 5 "127.0.0.1:$port" >"$work/client.log" 2>&1; then
    echo "A Test ran against the sandboxed Server"
else
    cat "$work/client.log"
    journalctl --user -u udpstp-check.service --no-pager | tail -n 20
    exit 1
fi
