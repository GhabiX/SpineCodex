#!/usr/bin/env python3

import json
import re
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PACKAGE_JSON = ROOT / "codex-cli" / "package.json"
CARGO_TOML = ROOT / "codex-rs" / "Cargo.toml"
README = ROOT / "README.md"
README_ZH_CN = ROOT / "README.zh-CN.md"
VERSIONING = ROOT / "codex-rs" / "docs" / "spinecodex-versioning.md"
WORKFLOW = ROOT / ".github" / "workflows" / "spine-release.yml"


def package_metadata() -> tuple[str, str, str]:
    package = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
    [(binary, _entrypoint)] = package["bin"].items()
    repository = package["repository"]["url"]
    repository = repository.removeprefix("git+").removesuffix(".git")
    return package["name"], binary, repository


class SpineProductDocsTest(unittest.TestCase):
    def test_readmes_follow_package_and_release_metadata(self) -> None:
        package, binary, repository = package_metadata()
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertRegex(workflow, r"(?m)^name: spine-release$")
        self.assertIn('- "v*.*.*"', workflow)
        self.assertIn(f'pkg.name !== "{package}"', workflow)
        self.assertIn(f'pkg.bin["{binary}"]', workflow)

        for path in (README, README_ZH_CN):
            with self.subTest(path=path.name):
                readme = path.read_text(encoding="utf-8")
                self.assertIn(f"npm install -g {package}@latest", readme)
                self.assertRegex(readme, rf"(?m)^\s*{re.escape(binary)}\s*$")
                self.assertIn(f"{repository}/releases", readme)
                self.assertIn("./.github/assets/spinecodex-tree.svg", readme)

    def test_versioning_document_follows_version_sources(self) -> None:
        cargo = tomllib.loads(CARGO_TOML.read_text(encoding="utf-8"))
        distribution = (
            ROOT / "codex-rs/install-context/src/distribution.rs"
        ).read_text()
        product_version = re.search(
            r'pub const PRODUCT_VERSION: &str = "([^"]+)";', distribution
        ).group(1)
        compat_version = cargo["workspace"]["package"]["version"]
        upstream_commit = re.search(
            r'pub const CODEX_UPSTREAM_COMMIT: &str = "([0-9a-f]{40})";',
            distribution,
        ).group(1)
        versioning = VERSIONING.read_text(encoding="utf-8")

        for value in (
            product_version,
            compat_version,
            f"rust-v{compat_version}",
            upstream_commit,
        ):
            with self.subTest(value=value):
                self.assertIn(f"`{value}`", versioning)

    def test_readme_local_links_resolve(self) -> None:
        for readme_path in (README, README_ZH_CN):
            text = readme_path.read_text(encoding="utf-8")
            links = re.findall(r"(?:href=\"|\]\()([^\"#)][^\"\)]*)", text)
            local_links = {
                link.removeprefix("./")
                for link in links
                if not link.startswith(("http://", "https://", "mailto:"))
            }
            missing = sorted(link for link in local_links if not (ROOT / link).exists())
            self.assertEqual(missing, [], readme_path.name)


if __name__ == "__main__":
    unittest.main()
