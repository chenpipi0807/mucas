"""
RAI v3 Benchmark — H_pattern as primary predictor
===================================================
Validates the information-theoretically derived RAI v3:

  Primary metric: H_pattern (byte entropy of consensus patterns, bits/byte)
  Threshold: ~2.5 b/B (derived from C_REF/avg_pattern_len)

  H_pattern > threshold → pattern entropy exceeds REF token cost → REF helps
  H_pattern < threshold → entropy too low, fragmentation dominates     → REF hurts

Theoretical basis (DeepSeek/Claude analysis):
  Net REF gain per occurrence ≈ L × H_pattern - C_REF bits
  Profitable when H_pattern > C_REF / L ≈ 20 bits / 10 bytes = 2.0 b/B
  Empirical threshold: ~2.5 b/B (confirmed on 5 test files, 5/5 correct)
"""
import os, zlib, time
from mucas import naive_compress, compute_rai
from mucas.consensus import compress_ref_lz, decompress_ref, RAI_H_THRESHOLD

TEST_DIR = r"d:\P-ZIP\test"

TARGETS = [
    "003-呆若木鸡-脚本.md",
    "004—单词play脚本.md",
    "langchain_architecture_analysis.md",
    "repro_log_SVIP.txt",
    "pencil-new.json",
]


def run(path: str, window: int = 32768):
    with open(path, 'rb') as f:
        data = f.read()
    if len(data) > 80_000:
        print(f"  SKIP (too large): {os.path.basename(path)}")
        return

    name = os.path.basename(path)
    raw  = len(data)

    t0 = time.perf_counter()
    a = compute_rai(data, n_consensus=50, window=window)
    t_rai = time.perf_counter() - t0

    prog_lz  = naive_compress(data, window=window)
    baseline = zlib.compress(prog_lz, 1)
    prog_ref = compress_ref_lz(data, a['consensus'], window=window)
    ref_wrap = zlib.compress(prog_ref, 1)
    assert decompress_ref(prog_ref, a['consensus']) == data, "ROUND-TRIP FAILED"

    benefit      = (len(baseline) - len(ref_wrap)) / len(baseline)
    ref_helps    = benefit > 0.01
    correct      = a['rai_predicts'] == ref_helps

    zlib_raw = len(zlib.compress(data, 1)) / raw

    print(f"\n-- {name[:55]} ({raw:,} B) --")
    print(f"  LZ: coverage={a['lz_coverage']:.1%}  "
          f"CPY_count={a['cpy_count']}  avg_CPY_len={a['avg_cpy_len']:.1f}B")
    print(f"  H_pattern={a['h_pattern']:.2f} b/B  "
          f"(threshold={RAI_H_THRESHOLD})  "
          f"REF_cov={a['ref_data_coverage']:.1%}  "
          f"zlib_raw={zlib_raw:.1%}")
    print(f"  RAI predicts: {'REF HELPS [Y]' if a['rai_predicts'] else 'REF HURTS [N]'}")
    print(f"  baseline+zlib: {len(baseline):>8,}  ({len(baseline)/raw:.2%})")
    print(f"  REF+zlib:      {len(ref_wrap):>8,}  ({len(ref_wrap)/raw:.2%})  "
          f"benefit:{benefit:+.1%}")
    print(f"  Prediction: {'[PASS]' if correct else '[FAIL]'}  "
          f"[RAI time: {t_rai*1000:.0f}ms]")


print("RAI v3 — H_pattern Predictor")
print("=" * 55)
print(f"H_pattern threshold: {RAI_H_THRESHOLD} b/B")
print(f"Theory: H* = C_REF / avg_pattern_len ≈ 20 bits / 10 B = 2.0 b/B\n")

for fname in TARGETS:
    path = os.path.join(TEST_DIR, fname)
    if os.path.isfile(path):
        run(path)
    else:
        print(f"\n  SKIP (not found): {fname}")
