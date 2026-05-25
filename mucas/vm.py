"""μCAS Virtual Machine — instruction decoder and executor."""
import struct, zlib


# ── LEB128 ───────────────────────────────────────────────────────────────────

def encode_leb128(n: int) -> bytes:
    assert n >= 0
    result = []
    while True:
        b = n & 0x7F; n >>= 7
        if n: b |= 0x80
        result.append(b)
        if not n: break
    return bytes(result)

def read_leb128(data: bytes, pos: int) -> tuple[int, int]:
    result, shift = 0, 0
    while True:
        b = data[pos]; pos += 1
        result |= (b & 0x7F) << shift
        shift += 7
        if not (b & 0x80):
            return result, pos


# ── Built-in MAP transforms ──────────────────────────────────────────────────

def _inc_u8(b):  return bytes([(x + 1) & 0xFF for x in b])
def _dec_u8(b):  return bytes([(x - 1) & 0xFF for x in b])
def _inc_u32(b): return struct.pack('<I', (struct.unpack('<I', b)[0] + 1) & 0xFFFFFFFF)
def _dec_u32(b): return struct.pack('<I', (struct.unpack('<I', b)[0] - 1) & 0xFFFFFFFF)

def _delta_u8(b):
    out, acc = [], 0
    for x in b: acc = (acc + x) & 0xFF; out.append(acc)
    return bytes(out)

def _delta_u32(b):
    out, acc = bytearray(), 0
    for i in range(0, len(b), 4):
        v = struct.unpack('<I', b[i:i+4])[0]
        acc = (acc + v) & 0xFFFFFFFF
        out += struct.pack('<I', acc)
    return bytes(out)

def _byteswap_u32(b):
    return b''.join(struct.pack('>I', struct.unpack('<I', b[i:i+4])[0])
                    for i in range(0, len(b), 4))

def _zigzag_u32(b):
    out = bytearray()
    for i in range(0, len(b), 4):
        n = struct.unpack('<I', b[i:i+4])[0]
        out += struct.pack('<i', (n >> 1) ^ -(n & 1))
    return bytes(out)

TRANSFORMS = {
    0x00: _inc_u8,
    0x01: _dec_u8,
    0x02: _inc_u32,
    0x03: _dec_u32,
    0x04: _delta_u8,
    0x05: _delta_u32,
    0x06: _byteswap_u32,
    0x07: _zigzag_u32,
}


# ── μCAS VM ──────────────────────────────────────────────────────────────────

class MuCASVM:
    """
    Executes μCAS programs. Opcodes:
      0x00 LIT  [len: LEB128] [bytes]
      0x01 CPY  [offset: LEB128] [len: LEB128]
      0x02 MAP  [transform_id: u8] [len: LEB128]
      0x03 LOOP [count: LEB128] [body_len: LEB128] [body...]
      0x04 CALL [macro_id: LEB128]
      0x05 REF  [hash_len: u8] [hash: bytes]
      0x06 ELIT [encoded_len: LEB128] [raw_len: LEB128] [zlib_payload]
      0xFF HALT
    """

    def __init__(self, macros: dict[int, bytes] = None,
                 consensus: dict[bytes, bytes] = None):
        self.macros = macros or {}
        self.consensus = consensus or {}
        self.out = bytearray()

    def reset(self):
        self.out = bytearray()

    def exec(self, prog: bytes, pos: int = 0) -> int:
        while pos < len(prog):
            op = prog[pos]; pos += 1

            if op == 0xFF:
                break

            elif op == 0x00:  # LIT
                n, pos = read_leb128(prog, pos)
                self.out.extend(prog[pos:pos+n]); pos += n

            elif op == 0x01:  # CPY
                offset, pos = read_leb128(prog, pos)
                length, pos = read_leb128(prog, pos)
                src = len(self.out) - offset
                assert src >= 0, f"CPY out of bounds: offset={offset}, out_len={len(self.out)}"
                for i in range(length):
                    self.out.append(self.out[src + i])

            elif op == 0x02:  # MAP
                tid = prog[pos]; pos += 1
                n, pos = read_leb128(prog, pos)
                assert tid in TRANSFORMS, f"Unknown transform 0x{tid:02X}"
                chunk = bytes(self.out[-n:]); del self.out[-n:]
                self.out.extend(TRANSFORMS[tid](chunk))

            elif op == 0x03:  # LOOP
                count, pos = read_leb128(prog, pos)
                blen,  pos = read_leb128(prog, pos)
                body = prog[pos:pos+blen]; pos += blen
                for _ in range(count):
                    self.exec(body)

            elif op == 0x04:  # CALL
                mid, pos = read_leb128(prog, pos)
                assert mid in self.macros, f"Unknown macro {mid}"
                self.exec(self.macros[mid])

            elif op == 0x05:  # REF
                hlen = prog[pos]; pos += 1
                h = bytes(prog[pos:pos+hlen]); pos += hlen
                assert h in self.consensus, "Unknown consensus ref"
                self.out.extend(self.consensus[h])

            elif op == 0x06:  # ELIT
                elen, pos = read_leb128(prog, pos)
                _, pos = read_leb128(prog, pos)  # raw_len (informational)
                payload = prog[pos:pos+elen]; pos += elen
                self.out.extend(zlib.decompress(payload))

        return pos
