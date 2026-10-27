# Fast local development builds

The parent repository now exposes `make run-fast` / `make build-fast` and
`make run-ui` / `make build-ui`. The original `make run`, Release Cargo profile,
and release/DMG targets retain their existing behavior.

The fast path selects local runtime sources and the `dev-fast` host profile:
incremental compilation, optimization level 1, no cross-crate LTO, 16 codegen
units, and line-table debugging information. Dependencies use optimization
level 2. Release panic and assertion behavior is inherited. Root entry points
use `arcbox/target/dev-fast`; Release remains in `arcbox/target/release`.
Guest agents continue using the existing Release cross-compilation/cache.

The UI path requires an existing runnable development bundle and shares its
DerivedData directory. It skips host/guest compilation, binary embedding,
asset preparation, payload copying, and payload signing. The post-build step
still repairs the development Info.plist and LaunchAgent after Xcode's copy
phases, validates the existing payload, and lets Xcode sign the outer app.
Missing payloads fail with instructions to run a complete build. After Rust,
protobuf, or resource changes, use the fast/full path; UI-only deliberately
keeps the previous backend.

Validation on macOS:

- `make check`: build-mode routing/error tests, desktop xtask unit/integration
  tests, formatting, and Clippy passed.
- `make build-fast`: complete application built successfully with host
  `dev-fast` binaries and cached Release guest agents.
- `make build-ui`: complete application built successfully through resource
  reuse. Existing host/guest binaries, boot assets, runtime tools, and daemon
  bundle retained their contents and modification timestamps.
- `codesign --verify --deep --strict` passed for the resulting app.
- A subsequent `build-runnable-rust` invocation with the same target directory
  and `ARCBOX_HOST_CARGO_PROFILE=dev-fast` reused the new host cache.

These checks establish build routing and bundle integrity, not a benchmark of
runtime performance or a launch/VM smoke test. The first `dev-fast` build must
populate a separate cache. Regression checks live in
`arcbox-desktop/script/test_build_modes.py` and the desktop xtask tests.
