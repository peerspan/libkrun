# Peerspan's libkrun fork

A thin fork of [libkrun](https://github.com/containers/libkrun) for Peerspan's runtime, which
runs every host-launched agent session in a microVM. The decision and its research are in
`docs/runtime.md` in `peerspan/docs-internal`.

## Branches

- `main` mirrors upstream `main` and only fast-forwards.
- `peerspan` is upstream plus our changes, rebased onto `main` when upstream moves.

Keep the diff small. Changes that are useful upstream (such as a custom virtio-fs backend)
go there as pull requests, following upstream's [AGENTS.md](../AGENTS.md) commit rules: a
`subsystem: title` subject, `Signed-off-by`, and `Assisted-by` rather than `Co-authored-by`
for agent help. Peerspan-only material stays under `peerspan/` and in workflows whose names
start with `peerspan-`, so rebases do not conflict with upstream.

## Checks

`tests/vhost-user-fs/run.sh` boots a guest whose virtio-fs device (type 26) is a generic
`VhostUserDevice` served by stock `virtiofsd --tag`, which is how peerfs will serve workspaces
on Linux. It checks that the guest mounts by tag, reads a host file, creates files and nested
directories the host sees, and writes 64 MiB whose checksum matches on both sides and after the
guest drops its caches.

```sh
make VHOST_USER=1
peerspan/tests/vhost-user-fs/run.sh            # the host's own kernel as the guest
K=$(mktemp -d); V=$(peerspan/tests/vhost-user-fs/fetch-ubuntu-kernel.sh "$K")
GUEST_ROOT=$K GUEST_KERNEL_VERSION=$V peerspan/tests/vhost-user-fs/run.sh
```

Building needs clang (for bindgen), the musl target (`rustup target add
x86_64-unknown-linux-musl`), a static busybox, `virtiofsd`, and `/dev/kvm`. The guest kernel
must have virtio-mmio command-line devices and virtio-fs; Ubuntu's generic kernel has both,
while cloud kernels may not. The timings it prints include a stock distribution kernel and the
64 MiB write; they are not a boot measurement.
