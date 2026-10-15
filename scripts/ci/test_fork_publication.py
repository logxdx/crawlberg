"""Require the independently versioned browser fork before core crate publication."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
FORK_MANIFEST = "vendor/crawlberg-chromiumoxide/Cargo.toml"
FORK_NAME = "crawlberg-chromiumoxide"


class ForkPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        self.steps = self.workflow["jobs"]["publish-crates"]["steps"]

    def metadata_step(self) -> dict:
        matches = [step for step in self.steps if step.get("id") == "fork_metadata"]
        assert len(matches) == 1, "Expected one executable fork metadata step"
        return matches[0]

    def extract_metadata(self, manifest: str) -> tuple[subprocess.CompletedProcess[str], str]:
        step = self.metadata_step()
        assert step["shell"] == "bash"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / FORK_MANIFEST
            source.parent.mkdir(parents=True)
            source.write_text(manifest)
            output = root / "output"
            result = subprocess.run(
                ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                cwd=root,
                env={**os.environ, "GITHUB_OUTPUT": str(output)},
                capture_output=True,
                text=True,
                check=False,
            )
            return result, output.read_text() if output.exists() else ""

    def test_fork_publishes_first_with_its_own_manifest_and_version(self) -> None:
        publishers = [
            step for step in self.steps if step.get("uses", "").startswith("xberg-io/actions/publish-crates@")
        ]
        assert len(publishers) == 2, "Publish the fork separately before the six release-version crates"
        fork, core = publishers
        assert fork["with"] == {
            "crates": FORK_NAME,
            "manifest-path": FORK_MANIFEST,
            "version": "${{ steps.fork_metadata.outputs.version }}",
            "dry-run": "${{ needs.prepare.outputs.dry_run }}",
        }
        assert core["with"] == {
            "crates": "${{ env.CRATES_TO_PUBLISH }}",
            "version": "${{ needs.prepare.outputs.version }}",
            "dry-run": "${{ needs.prepare.outputs.dry_run }}",
        }
        assert FORK_NAME not in self.workflow["env"]["CRATES_TO_PUBLISH"].split()
        assert len(self.workflow["env"]["CRATES_TO_PUBLISH"].split()) == 6
        assert self.steps.index(self.metadata_step()) < self.steps.index(fork) < self.steps.index(core)

    def test_metadata_reads_the_fork_version_instead_of_the_release_version(self) -> None:
        for version in ("0.9.3", "0.10.0-rc.1"):
            with self.subTest(version=version):
                result, output = self.extract_metadata(f'[package]\nname = "{FORK_NAME}"\nversion = "{version}"\n')
                assert result.returncode == 0, result.stderr
                assert output == f"version={version}\n"

    def test_missing_or_invalid_package_identity_cannot_emit_a_version(self) -> None:
        manifests = (
            '[package]\nversion = "0.9.3"\n',
            '[package]\nname = "other-crate"\nversion = "0.9.3"\n',
            f'[package]\nname = "{FORK_NAME}"\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = ""\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = "   "\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = 93\n',
        )
        for manifest in manifests:
            with self.subTest(manifest=manifest):
                result, output = self.extract_metadata(manifest)
                assert result.returncode != 0
                assert output == ""


if __name__ == "__main__":
    unittest.main()
