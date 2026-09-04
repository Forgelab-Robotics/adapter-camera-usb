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
cargo audit --ignore RUSTSEC-2026-0041
cargo package --locked
cargo publish --locked --dry-run
```

Complete and record the applicable hardware checks, including continuous capture, all supported output formats, shutdown/reopen behavior, and the failure modes affected by the release.

USB Camera 2.0.0 resolves the published `forge_msgs 2.0.0` and
`forgelab_common 2.0.0` crates from crates.io. Keep `Cargo.lock` on registry
sources and require successful `cargo package --locked` and publish dry-run
verification before release.

The Dora 1.0.1 lock currently resolves `lz4_flex 0.10.0`, which triggers
`RUSTSEC-2026-0041`. The affected Zenoh block-decompression path is not compiled
in this build because transport compression is disabled, so the audit uses a
targeted exception until Dora/Zenoh updates the dependency.

## Source release

Before creating a tag:

1. Confirm `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md` agree on the version and requirements.
2. Confirm the package archive contains `LICENSE` and no private files or generated artifacts.
3. Confirm the repository working tree is clean and CI passes.
4. Create an immutable annotated tag named `v<version>` at the validated commit.

Do not store crates.io or repository tokens in this repository. Published crates.io versions and public release tags must never be replaced.

## Binary release

Build the user-facing Linux x86_64 binary as a static PIE linked against musl. Install `musl-tools`, add the Rust target, and build with path remapping and symbol stripping:

```bash
rustup target add --toolchain 1.97.1 x86_64-unknown-linux-musl
RUSTFLAGS="--remap-path-prefix=${HOME}=/build -C link-self-contained=yes" \
CARGO_PROFILE_RELEASE_STRIP=symbols \
cargo +1.97.1 build --locked --release --bin usb_camera \
  --target x86_64-unknown-linux-musl
```

Verify that `file` reports `static-pie linked`, `ldd` reports `statically linked`, and `readelf -l` does not report an `INTERP` segment.

Do not reuse files from the local `dist/` directory. The `usb_camera_test_sink` binary is a development and Dora example utility and must not be included in public binary archives.

The minimal public binary archive is named `usb_camera-v<version>-linux-x86_64-musl.tar.gz` and contains only the stripped `usb_camera` executable. Project documentation and licensing remain available in the repository and GitHub-generated source archives.

Scan the final binary for private paths and internal URLs, verify its SHA-256 digest before upload, and test the archive on the oldest supported runtime environment before publishing it.
