#!/usr/bin/bash
# Task-owned fresh regular-file filesystem, within a disposable container only.
set -euo pipefail
umask 077
loci_owned_prefix="/tmp/loci-native-env-$BASHPID"
loci_mountpoint=/mnt/loci-native
loci_image_bytes=8589934592
loci_inodes=1500000
loci_extra_write_bytes=4294967296
loci_plan_only=0
usage() {
    cat <<'HELP'
Usage: loci-run-ext4 [--owned-prefix ABS] [--mountpoint ABS]
       [--image-bytes N] [--inodes N] [--host-extra-write-bytes N]
       [--print-plan] [--] COMMAND [ARG...]
Default: disposable8GiB ext4 image,1500000inodes,4GiB extra host write budget.
Never accepts an existing image, host block device or occupied mountpoint.
--print-plan performs argument checks only; no image/device/mount is created.
HELP
}
while (( $# )); do
    case "$1" in
        --owned-prefix|--mountpoint|--image-bytes|--inodes|--host-extra-write-bytes)
            (( $# >= 2 )) || { usage >&2; exit 2; }
            case "$1" in
                --owned-prefix) loci_owned_prefix=$2 ;;
                --mountpoint) loci_mountpoint=$2 ;;
                --image-bytes) loci_image_bytes=$2 ;;
                --inodes) loci_inodes=$2 ;;
                --host-extra-write-bytes) loci_extra_write_bytes=$2 ;;
            esac
            shift 2 ;;
        --print-plan) loci_plan_only=1; shift ;;
        --help) usage; exit 0 ;;
        --) shift; break ;;
        --*) usage >&2; exit 2 ;;
        *) break ;;
    esac
