# μCAS — Micro Compression Assembly

> **中文版请见 [README_CN.md](README_CN.md)**

A deterministic bytecode language for structured compression, paired with an information-theoretic framework for predicting when domain-specific knowledge helps.

μCAS was designed collaboratively over a multi-day conversation between a human user, [Claude](https://claude.ai) (Anthropic), and [DeepSeek](https://chat.deepseek.com). The full theoretical dialogue is preserved here:

- **[DeepSeek conversation archive (complete, v0.1 → v0.7) →](https://chat.deepseek.com/share/zys9ibg5775ot5r97m)**
- **[DeepSeek conversation archive (v0.8 → v0.12.1, cross-file REF theory) →](https://chat.deepseek.com/share/mpud8uac7z0bd0mra6)**
- [Earlier archive (v0.1 theory) →](https://chat.deepseek.com/share/exxssdoj9jkw1fgj0q)

---

## Install — one step, no terminal required

| Platform | Download | Steps |
|----------|----------|-------|
| **Windows** | `mucas-install-windows.zip` | Extract → double-click `install.bat` |
| **macOS**   | `mucas-install-macos.zip`   | Extract → run `./install.sh` in Terminal |
| **Linux**   | `mucas-install-linux.zip`   | Extract → run `./install.sh` in Terminal |

After installing: **right-click any folder → "Pack with μCAS"**, right-click any `.mcar` → **"Unpack here"**.
No terminal needed after installation.

> macOS note: on first use, enable the Quick Actions in System Settings → Privacy & Security → Extensions → Finder.

---

## Rust implementation — v0.12.1 (active)

**[`mucas-rs/`](mucas-rs/)** is the production Rust crate: a complete, self-contained
compressor, archiver, and CLI tool that implements the μCAS VM and all synthesis passes.

### Quick start — one command

```sh
# Download a pre-built binary from the Releases page, then:
mucas pack    my_folder/  -o archive.mcar         # standard pack
mucas pack    my_folder/  -o archive.mcar --deep  # cross-file REF (see below)
mucas unpack  archive.mcar  -o restored/          # restore
mucas list    archive.mcar                        # see what's inside
mucas check   archive.mcar                        # verify integrity
```

### Build from source

```sh
cargo build --release --manifest-path mucas-rs/Cargo.toml
mucas-rs/target/release/mucas bench your_file.csv
```

### Real-world benchmark (v0.9.1)

Tested on two representative mixed archives — the kind you'd actually want to back up
(videos, office documents, wheel archives, zip files, CSV data):

**1 GB mixed archive (19 files: MP4, PNG, PPTX, ZIP, CSV, WAV, MD)**

| Tool | Time | Output size |
|------|------|-------------|
| **μCAS v0.9** | **13 s** | 721 MB (99.0%) |
| ZIP (Deflate) | 17 s | 719 MB (98.9%) |
| 7-zip (LZMA2 -mx=5) | 28 s | 708 MB (97.4%) |

**8.5 GB mixed archive (videos, Python wheels, PPTX, EXE, ZIP, CSV, WAV)**

| Tool | Time | Output size |
|------|------|-------------|
| **μCAS v0.9** | **7 s** | 8.8 GB (99.9%) |
| 7-zip (LZMA2 -mx=5) | **313 s** | 8.8 GB (99.9%) |

μCAS is **45× faster** than 7-zip on large mixed archives, with identical output size.
The speedup comes entirely from *not* wasting CPU: already-compressed formats (MP4, WHL, PPTX,
ZIP, EXE, …) are detected by magic bytes in ≤ 12 bytes and stream-copied verbatim.
7-zip attempts full LZMA2 compression on every file regardless of content.

### `--deep`: cross-file REF compression (v0.12+)

For directories with many structurally identical files (logs, API responses, database exports):

```sh
mucas pack my_logs/ -o archive.mcar --deep
```

`--deep` runs a two-pass pipeline:
1. **Scan pass** — synthesizes each file, extracts shared patterns from residual LIT tokens
2. **Pack pass** — stores the pattern dictionary once in the archive header; each file's
   shared regions become 3-byte REF tokens instead of full literal bytes

**Cross-file REF benchmark** (40 homogeneous log files, 42 KB each, 1.7 MB total):

| Mode | Archive size | vs standard |
|------|-------------|------------|
| Standard (`mucas pack`) | 135.9 KB | baseline |
| **Deep (`mucas pack --deep`)** | **61.5 KB** | **−54.7%** |

The `--deep` mode includes an automatic gain estimator: if cross-file REF is predicted to
add overhead rather than savings, the REF step is skipped and the archive falls back to
standard μCAS quality — no manual tuning required.

### Features
- **Streaming constant-memory archiver** — packs 800 GB directories without loading more than
  one file at a time; memory budget configurable with `--max-memory MiB`.
- **Smart method selection** — μCAS vs Zlib vs Store chosen per-file by MDL comparison.
- **Already-compressed detection** — JPEG, PNG, MP4, ZIP, 7z, RAR, gzip, PE (.exe), OGG, and
  more detected by magic bytes and stored verbatim (no wasted CPU).
- **Cross-file REF** via `--deep` — archive-level consensus dictionary, linear gain scaling.
- **Progress bars** via `indicatif`.
- **Pre-built binaries** for Linux, macOS (Apple Silicon), and Windows via GitHub Actions.

### Performance vs zlib (v0.7.0+)

| Format | Example | vs zlib |
|--------|---------|---------|
| Pipe-delimited log | nginx access log | **−86%** |
| Identical log lines | batch output | **−47%** |
| Space-separated syslog | varied timestamps | **−76%** |
| Quoted CSV (RFC-4180) | address books | **−66%** |
| NDJSON | API responses | **−30%** |
| TSV | spreadsheet exports | **−17%** |
| CSV (plain) | database exports | **−12%** |

All results are round-trip verified. Methodology and per-file breakdowns:
**[mucas-rs/BENCHMARK.md](mucas-rs/BENCHMARK.md)**

### What v0.7 synthesizes

| Pattern | Instruction | Notes |
|---------|-------------|-------|
| CSV / TSV / pipe rows with fixed columns | `SCAN` | RFC-4180 quoted fields supported |
| NDJSON / JSON arrays | `SCAN` | preserves exact whitespace |
| Exact periodic repetition | `LOOP` | |
| Repeated byte sequences | `CALL` (macro) | Rabin-Karp up to 64 B; suffix-array up to 1024 B |
| Arithmetic / delta sequences | `MAP` | timestamps, counters |

---

## What is μCAS?

μCAS is **not** a replacement for 7-zip or zstd. It is a format standard for expressing *how to reconstruct data* using a minimal instruction set, designed so that:

- The **compressor** can be arbitrarily intelligent (LZ search, pattern synthesis, AI program generation)
- The **decompressor** is always a trivial, constant-complexity VM (8 opcodes, a table lookup)

The key novelty is the **REF instruction**: a 3-byte reference to a pre-agreed *consensus library* of domain-specific patterns. REF enables cross-file compression gains by replacing high-entropy repeated patterns with compact uniform tokens — enabling entropy collapse in the outer entropy coder (zlib/zstd).

```
Input data
    │
    ▼  LZ scan (CPY + LIT)
    │  REF replacement (if H_pattern > 2.5 b/B AND coverage > 10%)
    │  Structural synthesis (LOOP + MAP for runs and sequences)
    ▼
μCAS program  →  zlib/zstd  →  .ufi file
```

---

## Empirical results

| Data type | Condition | REF gain over LZ+zlib |
|-----------|-----------|----------------------|
| Chinese UTF-8 scripts | Single-file consensus (50 patterns) | **+9% to +14%** |
| Homogeneous API logs | Cross-file LOO (19-file corpus, n=50) | **+10% to +15%** |
| English tech text | — | −2% to 0% (REF skipped correctly) |
| Structured JSON | — | −3% to +2% (REF skipped correctly) |

**RAI prediction accuracy: 25/25 (100%)** across all test cases.

> Gains are relative to a naive LZ + zlib-1 baseline.
> μCAS is not benchmarked against 7-zip; see the [honest comparison](#honest-comparison) section.

---

## Quick start

```python
from mucas import naive_compress, MuCASVM

data = b"Hello " * 1000
prog = naive_compress(data)
vm = MuCASVM(); vm.exec(prog)
assert bytes(vm.out) == data   # round-trip verified
print(f"{len(data)} B  →  {len(prog)} B  ({len(prog)/len(data):.1%})")
```

### With the REF applicability predictor

```python
from mucas import compute_rai
from mucas.consensus import compress_ref_lz, decompress_ref

# Only use REF when H_pattern > 2.5 b/B (information-theoretic threshold)
rai = compute_rai(data)
print(f"H_pattern = {rai['h_pattern']:.2f} b/B   REF helps: {rai['rai_predicts']}")

if rai['rai_predicts']:
    prog_ref = compress_ref_lz(data, rai['consensus'])
    assert decompress_ref(prog_ref, rai['consensus']) == data
```

### Cross-file consensus library (.ufc)

```python
from mucas import build_cross_file_consensus, UfcFile
from mucas.consensus import compress_ref_lz, predict_cross_ref_benefit

# Build from corpus once; reuse for all new files of the same format
files = [open(f, "rb").read() for f in training_files]
lib = build_cross_file_consensus(files, n=50, h_min=2.5)

ufc = UfcFile.from_consensus(lib, domain="my-logs", version=(1, 0))
open("my-logs-v1.ufc", "wb").write(ufc.encode())

# Check 2D RAI before compressing a new file
ufc2 = UfcFile.decode(open("my-logs-v1.ufc", "rb").read())
pred = predict_cross_ref_benefit(new_data, ufc2.to_consensus())
# pred['predicts_benefit'] = (H_pattern > 2.5 b/B) AND (coverage > 10%)
```

---

## Package layout

```
mucas/
  vm.py         LEB128 encoding, 8 MAP transforms, MuCASVM executor
  compress.py   naive_compress (LZ), smart_compress (structural)
  consensus.py  build_consensus, compute_rai, predict_cross_ref_benefit
  format.py     UfiFile (.ufi) — EMBEDDED / EXTERNAL / HYBRID modes
  corpus.py     UfcFile (.ufc) — immutable consensus corpus snapshot

MUCAS_SPEC_v0.1.md   Full format specification (995 lines, v0.1 final)
bench_rai.py         Single-file RAI prediction benchmark
bench_crossfile_logs.py  Cross-file LOO benchmark (20 synthetic log files)
bench_coverage_curve.py  Coverage vs. benefit curve experiment
gen_log_corpus.py    Generate synthetic homogeneous log files for testing
test_mucas.py        Unit tests (all 8 instructions + round-trip)
```

---

## Honest comparison

μCAS `naive_compress` uses a simple sliding-window LZ (window=32 KB) plus zlib-1 wrapping. On typical files, its absolute ratios are:

| File | Raw | zlib-1 | zlib-9 | μCAS+zlib | μCAS+REF+zlib |
|------|-----|--------|--------|-----------|---------------|
| Chinese script (20 KB) | 100% | 41.5% | 38.1% | 50.3% | **45.6%** |
| API log (5.5 KB) | 100% | 26.5% | 25.9% | 31.3% | **30.5%** |
| JSON (42 KB) | 100% | 13.8% | 8.9% | 12.0% | 12.2% |

7-zip (LZMA) would achieve ~30–35% on Chinese text, comfortably beating μCAS. **The LZ layer is intentionally minimal.** The specification supports replacing `naive_compress` with any LZ implementation; a μCAS encoder backed by LZMA-level LZ is a future engineering task.

The research contribution is the **REF mechanism and its predictor**, not the raw compression ratio.

---

## Specification highlights

[`MUCAS_SPEC_v0.1.md`](MUCAS_SPEC_v0.1.md) documents:

- 8-instruction set with full formal semantics
- VM termination guarantee (all programs terminate; proof by structural induction)
- 11 error codes (UNKNOWN_OPCODE, CPY_UNDERFLOW, OUTPUT_OVERFLOW, CALL_CYCLE, …)
- MAX_CALL_DEPTH = 16 (prevents stack exhaustion)
- `.ufi` binary format with EMBEDDED / EXTERNAL / HYBRID consensus modes
- `.ufc` corpus format with SHA-256 content-addressing and integrity seal
- **Appendix C**: RAI v3 derivation — H* = C_REF / avg_pattern_len ≈ 2.0–2.5 b/B
- **Appendix D**: Cross-file consensus theory, 2D RAI model, coverage threshold calibration
- **Appendix E**: `.ufc` design principles, snapshot versioning, delta publishing protocol

---

## Running benchmarks

Generate test data first (no real files required):

```bash
python gen_log_corpus.py     # creates test/log_corpus/ with 20 synthetic log files
python bench_crossfile_logs.py
python bench_coverage_curve.py
```

For the single-file RAI benchmark, provide your own text files and edit `TEST_DIR` in `bench_rai.py`.

---

## Origin and credits

μCAS emerged from a question: *"why is compression always a black box?"*

The design and implementation were developed through a three-way collaboration:
- **User** — project initiator, relay between Claude and DeepSeek, empirical validation runner
- **[Claude](https://claude.ai) (Anthropic)** — implementation, specification writing, benchmark design
- **[DeepSeek](https://www.deepseek.com)** — theoretical analysis, information-theoretic derivations, architectural decisions

The complete theoretical dialogue (10+ sessions) is archived at:
**https://chat.deepseek.com/share/exxssdoj9jkw1fgj0q**

---

## Requirements

Python 3.10+. No external dependencies for core functionality.
`zstandard` optional for extended benchmarks.

## License

MIT — see [LICENSE](LICENSE)
