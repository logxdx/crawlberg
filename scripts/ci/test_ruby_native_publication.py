"""Exercise native gem packaging without compiling or loading an extension."""

from __future__ import annotations

import io
import json
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/publish/package-ruby-native.rb"


class RubyNativePublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        result = subprocess.run(
            [
                "ruby",
                "-rjson",
                "-rrbconfig",
                "-e",
                (
                    "puts JSON.generate(platform: Gem::Platform.local.to_s, "
                    'abi: RbConfig::CONFIG.fetch("ruby_version"), suffix: RbConfig::CONFIG.fetch("DLEXT"))'
                ),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        self.host = json.loads(result.stdout)
        platform = self.host["platform"]
        if "-darwin" in platform:
            self.host["platform"] = platform.split("-darwin")[0] + "-darwin"
        elif platform.endswith("-linux-gnu"):
            self.host["platform"] = platform.removesuffix("-gnu")
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.package = Path(self.directory.name)
        (self.package / "lib").mkdir()
        (self.package / "lib/crawlberg.rb").write_text("module Crawlberg; end\n")
        (self.package / "ext").mkdir()
        (self.package / "ext/extconf.rb").write_text("require 'mkmf'\n")
        (self.package / "lib/crawlberg").mkdir()
        (self.package / "lib/crawlberg/native.rb").write_text("module NativeFixture; end\n")
        (self.package / "README.md").write_text("Package README\n")
        self.foreign_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.foreign_directory.cleanup)
        self.foreign_cwd = Path(self.foreign_directory.name)
        (self.foreign_cwd / "README-UNRELATED.md").write_text("Must never ship\n")
        (self.package / "crawlberg.gemspec").write_text(
            "Gem::Specification.new do |s|\n"
            's.name = "crawlberg"; s.version = "1.11.0"; s.summary = "Fixture"\n'
            's.authors = ["Fixture"]; s.license = "MIT";\n'
            "s.files = Dir.glob(%w[README* lib/crawlberg.rb lib/crawlberg/**/*]).select { |f| File.file?(f) }\n"
            's.require_paths = ["lib"]; s.extensions = ["ext/extconf.rb"]\n'
            's.required_ruby_version = ">= 3.2"\nend\n'
        )
        self.extension = self.package / f"lib/crawlberg_rb.{self.host['suffix']}"
        self.payload = b"native-release-payload\x00" * 64
        self.extension.write_bytes(self.payload)

    def package_native(self, platform: str | None = None, version: str = "1.11.0") -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["ruby", str(SCRIPT), str(self.package), platform or self.host["platform"], version],
            cwd=self.foreign_cwd,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_host_fixture_normalizes_gnu_and_darwin_only(self) -> None:
        for platform, expected in (
            ("x86_64-linux-gnu", "x86_64-linux"),
            ("aarch64-linux-gnu", "aarch64-linux"),
            ("arm64-darwin-24", "arm64-darwin"),
            ("x86_64-linux-musl", "x86_64-linux-musl"),
            ("x64-mingw-ucrt", "x64-mingw-ucrt"),
            ("arm-linux-gnueabihf", "arm-linux-gnueabihf"),
        ):
            with self.subTest(platform=platform):
                fixture = RubyNativePublicationTests("test_native_gem_contains_exact_extension_and_only_its_ruby_abi")
                self.addCleanup(fixture.doCleanups)
                result = subprocess.CompletedProcess(
                    ["ruby"], 0, json.dumps({"platform": platform, "abi": "3.4.0", "suffix": "so"})
                )
                with patch.object(subprocess, "run", return_value=result):
                    fixture.setUp()
                assert fixture.host["platform"] == expected

    def test_native_gem_contains_exact_extension_and_only_its_ruby_abi(self) -> None:
        result = self.package_native()
        assert result.returncode == 0, result.stderr
        gems = list((self.package / "pkg").glob("*.gem"))
        assert [g.name for g in gems] == [f"crawlberg-1.11.0-{self.host['platform']}.gem"]
        with tarfile.open(gems[0]) as gem:
            payload = gem.extractfile("data.tar.gz")
            assert payload is not None
            with tarfile.open(fileobj=io.BytesIO(payload.read()), mode="r:gz") as contents:
                path = f"lib/crawlberg_rb/{self.host['abi']}/crawlberg_rb.{self.host['suffix']}"
                member = contents.extractfile(path)
                assert member is not None and member.read() == self.payload
                assert "lib/crawlberg.rb" in contents.getnames()
                assert "lib/crawlberg/native.rb" in contents.getnames()
                assert "README.md" in contents.getnames()
                assert "README-UNRELATED.md" not in contents.getnames()
        metadata = subprocess.run(
            [
                "ruby",
                "-rjson",
                "-rrubygems/package",
                "-e",
                (
                    "s=Gem::Package.new(ARGV[0]).spec; "
                    "puts JSON.generate(platform: s.platform.to_s, extensions: s.extensions, "
                    "requirements: s.required_ruby_version.to_s)"
                ),
                str(gems[0]),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        spec = json.loads(metadata.stdout)
        major, minor, _patch = self.host["abi"].split(".")
        assert spec["platform"] == self.host["platform"]
        assert spec["extensions"] == []
        assert spec["requirements"] == f">= {major}.{minor}, < {major}.{int(minor) + 1}"

    def test_missing_or_empty_extension_cannot_produce_a_gem(self) -> None:
        for exists in (False, True):
            with self.subTest(exists=exists):
                if exists:
                    self.extension.write_bytes(b"")
                else:
                    self.extension.unlink()
                result = self.package_native()
                assert result.returncode != 0
                assert "native extension" in result.stderr.lower()
                assert not list(self.package.glob("pkg/*.gem"))

    def test_wrong_platform_or_release_version_cannot_produce_a_gem(self) -> None:
        for platform, version in (("invalid-platform", "1.11.0"), (self.host["platform"], "1.12.0")):
            with self.subTest(platform=platform, version=version):
                result = self.package_native(platform, version)
                assert result.returncode != 0
                expected = "native platform" if platform == "invalid-platform" else "identity/version"
                assert expected in result.stderr, result.stderr
                assert not list(self.package.glob("pkg/*.gem"))

    def test_platform_normalization_preserves_windows_ucrt_and_linux_architecture(self) -> None:
        result = subprocess.run(
            [
                "ruby",
                "-rjson",
                "-e",
                (
                    "require ARGV.shift; puts JSON.generate(ARGV.map { |p| "
                    "RubyNativePackage.canonical_platform(Gem::Platform.new(p)) })"
                ),
                str(SCRIPT),
                "arm64-darwin-24",
                "x86_64-linux-gnu",
                "aarch64-linux-gnu",
                "x64-mingw-ucrt",
                "x86_64-linux-musl",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        assert json.loads(result.stdout) == [
            "arm64-darwin",
            "x86_64-linux",
            "aarch64-linux",
            "x64-mingw-ucrt",
            "x86_64-linux-musl",
        ]

    def test_repair_uses_workflow_tooling_while_native_sources_remain_at_release_ref(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        steps = workflow["jobs"]["ruby-gem"]["steps"]
        checkouts = [s for s in steps if s.get("uses", "").startswith("actions/checkout@")]
        assert len(checkouts) == 2, "Release sources and repair packaging tooling need separate pinned checkouts"
        assert checkouts[0]["with"]["ref"] == "${{ needs.prepare.outputs.checkout_ref }}"
        assert "path" not in checkouts[0]["with"]
        assert checkouts[1]["with"] == {
            "ref": "${{ github.sha }}",
            "path": ".release-tools",
            "sparse-checkout": "scripts/publish/package-ruby-native.rb",
            "sparse-checkout-cone-mode": "false",
            "persist-credentials": "false",
        }
        packager = next(s for s in steps if "package-ruby-native.rb" in s.get("run", ""))
        assert steps.index(checkouts[0]) < steps.index(checkouts[1]) < steps.index(packager)
        assert packager["run"] == (
            'ruby .release-tools/scripts/publish/package-ruby-native.rb packages/ruby "$EXPECTED_PLATFORM" "$VERSION"'
        )

    def test_each_builder_packages_native_payload_and_rejects_empty_upload(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        job = workflow["jobs"]["ruby-gem"]
        assert {item["platform"] for item in job["strategy"]["matrix"]["include"]} == {
            "x86_64-linux",
            "aarch64-linux",
            "arm64-darwin",
            "x86_64-darwin",
            "x64-mingw-ucrt",
        }
        packaging = [s for s in job["steps"] if "package-ruby-native.rb" in s.get("run", "")]
        assert len(packaging) == 1
        assert packaging[0]["env"]["EXPECTED_PLATFORM"] == "${{ matrix.platform }}"
        assert packaging[0]["env"]["VERSION"] == "${{ needs.prepare.outputs.gem_version }}"
        upload = next(s for s in job["steps"] if s.get("uses", "").startswith("actions/upload-artifact@"))
        assert upload["with"]["if-no-files-found"] == "error"


if __name__ == "__main__":
    unittest.main()
