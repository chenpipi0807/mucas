"""Test suite and benchmark runner for μCAS."""
import json, struct, random, time
from mucas import MuCASVM, naive_compress, encode_leb128, benchmark


# ── Unit tests: each instruction ────────────────────────────────────────────

def test_lit():
    vm = MuCASVM(); vm.exec(bytes([0x00, 3, 0x41, 0x42, 0x43, 0xFF]))
    assert bytes(vm.out) == b'ABC', f"LIT failed: {vm.out}"
    print("PASS  LIT")

def test_cpy():
    # Write "AB" then copy it twice
    prog = bytes([
        0x00, 2, 0x41, 0x42,   # LIT "AB"
        0x01, 2, 2,             # CPY offset=2 len=2  -> "AB"
        0x01, 2, 2,             # CPY offset=2 len=2  -> "AB"
        0xFF
    ])
    vm = MuCASVM(); vm.exec(prog)
    assert bytes(vm.out) == b'ABABAB', f"CPY failed: {vm.out}"
    print("PASS  CPY")

def test_cpy_overlap():
    # Write "AB" then CPY offset=2, length=6 -> overlapping: ABABABAB
    prog = bytes([0x00, 2, 0x41, 0x42, 0x01, 2, 6, 0xFF])
    vm = MuCASVM(); vm.exec(prog)
    assert bytes(vm.out) == b'ABABABAB', f"CPY overlap failed: {vm.out}"
    print("PASS  CPY (overlapping / RLE)")

def test_loop_basic():
    # LIT "X", then LOOP 4 times: CPY offset=1 len=1 -> "XXXXX"
    body = bytes([0x01, 1, 1])   # CPY offset=1, len=1
    prog = bytes([0x00, 1, 0x58]) + bytes([0x03]) + encode_leb128(4) + encode_leb128(len(body)) + body + bytes([0xFF])
    vm = MuCASVM(); vm.exec(prog)
    assert bytes(vm.out) == b'XXXXX', f"LOOP failed: {vm.out}"
    print("PASS  LOOP")

def test_loop_inc_u32():
    # Generate 4 incrementing little-endian U32s: 0,1,2,3
    body = bytes([0x01, 4, 4,    # CPY offset=4, len=4
                  0x02, 0x02, 4]) # MAP INC_U32, 4
    prog = (bytes([0x00, 4, 0,0,0,0]) +  # LIT [0,0,0,0]
            bytes([0x03]) + encode_leb128(3) + encode_leb128(len(body)) + body +
            bytes([0xFF]))
    vm = MuCASVM(); vm.exec(prog)
    expected = struct.pack('<4I', 0, 1, 2, 3)
    assert bytes(vm.out) == expected, f"LOOP+MAP INC_U32 failed: {list(vm.out)}"
    print("PASS  LOOP + MAP INC_U32")

def test_call():
    # Define macro 0: LIT "HI"
    macro = bytes([0x00, 2, 0x48, 0x49])
    prog = bytes([0x04, 0,   # CALL macro 0
                  0x04, 0,   # CALL macro 0
                  0xFF])
    vm = MuCASVM(macros={0: macro})
    vm.exec(prog)
    assert bytes(vm.out) == b'HIHI', f"CALL failed: {vm.out}"
    print("PASS  CALL")

def test_ref():
    import hashlib
    content = b'hello world'
    h = hashlib.sha256(content).digest()
    prog = bytes([0x05, len(h)]) + h + bytes([0xFF])
    vm = MuCASVM(consensus={h: content})
    vm.exec(prog)
    assert bytes(vm.out) == content, f"REF failed: {vm.out}"
    print("PASS  REF")

def test_roundtrip_random():
    random.seed(42)
    data = bytes(random.randint(0, 255) for _ in range(10_000))
    compressed = naive_compress(data)
    vm = MuCASVM(); vm.exec(compressed)
    assert bytes(vm.out) == data
    print(f"PASS  roundtrip random 10KB  (ratio: {len(compressed)/len(data):.2%})")

def test_roundtrip_text():
    data = ("The quick brown fox jumps over the lazy dog. " * 500).encode()
    compressed = naive_compress(data)
    vm = MuCASVM(); vm.exec(compressed)
    assert bytes(vm.out) == data
    print(f"PASS  roundtrip repeated text {len(data)//1024}KB  (ratio: {len(compressed)/len(data):.2%})")


# ── Benchmarks ───────────────────────────────────────────────────────────────

def run_benchmarks():
    print("\n" + "="*50)
    print("BENCHMARKS")
    print("="*50)

    # 1. Repeated text (best case for LZ)
    data1 = ("Hello World! This is a test. " * 2000).encode()
    benchmark(data1, "repeated text (58KB)")

    # 2. JSON with numeric sequences (realistic API data)
    records = [{"id": i, "name": f"user_{i:04d}", "score": i * 3 + 7,
                "active": i % 2 == 0, "tag": f"tag_{i % 10}"}
               for i in range(500)]
    data2 = json.dumps(records, separators=(',', ':')).encode()
    benchmark(data2, "JSON records (500 entries)")

    # 3. Binary: ascending U32 array (ideal for MAP INC_U32, but naive won't use it)
    data3 = struct.pack('<10000I', *range(10000))
    benchmark(data3, "binary ascending U32[10000]")

    # 4. Near-random (worst case)
    random.seed(0)
    data4 = bytes(random.randint(0, 255) for _ in range(50_000))
    benchmark(data4, "near-random bytes (50KB)")

    # 5. Try to find a real file on disk
    import os
    candidates = [
        "C:/Windows/System32/drivers/etc/hosts",
        os.path.expanduser("~/.bashrc"),
        os.path.expanduser("~/Desktop"),
    ]
    for path in candidates:
        if os.path.isfile(path):
            with open(path, 'rb') as f:
                real = f.read()
            if len(real) > 100:
                benchmark(real, f"real file: {os.path.basename(path)}")
            break


if __name__ == '__main__':
    print("── Unit Tests ──")
    test_lit()
    test_cpy()
    test_cpy_overlap()
    test_loop_basic()
    test_loop_inc_u32()
    test_call()
    test_ref()
    test_roundtrip_random()
    test_roundtrip_text()
    print("\nAll unit tests passed.")
    run_benchmarks()
