# Changelog

All notable changes to this project are documented in this file.

The project follows Semantic Versioning. Dates use the `YYYY-MM-DD` format.

## Unreleased

## 2.0.0 - 2026-09-04

### Changed

- Migrated the Dora node and sensor-to-sink example to the Dora 1.x line and Arrow 59; the current lock validates Dora 1.0.1.
- Updated `forge_msgs` and `forgelab_common` to their published Forge 2.0 crates.io releases.
- Updated the locked `h2` dependency to 0.4.19 to address `RUSTSEC-2026-0258`.
- Raised the package version to 2.0.0 because Dora 0.x and 1.x nodes cannot interoperate.

### Removed

- Removed the optional security-policy and code-of-conduct files to align the public documentation set across adapters.

## 1.0.6 - 2026-08-15

### Changed

- Changed the Linux x86_64 release binary to a static PIE linked against musl, removing its glibc runtime dependency.

## 1.0.5 - 2026-08-14

### Changed

- Raised the minimum supported Rust version to 1.97.1.
- Replaced internal Forge Git dependencies with the published `forge_msgs` and `forgelab_common` crates.
- Updated compatible transitive dependencies to remove the yanked `spin 0.9.8` and affected `anyhow 1.0.102` lockfile entries.

### Added

- Apache-2.0 licensing and open-source project metadata.
- Contributor, security, conduct, release, and continuous-integration documentation.

### Removed

- Removed environment-specific hardware baseline and parameter validation reports from the public repository.

## 1.0.4 - 2026-08-11

- Rejected incomplete MJPEG frames instead of forwarding truncated images.
- Added capture-health summaries and MJPEG buffer-boundary diagnostics.

## 1.0.3 - 2026-08-11

- Added V4L2 sequence-gap, timeout, capture-error, and stall diagnostics.

## 1.0.2 - 2026-08-10

- Improved handling of malformed MJPEG frame boundaries.

## 1.0.1 - 2026-08-10

- Dropped V4L2 buffers marked as corrupted by the driver.

## 1.0.0 - 2026-08-02

- Initial USB/UVC camera node release with Linux V4L2 capture, Dora streaming, device discovery, snapshots, raw/JPEG/PNG output, and hardware validation documentation.
