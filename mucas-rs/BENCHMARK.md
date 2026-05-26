# μCAS v0.7 Benchmark Report

## Methodology

All measurements were taken on the same machine with `mucas bench <file>` (release
build, `cargo build --release`).

**Compression ratio definition**: `compressed_size / original_size × 100%`.
Lower is better.

**`.mucas` size**: the MucasFile encoding of the μCAS program — zlib over the
synthesized instruction stream + subroutine table.

**`zlib(input)`**: standard zlib applied directly to the raw input bytes
(`flate2::write::ZlibEncoder`, default compression level 6).

**`vs zlib`**: `(zlib_size − mucas_size) / zlib_size × 100%`.
Positive = μCAS is smaller.

**Round-trip**: every benchmark verifies `decompress(compress(input)) == input` before
reporting results.  A round-trip failure is a hard error.

## Test corpus

All test files are synthetically generated so that results are reproducible.
Generation scripts are in `bench/gen/`.

| File | Description | Size |
|------|-------------|------|
| `synthetic_records.csv` | 200-row CSV, 4 columns (2 long fixed, 2 variable) | 13.8 KB |
| `synthetic_records_1k.csv` | 1000-row CSV, same schema | 71.0 KB |
| `synthetic_records.tsv` | 1000-row TSV, 4 columns (3 long fixed, 1 variable) | 41.0 KB |
| `access_log.pipe` | 500-row pipe-delimited access log, 5 columns | 25.5 KB |
| `synthetic_log.txt` | 500 identical log lines | 28.0 KB |
| `syslog_varied.txt` | 500 varied syslog lines (space-separated, unique timestamps) | 31.0 KB |
| `sa_ideal.txt` | 20 × 318-byte repeated blocks, 3-byte separators | 6.5 KB |
| `synthetic_ndjson.ndjson` | 500-row NDJSON (`id` varies; `status`, `region` fixed) | 26.9 KB |
| `synthetic_quoted.csv` | 500-row quoted CSV (first field contains comma, second fixed) | 18.9 KB |

## Full results

### synthetic_records.csv — CSV with fixed columns

```
Input size:  13800 bytes
Data class:  StructuredLog
Synth path:  hybrid
LZ ratio:    27.99%  (3862 bytes)
Synth ratio: 23.85%  (3291 bytes)  [gain +4.14%]
.mucas size: 11.84%  (1634 bytes)
zlib(input): 13.42%  (1852 bytes)
μCAS beats zlib by 11.8%
```

The SCAN detector finds the fixed columns (`fixed_category`, `fixed_region`) and
promotes them to the template subroutine.  The parameter stream carries only the two
variable columns per row.

---

### synthetic_records_1k.csv — Large CSV

```
Input size:  71000 bytes
Data class:  StructuredLog
Synth path:  hybrid
LZ ratio:    26.66%  (18930 bytes)
Synth ratio: 25.48%  (18092 bytes)  [gain +1.18%]
.mucas size: 11.13%  (7905 bytes)
zlib(input): 12.62%  (8963 bytes)
μCAS beats zlib by 11.8%
```

Consistent +11.8% gain at 5× the row count.  The per-row overhead of the SCAN
parameter stream amortises well over 1000 rows.

---

### synthetic_records.tsv — TSV with long fixed columns

```
Input size:  41000 bytes
Data class:  StructuredLog
Synth path:  hybrid
LZ ratio:    34.15%  (14003 bytes)
Synth ratio: 26.97%  (11056 bytes)  [gain +7.19%]
.mucas size: 10.09%  (4136 bytes)
zlib(input): 12.11%  (4967 bytes)
μCAS beats zlib by 16.7%
```

Higher gain than CSV because the fixed TSV columns are longer (more bytes saved per
NEXT substitution).  `detect_scan_best` finds the `\t` delimiter after the `,` search
returns nothing.

---

### access_log.pipe — Pipe-delimited access log

