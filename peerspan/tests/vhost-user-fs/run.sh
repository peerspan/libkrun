#!/bin/sh
# Boots a guest under libkrun with a vhost-user-fs device (virtio type 26) served by stock
# virtiofsd, and checks that the guest mounts it by tag and round-trips files.
#
# Environment:
#   GUEST_ROOT            where the guest kernel and modules live (default /, the host's)
#   GUEST_KERNEL_VERSION  default: uname -r
#   VIRTIOFSD             default: virtiofsd on PATH, else /usr/libexec/virtiofsd
#   RUNS                  boots to perform (default 1)
#
# Expects libkrun built with `make VHOST_USER=1` in the repository root.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
lib="$repo/target/release"
root=${GUEST_ROOT:-/}
ver=${GUEST_KERNEL_VERSION:-$(uname -r)}
virtiofsd=${VIRTIOFSD:-$(command -v virtiofsd || echo /usr/libexec/virtiofsd)}
runs=${RUNS:-1}

[ -e "$lib/libkrun.so" ] || { echo "build libkrun first: make VHOST_USER=1" >&2; exit 2; }
[ -x "$virtiofsd" ] || { echo "virtiofsd not found" >&2; exit 2; }
[ -e "$lib/libkrun.so.2" ] || ln -s "$(basename "$(readlink -f "$lib/libkrun.so")")" "$lib/libkrun.so.2"

work=$(mktemp -d)
trap 'kill $vfs_pid 2>/dev/null || true; rm -rf "$work"' EXIT
vfs_pid=

cc -o "$work/launch" "$here/launch.c" -I"$repo/include" -L"$lib" -lkrun -Wl,-rpath,"$lib"
"$here/extract-vmlinux.py" "$root/boot/vmlinuz-$ver" "$work/vmlinux"

mkdir -p "$work/initramfs/bin" "$work/initramfs/proc" "$work/initramfs/sys" \
  "$work/initramfs/dev" "$work/initramfs/mnt" "$work/initramfs/lib/modules"
cp "$(command -v busybox)" "$work/initramfs/bin/busybox"
cp "$here/init" "$work/initramfs/init"
ko=$(find "$root/lib/modules/$ver" "$root/usr/lib/modules/$ver" -name 'virtiofs.ko*' 2>/dev/null | head -1 || true)
case "$ko" in
  *.zst) zstd -dqc "$ko" > "$work/initramfs/lib/modules/virtiofs.ko" ;;
  *.xz) xz -dqc "$ko" > "$work/initramfs/lib/modules/virtiofs.ko" ;;
  *.ko) cp "$ko" "$work/initramfs/lib/modules/virtiofs.ko" ;;
  *) echo "no virtiofs module for $ver; assuming it is built in" >&2 ;;
esac
(cd "$work/initramfs" && find . | cpio -o -H newc 2>/dev/null | gzip) > "$work/initramfs.cpio.gz"

fail=0
n=1
while [ "$n" -le "$runs" ]; do
  share="$work/share"; rm -rf "$share" "$work/vfs.sock"*; mkdir -p "$share"
  echo "hello from host" > "$share/from-host.txt"
  "$virtiofsd" --socket-path="$work/vfs.sock" --shared-dir="$share" --tag peerfs \
    --cache=auto --sandbox=none > "$work/virtiofsd.log" 2>&1 &
  vfs_pid=$!
  i=0; while [ ! -S "$work/vfs.sock" ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i + 1)); done

  start=$(date +%s%N)
  rc=0
  timeout 120 "$work/launch" "$work/vmlinux" "$work/initramfs.cpio.gz" \
    "console=hvc0 rdinit=/init panic=-1 reboot=k quiet" "$work/vfs.sock" \
    < /dev/null > "$work/guest.log" 2>&1 || rc=$?
  ms=$(( ($(date +%s%N) - start) / 1000000 ))
  wait "$vfs_pid" 2>/dev/null || true

  problems=""
  [ "$rc" -eq 0 ] || problems="$problems launch-exit=$rc"
  grep -q "GUEST DONE" "$work/guest.log" || problems="$problems guest-did-not-finish"
  grep "CHECK .* FAIL" "$work/guest.log" | sed 's/.*CHECK /  guest: /' || true
  [ "$(grep -c 'CHECK .* ok' "$work/guest.log")" -eq 6 ] || problems="$problems guest-checks"
  [ "$(cat "$share/from-guest.txt" 2>/dev/null)" = "written by guest" ] || problems="$problems host-sees-guest-file"
  [ -f "$share/dir/sub/a" ] || problems="$problems host-sees-nested-dirs"
  big=$(grep -o 'BIG [0-9a-f]*' "$work/guest.log" | cut -d' ' -f2)
  [ -n "$big" ] && [ "$big" = "$(md5sum "$share/big" | cut -d' ' -f1)" ] || problems="$problems host-checksum"

  if [ -z "$problems" ]; then
    echo "run $n: ok (${ms} ms, kernel $ver)"
  else
    echo "run $n: FAIL:$problems"; tail -30 "$work/guest.log"; tail -5 "$work/virtiofsd.log"
    fail=1
  fi
  n=$((n + 1))
done
exit "$fail"
