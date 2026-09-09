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
```

Complete and record the applicable hardware checks, including continuous capture, all supported output formats, shutdown/reopen behavior, and the failure modes affected by the release.

USB Camera 2.1.0 resolves the published `forge_msgs 2.0.0` and
`forgelab_common 2.1.0` crates from crates.io. Keep `Cargo.lock` on registry
sources and require successful `cargo package --locked` verification before
release. This application is distributed through source tags and GitHub binary
assets, not crates.io.

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

The package is marked `publish = false` and must not be uploaded to crates.io.
Do not store repository tokens in this repository. Published public release tags
must never be replaced.

## Binary release

The normative binary release path is to trigger the **Build Ubuntu 20.04 binary** workflow (`.github/workflows/build-ubuntu20.yml`) manually through GitHub Actions. Supply all three workflow inputs:

- `ref`: the immutable release tag to check out, exactly `v<version>`.
- `version`: the package version without the `v` prefix. It must match `ref` and the exact `usb_camera <version>` output embedded in the binary.
- `upload_to_release`: whether to attach the verified archive and checksum to GitHub Releases. It defaults to `false`, so a normal run only stores a workflow artifact. Set it to `true` only after creating an existing draft release whose tag exactly matches `ref`; the workflow does not create the release and refuses to replace an existing asset with different content.

The workflow builds the user-facing binary with Rust 1.97.1 and the `x86_64-unknown-linux-gnu` target inside its Ubuntu 20.04 container. It applies workflow-specific path remapping through environment variables such as `GITHUB_WORKSPACE`, strips symbols, and produces a dynamically linked GNU/glibc binary whose maximum required glibc symbol version must not exceed 2.31. The build commands in the workflow are implementation details for that container and are not a supported copy-and-paste local release procedure.

The workflow verifies the exact embedded version, confirms that `file` reports a 64-bit ELF binary, uses `ldd` to ensure that all dynamic libraries resolve, checks the maximum required glibc symbol version with `objdump -T`, and scans for private build markers. After packaging, it verifies the SHA-256 checksum and extracts the final archive to run an additional `--version` smoke test.

Do not reuse files from the local `dist/` directory. The `usb_camera_test_sink` binary is a development and Dora example utility and must not be included in public binary archives.

The minimal public binary archive is named `usb_camera-v<version>-ubuntu20.04-x86_64.tar.gz` and contains only the stripped `usb_camera` executable. Publish the matching `usb_camera-v<version>-ubuntu20.04-x86_64.tar.gz.sha256` file alongside it. Every workflow run uploads both files as a GitHub Actions artifact retained for 30 days; enabling `upload_to_release` additionally attaches them to the matching draft release. Project documentation and licensing remain available in the repository and GitHub-generated source archives.

Downloading the final archive and testing it on a separate, clean Ubuntu 20.04 runtime is an additional manual release step. The current workflow's build-container checks and extracted-archive smoke test do not perform or replace that clean-runtime validation. Complete this manual test before publishing the draft release.
