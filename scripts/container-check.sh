#!/bin/sh
# Builds the Server's container image (contrib/container/Containerfile),
# runs it with the host's network and a Key Table secret, runs a
# Test against it with the given udpstp binary, and checks that it stops
# on SIGTERM. Uses podman, or docker where podman isn't installed.
#
# Usage: scripts/container-check.sh UDPSTP [PREBUILT_IMAGE]
set -eu

cd "$(dirname "$0")/.."
client=$1
engine=${UDPSTP_CONTAINER_ENGINE:-$(command -v podman || command -v docker)}
image=${2:-localhost/udpstp:check}
port=24613
work=$(mktemp -d)
name=udpstp-check-$(basename "$work")
owned=false
cleanup() {
    if [ "$owned" = true ]; then
        "$engine" rm -f "$name" >/dev/null 2>&1 || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

if [ "$#" -eq 1 ]; then
    "$engine" build -q -f contrib/container/Containerfile -t "$image" . >/dev/null
fi
printf '[[key]]\nadmin_key_name = "check"\nkey_id = 1\nkey_text = "container-check-key"\n' >"$work/keys.toml"
chmod 600 "$work/keys.toml"
# The secret as the image's user owns it; a bind mount stands in for it
# with docker, which has no secrets outside swarm.
cp "$work/keys.toml" "$work/secret.toml"
if [ "$(basename "$engine")" = podman ]; then
    podman unshare chown 65532:65532 "$work/secret.toml"
else
    sudo chown 65532:65532 "$work/secret.toml"
fi
"$engine" run -d --name "$name" --network host \
    -v "$work/secret.toml:/run/secrets/keys.toml:ro,Z" \
    "$image" --port "$port" --max-hops 8 >/dev/null
owned=true
sleep 2
if ! "$client" client --key-file "$work/keys.toml" --test-interval 5 "127.0.0.1:$port" >"$work/client.log" 2>&1; then
    cat "$work/client.log"
    "$engine" logs "$name"
    exit 1
fi
echo "A Test ran against the container"
started=$(date +%s)
"$engine" stop -t 10 "$name" >/dev/null
if [ $(($(date +%s) - started)) -ge 10 ]; then
    echo "The container didn't stop on SIGTERM"
    exit 1
fi
echo "The container stopped on SIGTERM"
