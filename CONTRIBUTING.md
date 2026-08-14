# Contributing to USB Camera

Thank you for contributing to the USB Camera node. Keep changes focused, preserve the documented message contract, and include tests for behavior changes.

## Development setup

The project supports Linux and requires Rust 1.97.1 or newer. On Ubuntu or Debian, install the native build and V4L2 dependencies:

```bash
sudo apt install build-essential clang libclang-dev libv4l-dev v4l-utils
```

Build the binaries:

```bash
cargo build --locked --bins
```

The unit and delivery-path tests do not require camera hardware.

## Required checks

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo package --locked
```

If dependencies change, regenerate `Cargo.lock`, review the dependency tree and licenses, and run a RustSec audit:

```bash
cargo audit
```

## Hardware-facing changes

Changes to device discovery, V4L2 negotiation, frame conversion, timestamps, buffering, or reconnect behavior should include:

1. Unit tests for behavior that can be exercised without hardware.
2. A hardware test covering the affected format or failure mode.
3. A pull request note recording the hardware, driver, negotiated format, test duration, and result.
4. Logs containing only sanitized device and performance information.

Do not commit camera images containing people, screens, documents, locations, serial numbers, or other private information. Do not commit large videos, MCAP files, generated binaries, or build directories.

## Pull request guidelines

- Keep each pull request focused on one behavior or maintenance concern.
- Explain compatibility and performance impact where relevant.
- Preserve the `forge_msgs.Image` and `forge_msgs.CompressedImage` semantics documented in `README.md`.
- Do not include secrets, credentials, private URLs, personal filesystem paths, or private hardware identifiers.
- Do not add vendor SDKs or binary blobs without prior discussion and a license review.

## License

By contributing, you agree that your contributions are licensed under the Apache License, Version 2.0.
