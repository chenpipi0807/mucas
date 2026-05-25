"""
Cross-file consensus — Leave-One-Out Validation
================================================
Trains consensus on N-1 files, tests on the held-out file.

Findings: Chinese scripts from different stories share very few patterns.
Only 1 word appears in all 4 test files (背景 = background/scene).
Cross-file REF requires a larger, more homogeneous corpus to be effective.
"""
import os, zlib, time
from mucas import naive_compress, compute_rai
from mucas.consensus import (
    build_cross_file_consensus, compress_ref_lz, decompress_ref,
    RAI_H_THRESHOLD, byte_entropy, find_matches,
)

TEST_DIR = r"d:\P-ZIP\test"

CHINESE_FILES = [
    "003-呆若木鸡-脚本.md",
    "004—单词play脚本.md",
    "Poly 海外AI漫剧内容评估标准体系1.0.md",
    "livepolt研发对接文档人话版.md",
]

MAX_BYTES = 80_000


def load(fname: str) -> bytes | None:
    path = os.path.join(TEST_DIR, fname)
    if not os.path.isfile(path):
        return None
    d = open(path, 'rb').read()
    return d if len(d) <= MAX_BYTES else None


print("Cross-file Consensus -- Leave-One-Out Validation")
print("=" * 60)
print(f"H_pattern threshold: {RAI_H_THRESHOLD} b/B\n")

files_data: dict[str, bytes] = {}
for fname in CHINESE_FILES:
    d = load(fname)
    if d is None:
        print(f"  SKIP (not found or too large): {fname}")
    else:
        files_data[fname] = d

if len(files_data) < 2:
    print("Need >=2 files for cross-file training. Aborting.")
    raise SystemExit(1)

# ── Part 1: Corpus shared-pattern analysis ─────────────────────────────────
print("Part 1: Shared pattern analysis")
print("-" * 40)
all_data = list(files_data.values())
for min_f in [2, 3, len(files_data)]:
    lib = build_cross_file_consensus(
        all_data, n=100, min_len=6, max_len=24, min_files=min_f, h_min=RAI_H_THRESHOLD,
    )
    if lib:
        all_b = b''.join(lib.values())
        avg_cov = sum(
            sum(e - s for s, e, _ in find_matches(d, lib)) / len(d)
            for d in all_data
        ) / len(all_data)
        print(f"  min_files={min_f}: {len(lib):>3} patterns  "
              f"avg_len={len(all_b)/len(lib):.1f}B  "
              f"H={byte_entropy(all_b):.2f} b/B  "
              f"avg_coverage={avg_cov:.1%}")
    else:
        print(f"  min_files={min_f}: 0 patterns")
print()

# ── Part 2: Leave-One-Out evaluation ───────────────────────────────────────
print("Part 2: Leave-One-Out evaluation (min_files=2, max_len=24)")
print("-" * 40)

results = []

for held_out, held_data in files_data.items():
    training_data = [d for fn, d in files_data.items() if fn != held_out]

    t0 = time.perf_counter()
    cross_consensus = build_cross_file_consensus(
        training_data, n=50, min_len=6, max_len=24,
        min_files=2, h_min=RAI_H_THRESHOLD,
    )
    t_train = time.perf_counter() - t0

    rai = compute_rai(held_data, n_consensus=50, window=32768)
    per_file_consensus = rai['consensus']

    raw = len(held_data)
    baseline = zlib.compress(naive_compress(held_data, window=32768), 1)
    prog_per = compress_ref_lz(held_data, per_file_consensus, window=32768)
    per_wrap = zlib.compress(prog_per, 1)
    assert decompress_ref(prog_per, per_file_consensus) == held_data

    if cross_consensus:
        cross_matches = find_matches(held_data, cross_consensus)
        cross_cov = sum(e - s for s, e, _ in cross_matches) / raw
        prog_cross = compress_ref_lz(held_data, cross_consensus, window=32768)
        cross_wrap = zlib.compress(prog_cross, 1)
        assert decompress_ref(prog_cross, cross_consensus) == held_data
    else:
        cross_cov = 0.0
        cross_wrap = baseline

    per_benefit   = (len(baseline) - len(per_wrap)) / len(baseline)
    cross_benefit = (len(baseline) - len(cross_wrap)) / len(baseline)

    name = held_out[:45]
    print(f"\n-- {name} ({raw:,} B) --")
    if cross_consensus:
        all_b = b''.join(cross_consensus.values())
        print(f"  Cross-lib ({t_train*1000:.0f}ms): {len(cross_consensus)} pats  "
              f"avg={len(all_b)/len(cross_consensus):.1f}B  "
              f"H={byte_entropy(all_b):.2f} b/B  "
              f"held-out cov={cross_cov:.1%}")
    else:
        print(f"  Cross-lib: 0 patterns found")
    print(f"  baseline+zlib : {len(baseline):>8,}  ({len(baseline)/raw:.2%})")
    print(f"  per-file+zlib : {len(per_wrap):>8,}  ({len(per_wrap)/raw:.2%})  "
          f"benefit:{per_benefit:+.1%}")
    print(f"  cross+zlib    : {len(cross_wrap):>8,}  ({len(cross_wrap)/raw:.2%})  "
          f"benefit:{cross_benefit:+.1%}")

    results.append(dict(
        name=held_out, raw=raw,
        baseline=len(baseline), per_file=len(per_wrap), cross=len(cross_wrap),
        per_benefit=per_benefit, cross_benefit=cross_benefit,
        cross_cov=cross_cov,
    ))

# ── Summary ─────────────────────────────────────────────────────────────────
print("\n" + "=" * 60)
print("SUMMARY")
print(f"{'File':<38} {'base':>6} {'per':>6} {'cross':>6}  {'cov':>5}")
print("-" * 60)
for r in results:
    print(f"{r['name'][:38]:<38} "
          f"{r['baseline']/r['raw']:>5.1%} "
          f"{r['per_file']/r['raw']:>5.1%} "
          f"{r['cross']/r['raw']:>5.1%}  "
          f"{r['cross_cov']:>4.1%}")

print()
print("Diagnosis: cross-file coverage is <5% on these 4 diverse scripts.")
print("Conclusion: REF cross-file requires larger/more homogeneous corpus.")
print("Best use case: many files of same format (logs, structured text).")
