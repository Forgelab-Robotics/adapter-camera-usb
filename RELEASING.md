# Releasing USB Camera

## Versioning

The project follows Semantic Versioning. Update the package version in `Cargo.toml`, update version-sensitive tests, regenerate `Cargo.lock` when required, and move the relevant entries from `Unreleased` into a dated section in `CHANGELOG.md`.

The first public release should use a new version after `1.0.4`; historical private tags must not be reused or moved.

## Release checks

Release from a clean, reviewed commit using Rust 1.97.1 or newer:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo audit
cargo package --locked
cargo publish --locked --dry-run
```

Complete the applicable hardware checks in `hardware_baseline.md`, including continuous capture, all supported output formats, shutdown/reopen behavior, and the failure modes affected by the release.

## Source release

Before creating a tag:

1. Confirm `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md` agree on the version and requirements.
2. Confirm the package archive contains `LICENSE` and no private files or generated artifacts.
3. Confirm the repository working tree is clean and CI passes.
4. Create an immutable annotated tag named `v<version>` at the validated commit.

Do not store crates.io or repository tokens in this repository. Published crates.io versions and public release tags must never be replaced.

## Binary release

Build binaries in a clean CI environment with an explicitly documented Linux distribution, architecture, and glibc baseline. Do not reuse files from the local `dist/` directory.

Each binary archive must contain:

- `usb_camera` and, when intended for users, `usb_camera_test_sink`.
- `README.md`, `LICENSE`, and the example configuration.
- Third-party license and attribution information.
- A SHA-256 checksum published separately from the archive.

Strip unnecessary build symbols, scan the final binaries for private paths and internal URLs, and test the archive on the oldest supported runtime environment before publishing it.