```
Input size:  25500 bytes
Data class:  StructuredLog
Synth path:  lz-first
LZ ratio:    13.88%  (3540 bytes)
Synth ratio: 13.88%  (3540 bytes)  [gain +0.00%]
.mucas size: 0.69%  (175 bytes)
zlib(input): 5.04%  (1284 bytes)
μCAS beats zlib by 86.4%
```

The structural synthesizer does not improve over raw LZ on this file (all columns vary).
The 86% gain comes from the format layer: the LZ-first program is a compact stream of
CPY back-references and short LIT residuals — far more compressible by zlib than the
original 500-row log.  This illustrates the two-level nature of μCAS compression.

---

### synthetic_log.txt — Identical log lines

```
Input size:  28000 bytes
Data class:  StructuredLog
Synth path:  hybrid
LZ ratio:    1.76%  (492 bytes)
Synth ratio: 0.22%  (62 bytes)  [gain +1.54%]
.mucas size: 0.31%  (87 bytes)
zlib(input): 0.59%  (165 bytes)
μCAS beats zlib by 47.3%
```

500 identical lines → single LOOP instruction.  The 62-byte synthesized program
encodes 28 KB.  The format-layer zlib then compresses the tiny program to 87 bytes.

---

### syslog_varied.txt — Space-separated log with unique timestamps

```
Input size:  31000 bytes
Data class:  StructuredLog
Synth path:  lz-first
LZ ratio:    15.74%  (4880 bytes)
Synth ratio: 15.74%  (4880 bytes)  [gain +0.00%]
.mucas size: 1.36%  (421 bytes)
zlib(input): 5.59%  (1732 bytes)
μCAS beats zlib by 75.7%
```

No structural synthesis gain (unique timestamps; space delimiter excluded to avoid prose
false-positives).  Same two-level mechanism as `access_log.pipe`.

---

### sa_ideal.txt — Long repeated blocks (SA macro target)

```
Input size:  6460 bytes
Data class:  StructuredLog
Synth path:  hybrid
LZ ratio:    8.53%  (551 bytes)
Synth ratio: 7.23%  (466 bytes)  [gain +1.30%]
.mucas size: 4.64%  (300 bytes)
zlib(input): 5.08%  (328 bytes)
μCAS beats zlib by 8.5%
```

20 × 318-byte repeated blocks separated by 3-byte unique tags.  The suffix-array macro
extractor (`find_long_macro_by_sa`) finds the 318-byte pattern (well above the
Rabin-Karp ceiling of 64 bytes) and promotes it to a CALL subroutine.

---

### synthetic_ndjson.ndjson — NDJSON with fixed keys

```
Input size:  26890 bytes
Data class:  JsonArray
Synth path:  hybrid
LZ ratio:    12.64%  (3399 bytes)
Synth ratio: 7.31%  (1966 bytes)  [gain +5.33%]
.mucas size: 3.47%  (934 bytes)
zlib(input): 4.93%  (1325 bytes)
μCAS beats zlib by 29.5%
```

500 NDJSON rows `{"id": N, "status": "active", "region": "us-east"}`.
`detect_scan_json` builds a template that preserves the exact whitespace around `:` and
`,` using position-tracked structural bytes from the first row.  The template emits
`"status"` and `"region"` as fixed LITs; only `id` varies (NEXT).

---

### synthetic_quoted.csv — Quoted CSV (RFC-4180)

```
Input size:  18890 bytes
Data class:  StructuredLog
Synth path:  lz-first
LZ ratio:    36.23%  (6844 bytes)
Synth ratio: 36.23%  (6844 bytes)  [gain +0.00%]
.mucas size: 4.36%  (824 bytes)
zlib(input): 12.65%  (2390 bytes)
μCAS beats zlib by 65.5%
```

Each row: `"Smith N, John",fixed_category,N+100`.  The quoted first field contains a
comma; `count_delimiters_quoted` correctly counts only the 2 structural commas.  Since
both the quoted field and the integer are variable, the synthesizer cannot extract a
more compact template than raw LZ — the lz-first path wins.  The +65.5% gain is again
the format-layer effect: the LZ program is far more regular than the original file.

