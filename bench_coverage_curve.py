"""
Coverage threshold curve: n → coverage → benefit
==================================================
Fix one held-out file; vary n (library size) to sweep coverage from ~0% to ~25%.
Goal: locate the minimum coverage for net positive cross-file REF benefit.

DeepSeek hypothesis: empirical threshold ≈ 5-10%.
"""
import os, zlib, time
from mucas import naive_compress
from mucas.consensus import (
    build_cross_file_consensus, compress_ref_lz, decompress_ref,
    find_matches, byte_entropy, RAI_H_THRESHOLD,
)

CORPUS_DIR = r"d:\P-ZIP\test\log_corpus"

files_data: dict[str, bytes] = {}
for fname in sorted(os.listdir(CORPUS_DIR)):
    if fname.endswith('.txt'):
        files_data[fname] = open(os.path.join(CORPUS_DIR, fname), 'rb').read()

# Use first file as held-out; train on all others
fnames = list(files_data.keys())
held_fn   = fnames[0]
held_data = files_data[held_fn]
training  = [d for fn, d in files_data.items() if fn != held_fn]

raw      = len(held_data)
baseline = zlib.compress(naive_compress(held_data, window=32768), 1)

print("Coverage Threshold Curve")
print("=" * 58)
print(f"Held-out: {held_fn}  ({raw:,} B)")
print(f"Training corpus: {len(training)} files")
print(f"baseline+zlib: {len(baseline):,} B ({len(baseline)/raw:.2%})\n")

# Build the FULL library once (n=500, collects all candidates)
t0 = time.perf_counter()
full_lib = build_cross_file_consensus(
    training, n=500, min_len=6, max_len=32, min_files=2, h_min=RAI_H_THRESHOLD,
)
t_build = time.perf_counter() - t0
print(f"Full library: {len(full_lib)} patterns  [{t_build:.2f}s]\n")

# The full library patterns are already ranked by expected savings.
# Re-extract ranked list and sweep n from 1 to len(full_lib).
# For efficiency: pre-sort patterns by expected savings as stored in full_lib,
# then for each n, take top-n patterns.
ranked_patterns = list(full_lib.values())  # already greedy-selected in order

print(f"{'n':>5}  {'cov':>6}  {'H':>5}  {'baseline':>9}  {'REF+zlib':>9}  {'benefit':>8}  {'verdict':>8}")
print("-" * 58)

results = []

# Test n = 1, 2, 3, 5, 8, 12, 18, 25, 35, 50, 75, 100, 150, 200, full
n_values = [1, 2, 3, 5, 8, 12, 18, 25, 35, 50, 75, 100, 150, 200]
n_values = [v for v in n_values if v <= len(ranked_patterns)]
if len(ranked_patterns) not in n_values:
    n_values.append(len(ranked_patterns))

for n in n_values:
    sub_lib = {i: p for i, p in enumerate(ranked_patterns[:n])}

    matches = find_matches(held_data, sub_lib)
    covered = sum(e - s for s, e, _ in matches)
    cov = covered / raw

    all_b = b''.join(sub_lib.values())
    h = byte_entropy(all_b)

    prog_cross = compress_ref_lz(held_data, sub_lib, window=32768)
    cross_wrap = zlib.compress(prog_cross, 1)
    assert decompress_ref(prog_cross, sub_lib) == held_data

    benefit = (len(baseline) - len(cross_wrap)) / len(baseline)
    verdict = "[Y]" if benefit > 0.01 else "[N]"

    print(f"{n:>5}  {cov:>5.1%}  {h:>4.2f}  "
          f"{len(baseline):>9,}  {len(cross_wrap):>9,}  "
          f"{benefit:>+7.1%}  {verdict:>8}")

    results.append(dict(n=n, coverage=cov, h=h, benefit=benefit, positive=(benefit > 0.01)))

# Locate threshold
crossings = [i for i in range(1, len(results))
             if not results[i-1]['positive'] and results[i]['positive']]
if crossings:
    lo = results[crossings[0]-1]
    hi = results[crossings[0]]
    print(f"\nThreshold crossing: n={lo['n']} -> {hi['n']}  "
          f"cov={lo['coverage']:.1%} -> {hi['coverage']:.1%}  "
          f"benefit={lo['benefit']:+.1%} -> {hi['benefit']:+.1%}")
    print(f"Empirical coverage threshold: ~{(lo['coverage']+hi['coverage'])/2:.1%}")
else:
    pos = [r for r in results if r['positive']]
    neg = [r for r in results if not r['positive']]
    if pos:
        print(f"\nAll tested points positive from n={pos[0]['n']} (cov={pos[0]['coverage']:.1%})")
    if neg:
        print(f"Negative cases: max cov = {max(r['coverage'] for r in neg):.1%}")
