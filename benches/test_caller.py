"""Controls for building one measured caller against two source checkouts."""
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from build_caller import compiler_args
from run import check_caller_dep_info


class CallerTests(unittest.TestCase):
    def test_only_the_declared_benchmark_input_changes(self):
        with TemporaryDirectory(prefix="bench source ") as tmp:
            base = Path(tmp).resolve()
            caller = base / "current tree/benches/transfer.rs"
            args = ["--crate-name", "transfer", "--edition=2024", "benches/transfer.rs", "--test"]
            self.assertEqual(compiler_args(args, base, caller), args[:3] + [str(caller), "--test"])
            for other in (["-vV"], ["--print", "sysroot"], ["--crate-name", "transfer", "--print=file-names"],
                          ["--crate-name", "orbite", "src/lib.rs"]):
                self.assertEqual(compiler_args(other, base, caller), other)
            for invalid in (["--crate-name", "transfer", "other/transfer.rs"], args + ["benches/transfer.rs"]):
                with self.assertRaises(ValueError):
                    compiler_args(invalid, base, caller)

    def test_dep_info_binds_the_executable_to_the_measured_source(self):
        exe = Path("/cache/rust output/transfer-abc")
        caller = Path("/measurement tree/benches/transfer.rs")
        escaped = str(caller).replace(" ", "\\ ")
        receipt = f"{exe}: {escaped}\n\n{escaped}:\n"
        check_caller_dep_info(receipt, exe, caller)
        for wrong in (receipt.replace(str(exe), "/other-binary"), receipt.replace(escaped, "benches/transfer.rs"),
                      f"{exe}: benches/transfer.rs\n\n{escaped}:\n"):
            with self.assertRaises(ValueError):
                check_caller_dep_info(wrong, exe, caller)
