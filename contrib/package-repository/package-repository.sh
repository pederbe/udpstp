#!/bin/bash
# Add release packages to the signed apt and dnf repository and re-sign it.
#
# usage: bash contrib/package-repository/package-repository.sh REPOSITORY PUBLIC_KEY PACKAGE...
#
# REPOSITORY is a local copy of the published repository (empty the first
# time). PUBLIC_KEY is the committed signing key (udpstp.asc here); its secret key must
# be in the caller's GnuPG keyring, and GPG_PASSPHRASE holds its passphrase
# when it has one. Each PACKAGE is a verified .deb or .rpm from a release.
#
# The .deb is published unchanged. The .rpm gets a header signature; its
# header and payload digests are checked to be unchanged by signing. Versions
# already in the repository are kept, and a package that differs from the one
# already published under the same name is refused. Needs gpg, gpgv,
# apt-ftparchive, rpm, rpmsign and createrepo_c.
set -euo pipefail

repository=$(realpath "$1") public_key=$(realpath "$2")
shift 2
test $# -gt 0 || { echo "no packages given" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The signing key is the committed one, and its secret part is available.
mapfile -t fingerprints < <(gpg --show-keys --with-colons "$public_key" |
    awk -F: '$1 == "pub" {primary = 1; next} $1 == "fpr" && primary {print $10; primary = 0}')
test "${#fingerprints[@]}" -eq 1 ||
    { echo "$public_key must hold exactly one primary key" >&2; exit 1; }
fingerprint=${fingerprints[0]}
gpg --batch --list-secret-keys "$fingerprint" >/dev/null 2>&1 ||
    { echo "the secret key for $fingerprint is not in the keyring" >&2; exit 1; }

sign() { # GPG_ARGUMENTS...: sign with the release key, unattended
    if [ -n "${GPG_PASSPHRASE:-}" ]; then
        gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3 \
            --local-user "$fingerprint" --digest-algo SHA512 "$@" 3<<<"$GPG_PASSPHRASE"
    else
        gpg --batch --yes --local-user "$fingerprint" --digest-algo SHA512 "$@"
    fi
}

# An rpm database that trusts only the release key, to check RPM signatures.
rpmdb=$work/rpmdb
mkdir "$rpmdb"
rpmkeys --dbpath "$rpmdb" --import "$public_key"
rpm_signed() { # RPM: signed by the release key, with intact digests
    rpmkeys --dbpath "$rpmdb" --checksig "$1" | grep -q ': digests signatures OK$'
}
# RPM 6 renamed PAYLOADDIGEST to PAYLOADSHA256. Query the installed tool's
# supported tags so both current and older supported RPMs check real digests.
rpm_tags=$(rpm --querytags)
rpm_payload_tag=PAYLOADSHA256
if ! grep -qx "$rpm_payload_tag" <<< "$rpm_tags"; then
    rpm_payload_tag=PAYLOADDIGEST
    grep -qx "$rpm_payload_tag" <<< "$rpm_tags" ||
        { echo 'rpm has no supported SHA256 payload digest tag' >&2; exit 1; }
fi
rpm_identity() { # RPM: the digests that header signing must not change
    local identity
    identity=$(rpm --dbpath "$rpmdb" -qp --nosignature \
        --qf "%{SHA256HEADER} %{$rpm_payload_tag}\n" "$1") || return
    [[ $identity =~ ^[[:xdigit:]]{64}\ [[:xdigit:]]{64}$ ]] ||
        { echo "$1 has no valid SHA256 header and payload identity" >&2; return 1; }
    printf '%s\n' "$identity"
}

