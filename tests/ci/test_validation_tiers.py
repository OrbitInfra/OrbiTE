"""Guard the workflow boundaries: daily checks must not dilute release gates.

These checks inspect the checked-in step blocks; actionlint validates YAML and
Actions expressions separately. No workflow runner or GitHub credentials needed.
"""
from pathlib import Path
import re
import unittest

WORKFLOWS = Path(__file__).resolve().parents[2] / ".github/workflows"


def workflow(name):
    return (WORKFLOWS / name).read_text()


def steps(name):
    return dict(re.findall(
        r"^      - name: ([^\n]+)\n(.*?)(?=^      - |\Z)",
        workflow(name), re.M | re.S))


class ValidationTiers(unittest.TestCase):
    def test_complete_rust_suite_is_unconditional_and_real_io_is_required(self):
        text = workflow("ci.yml")
        suite = steps("ci.yml")["Test CPU TCP and ownership"]
        self.assertNotIn("if:", suite)
        self.assertIn('cargo test --workspace --locked --features "$ORBITE_FEATURES"', suite)
        self.assertIn("ORBITE_REQUIRE_IO_URING=1", text)
        self.assertIn("toolchain: [stable, '1.85.0']", text)
        self.assertIn("'rdma,cuda,io-uring,gds,metrics,rdma-adaptive'", text)

    def test_stress_steps_require_extended_but_release_always_enables_it(self):
        for name in ("ci.yml", "python.yml"):
            self.assertIn("ORBITE_EXTENDED: ${{ inputs.release_gate || inputs.extended }}",
                          workflow(name))
            repeated = {label: body for label, body in steps(name).items()
                        if label.startswith(("Repeat ", "Stress "))}
            self.assertTrue(repeated, name)
            for label, body in repeated.items():
                with self.subTest(workflow=name, step=label):
                    self.assertTrue(body.startswith("        if: env.ORBITE_EXTENDED == 'true' && ("))
                    self.assertIn("run:", body)

    def test_release_calls_all_three_complete_workflows(self):
        text = workflow("release.yml")
        for name in ("ci.yml", "python.yml", "native-cpu.yml"):
            self.assertIn(f"uses: ./.github/workflows/{name}\n    with:\n      release_gate: true", text)
        self.assertIn("(inputs.release_gate || inputs.extended) && "
                      "'[\"3.10\", \"3.11\", \"3.12\", \"3.13\", \"3.14\"]' || '[\"3.13\"]'",
                      workflow("python.yml"))

    def test_weekly_and_manual_validation_include_the_oracle(self):
        text = workflow("extended.yml")
        self.assertIn("schedule:", text)
        self.assertIn("workflow_dispatch:", text)
        for name in ("ci.yml", "python.yml"):
            self.assertIn(f"uses: ./.github/workflows/{name}\n    with:\n      extended: true", text)
        self.assertIn("uses: ./.github/workflows/native-cpu.yml", text)
        self.assertNotIn("  pull_request:", workflow("native-cpu.yml"))
        self.assertIn("  workflow_dispatch:", workflow("native-cpu.yml"))

    def test_installed_wheels_and_sdist_still_run_in_regular_ci(self):
        for label, body in steps("python.yml").items():
            if label.startswith(("Build and test", "Run the publication wheel", "Drive installed CUDA")):
                self.assertNotIn("if:", body, label)
        sdist = steps("python.yml")["Check source distribution contents"]
        self.assertIn("matrix.python == '3.13'", sdist)
        self.assertNotIn("ORBITE_EXTENDED", sdist)

    def test_concurrency_cancels_only_obsolete_prs_not_releases(self):
        for name in ("ci.yml", "python.yml"):
            self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' && !inputs.release_gate }}",
                          workflow(name))
        self.assertIn("cancel-in-progress: false", workflow("release.yml"))


if __name__ == "__main__":
    unittest.main()
