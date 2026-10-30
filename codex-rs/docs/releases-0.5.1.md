# SpineCodex 0.5.1

This patch restores the local startup policy used before automatic managed-daemon startup was introduced.

- Local new, resume and fork sessions passively discover a compatible Spine app-server. When none is available, they start the embedded app-server from the current CLI.
- Invocation overrides, including --profile, keep their configuration in the embedded app-server.
- Unsupported managed-daemon lifecycle operations stop before accessing the official Codex daemon installation or state. An existing official Codex daemon is left untouched.
- Explicit remote app-server selection remains available.

This fixes default startup waiting for a Spine socket that the official Codex daemon never creates. The previously reported long profile startup delay was not localized; this release does not claim a separate performance fix for that delay.

The product release is 0.5.1. Codex compatibility remains 0.164.0, which is also the version printed by --version.
