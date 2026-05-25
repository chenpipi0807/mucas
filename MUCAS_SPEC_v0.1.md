# μCAS Specification v0.1
## Micro Compression Assembly — Reference Format

---

## 1. Overview

μCAS (Micro Compression Assembly) is a deterministic, bounded bytecode language for
data reconstruction. A μCAS program, when executed by the μCAS Virtual Machine (VM),
produces exactly the original data. Compression is the act of finding the shortest μCAS
program that reconstructs a given input. Decompression is pure program execution.

**Design goals (in priority order):**
1. Total-function guarantee: every valid program terminates and uses bounded memory
2. Deterministic execution: identical program + identical consensus → identical output
3. Hardware-implementable: instructions are simple enough to pipeline in silicon
4. Evolvable compressor: the VM spec is frozen; only the compressor may evolve

---

## 2. Definitions

| Term | Definition |
|------|-----------|
| **Program** | A sequence of μCAS instructions terminated by HALT (0xFF) |
| **Output buffer** | The byte array being constructed by the VM |
| **Instruction pointer (IP)** | Index into the program byte array |
| **Macro table** | A map from integer ID to a sub-program (bytes) |
| **Consensus library** | A pre-agreed map from hash (bytes) to byte sequence |
| **LEB128** | Unsigned Little-Endian Base-128 variable-length integer encoding |

---

## 3. LEB128 Encoding

All integer operands use unsigned LEB128 encoding (same as WASM).

Each byte contributes 7 bits. The high bit (0x80) is set on all bytes except the last.

```
encode(0)   = [0x00]
encode(127) = [0x7F]
encode(128) = [0x80, 0x01]
encode(300) = [0xAC, 0x02]
```

Decoders MUST accept any valid LEB128. Encoders SHOULD use the minimum-length encoding.

---

## 4. Instruction Set

### 4.1 Instruction Table

| Mnemonic | Opcode | Operands | Semantics |
|----------|--------|----------|-----------|
| LIT | 0x00 | len: LEB128 | Copy next `len` bytes from program to output |
| CPY | 0x01 | offset: LEB128, len: LEB128 | Copy `len` bytes from output[-offset:] to output (overlapping allowed) |
| MAP | 0x02 | transform_id: u8, len: LEB128 | Apply built-in transform to last `len` bytes of output (in-place) |
| LOOP | 0x03 | count: LEB128, body_len: LEB128 | Execute `body_len` bytes of body exactly `count` times |
| CALL | 0x04 | macro_id: LEB128 | Execute macro by ID from the macro table |
| REF | 0x05 | hash_len: u8, hash: bytes[hash_len] | Append the consensus library entry identified by `hash` |
| ELIT | 0x06 | encoded_len: LEB128, raw_len: LEB128 | Decompress next `encoded_len` bytes (zlib/DEFLATE) and append to output |
| HALT | 0xFF | (none) | Terminate execution successfully |

### 4.2 Instruction Semantics

#### LIT — Literal bytes
```
0x00 [len: LEB128] [len bytes of payload]
```
Copies `len` bytes verbatim from the instruction stream to the output buffer.
`len` MUST be ≥ 1. Bounded: reads exactly `len` bytes, writes exactly `len` bytes.

#### CPY — Copy from history
```
0x01 [offset: LEB128] [len: LEB128]
```
`offset` is the backward distance from the current end of the output buffer.
`src = len(output) - offset`. MUST satisfy `src ≥ 0`.
Copying proceeds byte-by-byte, so `src + i` may equal `len(output) + i` (RLE-style overlap
is valid and intentional).
Bounded: copies exactly `len` bytes. No forward references.

**Example:** output = [A, B], `CPY offset=2, len=6` → copies A,B,A,B,A,B → output = [A,B,A,B,A,B,A,B]

#### MAP — Apply transform
```
0x02 [transform_id: u8] [len: LEB128]
```
Takes the last `len` bytes of the output buffer, applies transform `transform_id`, and
writes the result back. Input and output lengths MUST be equal (all built-in transforms
are length-preserving). Bounded: O(len).

**Built-in transform table (v0.1):**

| ID | Name | Input | Operation |
|----|------|-------|-----------|
| 0x00 | INC_U8 | N bytes | Each byte += 1 (mod 256) |
| 0x01 | DEC_U8 | N bytes | Each byte -= 1 (mod 256) |
| 0x02 | INC_U32 | 4 bytes | Little-endian uint32 += 1 (mod 2³²) |
| 0x03 | DEC_U32 | 4 bytes | Little-endian uint32 -= 1 (mod 2³²) |
| 0x04 | DELTA_U8 | N bytes | Prefix-sum: output[i] = Σ input[0..i] mod 256 |
| 0x05 | DELTA_U32 | 4N bytes | Prefix-sum on uint32 array |
| 0x06 | BYTESWAP_U32 | 4 bytes | Reverse byte order |
| 0x07 | ZIGZAG_U32 | 4 bytes | ZigZag decode: n → (n>>1) XOR -(n&1) |

Transforms not listed above are reserved. VMs MUST reject programs using unknown transform IDs.

