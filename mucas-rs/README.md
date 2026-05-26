# μCAS — Structure-Aware Adaptive Compressor

μCAS is a compression system that **understands the structure of your data** before
compressing it.  Where general-purpose compressors (zlib, zstd) treat every file as
an opaque byte stream, μCAS first identifies structural patterns — repeated CSV rows,
NDJSON objects with fixed keys, periodic log lines — and encodes them as compact
parameterized templates.  The resulting program is then compressed by zlib as a second
pass, yielding sizes that general compressors cannot reach without semantic knowledge.

## Results at a glance

| File type | Example | μCAS vs zlib |
|-----------|---------|-------------|
| CSV (fixed columns) | database exports | **+11–17%** smaller |
| CSV (quoted fields) | address books | **+65%** smaller |
| TSV | spreadsheet exports | **+17%** smaller |
| Pipe-delimited log | nginx access log | **+86%** smaller |
| Space-separated log | syslog | **+76%** smaller |
| NDJSON | API responses | **+30%** smaller |
| Identical log lines | batch job output | **+47%** smaller |

All results are reproducible with the test suite in `bench/`.  See
[BENCHMARK.md](BENCHMARK.md) for methodology and full tables.

## How it works

μCAS compiles your file into a tiny **μCAS program** — a sequence of instructions
executed by a deterministic VM that reconstructs the original bytes exactly.

```
Input file
   │
   ▼
┌──────────────────────────────────────┐
│  Classifier                          │  ← detects: CSV / JSON / log / binary
│    ↓                                 │
│  PatternSynthesizer (Phase 0–2)      │
│    Phase 0  SCAN  ← CSV / NDJSON     │  ← finds parameterized row templates
│    Phase 1  LOOP  ← periodic data    │  ← finds exact repetitions
│             Macro ← repeated blocks  │  ← Rabin-Karp + suffix-array search
│    Phase 2  MAP   ← delta sequences  │  ← arithmetic / timestamp columns
│    ↓                                 │
│  LZ compensation (per-residual)      │  ← back-references in residual bytes
└──────────────────────────────────────┘
   │
   ▼
μCAS program (instruction stream + subroutine table)
   │
   ▼
MucasFile = zlib( program )   ← format-layer entropy coding
```

The VM has 10 opcodes: `LIT CPY LOOP CALL MAP REF ELIT SCAN NEXT HALT`.
SCAN is the key structural instruction — it executes a template subroutine N times,
pulling variable fields from a compact parameter stream per iteration.

## Quick start — pre-built binary

