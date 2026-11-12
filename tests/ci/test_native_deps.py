"""Drive the real apt wrapper with bounded, process-local command fixtures."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class NativeDependencies(unittest.TestCase):
    def run_case(self, failures, packages=("g++", "libibverbs-dev"), *,
                 interrupted_install=False, repair_fails=False):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            scripts = {
                "dpkg": '''#!/bin/bash
if [[ $1 == --print-architecture ]]; then
    echo amd64
elif [[ $1 == --configure && $2 == -a ]]; then
    printf 'repair\\n' >> "$RUNNER_TEMP/apt-log"
    [[ $REPAIR_FAILS == 0 ]] || exit 1
    rm -f "$RUNNER_TEMP/dpkg-dirty"
elif [[ $2 == "g++" ]]; then
    echo "Status: install ok installed"
else
    exit 1
fi
''',
                "sudo": '#!/bin/bash\nexec "$@"\n',
                "apt-get": '''#!/bin/bash
set -eu
printf '%s\\n' "$*" >> "$RUNNER_TEMP/apt-log"
if [[ -f $RUNNER_TEMP/dpkg-dirty ]]; then
    echo 'dpkg was interrupted; configure it before apt' >&2
    exit 100
fi
if [[ $INTERRUPTED_INSTALL == 1 && $* == *'install -y'* && ! -f $RUNNER_TEMP/interrupted ]]; then
    touch "$RUNNER_TEMP/interrupted" "$RUNNER_TEMP/dpkg-dirty"
    exit 124
fi
''',
                "timeout": '''#!/bin/bash
set -eu
[[ $1 == --kill-after=10s && $2 == 180 ]]
shift 2
printf '%s\\n' "$*" >> "$RUNNER_TEMP/attempt-log"
n=0; if [[ -f $RUNNER_TEMP/count ]]; then read -r n < "$RUNNER_TEMP/count"; fi
n=$((n+1)); echo "$n" > "$RUNNER_TEMP/count"
if ((n <= FAILURES)); then exit 124; fi
exec "$@"
''',
            }
            for name, text in scripts.items():
                path = root / name
                path.write_text(text)
                path.chmod(0o755)
            result = subprocess.run(["bash", str(ROOT / ".github/scripts/install-native-deps.sh"), *packages],
                                    env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                                         "RUNNER_TEMP": temp, "FAILURES": str(failures),
                                         "INTERRUPTED_INSTALL": str(int(interrupted_install)),
                                         "REPAIR_FAILS": str(int(repair_fails))},
                                    capture_output=True, text=True, timeout=10)
            count = int((root / "count").read_text()) if (root / "count").exists() else 0
            apt = (root / "apt-log").read_text() if (root / "apt-log").exists() else ""
            return result, count, apt

    def test_success_installs_only_absent_packages(self):
        result, count, apt = self.run_case(0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(count, 1)
        self.assertNotIn("g++", apt)
        self.assertIn("install -y --no-install-recommends libibverbs-dev", apt)
        self.assertIn("succeeded on attempt 1/3", result.stdout)

    def test_no_download_when_everything_is_present(self):
        result, count, apt = self.run_case(3, ("g++",))
        self.assertEqual((result.returncode, count, apt), (0, 0, ""))

    def test_transient_timeout_changes_mirror_and_recovers(self):
        result, count, apt = self.run_case(1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(count, 2)
        self.assertIn("mirror=runner-default", result.stdout)
        self.assertIn("mirror=https://archive.ubuntu.com/ubuntu", result.stdout)
        self.assertIn("Dir::Etc::sourcelist=", apt)
        self.assertIn("succeeded on attempt 2/3", result.stdout)

    def test_exhaustion_remains_fatal(self):
        result, count, apt = self.run_case(3)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((count, apt), (3, ""))
        self.assertIn("attempt 3/3: mirror=runner-default", result.stdout)
        self.assertIn("exhausted", result.stderr)

    def test_interrupted_install_is_repaired_before_next_apt_attempt(self):
        result, count, apt = self.run_case(0, interrupted_install=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(count, 2)
        self.assertEqual(apt.count("repair\n"), 1)
        before, after = apt.split("repair\n")
        self.assertIn("install -y", before)
        self.assertIn("install -y", after)
        self.assertNotIn("dpkg was interrupted", result.stderr)
        self.assertIn("succeeded on attempt 2/3", result.stdout)

    def test_failed_repair_is_bounded_and_does_not_run_apt(self):
        result, count, apt = self.run_case(0, interrupted_install=True, repair_fails=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(count, 3)
        self.assertEqual(apt.count("install -y"), 1)
        self.assertEqual(apt.count("repair\n"), 2)
        self.assertIn("exhausted", result.stderr)


if __name__ == "__main__":
    unittest.main()
