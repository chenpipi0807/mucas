"""Consensus library construction and REF-aware compression."""
from collections import defaultdict
from .vm import encode_leb128, MuCASVM
from .compress import naive_compress

# RAI v3: theoretical threshold — H* = C_REF / avg_pattern_len ≈ 20 bits / 10 B = 2.0 b/B
# Empirically confirmed at 2.5 b/B on 5 test files (5/5 correct).
RAI_H_THRESHOLD = 2.5  # bits/byte


def build_consensus(data: bytes, n: int = 50,
                    min_len: int = 6, max_len: int = 48) -> dict[int, bytes]:
    """
    Find top-N byte patterns by expected byte savings.
    REF cost: 3 bytes (opcode=1, hash_len=1, 1-byte ID).
    A pattern is worth REF-ing if count * (len - 3) > 0.
    """
    REF_COST = 3
    counts: dict[bytes, int] = defaultdict(int)
    for L in range(min_len, min(max_len + 1, len(data))):
        for i in range(len(data) - L + 1):
            counts[data[i:i+L]] += 1

    scored = [
        (count * (len(p) - REF_COST), len(p), p)
        for p, count in counts.items()
        if count >= 2 and len(p) > REF_COST
    ]
    scored.sort(reverse=True)

    selected: dict[int, bytes] = {}
    for _, _, pattern in scored:
        if len(selected) >= n:
            break
        if not any(pattern in existing for existing in selected.values()):
            selected[len(selected)] = pattern
    return selected


def build_cross_file_consensus(files: list[bytes], n: int = 50,
                               min_len: int = 6, max_len: int = 48,
                               min_files: int = 2,
                               h_min: float = RAI_H_THRESHOLD) -> dict[int, bytes]:
    """
    Build a pre-trained consensus library from a corpus of files.

    Algorithm (hash-accelerated — O(total_bytes × avg_len) instead of O(n²)):
      1. For each file, build a Counter of all substrings of each length in one pass.
         Patterns appearing ≥2 times in that file are "candidates".
      2. Track per-file presence count and corpus-wide occurrence count simultaneously.
      3. Keep only patterns in ≥ min_files distinct files.
      4. Filter by byte entropy ≥ h_min.
      5. Rank by corpus-wide expected savings: corpus_count × (len - 3).
      6. Greedy non-overlapping selection, return top-N.
    """
    from collections import Counter
    REF_COST = 3

    # Step 1+2: single-pass bulk counting per file
    file_presence: Counter[bytes] = Counter()   # how many files contain pattern ≥2×
    corpus_count:  Counter[bytes] = Counter()   # total occurrences across all files

    for data in files:
        file_cnt: Counter[bytes] = Counter()
        for L in range(min_len, min(max_len + 1, len(data))):
            file_cnt.update(data[i:i + L] for i in range(len(data) - L + 1))
        for p, c in file_cnt.items():
            if c >= 2:
                file_presence[p] += 1
                corpus_count[p] += c

    # Step 3: cross-file filter
    cross_patterns = [p for p, fc in file_presence.items() if fc >= min_files]

    # Step 4: entropy filter
    cross_patterns = [p for p in cross_patterns if byte_entropy(p) >= h_min]

    # Step 5: rank by corpus-wide expected savings
    scored = sorted(
        [(corpus_count[p] * (len(p) - REF_COST), len(p), p) for p in cross_patterns],
        reverse=True,
    )

    # Step 6: greedy non-overlapping selection
    selected: dict[int, bytes] = {}
    for _, _, pattern in scored:
        if len(selected) >= n:
            break
        if not any(pattern in existing for existing in selected.values()):
            selected[len(selected)] = pattern
    return selected


CROSS_COVERAGE_THRESHOLD = 0.10  # 10% — empirical minimum for net cross-file REF gain
# Derived from coverage curve experiment: n=2(10.4%,+0.3%) is marginal, n=3(12.9%,+4.5%) is reliable.


def predict_cross_ref_benefit(data: bytes, consensus: dict[int, bytes]) -> dict:
    """
    2D RAI model: quality (H_pattern) × quantity (cross_coverage).

    Both dimensions must exceed their thresholds for positive net benefit:
      H_pattern     > RAI_H_THRESHOLD          (2.5 b/B)  — per-pattern entropy
      cross_coverage > CROSS_COVERAGE_THRESHOLD (8%)       — fraction of data covered

    Returns dict with keys: h_pattern, cross_coverage, predicts_benefit,
                             quality_ok, coverage_ok.
    """
    if not consensus:
        return dict(h_pattern=0.0, cross_coverage=0.0,
                    quality_ok=False, coverage_ok=False, predicts_benefit=False)

    all_pat_bytes = b''.join(consensus.values())
    h = byte_entropy(all_pat_bytes)

    matches = find_matches(data, consensus)
    covered = sum(e - s for s, e, _ in matches)
    cov = covered / len(data) if data else 0.0

    quality_ok  = h   > RAI_H_THRESHOLD
    coverage_ok = cov > CROSS_COVERAGE_THRESHOLD

    return dict(
        h_pattern=h,
        cross_coverage=cov,
        quality_ok=quality_ok,
        coverage_ok=coverage_ok,
        predicts_benefit=quality_ok and coverage_ok,
    )