Download the latest release from the
[Releases page](https://github.com/chenpipi0807/mucas/releases):

| Platform | File |
|----------|------|
| Linux x86_64 (static) | `mucas-x86_64-unknown-linux-musl` |
| macOS Intel | `mucas-x86_64-apple-darwin` |
| macOS Apple Silicon | `mucas-aarch64-apple-darwin` |
| Windows x86_64 | `mucas-x86_64-pc-windows-msvc.exe` |

```sh
# macOS / Linux — make executable once
chmod +x mucas-*

# Pack a directory
./mucas pack  my_folder/  -o archive.mcar

# Unpack
./mucas unpack  archive.mcar  -o restored/

# List contents (no extraction)
./mucas list  archive.mcar

# Verify integrity
./mucas check  archive.mcar
```

## Build from source

```sh
git clone https://github.com/chenpipi0807/mucas
cd mucas/mucas-rs
cargo build --release
# binary: target/release/mucas
```

Requires Rust 1.70+.  No system dependencies beyond the standard library,
[flate2](https://crates.io/crates/flate2) (zlib bindings), and
[indicatif](https://crates.io/crates/indicatif) (progress bars).

## CLI usage

```sh
# --- Archive commands ---

# Pack a directory into a .mcar archive
mucas pack    my_folder/         -o archive.mcar

# Pack with a custom memory limit (default: 256 MiB)
mucas pack    my_folder/  --max-memory 512  -o archive.mcar

# Unpack to a directory
mucas unpack  archive.mcar       -o restored/

# List archive contents (path, method, sizes)
mucas list    archive.mcar

# Verify archive integrity without extracting
mucas check   archive.mcar

# --- Single-file commands ---

# Compress a single file
mucas compress   input.csv       output.mucas

# Decompress
mucas decompress output.mucas    restored.csv

# Benchmark (shows data class, compression path, ratio vs zlib)
mucas bench      input.csv
```

### Example benchmark output

```
File:        access_log.pipe
Input size:  25500 bytes
Data class:  StructuredLog
Synth path:  lz-first
──────────────────────────────────────────────────
LZ ratio:    13.88%  (3540 bytes)
Synth ratio: 13.88%  (3540 bytes)  [gain +0.00%]
──────────────────────────────────────────────────
.mucas size: 0.69%  (175 bytes)
zlib(input): 5.04%  (1284 bytes)
──────────────────────────────────────────────────
μCAS beats zlib by 86.4%
Round-trip:  OK ✓
```

## Library usage

```rust
use mucas::pipeline::Pipeline;
use mucas::format::{MucasFile, compress_zlib};
use mucas::{VmState, Consensus};

// Compress
let data = std::fs::read("records.csv")?;
let (prog, _class) = Pipeline::new().compress(&data);
let (prog_bytes, subs) = prog.to_bytes();
let encoded = MucasFile::new(prog_bytes, subs).encode();

// Decompress
let file = MucasFile::decode(&encoded)?;
let mut vm = VmState::new();
vm.exec(&file.program, &file.subs, &Consensus::new())?;
assert_eq!(vm.output, data);
```

## Supported data formats

| Format | Detection | Notes |
|--------|-----------|-------|
| CSV (plain) | `detect_scan_lit(b',')` | fixed + variable columns |
| CSV (RFC-4180 quoted) | `detect_scan_lit(b',')` | `""` escape, commas inside quotes |
| TSV | `detect_scan_lit(b'\t')` | |
| Pipe-delimited | `detect_scan_lit(b'|')` | |
| NDJSON | `detect_scan_json` | preserves exact whitespace; structurally-uniform rows |
| Compact JSON array | `detect_scan_json` | `[{…},\n{…}]` form |
| Periodic log lines | `detect_loop` | exact repetition; LOOP instruction |
| Long repeated blocks | `find_long_macro_by_sa` | suffix-array search, patterns 65–1024 B |
| Delta sequences | `estimate_delta_u8_gain` | arithmetic / timestamp columns via MAP |

## Data classification

μCAS automatically classifies each input before choosing a compression strategy:

| Class | Criteria | Strategy |
|-------|----------|----------|
| `JsonArray` | starts with `{` or `[{` | full synthesis + JSON SCAN priority |
| `StructuredLog` | high line similarity + low literal fraction | full synthesis |
| `SemiStructured` | default (JSON/XML/config) | full synthesis |
| `UnstructuredText` | high literal fraction + short matches | MAP only |
| `Binary` | entropy > 7.5 bits/byte + non-UTF-8 | LZ only |
| `AlreadyCompressed` | magic bytes: JPEG, PNG, MP4, ZIP, 7z, RAR, gzip, … | Store |

The `AlreadyCompressed` check runs before entropy analysis, so image and media
files are never wasted CPU cycles attempting re-compression.

## Architecture notes

- **No allocation on the hot decompression path**: the VM is a simple byte-level interpreter
  with a single output buffer and a param-stream stack for nested SCAN calls.
- **MDL-guided synthesis**: every rewrite (LOOP, Macro, SCAN) is accepted only when
  `encoded_after < encoded_before` in raw program bytes, before the zlib pass.
  This prevents the synthesizer from ever making things worse.
- **Pipeline competition**: both the hybrid path (structural synthesis first, LZ on residuals)
  and the LZ-first path are always evaluated; the smaller program wins.

## Versioning

| Version | Key addition |
|---------|-------------|
| v0.1 | LIT/CPY/MAP/LOOP/CALL/REF VM + basic synthesizer |
| v0.2 | Raw-first hybrid pipeline |
| v0.3 | ELIT, UTF-8 fix |
| v0.4 | Rabin-Karp macro search; SCAN + NEXT opcodes |
| v0.5 | Suffix-array macro extractor (patterns 65–1024 B) |
| v0.6 | Multi-delimiter SCAN (CSV / TSV / pipe) |
| v0.7 | Quoted CSV (RFC-4180); NDJSON SCAN; `DataClass::JsonArray` |
| v0.8 | `AlreadyCompressed` detection; MCAR multi-file archive format; rayon parallel compression |
| v0.9 | Streaming `ArchiveWriter`/`ArchiveReader` (constant memory); MDL-based method selection; `pack`/`unpack`/`list`/`check` CLI; `indicatif` progress bars; GitHub Actions CI/CD |

## License

MIT
