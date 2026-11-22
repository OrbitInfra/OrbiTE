"""Order-balanced, untraced singleton io_uring diagnostics on one tmpfs mount."""
from itertools import permutations, product
import json
from pathlib import Path
from statistics import median
from tempfile import TemporaryDirectory

from guards import LIMITS
from run import RSS_PREFIX, nearest_rank_p99, peak_rss, summarize


def schedule():
    orders = list(permutations(("baseline", "probe", "tent")))
    cases = list(product(("read", "write"), ("AA", "AB")))
    for cycle in range(2):
        for index, order in enumerate(orders):
            # Repeat each permutation with the opposite order of cases.
            for op, pair in (reversed(cases) if (cycle + index) % 2 else cases):
                yield cycle * len(orders) + index, op, pair, order


def summarize_pairs(rows):
    limit = LIMITS["p99_ns"][1] / 100
    by_case = {}
    for op, pair in product(("read", "write"), ("AA", "AB")):
        case = [row for row in rows if (row["op"], row["pair"]) == (op, pair)]
        horizons = {}
        for count in (100, 1000):
            key = f"p99_{count}_us"
            ratios = []
            order_ratios = {"baseline_first": [], "probe_first": []}
            tent_ratios = []
            for row in case:
                values = row["processes"]
                ratio = values["probe"][key] / values["baseline"][key]
                ratios.append(ratio)
                order = row["order"]
                first = "baseline_first" if order.index("baseline") < order.index("probe") else "probe_first"
                order_ratios[first].append(ratio)
                tent_ratios.append(values["probe"][key] / values["tent"][key])
            horizons[count] = {
                "paired_median": median(ratios),
                "pairs_over_limit": sum(x > limit for x in ratios),
                "by_order": {order: median(group) for order, group in order_ratios.items()},
                "probe_over_tent_median": median(tent_ratios),
                "median_us": {role: median(row["processes"][role][key] for row in case)
                              for role in ("baseline", "probe", "tent")},
            }
        by_case[f"{op}-{pair}"] = {"triplets": len(case), "sample_counts": horizons}
    return by_case


def measure(out, tmpfs, run, frozen, candidate, tent, tent_env):
    rows = []
    with TemporaryDirectory(prefix="orbite-tmpfs-tail-", dir=tmpfs) as scratch:
        for trial, op, pair, order in schedule():
            processes = {}
            binaries = {"baseline": frozen, "probe": frozen if pair == "AA" else candidate, "tent": tent}
            for role in order:
                name = f"{trial}-{op}-{pair}-{role}"
                path = Path(scratch) / name
                result = json.loads(run(name, ["/usr/bin/time", "-f", "\\n" + RSS_PREFIX + "%M", "--",
                                               binaries[role], "io_uring", op, 4096, 1, "contiguous", 3, 1000, 2, path],
                                        overrides=tent_env if role == "tent" else None))
                full = summarize(result, 1000, 4096)
                if role != "tent" and (result["direct_io"] is not False or type(result["physical_owners"]) is not int
                                       or result["physical_owners"] != 1):
                    raise ValueError(f"{name}: expected one buffered io_uring owner")
                rss = peak_rss((out / (name + ".stderr")).read_text())
                # The two horizons share the same process. The first 100 match the formal sample count.
                first = nearest_rank_p99(result["samples_ns"][:100]) / 1000
                processes[role] = {"p99_100_us": first, "p99_1000_us": full["p99_us"], "peak_rss_kib": rss}
                path.unlink()
            rows.append(dict(trial=trial, op=op, pair=pair, order=order, processes=processes))
    report = {"workload": "io_uring-tmpfs 1 x 4096 contiguous", "pairs": rows, "summary": summarize_pairs(rows)}
    (out / "tmpfs-tail.json").write_text(json.dumps(report, indent=2) + "\n")
