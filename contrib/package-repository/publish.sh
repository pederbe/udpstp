#!/bin/bash
# Upload a prepared repository to R2. Storage credentials use the AWS environment.
# usage: bash contrib/package-repository/publish.sh REPOSITORY BUCKET
set -euo pipefail
cd "$1"
bucket=$2
# Upload immutable packages, by-hash indexes and complete dnf snapshots first.
# Keep every earlier object for clients that fetched an older signed index or
# mirrorlist. Do not delete or overwrite an older snapshot during publication.
aws s3 sync --only-show-errors . "s3://$bucket" \
    --exclude 'apt/dists/stable/main/binary-*/Packages*' \
    --exclude 'apt/dists/stable/Release*' --exclude 'apt/dists/stable/InRelease' \
    --exclude 'rpm/mirrorlist' --exclude 'udpstp.*' \
    --cache-control 'public, max-age=31536000, immutable'
# Then advance the current indexes, which clients revalidate on every fetch.
for path in udpstp.asc udpstp.gpg udpstp.repo \
    apt/dists/stable/main/binary-amd64/Packages apt/dists/stable/main/binary-amd64/Packages.gz \
    apt/dists/stable/main/binary-arm64/Packages apt/dists/stable/main/binary-arm64/Packages.gz \
    apt/dists/stable/Release apt/dists/stable/Release.gpg apt/dists/stable/InRelease \
    rpm/mirrorlist; do
    aws s3 cp --only-show-errors "$path" "s3://$bucket/$path" --cache-control 'no-cache'
done
