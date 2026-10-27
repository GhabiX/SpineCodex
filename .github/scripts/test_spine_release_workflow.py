#!/usr/bin/env python3

from datetime import datetime
from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import textwrap
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPINE_WORKFLOW = ROOT / ".github" / "workflows" / "spine-release.yml"
UPSTREAM_WORKFLOW = ROOT / ".github" / "workflows" / "rust-release.yml"
STAGING_SCRIPT = ROOT / "scripts" / "stage_npm_packages.py"
README = ROOT / "README.md"
PACKAGE_JSON = ROOT / "codex-cli" / "package.json"
CARGO_TOML = ROOT / "codex-rs" / "Cargo.toml"
INSTALL_CONTEXT = ROOT / "codex-rs" / "install-context" / "src" / "distribution.rs"
RELEASE_NOTES_DIR = ROOT / "codex-rs" / "docs"

NATIVE_TARGETS = {
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
}


def workflow_text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def job_names(text: str) -> set[str]:
    jobs = text.split("\njobs:\n", 1)[1]
    return set(re.findall(r"^  ([a-z0-9-]+):$", jobs, flags=re.MULTILINE))


def matrix_targets(text: str) -> set[str]:
    return set(re.findall(r"^\s+target: ([a-z0-9_-]+)$", text, flags=re.MULTILINE))


