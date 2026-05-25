"""μCAS .ufc file format — immutable static consensus corpus snapshot."""
import hashlib
from dataclasses import dataclass, field
from .vm import encode_leb128, read_leb128
from .consensus import byte_entropy

UFC_MAGIC = b'\x55\x46\x43\x01'  # "UFC\x01"
UFC_FORMAT_VERSION = (1, 0)


@dataclass
class UfcFile:
    """
    Immutable static snapshot of a cross-file consensus library.

    Addressing:
      - snapshot_hash (SHA-256 of all entries): global content address, stable
        across copies and implementations. Serves as the canonical identifier
        when a .ufi file externally references this snapshot.
      - seq_id (int): local ordinal within this snapshot. REF instructions in
        .ufi files that depend on this snapshot use seq_id for compactness.

    Typical usage:
        lib  = build_cross_file_consensus(training_files, ...)
        ufc  = UfcFile.from_consensus(lib, domain="api-logs")
        blob = ufc.encode()                    # write to disk as .ufc
        ufc2 = UfcFile.decode(blob)            # round-trip: verifies hash
        cons = ufc2.to_consensus()             # use with compress_ref_lz
    """
    domain: str
    entries: dict[int, bytes]              # seq_id → pattern bytes
    version: tuple[int, int] = field(default_factory=lambda: (1, 0))

    # ── Content address ────────────────────────────────────────────────────

    @property
    def snapshot_hash(self) -> bytes:
        """SHA-256 over sorted (seq_id ‖ pat_len ‖ pattern) — content address."""
        h = hashlib.sha256()
        for seq_id in sorted(self.entries):
            pat = self.entries[seq_id]
            h.update(seq_id.to_bytes(4, 'little'))
            h.update(len(pat).to_bytes(4, 'little'))
            h.update(pat)
        return h.digest()

    @property
    def snapshot_hash_hex(self) -> str:
        return self.snapshot_hash.hex()

    def entry_hash(self, seq_id: int) -> bytes:
        """SHA-256 of the pattern bytes — per-entry content address."""
        return hashlib.sha256(self.entries[seq_id]).digest()

    # ── Statistics ─────────────────────────────────────────────────────────

    def stats(self) -> dict:
        if not self.entries:
            return dict(n=0, avg_len=0.0, h_pattern=0.0)
        all_b = b''.join(self.entries.values())
        return dict(
            n=len(self.entries),
            avg_len=len(all_b) / len(self.entries),
            h_pattern=byte_entropy(all_b),
        )

    # ── Serialization ──────────────────────────────────────────────────────

    def encode(self) -> bytes:
        """
        Serialize to .ufc bytes.

        Layout:
          [magic: 4B] [major: 1B] [minor: 1B] [flags: 1B]
          [domain_len: LEB128] [domain: UTF-8]
          [entry_count: LEB128]
          for each entry (ascending seq_id):
            [seq_id: LEB128] [pat_len: LEB128] [pattern: pat_len B]
          [snapshot_hash: 32B]   ← SHA-256 integrity seal
        """
        out = bytearray(UFC_MAGIC)
        major, minor = self.version
        out += bytes([major, minor, 0x00])  # version + reserved flags
        domain_b = self.domain.encode('utf-8')
        out += encode_leb128(len(domain_b)) + domain_b
        out += encode_leb128(len(self.entries))
        for seq_id in sorted(self.entries):
            pat = self.entries[seq_id]
            out += encode_leb128(seq_id)
            out += encode_leb128(len(pat))
            out += pat
        out += self.snapshot_hash   # integrity seal appended last
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> 'UfcFile':
        """
        Deserialize from .ufc bytes and verify snapshot_hash integrity.
        Raises ValueError on bad magic or hash mismatch.
        """
        if len(data) < 4 or data[:4] != UFC_MAGIC:
            raise ValueError(f"Invalid UFC magic: {data[:4]!r}")
        version = (data[4], data[5])
        pos = 7                              # skip magic(4) + major(1) + minor(1) + flags(1)
        domain_len, pos = read_leb128(data, pos)
        domain = data[pos:pos + domain_len].decode('utf-8')
        pos += domain_len
        n_entries, pos = read_leb128(data, pos)
        entries: dict[int, bytes] = {}
        for _ in range(n_entries):
            seq_id, pos = read_leb128(data, pos)
            pat_len, pos = read_leb128(data, pos)
            entries[seq_id] = bytes(data[pos:pos + pat_len])
            pos += pat_len
        stored_hash = bytes(data[pos:pos + 32])
        obj = cls(domain=domain, entries=entries, version=version)
        if stored_hash != obj.snapshot_hash:
            raise ValueError(
                f"UFC snapshot hash mismatch\n"
                f"  stored:   {stored_hash.hex()}\n"
                f"  computed: {obj.snapshot_hash.hex()}\n"
                "File is corrupted or tampered."
            )
        return obj

    # ── Convenience constructors / converters ──────────────────────────────

    @classmethod
    def from_consensus(cls, consensus: dict[int, bytes],
                       domain: str = "default",
                       version: tuple[int, int] = (1, 0)) -> 'UfcFile':
        """Build a UfcFile from a build_cross_file_consensus output dict."""
        return cls(domain=domain, entries=dict(consensus), version=version)

    def to_consensus(self) -> dict[int, bytes]:
        """Export as consensus dict compatible with compress_ref_lz / decompress_ref."""
        return dict(self.entries)