done
(( $# )) || { usage >&2; exit 2; }
for loci_value in "$loci_image_bytes" "$loci_inodes" "$loci_extra_write_bytes"; do
    [[ $loci_value =~ ^[1-9][0-9]{0,11}$ ]] || { echo 'invalid positive numeric budget' >&2; exit 2; }
done
(( loci_image_bytes >= 536870912 && loci_image_bytes <= 68719476736 )) || { echo 'image budget must be512MiB..64GiB' >&2; exit 2; }
(( loci_inodes >= 1000 && loci_inodes <= 10000000 )) || { echo 'inode budget must be1000..10000000' >&2; exit 2; }
# Fresh direct paths only. The image path is derived inside a newly created
# private directory; no caller-supplied image/device is ever formatted.
for loci_path in "$loci_owned_prefix" "$loci_mountpoint"; do
    [[ $loci_path == /* && $loci_path != / && $loci_path != *$'\n'* && $loci_path != *$'\r'* && $loci_path != *$'\t'* ]] || { echo 'absolute plain paths required' >&2; exit 2; }
    [[ $loci_path != /dev/* && $loci_path != /proc/* && $loci_path != /sys/* ]] || { echo 'device/proc/sys paths are forbidden' >&2; exit 2; }
    [[ ! -e $loci_path && ! -L $loci_path ]] || { echo 'owned prefix/mountpoint must not already exist' >&2; exit 2; }
    loci_parent=${loci_path%/*}
    [[ -n $loci_parent ]] || loci_parent=/
    [[ -d $loci_parent && $(readlink -e -- "$loci_parent") == "$loci_parent" ]] || { echo 'existing direct parent without symlinks/dot components required' >&2; exit 2; }
done
[[ $loci_mountpoint != "$loci_owned_prefix" && $loci_mountpoint != "$loci_owned_prefix/"* && $loci_owned_prefix != "$loci_mountpoint/"* ]] || { echo 'owned prefix and mountpoint must be separate' >&2; exit 2; }
if (( loci_plan_only )); then
    printf 'LOCI_NATIVE_ENV_PLAN image_bytes=%s inodes=%s extra_host_write_bytes=%s reserve_percent=15 uid=1000 gid=1000\n' "$loci_image_bytes" "$loci_inodes" "$loci_extra_write_bytes"
    printf 'owned_prefix=%q mountpoint=%q command=' "$loci_owned_prefix" "$loci_mountpoint"
    printf '%q ' "$@"
    printf '\n'
    exit 0
fi
[[ ${LOCI_NATIVE_CONTAINER:-} == 1 && ( -e /.dockerenv || -e /run/.containerenv ) ]] || { echo 'disposable container is required; host execution refused' >&2; exit 2; }
[[ $(id -u) == 0 ]] || { echo 'container-local setup must start as root' >&2; exit 2; }
# Reserve the entire sparse image growth and declared outside-image writes.
loci_parent=${loci_owned_prefix%/*}
read -r loci_free_blocks loci_total_blocks loci_fragment_size <<<"$(stat -f -c '%a %b %S' -- "$loci_parent")"
[[ $loci_free_blocks =~ ^[0-9]+$ && $loci_total_blocks =~ ^[1-9][0-9]*$ && $loci_fragment_size =~ ^[1-9][0-9]*$ ]] || exit 2
loci_available=$((loci_free_blocks * loci_fragment_size))
loci_total=$((loci_total_blocks * loci_fragment_size))
(( loci_available - loci_image_bytes - loci_extra_write_bytes >= loci_total * 15 / 100 )) || { echo 'insufficient backing filesystem budget to retain15% reserve' >&2; exit 2; }
mkdir -m 700 -- "$loci_owned_prefix"
loci_image="$loci_owned_prefix/data.ext4"
loci_mounted=0
loci_mountpoint_created=0
cleanup() {
    loci_status=$?
    trap - EXIT INT TERM
    set +e
    cd /
    loci_released=1
    if (( loci_mounted )); then
        if ! umount -- "$loci_mountpoint"; then loci_released=0; fi
    fi
    if (( loci_mountpoint_created )) && findmnt -n -M "$loci_mountpoint" >/dev/null 2>&1; then loci_released=0; fi
    if [[ -e $loci_image ]]; then
        loci_loops=$(losetup -j "$loci_image" 2>&1)
        loci_loop_check=$?
        if (( loci_loop_check != 0 )) || [[ -n $loci_loops ]]; then loci_released=0; fi
    fi
    if (( loci_released )); then
        if [[ -e $loci_image ]] && ! rm -- "$loci_image"; then loci_released=0; fi
        if (( loci_mountpoint_created )) && ! rmdir -- "$loci_mountpoint"; then loci_released=0; fi
        if ! rmdir -- "$loci_owned_prefix"; then loci_released=0; fi
    fi
    if (( loci_released )); then
        echo 'LOCI_NATIVE_ENV_CLEANUP_PASS mount_released=1 loop_released=1'
    else
        echo 'LOCI_NATIVE_ENV_CLEANUP_UNVERIFIED cleanup incomplete; unresolved owned paths retained; no unrelated loop detached' >&2
        (( loci_status != 0 )) || loci_status=1
    fi
    exit "$loci_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Device nodes refer to the container's allowed kernel loop interface. Allocation
# remains mount/loop-autoclear's job: never detach an existing loop by its number.
[[ -c /dev/loop-control ]] || mknod /dev/loop-control c 10 237
for loci_loop_index in 0 1 2 3 4 5 6 7; do
    [[ -b /dev/loop$loci_loop_index ]] || mknod "/dev/loop$loci_loop_index" b 7 "$loci_loop_index"
done
truncate -s "$loci_image_bytes" -- "$loci_image"
[[ -f $loci_image && ! -L $loci_image ]] || exit 2
mke2fs -q -t ext4 -N "$loci_inodes" "$loci_image"
mkdir -m 700 -- "$loci_mountpoint"
loci_mountpoint_created=1
mount -t ext4 -o loop -- "$loci_image" "$loci_mountpoint"
loci_mounted=1
chown 1000:1000 -- "$loci_mountpoint"
findmnt -T "$loci_mountpoint" -o TARGET,SOURCE,FSTYPE,OPTIONS
[[ $(findmnt -n -M "$loci_mountpoint" -o FSTYPE) == ext4 ]] || exit 2
cd -- "$loci_mountpoint"
setpriv --reuid=1000 --regid=1000 --clear-groups -- "$@"
