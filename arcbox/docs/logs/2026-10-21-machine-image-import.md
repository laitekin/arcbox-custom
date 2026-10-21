# 2026-10-21 — Users can import their own root filesystems as machine images

- Type: change
- Area: arcbox-image (`machine_import`), arcbox-engine (machine archive),
  arcbox-api (`MachineImageService`), arcbox-cli (`abctl machine image`)
- Commits / PRs: laitekin/arcbox-custom#3 (issue)
- Related: [Host-side proxy authentication](2026-10-21-proxy-authentication.md)

## Problem or trigger

Linux machines could only boot the distro images ArcBox publishes. A user
with their own root filesystem (a rootfs tarball, a squashfs, a Docker image)
had no way to boot it, and an exported machine could only be restored on a
Mac that could download the same published image.

## What was done

An import turns every source into what a published image is: an xz squashfs
(1 MiB blocks, the published images' settings) plus a `manifest.json`,
registered under the user's name with version `local-YYYYMMDD_HHMMSS`,
variant `custom` and upstream server `local`. Creating, booting, exporting
and removing a machine then never needs to know where its image came from;
the machine still boots ArcBox's kernel and shim.

- **Sources.** A tar archive (plain, gzip, xz, zstd; recognized by content), a
  squashfs image, or `docker:<image>`, exported through the daemon's own Docker
  socket from a container that never starts (so the image is never unpacked on
  the Mac). `abctl … --add-init` first builds a derived Docker image with an
  init system for images that have none.
- **Metadata stays Linux's.** The tar is read into an in-memory tree with file
  contents spilled to the staging directory; owners, setuid bits, device
  nodes, fifos, symlinks and hard links come from the headers and go straight
  into the squashfs (backhand). Extended attributes cannot be kept and are
  reported per import.
- **Checks.** Archive paths escaping the root are refused. `/sbin/init` must
  resolve inside the rootfs (merged-usr and absolute links included) to an ELF
  for this Mac's architecture, or a script. `os-release` names the image.
- **Atomic.** Staged under `machine-images/.import-*` and renamed into place
  last; a failure leaves nothing registered.
- **Management.** `MachineImageService` List/ListRemote/Import/Remove; removal
  is refused while a machine boots the image. `[machine] image_base` points the
  image source at a private mirror (the environment variable still wins), and
  the remote list is fetched by the daemon, through the egress proxy.
- **Portability.** A machine archive can carry its image (`image/` after
  `data.img`, `image_included` in the manifest; additive to format 1). Export
  carries imported images by default; import registers a carried image,
  verified against its manifest, before resolving the rootfs.

## Evidence

The published `alpine-3.24-amd64` image, unpacked to a tarball and imported,
produced a squashfs of the same size whose `unsquashfs -lln` listing (mode,
owner, size, path and link target for all 799 entries) matches the original
exactly. A synthetic rootfs with setuid, a character device, a fifo, a hard
link and merged-usr links round-trips through `unsquashfs` with that metadata
intact. Against a Linux daemon started with `--no-linux-vm`, `abctl machine
image import/ls/rm` worked end to end and an arm64 squashfs was refused on the
amd64 host for its architecture.

```sh
cargo test -p arcbox-image                     # import, checks, registry
cargo test -p arcbox-engine --lib machine::    # archive carrying an image
cargo test -p arcbox-api --lib machine_image   # Docker export client
cargo test -p arcbox-daemon --bin arcbox-daemon a_rootfs_archive
```

Not yet done on a Mac: booting a machine from an imported image (systemd and
openrc), the `docker:` source against a real System VM, and an export/import of
such a machine between two Macs.

## Follow-ups

- Boot-test imported Ubuntu (systemd) and Alpine (openrc) images on a Mac.
- qcow2/raw root partition extraction (P2).