class SpineReleaseWorkflowTest(unittest.TestCase):
    def test_product_docs_match_package_and_release_identity(self) -> None:
        readme = README.read_text(encoding="utf-8")
        package = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
        cargo = CARGO_TOML.read_text(encoding="utf-8")
        workflow = workflow_text(SPINE_WORKFLOW)

        repository_url = package["repository"]["url"]
        repository_url = repository_url.removeprefix("git+").removesuffix(".git")
        release_url = f"{repository_url}/releases"
        self.assertEqual(package["name"], "@spinejit/spine-codex")
        self.assertEqual(set(package["bin"]), {"spine-codex"})
        product_bin = next(iter(package["bin"]))

        self.assertIn(package["name"], readme)
        self.assertIn(product_bin, readme)
        self.assertIn(release_url, readme)
        self.assertIn(package["name"], workflow)
        self.assertIn(product_bin, workflow)
        self.assertIn("name: spine-release", workflow)
        self.assertIn('- "v*.*.*"', workflow)
        product_version = re.search(
            r'pub const PRODUCT_VERSION: &str = "([^"]+)";',
            INSTALL_CONTEXT.read_text(encoding="utf-8"),
        )
        self.assertIsNotNone(product_version)
        release_notes = RELEASE_NOTES_DIR / f"releases-{product_version.group(1)}.md"
        self.assertTrue(release_notes.is_file())
        self.assertIn(
            product_version.group(1), release_notes.read_text(encoding="utf-8")
        )
        # Compatibility version/tag derive from Cargo; the baseline commit has
        # a single literal source in distribution.rs, not duplicate metadata.
        workspace = tomllib.loads(cargo)["workspace"]
        self.assertNotEqual(workspace["package"]["version"], product_version.group(1))
        install_context = tomllib.loads(
            (ROOT / "codex-rs/install-context/Cargo.toml").read_text(encoding="utf-8")
        )
        self.assertIs(install_context["package"]["version"]["workspace"], True)
        distribution = INSTALL_CONTEXT.read_text(encoding="utf-8")
        self.assertRegex(
            distribution,
            r'pub const CODEX_COMPAT_VERSION: &str =\s*env!\("CARGO_PKG_VERSION"\);',
        )
        self.assertRegex(
            distribution,
            r'pub const CODEX_UPSTREAM_TAG: &str =\s*'
            r'concat!\("rust-v",\s*env!\("CARGO_PKG_VERSION"\)\);',
        )
        self.assertRegex(
            distribution,
            r'pub const CODEX_UPSTREAM_COMMIT: &str = "[0-9a-f]{40}";',
        )

    def test_release_gate_uses_product_not_workspace_version(self) -> None:
        workflow = workflow_text(SPINE_WORKFLOW)
        run = re.search(
            r"(?ms)^      - name: Validate version and source\n"
            r".*?^        run: \|\n(.*?)(?=^      - name:)",
            workflow,
        )
        self.assertIsNotNone(run)
        script = textwrap.dedent(run.group(1))
        self.assertIn("PRODUCT_VERSION", script)
        self.assertIn("codex-rs/install-context/src/distribution.rs", script)
        self.assertNotIn("codex-rs/Cargo.toml", script)

        product_version = re.search(
            r'pub const PRODUCT_VERSION: &str = "([^"]+)";',
            INSTALL_CONTEXT.read_text(encoding="utf-8"),
        ).group(1)
        cargo = tomllib.loads(CARGO_TOML.read_text(encoding="utf-8"))
        workspace_version = cargo["workspace"]["package"]["version"]
        self.assertNotEqual(product_version, workspace_version)

        output_root = ROOT / "temp" / "null" / datetime.now().strftime("%Y%m%d_%H%M")
        output_root.mkdir(parents=True, exist_ok=True)
        retained = Path(tempfile.mkdtemp(prefix="release-gate-", dir=output_root))
        for version, accepted in ((product_version, True), (workspace_version, False)):
            with self.subTest(version=version):
                output = retained / f"{version}.output"
                result = subprocess.run(
                    ["bash", "-c", script],
                    cwd=ROOT,
                    env={
                        "PATH": os.environ["PATH"],
                        "GITHUB_EVENT_NAME": "push",
                        "GITHUB_REF_TYPE": "tag",
                        "GITHUB_REF_NAME": f"v{version}",
                        "GITHUB_OUTPUT": str(output),
                    },
                    capture_output=True,
                    text=True,
                )
                if accepted:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(
                        output.read_text(encoding="utf-8"),
                        f"version={version}\ntag=v{version}\npublish=true\n",
                    )
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertFalse(output.exists())

    def test_product_and_upstream_release_lanes_are_separate(self) -> None:
        spine = workflow_text(SPINE_WORKFLOW)
        upstream = workflow_text(UPSTREAM_WORKFLOW)

        self.assertRegex(spine, r"(?m)^name: spine-release$")
        self.assertIn('- "v*.*.*"', spine)
        self.assertIn("  workflow_dispatch:", spine)
        self.assertIn('- "rust-v*.*.*"', upstream)
        self.assertNotIn("  workflow_dispatch:", upstream)

    def test_product_lane_covers_release_and_rehearsal_contract(self) -> None:
        text = workflow_text(SPINE_WORKFLOW)
        self.assertEqual(
            job_names(text),
            {
                "metadata",
                "build-unix",
                "build-windows",
                "package",
                "smoke",
                "release",
                "publish-npm",
                "verify-release",
            },
        )

        self.assertEqual(matrix_targets(text), NATIVE_TARGETS)
        self.assertIn('pkg.name !== "@spinejit/spine-codex"', text)
        self.assertIn("Object.keys(pkg.bin).length !== 1", text)
        self.assertIn("const expectedDependencies = Object.fromEntries(", text)
        self.assertIn(
            "pkg.version !== `${process.env.VERSION}-${process.env.PLATFORM}`", text
        )
        self.assertIn('"${payload_targets[0]}" != "$target"', text)
        self.assertIn("Create GitHub Release", text)
        self.assertIn("Verify GitHub latest and npm latest converge", text)
        self.assertIn("npm_tag=alpha", text)
        self.assertIn("releases/tags/${TAG}", text)
        self.assertNotIn("publish-r2", text)
        self.assertNotIn("codesigning", text)
        self.assertNotIn("codex-runners", text)
        self.assertNotIn("self-hosted", text)

    def test_native_packages_publish_before_the_root_package(self) -> None:
        text = workflow_text(SPINE_WORKFLOW)
        root = '"dist/npm/codex-npm-${VERSION}.tgz"'
        platform = '"dist/npm/codex-npm-linux-x64-${VERSION}.tgz"'
        self.assertLess(text.index(platform), text.index(root))

    def test_rehearsal_audits_current_native_executables(self) -> None:
        rehearsal = ROOT / ".github/workflows/spine-npm-rehearsal.yml"
        loop = re.search(
            r"(?ms)^          for required in .*?^          done$",
            rehearsal.read_text(encoding="utf-8"),
        )
        self.assertIsNotNone(loop)
        script = textwrap.dedent(loop.group(0))
        executables = (
            "bin/codex", "bin/codex-code-mode-host",
            "codex-resources/bwrap", "codex-path/rg",
        )
        output_root = ROOT / "temp" / "null" / datetime.now().strftime("%Y%m%d_%H%M")
        output_root.mkdir(parents=True, exist_ok=True)
        retained = Path(tempfile.mkdtemp(prefix="rehearsal-audit-", dir=output_root))
        # The current native layout omits the upstream-retired patched zsh.
        for missing in (None, *executables):
            native = retained / ("complete" if missing is None else missing.replace("/", "-"))
            native.mkdir()
            for name in executables:
                if name != missing:
                    binary = native / name
                    binary.parent.mkdir(parents=True, exist_ok=True)
                    binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                    binary.chmod(0o700)
            result = subprocess.run(
                ["bash", "-eu", "-c", script], env=os.environ | {"native": str(native)},
                capture_output=True, text=True,
            )
            with self.subTest(missing=missing):
                self.assertEqual(result.returncode == 0, missing is None, result.stderr)

    def test_staging_helper_resolves_the_product_workflow(self) -> None:
        text = STAGING_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('WORKFLOW_NAME = ".github/workflows/spine-release.yml"', text)


if __name__ == "__main__":
    unittest.main()
