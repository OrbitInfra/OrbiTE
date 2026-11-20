"""The oracle controller must consume every receipt from each child."""

import runpy
import sys
import unittest
from pathlib import Path

controller = runpy.run_path(
    str(Path(__file__).resolve().parents[1] / "reference" / "pending_read.py")
)


class ReferenceReceipts(unittest.TestCase):
    def test_rejects_buffered_duplicate_after_last_receipt(self):
        # Both lines enter the pipe in one write before the first readline.
        writer = "import os; os.write(1, b'DONE\\nDONE\\n')"
        with controller["Drivers"]([sys.executable, "-I", "-c", writer]) as drivers:
            child = drivers.start()
            self.assertEqual(controller["line"](child), "DONE")
            with self.assertRaisesRegex(RuntimeError, r"exited 0: 'DONE\\n'$"):
                controller["finish"](child)
