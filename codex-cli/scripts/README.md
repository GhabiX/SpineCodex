# npm releases

Use the staging helper in the repo root to generate npm tarballs for a release. For
example, to stage the CLI, responses proxy, and SDK packages for version `0.6.0`:

```bash
./scripts/stage_npm_packages.py \
  --release-version 0.6.0 \
  --package codex \
  --package codex-responses-api-proxy \
  --package codex-sdk
```

This downloads the required native package archive artifacts, hydrates `vendor/` for
each package, and writes tarballs to `dist/npm/`.

When `--package codex` is provided, the staging helper builds the lightweight
`@spinejit/spine-codex` meta package plus all six platform-native variants that
are later published under platform-specific dist-tags. These SpineCodex packages
are a separate release lane from the upstream SDK and responses proxy packages.

Direct `build_npm_package.py` invocations are still useful for package-specific
debugging, but native packages expect `--vendor-src` to point at a prehydrated
`vendor/` tree. Release packaging should use `scripts/stage_npm_packages.py`.

SpineCodex product releases are owned by
`.github/workflows/rust-release.yml`. A `vX.Y.Z` tag must match the workspace
version in `codex-rs/Cargo.toml`. The workflow builds canonical package archives
on all six native platforms, stages and smokes the root plus platform npm
packages, creates the GitHub Release, publishes platform versions before the
root `latest` package through npm trusted publishing, and verifies the registry
install path. Manual dispatch runs the same six-platform gate with a synthetic
version but never creates a release or publishes to npm.

Unix release binaries are stripped before canonical packaging. The package
audit rejects any npm tarball larger than 200 MiB so registry upload limits are
enforced before GitHub Release creation or npm publishing.

The inherited `.github/workflows/rust-release-upstream.yml` remains reserved for
upstream-style `rust-vX.Y.Z` tags because it requires signing and runner
infrastructure that is not available in the public SpineCodex repository.