pool=$repository/apt/pool/main/u/udpstp
mkdir -p "$pool" "$repository/rpm"
for package in "$@"; do
    name=$(basename "$package")
    case $name in
    udpstp_*-1_amd64.deb | udpstp_*-1_arm64.deb)
        if [ -e "$pool/$name" ]; then
            cmp -s "$package" "$pool/$name" ||
                { echo "$name differs from the published package" >&2; exit 1; }
        else
            cp "$package" "$pool/$name"
        fi
        ;;
    udpstp-*-1.x86_64.rpm | udpstp-*-1.aarch64.rpm)
        cp "$package" "$work/$name"
        before=$(rpm_identity "$work/$name")
        # RPM 6 otherwise defaults to signatures that RPM 4.x clients cannot read.
        if [ -n "${GPG_PASSPHRASE:-}" ]; then
            rpmsign --addsign --rpmv4 --define "_gpg_name $fingerprint" \
                --define "_gpg_sign_cmd_extra_args --batch --pinentry-mode loopback --passphrase-fd 3" \
                "$work/$name" 3<<<"$GPG_PASSPHRASE" >/dev/null
        else
            rpmsign --addsign --rpmv4 --define "_gpg_name $fingerprint" \
                --define "_gpg_sign_cmd_extra_args --batch" "$work/$name" >/dev/null
        fi
        test "$(rpm_identity "$work/$name")" = "$before" ||
            { echo "signing changed the contents of $name" >&2; exit 1; }
        rpm_signed "$work/$name" || { echo "$name signature check failed" >&2; exit 1; }
        if [ -e "$repository/rpm/$name" ]; then
            test "$(rpm_identity "$repository/rpm/$name")" = "$before" ||
                { echo "$name differs from the published package" >&2; exit 1; }
        else
            mv "$work/$name" "$repository/rpm/$name"
        fi
        ;;
    *)
        echo "$name is not a udpstp .deb or .rpm" >&2
        exit 1
        ;;
    esac
done