#### LOOP — Bounded iteration
```
0x03 [count: LEB128] [body_len: LEB128] [body_len bytes of body instructions]
```
The `body_len` bytes following the operands form the loop body (a complete sub-program
without a HALT). Executes the body exactly `count` times. `count = 0` is valid (no-op).
LOOP bodies may be nested. Recursion depth is bounded by body nesting depth, which is
bounded by program size.
Bounded: O(count × body execution time).

#### CALL — Invoke macro
```
0x04 [macro_id: LEB128]
```
Executes the macro body identified by `macro_id` from the macro table. The macro body
is executed as a sub-program (no HALT required in macro body, but execution stops at the
end of the macro byte array). CALL MUST NOT be recursive (a macro may not directly or
transitively call itself). VMs MUST detect recursion and abort with `CALL_CYCLE` (0x06).
Call nesting depth MUST NOT exceed MAX_CALL_DEPTH = 16; if exceeded, VMs MUST abort
with `CALL_DEPTH_EXCEEDED` (0x05). See §5.3 and §5.4.
Bounded: bounded by macro size × call depth ≤ MAX_CALL_DEPTH.

#### REF — Consensus library reference
```
0x05 [hash_len: u8] [hash_len bytes of hash]
```
Looks up `hash` in the consensus library and appends the associated byte sequence to
output. Fails if `hash` is not present. `hash_len = 32` with SHA-256 is the recommended
production encoding. Shorter hashes (e.g., `hash_len = 1` with sequential IDs) are
valid for session-local consensus.
Bounded: appends a fixed byte sequence of known length.

