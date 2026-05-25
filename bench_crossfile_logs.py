"""
Cross-file consensus on homogeneous log corpus — coverage threshold validation.

20 synthetic log files, same program format, different content.
Leave-One-Out: train on 19 files, compress the held-out file.

Hypothesis (DeepSeek): cross-file REF is positive when coverage > 8-10%.
"""
import os, zlib, time
from mucas import naive_compress
from mucas.consensus import (
    build_cross_file_consensus, compress_ref_lz, decompress_ref,
    predict_cross_ref_benefit, RAI_H_THRESHOLD, CROSS_COVERAGE_THRESHOLD,
    byte_entropy,
)

CORPUS_DIR = r"d:\P-ZIP\test\log_corpus"

files_data: dict[str, bytes] = {}
for fname in sorted(os.listdir(CORPUS_DIR)):
    if fname.endswith('.txt'):
        path = os.path.join(CORPUS_DIR, fname)
        data = open(path, 'rb').read()
        files_data[fname] = data

print("Cross-file Consensus — Homogeneous Log Corpus")
print("=" * 62)
print(f"Corpus: {len(files_data)} files  "
      f"avg size: {sum(len(d) for d in files_data.values())//len(files_data):,} B")
print(f"H_pattern threshold:  {RAI_H_THRESHOLD} b/B")
print(f"Coverage threshold:   {CROSS_COVERAGE_THRESHOLD:.0%}\n")

# ── Part 1: shared pattern analysis across corpus ──────────────────────────
print("Part 1: Shared pattern analysis (full corpus, no LOO)")
print("-" * 50)
all_data = list(files_data.values())
for min_f in [2, 5, 10, 15, len(files_data)]:
    lib = build_cross_file_consensus(
        all_data, n=100, min_len=6, max_len=32, min_files=min_f, h_min=RAI_H_THRESHOLD,
    )
    if lib:
        all_b = b''.join(lib.values())
        avg_cov = sum(
            sum(e - s for s, e, _ in
                __import__('mucas.consensus', fromlist=['find_matches']).find_matches(d, lib)
               ) / len(d)
            for d in all_data
        ) / len(all_data)
        print(f"  min_files={min_f:>2}: {len(lib):>3} patterns  "
              f"avg_len={len(all_b)/len(lib):.1f}B  "
              f"H={byte_entropy(all_b):.2f} b/B  "
              f"avg_cov={avg_cov:.1%}")
    else:
        print(f"  min_files={min_f:>2}: 0 patterns")
print()

# ── Part 2: Leave-One-Out evaluation ───────────────────────────────────────
print("Part 2: Leave-One-Out evaluation")
print("-" * 50)

results = []
fnames = list(files_data.keys())

t_total = time.perf_counter()
for held_out in fnames:
    held_data = files_data[held_out]
    training_data = [d for fn, d in files_data.items() if fn != held_out]

    cross_lib = build_cross_file_consensus(
        training_data, n=50, min_len=6, max_len=32,
        min_files=2, h_min=RAI_H_THRESHOLD,
    )

    raw = len(held_data)
    baseline = zlib.compress(naive_compress(held_data, window=32768), 1)

    pred = predict_cross_ref_benefit(held_data, cross_lib)

    if cross_lib:
        prog_cross = compress_ref_lz(held_data, cross_lib, window=32768)
        cross_wrap = zlib.compress(prog_cross, 1)
        assert decompress_ref(prog_cross, cross_lib) == held_data
    else:
        cross_wrap = baseline

    benefit = (len(baseline) - len(cross_wrap)) / len(baseline)
    rai_correct = (pred['predicts_benefit'] == (benefit > 0.01))

    results.append(dict(
        name=held_out,
        raw=raw,
        baseline=len(baseline),
        cross=len(cross_wrap),
        benefit=benefit,
        coverage=pred['cross_coverage'],
        h_pattern=pred['h_pattern'],
        predicts=pred['predicts_benefit'],
        rai_correct=rai_correct,
    ))

t_total = time.perf_counter() - t_total

# Print results grouped by coverage
results.sort(key=lambda r: r['coverage'], reverse=True)
print(f"\n{'File':<30} {'cov':>5} {'H':>5} {'2D-RAI':>7} {'benefit':>8} {'verdict':>7}")
print("-" * 62)
for r in results:
    rai_str = "[Y]" if r['predicts'] else "[N]"
    ok_str  = "[PASS]" if r['rai_correct'] else "[FAIL]"
    print(f"{r['name'][:30]:<30} "
          f"{r['coverage']:>4.1%} "
          f"{r['h_pattern']:>4.2f} "
          f"{rai_str:>7} "
          f"{r['benefit']:>+7.1%} "
          f"{ok_str:>7}")

n_pass = sum(r['rai_correct'] for r in results)
print(f"\n2D RAI accuracy: {n_pass}/{len(results)} correct  "
      f"[total time: {t_total:.1f}s]")

# ── Coverage threshold analysis ─────────────────────────────────────────────
print("\nPart 3: Coverage vs benefit scatter")
print("-" * 50)
print(f"{'Coverage':>10}  {'Benefit':>8}  {'H_pattern':>10}")
for r in sorted(results, key=lambda r: r['coverage']):
    bar = "+" * int(max(0, r['benefit']) * 100) + ("-" if r['benefit'] < 0 else "")
    print(f"  {r['coverage']:>6.1%}     {r['benefit']:>+6.1%}     "
          f"{r['h_pattern']:.2f}    {bar[:30]}")

# estimate threshold
positives = [r for r in results if r['benefit'] > 0.01]
negatives = [r for r in results if r['benefit'] <= 0.01]
if positives and negatives:
    min_pos_cov = min(r['coverage'] for r in positives)
    max_neg_cov = max(r['coverage'] for r in negatives)
    print(f"\n  Min coverage in positive cases: {min_pos_cov:.1%}")
    print(f"  Max coverage in negative cases: {max_neg_cov:.1%}")
    if min_pos_cov > max_neg_cov:
        print(f"  --> Clean separation. Empirical threshold: {(min_pos_cov+max_neg_cov)/2:.1%}")
    else:
        print(f"  --> Overlapping. Threshold region: {max_neg_cov:.1%} - {min_pos_cov:.1%}")