def find_matches(data: bytes,
                 consensus: dict[int, bytes]) -> list[tuple[int, int, int]]:
    """Greedy left-to-right, longest-first consensus matching."""
    by_len = sorted(consensus.items(), key=lambda x: len(x[1]), reverse=True)
    matches, i = [], 0
    while i < len(data):
        for cid, pattern in by_len:
            plen = len(pattern)
            if data[i:i+plen] == pattern:
                matches.append((i, i + plen, cid))
                i += plen
                break
        else:
            i += 1
    return matches


def emit_ref(cid: int) -> bytes:
    id_enc = encode_leb128(cid)
    return bytes([0x05, len(id_enc)]) + id_enc


def compress_ref_lz(data: bytes, consensus: dict[int, bytes],
                    window: int = 32768) -> bytes:
    """Compress using REF instructions for consensus matches + LZ for gaps."""
    matches = find_matches(data, consensus)
    out, cursor = bytearray(), 0

    for start, end, cid in matches:
        if cursor < start:
            gap = data[cursor:start]
            prog = naive_compress(gap, window=min(window, len(gap) + 1))
            out.extend(prog[:-1])  # strip HALT
        out.extend(emit_ref(cid))
        cursor = end

    if cursor < len(data):
        tail = naive_compress(data[cursor:], window=window)
        out.extend(tail[:-1])

    out.append(0xFF)
    return bytes(out)


def decompress_ref(prog: bytes, consensus: dict[int, bytes]) -> bytes:
    """Execute a REF-aware μCAS program against a consensus dict[id -> bytes]."""
    consensus_vm = {encode_leb128(cid): pattern for cid, pattern in consensus.items()}
    vm = MuCASVM(consensus=consensus_vm)
    vm.exec(prog)
    return bytes(vm.out)


# RAI_H_THRESHOLD is defined at the top of this module.


def byte_entropy(b: bytes) -> float:
    """Shannon entropy of a byte sequence in bits/byte (public API)."""
    import math
    from collections import Counter
    if not b:
        return 0.0
    c = Counter(b)
    n = len(b)
    return -sum((v / n) * math.log2(v / n) for v in c.values())


def _byte_entropy(b: bytes) -> float:
    """Shannon entropy of a byte sequence in bits/byte."""
    import math
    from collections import Counter
    if not b:
        return 0.0
    c = Counter(b)
    n = len(b)
    return -sum((v / n) * math.log2(v / n) for v in c.values())


def compute_rai(data: bytes, n_consensus: int = 50,
                min_len: int = 6, max_len: int = 48,
                window: int = 32768) -> dict:
    """
    Compute the REF Applicability Index (RAI v3) for a data block.

    Theoretical basis (DeepSeek, 2026):
      REF benefit comes from replacing diverse CPY instructions with uniform REF
      tokens, providing entropy collapse at the instruction-stream level.
      Net gain per pattern occurrence ≈ L × H_pattern - C_REF bits.
      REF is profitable when H_pattern > H_threshold = C_REF / avg_pattern_len.

    Primary predictor:
      H_pattern — byte entropy of the consensus patterns (bits/byte).
      Threshold: RAI_H_THRESHOLD ≈ 2.5 b/B (theoretical) / 3.0 b/B (empirical).
        > threshold → REF helps (high-entropy content: CJK UTF-8, logs)
        < threshold → REF hurts (low-entropy content: English ASCII, JSON keys)

    Secondary metrics reported for diagnostic purposes.

    Returns dict with keys:
      total, cpy_bytes, lit_bytes, lz_coverage, cpy_count, avg_cpy_len,
      h_pattern, ref_data_coverage, rai_predicts, consensus_size, consensus
    """
    from .compress import naive_compress
    from .vm import read_leb128

    prog = naive_compress(data, window=window)

    cpy_lengths, cpy_total, lit_total = [], 0, 0
    pos = 0
    while pos < len(prog):
        op = prog[pos]; pos += 1
        if op == 0xFF:
            break
        elif op == 0x00:  # LIT
            n, pos = read_leb128(prog, pos)
            pos += n; lit_total += n
        elif op == 0x01:  # CPY
            _, pos   = read_leb128(prog, pos)
            ln, pos  = read_leb128(prog, pos)
            cpy_lengths.append(ln); cpy_total += ln
        elif op == 0x06:  # ELIT
            elen, pos = read_leb128(prog, pos)
            rlen, pos = read_leb128(prog, pos)
            pos += elen; lit_total += rlen

    total = len(data)
    lz_coverage = cpy_total / total if total > 0 else 0.0
    avg_cpy_len  = sum(cpy_lengths) / len(cpy_lengths) if cpy_lengths else 0.0

    # Build consensus from full data
    consensus = build_consensus(data, n=n_consensus, min_len=min_len, max_len=max_len)
    ref_cov = sum(e - s for s, e, _ in find_matches(data, consensus))

    # H_pattern: byte entropy of all consensus patterns concatenated
    all_pat_bytes = b''.join(consensus.values())
    h_pattern = _byte_entropy(all_pat_bytes)

    # Primary RAI decision: H_pattern vs information-theoretic threshold
    rai_predicts = h_pattern > RAI_H_THRESHOLD

    return dict(
        total=total,
        cpy_bytes=cpy_total,
        lit_bytes=lit_total,
        lz_coverage=lz_coverage,
        cpy_count=len(cpy_lengths),
        avg_cpy_len=avg_cpy_len,
        h_pattern=h_pattern,
        ref_data_coverage=ref_cov / total if total > 0 else 0.0,
        rai_predicts=rai_predicts,
        consensus_size=len(consensus),
        consensus=consensus,
    )