# The indexers below list every package file they find, so the repository may
# hold only udpstp packages whose metadata matches their names, and every
# RPM must carry a valid release-key signature.
version_pattern='[0-9]+\.[0-9]+\.[0-9]+'
while IFS= read -r -d '' file; do
    name=${file#"$repository"/}
    if [[ $name =~ ^apt/pool/main/u/udpstp/udpstp_($version_pattern)-1_(amd64|arm64)\.deb$ ]]; then
        test "$(dpkg-deb -f "$file" Package Version Architecture | tr '\n' ' ')" = \
            "Package: udpstp Version: ${BASH_REMATCH[1]}-1 Architecture: ${BASH_REMATCH[2]} " ||
            { echo "$name does not match its control fields" >&2; exit 1; }
    elif [[ $name =~ ^rpm/udpstp-($version_pattern)-1\.(x86_64|aarch64)\.rpm$ ]]; then
        test "$(rpm --dbpath "$rpmdb" -qp --nosignature --qf '%{NAME} %{VERSION} %{ARCH}' "$file")" = \
            "udpstp ${BASH_REMATCH[1]} ${BASH_REMATCH[2]}" ||
            { echo "$name does not match its header" >&2; exit 1; }
        rpm_signed "$file" || { echo "$name is not signed by the release key" >&2; exit 1; }
    elif [[ ! $name =~ ^apt/dists/ ]] &&
        [[ ! $name =~ ^rpm/snapshots/[a-f0-9]{64}/repodata/ ]] &&
        [[ $name != rpm/mirrorlist ]] &&
        [[ ! $name =~ ^udpstp\.(asc|gpg|repo)$ ]]; then
        echo "unexpected file in the repository: $name" >&2
        exit 1
    fi
done < <(find "$repository" -type f -print0)

# Advertise only the newest release. Older files remain for clients already
# using a signed index and for the internal upgrade test's direct-file route.
latest=$(find "$pool" -name 'udpstp_*_amd64.deb' -printf '%f\n' |
    sed 's/^udpstp_\(.*\)-1_amd64\.deb$/\1/' | sort -V | tail -n 1)
test -n "$latest" || { echo 'no release packages in the repository' >&2; exit 1; }
for file in "$pool/udpstp_${latest}-1_amd64.deb" "$pool/udpstp_${latest}-1_arm64.deb" \
    "$repository/rpm/udpstp-${latest}-1.x86_64.rpm" "$repository/rpm/udpstp-${latest}-1.aarch64.rpm"; do
    test -f "$file" || { echo "missing latest release package: $file" >&2; exit 1; }
done

# apt: InRelease and Release.gpg sign the Release file holding the index digests.
apt=$repository/apt
dists=$apt/dists/stable
for arch in amd64 arm64; do
    binary=$dists/main/binary-$arch
    mkdir -p "$binary"
    (cd "$apt" && apt-ftparchive --arch "$arch" packages pool) |
        awk -v version="$latest-1" 'BEGIN { RS=""; FS="\n"; ORS="\n\n" }
            { for (i=1; i<=NF; i++) if ($i == "Version: " version) { print; break } }' > "$binary/Packages"
    gzip -9nkf "$binary/Packages"
    # Old signed InRelease files must still find their exact indexes after an
    # update. Keep both strong hash forms that the supported apt clients can use.
    for algorithm in SHA256 SHA512; do
        hash_directory=$binary/by-hash/$algorithm
        mkdir -p "$hash_directory"
        for index in Packages Packages.gz; do
            file=$binary/$index
            digest=$("${algorithm,,}sum" < "$file" | cut -d' ' -f1)
            if [ -e "$hash_directory/$digest" ]; then
                cmp "$file" "$hash_directory/$digest"
            else
                cp "$file" "$hash_directory/$digest"
            fi
        done
    done
done
rm -f "$dists/Release" "$dists/InRelease" "$dists/Release.gpg"
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=udpstp \
    -o APT::FTPArchive::Release::Label=udpstp \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    -o APT::FTPArchive::Release::Acquire-By-Hash=yes \
    -o "APT::FTPArchive::Release::Description=udpstp release packages" \
    release "$dists" > "$work/Release"
mv "$work/Release" "$dists/Release"
sign --clearsign --output "$dists/InRelease" "$dists/Release"
sign --armor --detach-sign --output "$dists/Release.gpg" "$dists/Release"

# dnf: a mirrorlist selects a complete, permanent metadata snapshot, so the
# index and its detached signature always come from the same generation.
# Package paths go back to the shared rpm pool, without copying the packages.
mkdir "$work/rpm"
printf 'udpstp-%s-1.%s.rpm\n' "$latest" x86_64 "$latest" aarch64 > "$work/rpm-packages"
createrepo_c --quiet --pkglist "$work/rpm-packages" --location-prefix '../../' --outputdir "$work/rpm" "$repository/rpm"
sign --armor --detach-sign --output "$work/rpm/repodata/repomd.xml.asc" \
    "$work/rpm/repodata/repomd.xml"
generation=$(sha256sum < "$work/rpm/repodata/repomd.xml" | cut -d' ' -f1)
snapshot=$repository/rpm/snapshots/$generation
if [ -d "$snapshot" ]; then
    # An interrupted upload can leave only part of this snapshot in storage.
    # Fill missing files, and reuse an existing signature for identical metadata.
    mkdir -p "$snapshot/repodata"
    for file in "$work/rpm/repodata/"*; do
        name=$(basename "$file")
        if [ -e "$snapshot/repodata/$name" ]; then
            [ "$name" = repomd.xml.asc ] || cmp "$file" "$snapshot/repodata/$name"
        else
            cp "$file" "$snapshot/repodata/$name"
        fi
    done
else
    mkdir -p "$(dirname "$snapshot")"
    mv "$work/rpm" "$snapshot"
fi
printf 'https://packages.udpstp.com/rpm/snapshots/%s/\n' "$generation" > "$repository/rpm/mirrorlist"

# The public key in both forms, and the dnf repository definition.
cp "$public_key" "$repository/udpstp.asc"
gpg --batch --yes --dearmor --output "$repository/udpstp.gpg" "$public_key"
cat > "$repository/udpstp.repo" <<'EOF'
[udpstp]
name=udpstp
mirrorlist=https://packages.udpstp.com/rpm/mirrorlist
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://packages.udpstp.com/udpstp.asc
EOF

# Check the signatures as a client would, with only the published key.
gpgv --keyring "$repository/udpstp.gpg" "$dists/InRelease" 2>/dev/null
gpgv --keyring "$repository/udpstp.gpg" "$dists/Release.gpg" "$dists/Release" 2>/dev/null
gpgv --keyring "$repository/udpstp.gpg" "$snapshot/repodata/repomd.xml.asc" \
    "$snapshot/repodata/repomd.xml" 2>/dev/null
echo "$repository: $(find "$pool" -name '*.deb' | wc -l) .deb and $(find "$repository/rpm" -maxdepth 1 -name '*.rpm' | wc -l) .rpm, signed by $fingerprint"
