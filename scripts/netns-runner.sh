#!/bin/sh
# Cargo's runner for the network-namespace tier (scripts/netns-test.sh):
# runs a test binary as root of a new user namespace, in its own network,
# mount and PID namespaces, with a private /run for `ip netns`. When the
# binary exits, the PID namespace ends and takes any process it left.
set -eu
exec unshare --user --map-root-user --net --mount --pid --fork --kill-child --mount-proc \
    sh -c 'mount -t tmpfs none /run && UDPSTP_NETNS=1 exec "$@"' netns-runner "$@"
