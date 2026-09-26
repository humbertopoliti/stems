"""Golden test of the rendered Homebrew formula (deliverable 32).

Regenerate the golden after a reviewed template change with
`STEMS_UPDATE_GOLDEN=1 make test-release`.
"""

import hashlib
import os
import pathlib
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import render_formula  # noqa: E402

GOLDEN = HERE / "stems.rb.golden"


def fake_dist(root: pathlib.Path) -> None:
    """One tarball per target (content = the target name), except linux arm
    which only has its .sha256 sidecar, to exercise both sources."""
    for target in render_formula.TARGETS:
        name = render_formula.tarball_name(target)
        if target == "aarch64-unknown-linux-gnu":
            digest = hashlib.sha256(target.encode()).hexdigest()
            (root / f"{name}.sha256").write_text(f"{digest}  {name}\n")
        else:
            (root / name).write_bytes(target.encode())


class RenderFormula(unittest.TestCase):
    def render(self, **kw):
        with tempfile.TemporaryDirectory() as d:
            dist = pathlib.Path(d)
            fake_dist(dist)
            out = dist / "stems.rb"
            args = [
                "--dist", str(dist),
                "--version", kw.get("version", "v0.1.0-rc.1"),
                "--base-url", "https://github.com/acme/stems/releases/download/v0.1.0-rc.1/",
                "--homepage", "https://github.com/acme/stems",
                "--out", str(out),
            ]
            self.assertEqual(render_formula.main(args), 0)
            return out.read_text()

    def test_golden(self):
        text = self.render()
        if os.environ.get("STEMS_UPDATE_GOLDEN"):
            GOLDEN.write_text(text)
        self.assertEqual(text, GOLDEN.read_text(), "formula drifted; see module docstring")

    def test_installs_completions_and_man_pages(self):
        text = self.render()
        for line in (
            'bin.install "stems"',
            'bash_completion.install "completions/stems.bash" => "stems"',
            'zsh_completion.install "completions/_stems"',
            'fish_completion.install "completions/stems.fish"',
            'man1.install Dir["man/*.1"]',
            'system bin/"stems", "validate", "--skip-requires"',
            'version "0.1.0-rc.1"',
        ):
            self.assertIn(line, text)
        self.assertNotRegex(text, render_formula.PLACEHOLDER)

    def test_every_target_needs_an_artifact(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(SystemExit):
                render_formula.main(["--dist", d, "--version", "0.1.0", "--base-url", "file:///x"])


if __name__ == "__main__":
    unittest.main()
