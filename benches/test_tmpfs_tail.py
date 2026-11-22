"""Controls for order balance and within-triplet p99 ratios."""
from collections import Counter, defaultdict
from itertools import permutations
import unittest

from run import nearest_rank_p99
from tmpfs_tail import schedule, summarize_pairs


class TmpfsTailTests(unittest.TestCase):
    def test_every_case_runs_each_permutation_twice_and_reverses_case_order(self):
        by_case, by_trial = defaultdict(Counter), defaultdict(list)
        for trial, op, pair, order in schedule():
            by_case[op, pair][order] += 1
            by_trial[trial].append((op, pair))
        self.assertEqual(set(by_case), {("read", "AA"), ("read", "AB"), ("write", "AA"), ("write", "AB")})
        expected = {order: 2 for order in permutations(("baseline", "probe", "tent"))}
        for counts in by_case.values():
            self.assertEqual(counts, expected)
        for trial in range(6):
            self.assertEqual(by_trial[trial + 6], by_trial[trial][::-1])

    def test_position_effect_is_visible_and_equality_is_within_guard(self):
        rows = []
        for trial, op, pair, order in schedule():
            processes = {role: {"p99_100_ns": 100 + 10 * index,
                                "p99_1000_ns": 21420 + (trial == 0) if role == "probe" else 20400}
                         for index, role in enumerate(order)}
            rows.append(dict(trial=trial, op=op, pair=pair, order=order, processes=processes))
        for result in summarize_pairs(rows).values():
            short, full = result["sample_counts"].values()
            self.assertGreater(short["by_order"]["baseline_first"], 1.05)
            self.assertLess(short["by_order"]["probe_first"], 1.0)
            self.assertEqual(short["pairs_over_limit"], 6)
            self.assertEqual(full["pairs_over_limit"], 1)
            self.assertEqual(full["probe_over_tent_median"], 1.05)
        self.assertEqual(nearest_rank_p99(range(1, 101)), 99)
        self.assertEqual(nearest_rank_p99(range(1000, 0, -1)), 990)


if __name__ == "__main__":
    unittest.main()
