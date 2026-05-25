"""μCAS compressors: naive LZ and smart structural."""
import struct, zlib
from .vm import encode_leb128, MuCASVM


# ── Literal emitter ───────────────────────────────────────────────────────────

def _emit_literal(run: bytes) -> bytes:
    lit = bytes([0x00]) + encode_leb128(len(run)) + run
    if len(run) <= 32:
        return lit
    compressed = zlib.compress(run, 1)
    elit = (bytes([0x06]) +
            encode_leb128(len(compressed)) +
            encode_leb128(len(run)) +
            compressed)
    return elit if len(elit) < len(lit) else lit


# ── Naive LZ compressor ───────────────────────────────────────────────────────

def naive_compress(data: bytes, min_match: int = 4, window: int = 4096) -> bytes:
    """LZ-style: LIT, CPY, and ELIT only. No LOOP/MAP/REF."""
    out = bytearray()
    i = 0

    while i < len(data):
        best_off, best_len = 0, 0
        search_start = max(0, i - window)
        for j in range(search_start, i):
            k = 0
            while (i + k < len(data) and
                   data[j + k] == data[i + k] and k < 258):
                k += 1
            if k > best_len:
                best_len, best_off = k, i - j

        if best_len >= min_match:
            out.append(0x01)
            out.extend(encode_leb128(best_off))
            out.extend(encode_leb128(best_len))
            i += best_len
        else:
            lit_start = i; i += 1
            while i < len(data):
                found = False
                for j in range(max(0, i - window), i):
                    k = 0
                    while (i + k < len(data) and
                           data[j + k] == data[i + k] and k < min_match):
                        k += 1
                    if k >= min_match:
                        found = True; break
                if found: break
                i += 1
            out.extend(_emit_literal(data[lit_start:i]))

    out.append(0xFF)
    return bytes(out)


# ── Smart structural compressor ───────────────────────────────────────────────

def encode_loop(count: int, body: bytes) -> bytes:
    return (bytes([0x03]) +
            encode_leb128(count) +
            encode_leb128(len(body)) +
            body)


def _try_inc_u32_run(data: bytes, pos: int) -> tuple[int, bytes] | None:
    if pos + 8 > len(data) or (pos % 4) != 0:
        return None
    v0 = struct.unpack('<I', data[pos:pos+4])[0]
    v1 = struct.unpack('<I', data[pos+4:pos+8])[0]
    if (v1 - v0) & 0xFFFFFFFF != 1:
        return None
    count = 2
    while pos + (count + 1) * 4 <= len(data):
        vn = struct.unpack('<I', data[pos+count*4:pos+count*4+4])[0]
        if (vn - v0 - count) & 0xFFFFFFFF != 0:
            break
        count += 1
    if count < 3:
        return None
    body = bytes([0x01]) + encode_leb128(4) + encode_leb128(4) + bytes([0x02, 0x02, 4])
    prog = bytes([0x00, 4]) + struct.pack('<I', v0) + encode_loop(count - 1, body)
    return count * 4, prog


def _try_byte_run(data: bytes, pos: int) -> tuple[int, bytes] | None:
    b = data[pos]
    count = 1
    while pos + count < len(data) and data[pos + count] == b and count < 65535:
        count += 1
    if count < 4:
        return None
    body = bytes([0x01]) + encode_leb128(1) + encode_leb128(1)
    prog = bytes([0x00, 1, b]) + encode_loop(count - 1, body)
    return count, prog


def smart_compress(data: bytes) -> bytes:
    """Two-pass: detect INC_U32/byte-run patterns first, LZ on gaps."""
    segments = []
    i = 0
    while i < len(data):
        if i % 4 == 0:
            result = _try_inc_u32_run(data, i)
            if result:
                run_len, prog = result
                segments.append((i, i + run_len, prog))
                i += run_len
                continue
        result = _try_byte_run(data, i)
        if result:
            run_len, prog = result
            segments.append((i, i + run_len, prog))
            i += run_len
            continue
        i += 1

    out = bytearray()
    covered = 0
    for start, end, prog in segments:
        if covered < start:
            lz = naive_compress(data[covered:start])
            out.extend(lz[:-1])  # strip HALT from gap sub-program
        out.extend(prog)
        covered = end
    if covered < len(data):
        lz = naive_compress(data[covered:])
        out.extend(lz[:-1])
    out.append(0xFF)
    return bytes(out)


# ── LZ coverage analysis ─────────────────────────────────────────────────────

def parse_program_coverage(prog: bytes) -> list[tuple[int, str]]:
    """
    Parse a naive_compress output and return [(byte_count, 'CPY'|'LIT'), ...].
    Covers all original data bytes in left-to-right order.
    Only valid for programs using LIT (0x00), CPY (0x01), ELIT (0x06), HALT (0xFF).
    """
    from .vm import read_leb128
    result = []
    pos = 0
    while pos < len(prog):
        op = prog[pos]; pos += 1
        if op == 0xFF:
            break
        elif op == 0x00:  # LIT
            n, pos = read_leb128(prog, pos)
            pos += n
            result.append((n, 'LIT'))
        elif op == 0x01:  # CPY
            _, pos = read_leb128(prog, pos)     # offset (discard)
            length, pos = read_leb128(prog, pos)
            result.append((length, 'CPY'))
        elif op == 0x06:  # ELIT — counts as LIT for coverage purposes
            elen, pos = read_leb128(prog, pos)
            rlen, pos = read_leb128(prog, pos)
            pos += elen
            result.append((rlen, 'LIT'))
    return result


def lz_analyze(data: bytes, min_match: int = 4,
               window: int = 4096) -> list[tuple[int, int, str]]:
    """
    Run LZ compression and return a coverage map of the original data.
    Returns [(start, end, 'CPY'|'LIT'), ...] covering [0, len(data)) exactly.
    """
    prog = naive_compress(data, min_match=min_match, window=window)
    seq = parse_program_coverage(prog)
    result, pos = [], 0
    for length, itype in seq:
        result.append((pos, pos + length, itype))
        pos += length
    return result


# ── Benchmark utility ─────────────────────────────────────────────────────────

def benchmark(data: bytes, label: str = "data") -> float:
    import zlib
    try:
        import zstandard as zstd
        zstd_1  = len(zstd.ZstdCompressor(level=1).compress(data))
        zstd_19 = len(zstd.ZstdCompressor(level=19).compress(data))
    except ImportError:
        zstd_1 = zstd_19 = None

    mucas = naive_compress(data)
    vm = MuCASVM()
    vm.exec(mucas)
    assert bytes(vm.out) == data, "ROUND-TRIP FAILED"

    raw = len(data)
    print(f"\n── {label} ({raw:,} bytes) ──")
    print(f"  原始:      {raw:>10,}  (100.00%)")
    print(f"  μCAS:      {len(mucas):>10,}  ({len(mucas)/raw:.2%})")
    print(f"  zlib-1:    {len(zlib.compress(data,1)):>10,}  ({len(zlib.compress(data,1))/raw:.2%})")
    print(f"  zlib-9:    {len(zlib.compress(data,9)):>10,}  ({len(zlib.compress(data,9))/raw:.2%})")
    if zstd_1:
        print(f"  zstd-1:    {zstd_1:>10,}  ({zstd_1/raw:.2%})")
        print(f"  zstd-19:   {zstd_19:>10,}  ({zstd_19/raw:.2%})")
    print(f"  轮回验证:  PASS")
    return len(mucas) / raw
