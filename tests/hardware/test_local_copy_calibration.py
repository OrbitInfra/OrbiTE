import copy
import json
import unittest

from local_copy_run import SAMPLES, calibration_rows, proposals


def rows(mode='kernel', cap=1):
    return [dict(case='calibration', cuda_device_uuid='GPU-test', direction=direction,
                 count=128, width=4096, bytes=128 * 4096, backend=mode, cta_limit=cap,
                 kernel_ctas=cap if mode == 'kernel' else 0,
                 dma_batch_calls=SAMPLES + 5 if mode == 'batch' else 0,
                 interference=True, samples=SAMPLES, warmup=5,
                 intent='ForegroundGet' if direction == 'h2d' else 'Checkpoint',
                 priority='High' if direction == 'h2d' else 'Low',
                 byte_source_guard_oracle='passed', copy_ms=[1.] * SAMPLES,
                 compute_alone_ms=[2.] * SAMPLES, compute_with_copy_ms=[2.05] * SAMPLES)
            for direction in ('h2d', 'd2h')]


class Calibration(unittest.TestCase):
    def check_rows(self, value, mode='kernel', cap=1):
        return calibration_rows('\n'.join(map(json.dumps, value)), 'GPU-test', 128, 4096, mode, cap, True)

    def test_complete_checks_keep_raw_samples_and_compute_slowdown(self):
        result = self.check_rows(rows())
        self.assertEqual(len(result), 2)
        self.assertAlmostEqual(result[0]['compute_slowdown'], 1.025)
        self.assertEqual(result[0]['copy_gib_s'], 0.48828125)
        self.assertEqual(len(result[0]['copy_ms']), SAMPLES)

    def test_partial_unchecked_wrong_class_and_over_cap_rows_fail(self):
        for key, bad in [('kernel_ctas', 2), ('copy_ms', [1.]), ('compute_alone_ms', []),
                         ('byte_source_guard_oracle', 'not checked'), ('priority', 'Low'),
                         ('intent', 'Unspec'), ('dma_batch_calls', 1), ('width', 8192)]:
            value = rows()
            value[0][key] = bad
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.check_rows(value)
        with self.assertRaises(ValueError):
            self.check_rows(rows()[:1])
        value = rows('batch', 0)
        value[0]['dma_batch_calls'] = 0
        with self.assertRaises(ValueError):
            self.check_rows(value, 'batch', 0)

    def test_proposals_require_both_dma_baselines_and_interference_evidence(self):
        group = self.check_rows(rows())
        self.assertTrue(all(p['kernel_threshold_candidate'] is None for p in proposals(group)))
        for mode in ('dma', 'batch'):
            baseline = rows(mode, 0)
            for r in baseline:
                r['copy_ms'] = [2.] * SAMPLES
            group += self.check_rows(baseline, mode, 0)
        candidate = next(p for p in proposals(group) if p['width'] == 4096 and p['direction'] == 'd2h')
        self.assertEqual(candidate['kernel_threshold_candidate'], 128)
        self.assertIsNone(candidate['kernel_threshold_background_candidate'])
        later = copy.deepcopy(group)
        for r in later:
            r['count'] = 256
        candidate = next(p for p in proposals(group + later) if p['width'] == 4096 and p['direction'] == 'd2h')
        self.assertEqual(candidate['kernel_threshold_background_candidate'], 256)
        for r in group:
            r['compute_slowdown'] = 1.2
        self.assertTrue(all(p['kernel_threshold_candidate'] is None for p in proposals(group)))


if __name__ == '__main__':
    unittest.main()