---

## Summary table

| File | Input | Class | Path | Synth gain | .mucas | zlib | **vs zlib** |
|------|-------|-------|------|-----------|--------|------|-------------|
| synthetic_records.csv | 13.8 KB | StructuredLog | hybrid | +4.14% | 1,634 B | 1,852 B | **+11.8%** |
| synthetic_records_1k.csv | 71.0 KB | StructuredLog | hybrid | +1.18% | 7,905 B | 8,963 B | **+11.8%** |
| synthetic_records.tsv | 41.0 KB | StructuredLog | hybrid | +7.19% | 4,136 B | 4,967 B | **+16.7%** |
| access_log.pipe | 25.5 KB | StructuredLog | lz-first | 0% | 175 B | 1,284 B | **+86.4%** |
| synthetic_log.txt | 28.0 KB | StructuredLog | hybrid | +1.54% | 87 B | 165 B | **+47.3%** |
| syslog_varied.txt | 31.0 KB | StructuredLog | lz-first | 0% | 421 B | 1,732 B | **+75.7%** |
| sa_ideal.txt | 6.5 KB | StructuredLog | hybrid | +1.30% | 300 B | 328 B | **+8.5%** |
| synthetic_ndjson.ndjson | 26.9 KB | **JsonArray** | hybrid | +5.33% | 934 B | 1,325 B | **+29.5%** |
| synthetic_quoted.csv | 18.9 KB | StructuredLog | lz-first | 0% | 824 B | 2,390 B | **+65.5%** |

μCAS beats zlib on every structured-data file in the corpus.  Gains range from +8.5%
on block-repetition data to +86.4% on highly regular log files.

## Reproducing results

```sh
git clone https://github.com/your-org/mucas-rs
cd mucas-rs
cargo build --release

# Run one benchmark
./target/release/mucas bench bench/data/access_log.pipe

# Run all
for f in bench/data/*; do ./target/release/mucas bench "$f"; done
```

Test data generation scripts (Python):

```sh
python bench/gen/gen_csv.py          # synthetic_records.csv, synthetic_records_1k.csv
python bench/gen/gen_tsv.py          # synthetic_records.tsv
python bench/gen/gen_pipe_log.py     # access_log.pipe
python bench/gen/gen_log.py          # synthetic_log.txt, syslog_varied.txt
python bench/gen/gen_sa_ideal.py     # sa_ideal.txt
python bench/gen/gen_ndjson.py       # synthetic_ndjson.ndjson
python bench/gen/gen_quoted_csv.py   # synthetic_quoted.csv
```

## Design decisions

### Why two compression passes?

The synthesized μCAS program has much lower entropy than the raw input.  LOOP and SCAN
instructions replace hundreds of duplicate bytes with a handful of opcodes and LEB128
counts.  The residual LIT tokens and CPY offsets are already "pre-sorted" into a more
regular structure.  Zlib then achieves far higher ratios on this already-structured
program than it could on the raw bytes.

### Why not just use zstd?

zstd can match or beat μCAS on many files — but only when trained on a reference
dictionary (which requires a corpus, offline pre-computation, and per-domain setup).
μCAS performs structural analysis per-file, requires no training, and is especially
effective on novel record schemas that a generic dictionary would not cover.

### What is "synth gain = 0%" with high vs-zlib?

The synthesizer output (in raw program bytes) is no smaller than the plain LZ program —
meaning no structural pattern was profitably extracted.  But the LZ program itself
(CPY offsets + short LIT residuals) is far more compressible by the format-layer zlib
than the raw file.  This is the "two-level" effect: structure helps even when it isn't
explicitly encoded in SCAN/LOOP instructions.

### MDL as the correctness invariant

Every rewrite is gated by a minimum-description-length check:
`program_bytes_after < program_bytes_before`.  This guarantees the synthesizer can
never increase the encoded program size.  It also means that "synth gain = 0%" is a
correct and safe outcome — not a failure.
