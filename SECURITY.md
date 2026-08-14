# Security Policy

## Supported versions

Security fixes are provided for the latest published `1.x` release. Older releases may be assessed, but users should first reproduce the issue on the latest release when it is safe to do so.

## Reporting a vulnerability

Do not report security vulnerabilities through public issues.

Until a dedicated security address is published, use the private contact channel listed on the public repository profile or contact the maintainers directly. Include:

- A description of the issue and affected version.
- Reproduction steps or proof-of-concept code, if safe to share.
- Potential impact and required hardware or permissions.
- Suggested mitigations, if known.

Do not attach private camera images, videos, hardware credentials, API tokens, internal URLs, or unsanitized logs. Maintainers should acknowledge reports promptly and coordinate disclosure timing with the reporter.

## Scope

Security-sensitive areas include:

- Parsing and validating untrusted V4L2/MJPEG frame data.
- Image allocation, stride calculations, pixel conversion, and encoding.
- Device discovery and camera path handling.
- Dora input, output, metadata, and Arrow message handling.
- The privileged `scripts/install_permissions.sh` helper and `video` group access.
- Dependency and release supply-chain configuration.

Joining the `video` group can grant access to cameras and other video devices. Only trusted users should receive that permission.
