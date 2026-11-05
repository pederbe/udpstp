# Shared filesystem checks for the Rust and legacy static-host adapters.
# These functions run under bash on the participating host.
bench_parent() {
    local parent owner permissions mode
    parent=$(dirname -- "$1") || return 1
    [[ "$parent" == "$(cd -P -- "$parent" && pwd)" ]] || return 1
    while :; do
        [[ -d "$parent" && ! -L "$parent" ]] || return 1
        owner=$(stat -c %u -- "$parent") || return 1
        permissions=$(stat -c %a -- "$parent") || return 1
        mode=$((8#$permissions))
        [[ "$owner" == "$EUID" || "$owner" == 0 ]] || return 1
        # Shared writable ancestors must have sticky-directory protection.
        (( (mode & 0022) == 0 || (mode & 01000) != 0 )) || return 1
        [[ "$parent" == / ]] && break
        parent=$(dirname -- "$parent") || return 1
    done
}

bench_guard() {
    local work=$1 proof=$2 identity=${3:-} current
    bench_parent "$work" || return 72
    [[ -d "$work" && ! -L "$work" && -O "$work" ]] || return 1
    [[ "$(stat -c %a -- "$work")" == 700 ]] || return 1
    current=$(stat -c '%d:%i' -- "$work") || return 1
    if [[ -n "$identity" ]]; then
        [[ "$current" == "$identity" ]]
    else
        # A failed transport may not have returned the inode yet.
        [[ -f "$work/.bench-owner" && ! -L "$work/.bench-owner" && -O "$work/.bench-owner" ]] || return 1
        [[ "$(cat -- "$work/.bench-owner")" == "$proof" ]]
    fi
}

bench_allocate() {
    local work=$1 proof=$2
    bench_parent "$work" || return 72
    umask 077
    # No -p: a directory, file or link already there is never adopted.
    mkdir -m 700 -- "$work" || return 73
    if ! (set -o noclobber; printf '%s' "$proof" > "$work/.bench-owner"); then
        rmdir -- "$work"
        return 1
    fi
    bench_guard "$work" "$proof" || return 1
    stat -c '%d:%i' -- "$work"
}

bench_remove() {
    local work=$1 proof=$2 identity=$3 name
    shift 3
    [[ -e "$work" || -L "$work" ]] || return 0
    bench_guard "$work" "$proof" "$identity" || {
        echo 'private workspace ownership check failed' >&2
        return 1
    }
    cd -- "$work" || return 1
    local find_args=(. -mindepth 1 -maxdepth 1 ! -name .bench-owner)
    for name; do
        find_args+=(! -name "$name")
    done
    find "${find_args[@]}" -exec rm -rf {} + || return 1
    if (( $# == 0 )); then
        rm -f -- .bench-owner || return 1
        cd / || return 1
        rmdir -- "$work"
    fi
}
