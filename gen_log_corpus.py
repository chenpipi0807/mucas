"""
Generate synthetic log files designed to demonstrate cross-file REF compression benefit.

Design goals (v4 — single-line JSON boilerplate, stride-aligned patterns):
  - 40 files, each with 1 session of ~42 KB (> Zlib 32 KB window).
  - Every file starts with an IDENTICAL 640-byte JSON boilerplate (including \\n).
  - The boilerplate is ONE LINE (no embedded \\n): after SCAN, it becomes a SINGLE
    640-byte LIT token in the synthesized program.
  - ConsensusBuilder extracts 64-byte patterns at stride=32 (positions 0,32,...,576)
    inside the 640-byte raw file region.  All 19 patterns fall WITHIN the single LIT
    token → apply_ref_pass can match all of them.
  - apply_ref_pass picks 10 non-overlapping 64-byte matches (greedy, covers all 640B).
  - REF savings: 640 → 30 bytes per file → ~350 B saved after Zlib × 40 files = 14 KB.
  - Consensus overhead: 19+ patterns × ~70 B each ≈ 1400 B (well under 14 KB).
  - Log lines: 8 fixed + 2 variable columns → literal_fraction ~20% → SCAN fires.
"""
import os, json, hashlib

OUT_DIR = r"d:\P-ZIP\test\log_corpus"
os.makedirs(OUT_DIR, exist_ok=True)

# ------------------------------------------------------------------
# Build a JSON boilerplate that is EXACTLY 639 bytes (so + "\n" = 640)
# and has no repeated 8-byte substring (verified below).
# ------------------------------------------------------------------
_BASE = {
    "benchmark": "muCAS-v2.4.0",
    "endpoint": "https://video-gen-proxy.zuoyebang.cc/rp/doubao/v3/content/gen/tasks",
    "auth_hash": "zyb-hmac-sha256-X7K9P2M4N8Q1R5S6",
    "model": "doubao-seedance-2-0-260128",
    "resolution": "1920x1080",
    "fps": 24,
    "duration_s": 10,
    "seed_mode": "deterministic",
    "scene_ver": "v2.3",
    "backoff_on": ["timeout", "server_error", "rate_limit"],
    "max_attempts": 3,
    "abort_conds": ["invalid_key", "quota_exceeded", "content_policy"],
    "out_format": "csv-v2",
    "charset": "utf-8-bom",
    "csum_algo": "crc32",
    "trace": "f3a8b2-c4d6e1f0a9b8-c7d6-e5f4-a3b2c1d0e9f8",
}
_s = json.dumps(_BASE, separators=(',', ':'))
_target = 639   # BOILERPLATE will be _target + "\n" = 640 bytes = 10 × 64

# Pad to exact target with a unique padding field.
_needed = _target - len(_s.encode())
if _needed < 0:
    raise ValueError(f"Base JSON too long ({len(_s)} > {_target}); shorten it")
if _needed > 0:
    # Ensure _pad value fills exactly _needed bytes within the JSON encoding.
    # ',"_pad":"' = 9 bytes,  '"' at end = 1 byte → value needs to be _needed-10 chars.
    _val_len = _needed - 10
    if _val_len < 0:
        raise ValueError(f"Cannot pad by {_needed} bytes (too small for field wrapper)")
    # Use two different SHA256 seeds to build a non-repeating hex padding string.
    _pad_raw = (hashlib.sha256(b"mucas-pad-seed-A").hexdigest() +
                hashlib.sha256(b"mucas-pad-seed-B").hexdigest())
    _pad_val = _pad_raw[:_val_len]
    _s = _s[:-1] + f',"_pad":"{_pad_val}"' + '}'

assert len(_s.encode()) == _target, f"JSON = {len(_s.encode())} bytes, expected {_target}"

BOILERPLATE = _s + "\n"   # total 640 bytes
BP_BYTES = len(BOILERPLATE.encode("utf-8"))
assert BP_BYTES == 640, f"Boilerplate = {BP_BYTES} bytes, expected 640"

# Verify no 8-byte internal repeat in the boilerplate bytes.
def _check_no_8byte_repeat(text: str) -> None:
    b = text.encode("utf-8")
    seen: dict[bytes, int] = {}
    for i in range(len(b) - 7):
        pat = bytes(b[i:i+8])
        if pat in seen:
            raise AssertionError(
                f"8-byte repeat at {i} (first at {seen[pat]}): {pat!r}"
            )
        seen[pat] = i

_check_no_8byte_repeat(BOILERPLATE)

# ------------------------------------------------------------------
# Log line format: 10 comma-separated columns.
# 8 fixed + 2 variable (row_id and timestamp_ms):
#   literal_fraction ≈ 17 / 83 ≈ 0.20 < 0.45 → SCAN fires (StructuredLog).
# ------------------------------------------------------------------
def gen_log_line(row_id: int, ts_ms: int) -> str:
    return (
        f"{row_id},page_view,api-gateway,/api/v1/track,"
        f"v2.3.1,production,200,45,us-east-1,{ts_ms}\n"
    )


# 500 lines × ~83 bytes = 41 500 B + 640 B header → 42 140 B per file > 32 KB.
LINES_PER_FILE = 500

KEY_NAMES = [
    "SVIP",       "poly_spark",  "premium_cn",  "basic_cn",    "test_v2",
    "prod_east",  "prod_west",   "staging_01",  "staging_02",  "canary",
    "user_a1",    "user_b2",     "user_c3",     "user_d4",     "user_e5",
    "batch_01",   "batch_02",    "batch_03",    "batch_04",    "batch_05",
    "team_alpha", "team_beta",   "team_gamma",  "team_delta",  "team_epsilon",
    "zone_cn1",   "zone_cn2",    "zone_eu1",    "zone_eu2",    "zone_us1",
    "exp_v1",     "exp_v2",      "exp_v3",      "exp_v4",      "exp_v5",
    "shard_01",   "shard_02",    "shard_03",    "shard_04",    "shard_05",
]   # 40 files


print(
    f"Generating {len(KEY_NAMES)} files "
    f"(1 session x {LINES_PER_FILE} lines, 640-byte JSON boilerplate) -> {OUT_DIR}"
)
print(
    f"Boilerplate: {BP_BYTES} bytes (= 10 x 64, single-line JSON, no 8-byte repeat)"
)
print("  → ConsensusBuilder 64-byte patterns at stride=32 all within one LIT token")

for i, name in enumerate(KEY_NAMES):
    base_ts = 1_705_316_400_000 + i * 500_000
    parts = [BOILERPLATE]
    for row in range(LINES_PER_FILE):
        parts.append(gen_log_line(row, base_ts + row * 150))
    content = "".join(parts)
    path = os.path.join(OUT_DIR, f"repro_log_{name}.txt")
    with open(path, "w", encoding="utf-8") as f:
        f.write(content)
    size = len(content.encode("utf-8"))
    ok = "OK" if size > 32768 else "SMALL"
    print(f"  {name:<20} {size:>10,} bytes [{ok}]")

print("Done.")