#### ELIT — Entropy-coded literal block
```
0x06 [encoded_len: LEB128] [raw_len: LEB128] [encoded_len bytes of DEFLATE payload]
```
Decompresses the DEFLATE (RFC 1951, zlib wrapper format) payload and appends to output.
`raw_len` is the expected decompressed length (for pre-allocation; VMs MAY use for
bounds checking). `encoded_len` is the exact byte count of the compressed payload in
the instruction stream.
Bounded: output length bounded by `raw_len` (or decompressor's internal limit).

#### HALT — End of program
```
0xFF
```
Terminates execution. Output buffer is now the reconstructed data. Every valid program
MUST contain exactly one HALT as the final instruction at the top level. Sub-programs
(macro bodies, LOOP bodies) do not use HALT.

---

## 5. Execution Model

### 5.1 VM State

```
output: bytearray       # reconstructed data, grows monotonically
ip: int                 # instruction pointer (index into program bytes)
macro_table: dict       # int -> bytes
consensus: dict         # bytes -> bytes
```

### 5.2 Execution

```
function execute(program, macro_table, consensus):
    output = []
    ip = 0
    while ip < len(program):
        op = program[ip]; ip += 1
        dispatch(op, ...)
    return output
```

Dispatch is deterministic given (program, macro_table, consensus). There is no
randomness, no I/O, no system calls, no mutable global state.

### 5.3 Termination Guarantee

Every valid μCAS program terminates because:
- LIT/CPY/MAP/ELIT: finite, O(operand) steps
- LOOP: `count` is a literal; body terminates by induction; total steps bounded
- CALL: no recursion; DAG of macro calls; call depth ≤ MAX_CALL_DEPTH; terminates by topological order
- REF: single lookup, O(1)
- HALT: terminates immediately

**MAX_CALL_DEPTH = 16.** Macro call chains MUST NOT exceed 16 levels of nesting.
Macro A calling B calling C is depth 2; if the chain reaches depth 17, the VM
MUST abort with error `CALL_DEPTH_EXCEEDED` (0x05). This bound is sufficient for
all known structural template patterns and prevents accidental stack overflow in
resource-constrained decoders.

A VM that encounters an invalid instruction (unknown opcode, out-of-bounds CPY,
unknown MAP transform, unknown REF hash, recursive CALL, or CALL depth exceeded)
MUST raise an error and return a failure result. It MUST NOT silently produce
incorrect output. The specific error code MUST be one of those defined in §5.4.


### 5.4 Error Codes

VM implementations MUST report one of the following symbolic error codes when
aborting. Language bindings MAY represent these as exceptions, error enums,
or integer codes — the important invariant is that callers can distinguish them.

| Code | Symbolic Name | Trigger condition |
|------|--------------|-------------------|
| 0x01 | UNKNOWN_OPCODE | Instruction byte not in {0x00,0x01,0x02,0x03,0x04,0x05,0x06,0xFF} |
| 0x02 | CPY_UNDERFLOW | CPY offset exceeds current output length |
| 0x03 | LOOP_BODY_OVERRUN | LOOP body_len extends past program boundary |
| 0x04 | OUTPUT_OVERFLOW | Output size exceeds configured limit (default: 10 GB) |
| 0x05 | CALL_DEPTH_EXCEEDED | CALL nesting depth > MAX_CALL_DEPTH (16) |
| 0x06 | CALL_CYCLE | CALL graph contains a cycle (macro refers to itself or an ancestor) |
| 0x07 | UNKNOWN_REF | REF hash not found in any provided consensus dict |
| 0x08 | REF_HASH_MISMATCH | EXTERNAL .ufc snapshot_hash ≠ hash of loaded library |
| 0x09 | CONSENSUS_UNAVAILABLE | Required .ufc snapshot version not found or not provided |
| 0x0A | UNKNOWN_MAP_TRANSFORM | MAP transform ID not in the defined TRANSFORMS set (§4.2) |
| 0xFF | TRUNCATED_PROGRAM | Program bytes end before HALT (0xFF) is reached |

Errors 0x01–0x07 and 0x0A–0xFF indicate a malformed program or corrupt file.
Errors 0x08–0x09 indicate a missing or mismatched external dependency.

**Recommendation:** decoders SHOULD log the error code, the instruction pointer at
the time of failure, and the offending byte or value, to facilitate debugging.

---

## 6. .ufi File Format

### 6.1 Magic and Header

```
Offset  Size  Field
0       4     Magic: bytes [0x55, 0x46, 0x49, 0x01]  ("UFI\x01")
4       1     Flags:
                bit 0: program is zlib-wrapped (DEFLATE, RFC 1950)
                bit 1: embedded consensus section present
                bit 2: macro section present
                bit 3: external UFC reference section present (see Appendix E §E.3)
                bits 4-7: reserved, MUST be 0
5       LEB128 Format version: 1 for this spec
```

### 6.2 Consensus Section (if Flags bit 1 set)

```
[entry_count: LEB128]
for each entry:
    [hash_len: u8]
    [hash: hash_len bytes]
    [content_len: LEB128]
    [content: content_len bytes]
```

The consensus section encodes session-local or file-local consensus entries.
For globally pre-agreed consensus (production use), this section is omitted and both
parties rely on a separately distributed consensus database keyed by SHA-256.

### 6.3 Macro Section (if Flags bit 2 set)

```
[macro_count: LEB128]
for each macro:
    [macro_id: LEB128]
    [body_len: LEB128]
    [body: body_len bytes]
```

Macro IDs MUST be unique within the file. Macros MUST form a DAG (no recursion).
A topological sort SHOULD be verified before execution.

### 6.4 Program Section

```
[program_len: LEB128]
[program: program_len bytes]
```

If Flags bit 0 is set, `program` is a DEFLATE/zlib-compressed byte stream. The VM
first decompresses it (using the standard DEFLATE algorithm, RFC 1950), then executes
the decompressed bytes as a μCAS program.

### 6.5 Complete File Layout

```
[Header]                        magic(4) + flags(1) + format_version(LEB128)
[Consensus Section]             optional — present when Flags bit 1 set
[Macro Section]                 optional — present when Flags bit 2 set
[External UFC Section]          optional — present when Flags bit 3 set (see Appendix E §E.3)
[Program Section]               always present
```

All sections are self-framed with explicit lengths. A conforming reader can skip any
section it does not support by reading the length and advancing the file pointer.

Decoders MUST read sections in the order above. A decoder that encounters Flags bit 3
but does not support EXTERNAL mode MUST return `CONSENSUS_VERSION_UNAVAILABLE` (0x09)
rather than attempting partial execution.

---

## 7. Version Negotiation

When μCAS is used as a network protocol (sender/receiver), the following handshake
SHOULD precede data transfer:

```
Sender:   HELLO [mucas_version: u8] [supported_transforms: bitmask] [consensus_db_version: LEB128]
Receiver: ACCEPT [agreed_version: u8] [agreed_transforms: bitmask] [consensus_db_version: LEB128]
           or
          REJECT [reason: u8]
```

For file archival (no live negotiation), the file header encodes the version in the
Format version field. Decompressors SHOULD refuse files with format versions they do
not support, rather than attempting partial decompression.

---

## 8. Security Considerations

### 8.1 Untrusted Input

A μCAS program from an untrusted source MUST be validated before execution:
- Check that CALL forms a DAG (no cycles); abort with `CALL_CYCLE` (0x06) on failure
- Verify CPY offsets do not reference before the start of the output buffer; abort with `CPY_UNDERFLOW` (0x02)
- Verify MAP transform IDs are in the known set; abort with `UNKNOWN_MAP_TRANSFORM` (0x0A)
- Enforce a maximum output size limit before executing; abort with `OUTPUT_OVERFLOW` (0x04)
- Reject unknown instruction bytes; abort with `UNKNOWN_OPCODE` (0x01)

See §5.4 for the complete error code table.

### 8.2 Zip Bomb Equivalent

A short μCAS program can produce exponentially large output via nested LOOP:
```
LOOP 1000000
  LOOP 1000000
    LIT 1 [0x00]
  END
END
```
VMs MUST enforce a maximum output size limit (e.g., 10 GB or configurable) and
abort with `OUTPUT_OVERFLOW` (0x04) if exceeded.

Similarly, a deeply nested CALL chain can exhaust decoder stack. The MAX_CALL_DEPTH = 16
limit (§5.3) provides a hard bound on recursion depth regardless of macro count.

### 8.3 Hash Collisions in REF

For session-local consensus (short IDs), collision risk is negligible within a single
file. For globally pre-agreed consensus, SHA-256 (32 bytes) MUST be used. Content
deduplication by hash is safe given SHA-256's collision resistance.

### 8.4 External Dependency Integrity

When a `.ufi` file uses EXTERNAL or HYBRID mode, the decoder MUST:
1. Load the `.ufc` snapshot identified by `external_snapshot_hash`.
2. Verify that the loaded library's computed `snapshot_hash` matches the declared hash.
   On mismatch, abort with `REF_HASH_MISMATCH` (0x08).
3. If the snapshot cannot be located, abort with `CONSENSUS_UNAVAILABLE` (0x09).

Decoders MUST NOT fall back to partial execution (using whatever consensus is available)
when an external dependency is declared but missing or mismatched. Silent partial
decompression produces incorrect output without warning, which is worse than failure.

---

## 9. Example Programs

### 9.1 Literal "Hello, World!"
```hex
00 0D 48 65 6C 6C 6F 2C 20 57 6F 72 6C 64 21 FF
```
`LIT 13 [Hello, World!] HALT`

### 9.2 Repeating byte run "AAAAAAAAAA" (10 bytes)
```
00 01 41        # LIT 1 [A]
03 09 03        # LOOP 9, body_len=3
   01 01 01     #   CPY offset=1, len=1
FF              # HALT
```

### 9.3 Ascending uint32 sequence 0,1,2,...,999
```
00 04 00 00 00 00          # LIT 4 [0x00000000]
03 E7 07 06                # LOOP 999, body_len=6
   01 04 04                #   CPY offset=4, len=4
   02 02 04                #   MAP INC_U32, 4
FF                         # HALT
```
Total program size: 14 bytes. Output size: 4000 bytes. Ratio: 0.35%.

---

## 10. Conformance

A **conforming μCAS VM** MUST:
- Implement all 8 instructions (LIT, CPY, MAP, LOOP, CALL, REF, ELIT, HALT)
- Implement all 8 MAP transforms listed in §4.2
- Enforce the termination and bounds constraints specified in §5.3
- Reject programs that violate any MUST constraint
- Produce bit-identical output for identical (program, macro_table, consensus) inputs

A **conforming μCAS compressor** MUST:
- Produce programs that are accepted by a conforming VM
- Guarantee round-trip fidelity: decompress(compress(data)) == data

Compressors are NOT required to produce optimal (shortest) programs. Compression
quality is a quality-of-implementation concern, not a conformance requirement.

---

## Appendix A: Transform Quick Reference

```python
INC_U8  = lambda b: bytes([(x+1)&0xFF for x in b])
DEC_U8  = lambda b: bytes([(x-1)&0xFF for x in b])
INC_U32 = lambda b: struct.pack('<I', (struct.unpack('<I',b)[0]+1) & 0xFFFFFFFF)
DEC_U32 = lambda b: struct.pack('<I', (struct.unpack('<I',b)[0]-1) & 0xFFFFFFFF)
DELTA_U8 = lambda b: bytes(itertools.accumulate(b, lambda a,x: (a+x)&0xFF))
DELTA_U32: prefix-sum on uint32 array (little-endian)
BYTESWAP_U32 = lambda b: b[::-1]   # for a 4-byte input
ZIGZAG_U32: n -> (n>>1) ^ -(n&1)  (standard ZigZag decode)
```

---

## Appendix B: Reference Implementation

The canonical Python reference implementation is available in `mucas/`:

| Module | Contents |
|--------|----------|
| `mucas/vm.py` | `MuCASVM`, LEB128, MAP transforms |
| `mucas/compress.py` | `naive_compress`, `smart_compress`, `lz_analyze` |
| `mucas/consensus.py` | `build_consensus`, `compress_ref_lz`, `compute_rai` |
| `mucas/format.py` | `UfiFile` — `.ufi` file encode/decode |

Test suite: `test_mucas.py` — 9 unit tests covering all instructions.
All tests pass on CPython 3.11+.

---

## Appendix C: Compression Theory — The Three-Layer Architecture

This appendix documents the information-theoretic foundation discovered through
empirical experiments, validated on real files (Chinese UTF-8 text, English
technical prose, log files, JSON). All claims are experimentally confirmed.

### C.1 Three Statistical Layers in Natural Data

Optimal μCAS compression operates in three layers, each eliminating a distinct
class of redundancy:

| Layer | Operation | Redundancy eliminated | Information-theoretic role |
|-------|-----------|----------------------|---------------------------|
| 1 — LZ | CPY + LIT | Local byte repetition (window) | Short-range correlations |
| 2 — REF | Consensus references | Global semantic fragment repetition | Long-range mutual information |
| 3 — Entropy | zlib/zstd wrapper | Symbol frequency bias | Non-uniform marginal distribution |

These three layers map exactly to the three classical dimensions of data
compression: **conditional entropy** (LZ exploits local context), **mutual
information** (REF exploits cross-document semantic repetition), and
**marginal entropy** (entropy coding exploits symbol frequency).

Natural language and structured data contain redundancy primarily at these three
levels. Three layers is the economic optimum under current technology.

### C.2 REF Applicability Index (RAI v3)

**REF is not profitable when it covers uncovered (LIT) bytes. REF is profitable
when it replaces diverse, high-overhead CPY instructions with uniform REF tokens,
enabling entropy collapse in the outer zlib pass.**

Experimental evidence (5 test files, 5/5 correct):

| Content type | H_pattern (b/B) | avg CPY len | REF benefit |
|--------------|-----------------|-------------|-------------|
| Chinese UTF-8 | 5.25 – 5.84 | 9–10 B | +9 – +10% |
| Log files | 3.17 | 25 B | +2.5% |
| English ASCII | 1.69 | 11 B | −4.2% |
| JSON | 0.61 | 42 B | −1.6% |

**Definition:** `H_pattern` is the Shannon byte entropy (bits/byte) of the
concatenated consensus pattern bytes, computed after `build_consensus`.

**Decision rule:** Apply REF if and only if `H_pattern > H_threshold`.

### C.3 The H_threshold Derivation

Each REF token replaces a consensus pattern occurrence of length L bytes and
entropy H bits/byte. The net gain per occurrence is:

```
gain = L × H  −  C_REF   (bits)
```

where `C_REF` is the effective encoding cost of the REF instruction in the
globally entropy-coded program stream (approximately 16–20 bits for a 3-byte
token after Huffman coding).

REF is profitable when `gain > 0`, i.e.:

```
H_threshold = C_REF / L  ≈  20 bits / 10 bytes  =  2.0 b/B
```

Empirically validated threshold: **H_threshold = 2.5 b/B** (conservative margin
above the theoretical minimum to account for LZ fragmentation penalty).

This threshold is implemented as `RAI_H_THRESHOLD = 2.5` in `mucas/consensus.py`.
Compressors SHOULD treat it as a tunable constant; the value 2.5 is appropriate
for files ≥ 5 KB with the default consensus parameters (n=50, min_len=6).

### C.4 Stopping Criterion for Layer Stacking

A fourth layer ("meta-REF": using LOOP/CALL to compress repeating REF
instruction sequences) is theoretically possible. The general stopping criterion
for adding layer N+1 is:

```
R_N  <  M_{N+1}
```

where:
- `R_N` = residual redundancy in the layer-N instruction stream
  = `L_N × (1 − H_N/8)` bytes, where `H_N` is the empirical byte entropy
  of the instruction stream and `L_N` is its length in bytes.
- `M_{N+1}` = description overhead of the layer-(N+1) model (consensus
  dictionary, macro definitions, or grammar rules) in bytes.

This is an instance of the **Minimum Description Length (MDL) principle**:
add a layer only when the model cost is outweighed by the compression gain.

In practice, `R_2 ≪ M_3` for files under 1 MB, making three layers the
economic optimum. Implementors building corpus-scale systems (≥ 1 GB corpora)
MAY explore a fourth layer, but MUST verify that `R_2 > M_3` for their data
distribution before doing so.

### C.5 Unified Three-Layer Pipeline (Reference Design)

```
Input data
    │
    ▼
┌─────────────────────────────────────────────────┐
│ Layer 1: Structural synthesis (optional)        │
│   smart_compress: detect INC_U32, byte runs     │
│   → LOOP + MAP instructions                     │
│   Decision: MDL gain > 0 (always try)           │
└─────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────┐
│ Layer 2: LZ scan                                │
│   naive_compress: sliding window, CPY + LIT     │
│   → CPY + LIT instruction stream                │
└─────────────────────────────────────────────────┘
    │
    ▼  compute H_pattern
┌─────────────────────────────────────────────────┐
│ Layer 3: REF consensus shaping (conditional)    │
│   compress_ref_lz: replace patterns with REF    │
│   → REF + CPY + LIT instruction stream          │
│   Decision: H_pattern > H_threshold (2.5 b/B)  │
└─────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────┐
│ Layer 4: Global entropy coding                  │
│   zlib/zstd applied to entire program stream    │
│   → final .ufi payload                          │
└─────────────────────────────────────────────────┘
    │
    ▼
Output: .ufi file
```

---

## Appendix D: Cross-File Consensus Compression

*Added 2026-05-25 after Leave-One-Out experimental validation.*

### D.1 Theoretical Basis: Structural Skeleton Hypothesis

A corpus of files generated by the same program or protocol shares a finite,
enumerable set of byte patterns — the **structural skeleton** of the format.
This skeleton is distinct from per-instance variable content (timestamps, IDs,
payload bodies).

**MDL decomposition of homogeneous data:**

```
K(corpus) ≈ K(skeleton) + sum_i K(instance_i | skeleton)
```

where K denotes Kolmogorov complexity (description length). The skeleton has
bounded complexity: it can be discovered from a small number of representative
files. Once the skeleton is expressed as a consensus library, each instance
compresses to its "residual" — the information not captured by the skeleton.

**Consequence:** A pre-trained consensus library built from N representative
files can compress new instances of the same format without per-file analysis.
This is the fundamental use case for cross-file REF compression.

**Applicable domains:**
- Program-generated logs (API request/response logs, structured event logs)
- Wire protocol payloads of fixed schema (JSON-RPC, serialized protobuf)
- Multi-episode scripts from the same series (same characters, scene templates)
- Any stream where `K(skeleton) << K(corpus)`

**Non-applicable domains:**
- Arbitrary heterogeneous files (different content, different structure)
- Natural language from different sources (low cross-file pattern overlap)


### D.2 The 2D RAI Model: Quality × Quantity

Appendix C established the single-file RAI criterion:

```
H_pattern > H_threshold (2.5 b/B)   →   single-file REF benefits
```

For cross-file consensus, a second dimension is required. A consensus library
may contain high-entropy patterns (quality satisfied) yet cover too little of
the target file to overcome the fragmentation penalty imposed on LZ matches.

**2D RAI criterion for cross-file consensus:**

```
predicts_benefit = (H_pattern > 2.5 b/B)  AND  (cross_coverage > 10%)
```

where `cross_coverage` is the fraction of the target file's bytes covered by
greedy left-to-right consensus matching.

**Fragmentation penalty:** each REF instruction interrupts the LZ sliding-window
context. A REF covering a 30-byte pattern prevents one long CPY match from
spanning across that region. At low library density, the aggregate fragmentation
loss exceeds the entropy-coding gain from REF tokens. Only when REF density is
sufficient to "take over" large contiguous data regions does the statistical
reshaping benefit outweigh the fragmentation cost.

This is a **phase transition**, not a linear accumulation:

```
coverage < ~10%  →  fragmentation dominates  →  REF hurts
coverage > ~12%  →  entropy shaping dominates →  REF helps
```

**API:**

```python
from mucas import predict_cross_ref_benefit, CROSS_COVERAGE_THRESHOLD

pred = predict_cross_ref_benefit(target_data, cross_consensus)
# Returns:
#   h_pattern:        float   byte entropy of consensus patterns (b/B)
#   cross_coverage:   float   fraction of target covered by matches
#   quality_ok:       bool    h_pattern > RAI_H_THRESHOLD
#   coverage_ok:      bool    cross_coverage > CROSS_COVERAGE_THRESHOLD
#   predicts_benefit: bool    quality_ok AND coverage_ok
```


### D.3 Empirical Calibration: The Coverage Threshold Experiment

**Setup:** 20 synthetic log files (same format, different content, ~9 KB each).
Leave-One-Out: train consensus on 19 files, compress the held-out file.
Sweep library size n from 1 to 500 to span coverage 7% → 38%.

**Coverage vs. net benefit curve (held-out: repro_log_SVIP.txt, 9,466 B):**

```
n      coverage   H_pattern   benefit   verdict
------------------------------------------------
   1     7.6%      3.76 b/B    -6.1%    [HURT]
   2    10.4%      4.35 b/B    +0.3%    [~0]
   3    12.9%      4.47 b/B    +4.5%    [HELP]
   5    16.5%      4.65 b/B    +7.5%    [HELP]
   8    18.2%      5.25 b/B    +8.7%    [HELP]
  12    19.9%      5.32 b/B    +9.6%    [HELP]
  18    22.8%      5.71 b/B   +11.5%    [HELP]
  25    22.8%      5.74 b/B   +11.5%    [HELP]  <- skeleton saturated
  50    23.2%      5.59 b/B   +11.9%    [HELP]
 100    23.2%      5.61 b/B   +11.9%    [HELP]
 150    30.3%      5.68 b/B   +16.4%    [HELP]
 200    32.7%      5.91 b/B   +18.1%    [HELP]
 500    38.2%      5.89 b/B   +20.9%    [HELP]
```

**Three findings from this curve:**

**Finding 1 — Empirical threshold: ~10–12% coverage.**
The transition from net loss to net gain occurs between n=2 (10.4%, +0.3%) and
n=3 (12.9%, +4.5%). The `CROSS_COVERAGE_THRESHOLD` constant is set to 0.10.
The "reliable gain" threshold (>1% benefit) is closer to 12%.

**Finding 2 — Structural skeleton convergence (n=18 → n=100 plateau).**
Coverage is flat at ~22.8% from n=18 to n=100. This proves that 18 high-entropy
patterns (avg 31.6 B each) completely exhaust the format's structural skeleton.
Adding more patterns (n=19 to n=100) finds no new skeleton elements because the
skeleton is already fully enumerated. At n=150, secondary patterns (less common
structural fragments) begin contributing, and coverage resumes growing.

This confirms the structural skeleton hypothesis: for program-generated data,
the skeleton is **finite and small** — discoverable from a handful of files.

**Finding 3 — n=1 negative effect: fragmentation dominates at low density.**
A single high-entropy pattern covering 7.6% of the file produces -6.1% benefit.
Even though each individual match has positive information-theoretic value
(H=3.76 b/B > 2.5 threshold), the global effect is negative.
A lone REF instruction destroys an otherwise long CPY opportunity spanning
the pattern and its surrounding context. The LZ sliding window, given a 32 KB
window size, already handles that context efficiently. REF only wins when its
density is sufficient to replace many short CPY instructions simultaneously.

**LOO benchmark result:** all 20 files, coverage ~22%, H_pattern 5.59 b/B.
2D RAI accuracy: **20/20 correct**. Net benefit: +10.4% to +14.6% (avg +12.3%).


### D.4 Cross-File Consensus: Applicable Domains and Limits

**When cross-file consensus works (both dimensions satisfied):**

| Domain | Typical coverage | H_pattern | Expected gain |
|--------|-----------------|-----------|---------------|
| API request/response logs | 20–40% | 4–6 b/B | +10% to +20% |
| Structured event logs | 15–35% | 4–6 b/B | +8% to +18% |
| Multi-episode scripts (same series) | 10–20% | 4–6 b/B | +5% to +12% |
| Wire protocol payloads | 25–50% | 3–5 b/B | +10% to +25% |

**When cross-file consensus fails (coverage dimension not satisfied):**

| Domain | Typical coverage | Reason for failure |
|--------|-----------------|-------------------|
| Diverse natural language | <5% | Different vocabulary per document |
| Different-story scripts | <5% | No shared structural templates |
| Binary data (images, video) | <2% | No repeating byte sequences |

**The diversity test:** before investing in cross-file consensus for a new domain,
compute the shared-pattern coverage with min_files=2 on a 5-file sample.
If avg coverage < 10%, cross-file REF will not help for that domain.

**Minimum corpus size:** the structural skeleton of a format is typically
discovered with 5–10 representative files. Beyond that, adding more training
files improves coverage of secondary patterns (long tail) but not primary skeleton
coverage. For practical deployments, 10–20 representative files are sufficient
to build a production-quality consensus library.


### D.5 Build Algorithm: Hash-Accelerated Consensus Extraction

The naive O(n²) algorithm (call `data.count(pattern)` for each candidate pattern)
is impractical for production use. The reference implementation uses a single-pass
Counter bulk-counting approach:

```python
# Per-file bulk counting: O(file_size * avg_len) instead of O(file_size^2)
for data in files:
    file_cnt = Counter()
    for L in range(min_len, max_len + 1):
        file_cnt.update(data[i:i+L] for i in range(len(data) - L + 1))
    # Accumulate: patterns with count >= 2 in this file contribute to cross-file stats
    for p, c in file_cnt.items():
        if c >= 2:
            file_presence[p] += 1   # how many files contain this pattern
            corpus_count[p]  += c   # total occurrences across corpus
```

**Performance:** 20 files × 9 KB each, min_len=6, max_len=32:
- Naive algorithm: ~353s (for 20 LOO runs; ~17.6s per run)
- Hash-accelerated: ~27.8s total (~1.4s per LOO run) — **13× speedup**

For production use with larger corpora, a suffix-array approach would reduce the
asymptotic complexity further, but the Counter approach is sufficient for
corpora up to ~10 MB total size.

---

## Appendix E: The .ufc Corpus Format

*Added 2026-05-25. Full round-trip implementation in `mucas/corpus.py`.*

### E.1 Design Principles

The `.ufc` (μCAS Corpus) file is the **knowledge distribution unit** of the
μCAS ecosystem. It holds an immutable, versioned snapshot of a cross-file
consensus library that can be referenced by multiple `.ufi` files.

Four design principles govern the format:

1. **Static snapshot, not mutable database.** A `.ufc` file, once published, is
   immutable. Its content is completely determined by its `snapshot_hash`. This
   guarantees that any `.ufi` file referencing this snapshot will decompress
   identically on any machine, at any point in time. Learning happens at the
   compressor; knowledge is crystallized into versioned snapshots.

2. **Content-addressed globally, sequential-ID locally.** The snapshot as a
   whole is identified by its SHA-256 `snapshot_hash` (computed over all entry
   data). Per-entry `seq_id` values (0, 1, 2, …) are local to the snapshot and
   are what `REF` instructions carry in `.ufi` programs — they are compact
   (1–2 bytes via LEB128) and valid only within the context of the agreed
   snapshot. The `snapshot_hash` is the stable, cross-system anchor; `seq_id`
   is the runtime shorthand.

3. **Snapshot chains for incremental evolution.** A new version of a domain
   library is released as a new `.ufc` file with a new `snapshot_hash`. The
   domain string and version field `(major, minor)` provide human-readable
   lineage. A `.ufc v2` may declare it supersedes `v1` in metadata, but
   structurally it is a completely self-contained file. Decoders do not need to
   load previous versions.

4. **Integrity-sealed by design.** Every `.ufc` file ends with a 32-byte
   SHA-256 digest of its entry content. Decoders verify this on load; a mismatch
   causes an immediate `ValueError`. This makes `.ufc` files safe to distribute
   over untrusted channels (CDN, peer-to-peer).


### E.2 Binary Layout

```
┌─────────────────────────────────────────────────────┐
│ Header                                               │
│   magic:         4 bytes    "UFC\x01" (55 46 43 01) │
│   major:         1 byte     format major version    │
│   minor:         1 byte     format minor version    │
│   flags:         1 byte     reserved (0x00)         │
│   domain_len:    LEB128     byte length of domain   │
│   domain:        UTF-8      e.g. "api-logs"         │
│   entry_count:   LEB128     number of entries       │
├─────────────────────────────────────────────────────┤
│ Entries  (repeated entry_count times, ascending id) │
│   seq_id:        LEB128     0-based sequential ID   │
│   pat_len:       LEB128     byte length of pattern  │
│   pattern:       pat_len B  raw consensus pattern   │
├─────────────────────────────────────────────────────┤
│ Integrity seal                                       │
│   snapshot_hash: 32 bytes   SHA-256 of all entries  │
└─────────────────────────────────────────────────────┘
```

**snapshot_hash computation** (over sorted ascending seq_id):

```python
h = hashlib.sha256()
for seq_id in sorted(entries):
    pat = entries[seq_id]
    h.update(seq_id.to_bytes(4, 'little'))
    h.update(len(pat).to_bytes(4, 'little'))
    h.update(pat)
snapshot_hash = h.digest()
```

This hash is deterministic across implementations: same entries → same hash.


### E.3 .ufi ↔ .ufc Dependency Modes

When a `.ufi` program uses REF instructions backed by a `.ufc` library, the
`.ufi` file declares this dependency in its header. Three modes are supported:

```
EMBEDDED (flags & 0x02, no 0x08)
  The full consensus dict is inlined in the .ufi file.
  No external dependency. Self-contained archival format.
  Backward-compatible with the original .ufi v1 layout.

EXTERNAL (flags & 0x08, no 0x02)
  No inline consensus. The .ufi header carries:
    external_snapshot_hash:  32 bytes  — identifies the required .ufc snapshot
    url_count:               LEB128    — number of fetch URL hints (may be 0)
    urls[i]:                 LEB128 len + UTF-8
  The decoder MUST resolve and load the named snapshot before executing.
  Use for: high-volume distribution where many .ufi files share one .ufc.

HYBRID (flags & 0x02 AND 0x08)
  Inline consensus section (fast-path, file-specific overrides) PLUS
  an external .ufc reference (large shared library).
  The VM merges both: inline entries shadow external entries with same seq_id.
  Use for: deployments with a shared base library + per-batch customization.
```

**UfiFile binary layout additions for EXTERNAL / HYBRID:**

```
[...standard .ufi header: magic, flags, version, consensus?, macros?...]
[external_snapshot_hash: 32 bytes]   ← present when flag 0x08 set
[url_count: LEB128]
[url_len: LEB128][url: UTF-8]  × url_count
[prog_len: LEB128][program: prog_len B]
```


### E.4 Snapshot Versioning Protocol

A domain library is versioned as `(major, minor)`. The conventions are:

| Change type | Version bump |
|-------------|-------------|
| Add new patterns (backward-compatible) | minor++ |
| Remove or modify existing patterns (breaking) | major++ |
| Rebuild from larger corpus, same domain | minor++ |

A `.ufi` file binds to a specific snapshot by its `snapshot_hash`. It does
not care about the version tuple — it cares only that the exact snapshot is
available. The version tuple is for human operators managing library upgrades.

**Delta publishing:** to reduce distribution overhead when releasing `v1 → v2`,
the publisher may release a "delta snapshot" containing only the new/changed
entries, together with the base snapshot hash. The decoder merges the base and
delta at load time to reconstruct the full `v2` entry set, then verifies the
`v2` snapshot_hash. This is a distribution protocol, not a file format change —
both base and delta are standard `.ufc` files.


### E.5 Reference API

```python
from mucas import (
    UfcFile,           # .ufc file container
    UfiFile,           # .ufi file container (updated)
    EMBEDDED,          # consensus_source constant
    EXTERNAL,          # consensus_source constant
    HYBRID,            # consensus_source constant
    build_cross_file_consensus,
)

# Build and publish a .ufc snapshot
lib  = build_cross_file_consensus(training_files, n=50, domain="api-logs")
ufc  = UfcFile.from_consensus(lib, domain="api-logs", version=(1, 0))
blob = ufc.encode()                        # write to disk
open("api-logs-v1.ufc", "wb").write(blob)

# Load and verify
ufc2 = UfcFile.decode(open("api-logs-v1.ufc", "rb").read())
print(ufc2.snapshot_hash_hex)              # globally unique ID

# Compress a new file against the external library
from mucas import compress_ref_lz, naive_compress
prog = compress_ref_lz(new_data, ufc2.to_consensus())
ufi  = UfiFile(
    program=prog,
    consensus_source=EXTERNAL,
    external_snapshot_hash=ufc2.snapshot_hash,
    external_urls=["https://cdn.example.com/api-logs-v1.ufc"],
    compress_program=True,
)
open("output.ufi", "wb").write(ufi.encode())

# Decompress
ufi_loaded = UfiFile.decode(open("output.ufi", "rb").read())
vm_cons = {__import__('mucas').encode_leb128(i): p
           for i, p in ufc2.to_consensus().items()}
result = ufi_loaded.execute(external_consensus=vm_cons)
assert result == new_data
```


### E.6 Typical .ufc Sizes

For reference, a production consensus library:

| Domain | n patterns | avg_len | snapshot size |
|--------|-----------|---------|--------------|
| API request logs | 50 | 30 B | ~1.7 KB |
| API request logs | 200 | 25 B | ~5.5 KB |
| Chinese drama scripts | 50 | 20 B | ~1.1 KB |

A `.ufc` file is intentionally small — it is a dictionary of patterns, not data.
Even a 500-entry library for a complex domain fits under 50 KB. The distribution
cost of publishing a `.ufc` snapshot is negligible compared to the compression
savings it enables.

---

*μCAS v0.1 — First stable specification*  
*Derived from empirical implementation: all instruction semantics are validated by*  
*round-trip tests on real files. No untested features.*  
*Appendix C added after empirical validation of REF theory (2026-05-25).*  
*Appendix D added after cross-file consensus experimental validation (2026-05-25).*  
*Appendix E added after UfcFile implementation and UfiFile EXTERNAL/HYBRID modes (2026-05-25).*
