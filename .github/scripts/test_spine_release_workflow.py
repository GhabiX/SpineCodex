#!/usr/bin/env python3

from pathlib import Path
import json
import re
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
        workspace_version = re.search(
            r"(?ms)^\[workspace\.package\]\s+version = \"([^\"]+)\"",
            cargo,
        )
        self.assertIsNotNone(workspace_version)
        self.assertIn(f'version = "{workspace_version.group(1)}"', cargo)
        release_notes = RELEASE_NOTES_DIR / f"releases-{workspace_version.group(1)}.md"
        self.assertTrue(release_notes.is_file())
        self.assertIn(
            workspace_version.group(1), release_notes.read_text(encoding="utf-8")
        )
        metadata = re.search(
            r"(?ms)^\[workspace\.metadata\.spinecodex\](.*?)(?=^\[|\Z)",
            cargo,
        )
        self.assertIsNotNone(metadata)
        compat_version = re.search(
            r'^codex_compat_version = "([^"]+)"$', metadata.group(1), re.MULTILINE
        )
        upstream_tag = re.search(
            r'^codex_upstream_tag = "([^"]+)"$', metadata.group(1), re.MULTILINE
        )
        upstream_commit = re.search(
            r'^codex_upstream_commit = "([^"]+)"$', metadata.group(1), re.MULTILINE
        )
        self.assertIsNotNone(compat_version)
        self.assertIsNotNone(upstream_tag)
        self.assertIsNotNone(upstream_commit)
        distribution = INSTALL_CONTEXT.read_text(encoding="utf-8")
        for name, value in (
            ("CODEX_COMPAT_VERSION", compat_version.group(1)),
            ("CODEX_UPSTREAM_TAG", upstream_tag.group(1)),
            ("CODEX_UPSTREAM_COMMIT", upstream_commit.group(1)),
        ):
            with self.subTest(name=name):
                self.assertRegex(
                    distribution,
                    rf'pub const {name}: &str = "{re.escape(value)}";',
                )

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

    def test_staging_helper_resolves_the_product_workflow(self) -> None:
        text = STAGING_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('WORKFLOW_NAME = ".github/workflows/spine-release.yml"', text)


if __name__ == "__main__":
    unittest.main()
