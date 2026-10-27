# Repository Guidelines

## Project structure

ArcBox Custom combines two Git subtrees:

- `arcbox/`: Rust runtime; `app/` contains daemon and CLI, `virt/` virtualization/networking, `guest/` Linux agents, and `rpc/` protobuf contracts. Tests live beside code and under `tests/`; packaging resources live in `bundle/`.
- `arcbox-desktop/`: SwiftUI app in `ArcBox/`, reusable clients in `Packages/`, XCTest suites in `ArcBoxTests/`, UI tests in `ArcBoxUITests/`, and images in `ArcBox/Assets.xcassets/`.
- `arcbox/docs/`: architecture and verification records; `TODO-PRD.md`: feature plans.

Read the nearest nested `AGENTS.md` before editing a component. Preserve subtree history and licenses. Keep archived Fleet/Platform integrations disabled.

## Build, test, and development commands

Prerequisites: Apple Silicon macOS, full Xcode, Rust, Node.js, protobuf, xcodegen, and Linux musl cross-toolchain. Run from the repository root:

| Command | Purpose |
| --- | --- |
| `make run` | Build and launch the local-runtime development app with default ad-hoc signing. |
| `make verify` | Build, launch, and check development-instance readiness. |
| `make check` | Test and lint desktop Rust `xtask` tooling only. |
| `make -C arcbox-desktop build` | Compile Swift without embedding Rust binaries. |
| `make -C arcbox-desktop test` | Run desktop XCTest suites. |
| `make -C arcbox-desktop format lint` | Format Swift and run swift-format/SwiftLint checks. |

Use desktop Makefile wrappers; never invoke `xcodebuild` or `xcodegen` directly. After adding/removing Swift files, run `make -C arcbox-desktop generate-xcodeproj`.

## Coding style and naming

Use four-space indentation. Swift uses UpperCamelCase types, lowerCamelCase members, 120-column formatting, alphabetized imports, Swift 6 concurrency, `@Observable` models, and `async/await`. Rust uses snake_case functions/modules and UpperCamelCase types; run `cargo fmt` and Clippy from `arcbox/`. Explain unsafe invariants with `// SAFETY:` comments.

## Testing guidelines

Cover behavior, error paths, and regressions. Name XCTest files `<Feature>Tests.swift` and methods `test...`; use descriptive Rust test functions. From `arcbox/`, run `cargo test --workspace --exclude arcbox-agent`; validate Linux agents separately. Register local-package test sources in `project.yml` under `ArcBoxTests`. Verify UI changes in the running app and capture screenshots.

## Commits and pull requests

Use `type(scope): summary`, such as `fix(proxy): preserve saved settings (refs #2)`. Keep commits atomic and buildable; omit attribution trailers. PRs should link the issue, explain behavior changes, list verification results and limitations, and include screenshots for UI changes.

## Configuration and agent guidance

Never commit `Local.xcconfig`, credentials, certificates, provisioning profiles, or build outputs. If `.codegraph/` exists, use `codegraph explore` before text searches to locate or understand code; do not create an index automatically.
