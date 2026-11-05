#!/bin/bash
# Install udpstp from a prepared repository in clean Debian, Ubuntu and
# AlmaLinux containers, with signature checks on, as a user following the
# installation guide would; then upgrade from PREVIOUS when given, run the
# binary and remove the package.
#
# usage: bash contrib/package-repository/test.sh REPOSITORY VERSION [PREVIOUS]
#
# The repository is mounted read-only and served as file:// URLs, so nothing
# is fetched from packages.udpstp.com. Needs podman or docker, and runs the
# host's architecture only (amd64 or arm64).
set -euo pipefail
repository=$(realpath "$1") version=$2 previous=${3:-}
engine=${CONTAINER_ENGINE:-$(command -v podman || command -v docker)}
# Debian 10 and RHEL 8 are the oldest the glibc build supports (glibc 2.28).
debian_images=(docker.io/library/debian:10 docker.io/library/debian:12 docker.io/library/ubuntu:24.04)
rpm_images=(docker.io/library/almalinux:8 docker.io/library/fedora:42)

check_installed() { # the binary, the documented files and the unit, not enabled
    cat <<'CHECK'
test "$(udpstp --version)" = "udpstp $1"
test -f /usr/lib/systemd/system/udpstp-server.service || test -f /lib/systemd/system/udpstp-server.service
test ! -e /etc/systemd/system/multi-user.target.wants/udpstp-server.service
CHECK
}

for image in "${debian_images[@]}"; do
    echo "== $image"
    "$engine" run --rm --network none -v "$repository:/repository:ro" \
        -e VERSION="$version" -e PREVIOUS="$previous" "$image" bash -euo pipefail -c '
        export DEBIAN_FRONTEND=noninteractive
        # Old releases move to the archive; the image keeps working offline,
        # so only the udpstp repository is consulted.
        rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*
        install -m 644 /repository/udpstp.gpg /usr/share/keyrings/udpstp.gpg
        arch=$(dpkg --print-architecture)
        echo "deb [arch=$arch signed-by=/usr/share/keyrings/udpstp.gpg] file:///repository/apt stable main" \
            > /etc/apt/sources.list.d/udpstp.list
        apt-get update -q
        if [ -n "$PREVIOUS" ]; then
            apt-get install -qy "udpstp=$PREVIOUS-1"
            test "$(udpstp --version)" = "udpstp $PREVIOUS"
            apt-get upgrade -qy
        else
            apt-get install -qy udpstp
        fi
        '"$(check_installed)"'
        apt-get remove -qy udpstp
        ! command -v udpstp
    ' check "$version"
done

for image in "${rpm_images[@]}"; do
    echo "== $image"
    "$engine" run --rm --network none -v "$repository:/repository:ro" \
        -e VERSION="$version" -e PREVIOUS="$previous" "$image" bash -euo pipefail -c '
        # The published udpstp.repo, pointed at the mounted snapshot.
        snapshot=$(sed "s|^https://packages.udpstp.com/|/repository/|" /repository/rpm/mirrorlist)
        sed -e "s|^mirrorlist=.*|baseurl=file://$snapshot|" \
            -e "s|^gpgkey=.*|gpgkey=file:///repository/udpstp.asc|" \
            /repository/udpstp.repo > /etc/yum.repos.d/udpstp.repo
        dnf=(dnf -q -y --disablerepo="*" --enablerepo=udpstp)
        if [ -n "$PREVIOUS" ]; then
            "${dnf[@]}" install "udpstp-$PREVIOUS"
            test "$(udpstp --version)" = "udpstp $PREVIOUS"
            "${dnf[@]}" upgrade udpstp
        else
            "${dnf[@]}" install udpstp
        fi
        rpm -q --qf "%{SIGPGP:pgpsig}%{RSAHEADER:pgpsig}\n" udpstp | grep -q "Key ID"
        '"$(check_installed)"'
        "${dnf[@]}" remove udpstp
        ! command -v udpstp
    ' check "$version"
done
echo "repository installs checked for udpstp $version${previous:+, upgraded from $previous}"
