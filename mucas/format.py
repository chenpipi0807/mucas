"""μCAS .ufi file format — encode and decode UFI\x01 containers."""
import zlib
from dataclasses import dataclass, field
from .vm import encode_leb128, read_leb128, MuCASVM

MAGIC          = b'\x55\x46\x49\x01'  # "UFI\x01"
FORMAT_VERSION = 1

_FLAG_COMPRESS            = 0x01  # program is zlib-wrapped
_FLAG_CONSENSUS           = 0x02  # embedded consensus section present
_FLAG_MACROS              = 0x04  # macro section present
_FLAG_CONSENSUS_EXTERNAL  = 0x08  # external UFC reference section present

# consensus_source values (stored in consensus_source field, not in file bytes directly)
EMBEDDED = "EMBEDDED"   # consensus section follows inline
EXTERNAL = "EXTERNAL"   # no inline consensus; UFC snapshot hash + optional URLs follow
HYBRID   = "HYBRID"     # both inline consensus AND external UFC reference


@dataclass
class UfiFile:
    """
    In-memory representation of a .ufi file.

    consensus_source controls how consensus data is stored/referenced:
      EMBEDDED (default): consensus section inlined in file.
      EXTERNAL:           no inline data; file header carries UFC snapshot hash
                          and optional fetch URLs. The decoder must resolve
                          the external library before executing.
      HYBRID:             inline consensus (local/fast entries) + external UFC
                          reference (large shared library). Decoder merges both.

    Fields:
      program               — raw μCAS bytecode (before any zlib wrapper)
      macros                — dict[macro_id: int -> body: bytes]
      consensus             — dict[hash: bytes -> content: bytes]  (EMBEDDED/HYBRID)
      consensus_source      — EMBEDDED | EXTERNAL | HYBRID
      external_snapshot_hash — UFC snapshot hash (32 bytes) for EXTERNAL/HYBRID
      external_urls         — optional fetch hints for the external snapshot
      compress_program      — whether to zlib-wrap the program in the file
    """
    program: bytes
    macros: dict[int, bytes] = field(default_factory=dict)
    consensus: dict[bytes, bytes] = field(default_factory=dict)
    consensus_source: str = EMBEDDED
    external_snapshot_hash: bytes | None = None
    external_urls: list[str] = field(default_factory=list)
    compress_program: bool = False

    def encode(self) -> bytes:
        """Serialize to .ufi bytes."""
        if self.consensus_source == EXTERNAL and self.external_snapshot_hash is None:
            raise ValueError("EXTERNAL consensus_source requires external_snapshot_hash")
        if self.consensus_source == HYBRID and self.external_snapshot_hash is None:
            raise ValueError("HYBRID consensus_source requires external_snapshot_hash")

        flags = 0
        if self.compress_program:                          flags |= _FLAG_COMPRESS
        if self.consensus:                                 flags |= _FLAG_CONSENSUS
        if self.macros:                                    flags |= _FLAG_MACROS
        if self.consensus_source in (EXTERNAL, HYBRID):   flags |= _FLAG_CONSENSUS_EXTERNAL

        out = bytearray(MAGIC)
        out.append(flags)
        out.extend(encode_leb128(FORMAT_VERSION))

        if flags & _FLAG_CONSENSUS:
            out.extend(encode_leb128(len(self.consensus)))
            for h, content in self.consensus.items():
                out.append(len(h))
                out.extend(h)
                out.extend(encode_leb128(len(content)))
                out.extend(content)

        if flags & _FLAG_MACROS:
            out.extend(encode_leb128(len(self.macros)))
            for mid, body in self.macros.items():
                out.extend(encode_leb128(mid))
                out.extend(encode_leb128(len(body)))
                out.extend(body)

        if flags & _FLAG_CONSENSUS_EXTERNAL:
            # 32-byte snapshot hash (mandatory)
            assert len(self.external_snapshot_hash) == 32
            out.extend(self.external_snapshot_hash)
            # optional URL hints
            out.extend(encode_leb128(len(self.external_urls)))
            for url in self.external_urls:
                url_b = url.encode('utf-8')
                out.extend(encode_leb128(len(url_b)))
                out.extend(url_b)

        prog = zlib.compress(self.program, 1) if self.compress_program else self.program
        out.extend(encode_leb128(len(prog)))
        out.extend(prog)
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> 'UfiFile':
        """Deserialize from .ufi bytes."""
        assert data[:4] == MAGIC, f"Bad magic: {data[:4]!r}"
        pos = 4
        flags = data[pos]; pos += 1
        version, pos = read_leb128(data, pos)
        assert version == FORMAT_VERSION, f"Unsupported version: {version}"

        consensus: dict[bytes, bytes] = {}
        if flags & _FLAG_CONSENSUS:
            count, pos = read_leb128(data, pos)
            for _ in range(count):
                hlen = data[pos]; pos += 1
                h = bytes(data[pos:pos + hlen]); pos += hlen
                clen, pos = read_leb128(data, pos)
                content = bytes(data[pos:pos + clen]); pos += clen
                consensus[h] = content

        macros: dict[int, bytes] = {}
        if flags & _FLAG_MACROS:
            count, pos = read_leb128(data, pos)
            for _ in range(count):
                mid, pos = read_leb128(data, pos)
                blen, pos = read_leb128(data, pos)
                body = bytes(data[pos:pos + blen]); pos += blen
                macros[mid] = body

        external_snapshot_hash = None
        external_urls: list[str] = []
        if flags & _FLAG_CONSENSUS_EXTERNAL:
            external_snapshot_hash = bytes(data[pos:pos + 32]); pos += 32
            url_count, pos = read_leb128(data, pos)
            for _ in range(url_count):
                url_len, pos = read_leb128(data, pos)
                external_urls.append(data[pos:pos + url_len].decode('utf-8'))
                pos += url_len

        plen, pos = read_leb128(data, pos)
        prog_raw = bytes(data[pos:pos + plen])
        compress_program = bool(flags & _FLAG_COMPRESS)
        program = zlib.decompress(prog_raw) if compress_program else prog_raw

        if flags & _FLAG_CONSENSUS_EXTERNAL:
            source = HYBRID if (flags & _FLAG_CONSENSUS) else EXTERNAL
        else:
            source = EMBEDDED

        return cls(
            program=program, macros=macros, consensus=consensus,
            consensus_source=source,
            external_snapshot_hash=external_snapshot_hash,
            external_urls=external_urls,
            compress_program=compress_program,
        )

    def execute(self, external_consensus: dict[bytes, bytes] | None = None) -> bytes:
        """
        Run the program through the VM and return the reconstructed data.

        For EXTERNAL or HYBRID files, pass the resolved external consensus dict
        as `external_consensus`. The merged dict (external + inline) is used.
        """
        merged = dict(self.consensus)
        if external_consensus:
            merged.update(external_consensus)
        vm = MuCASVM(macros=self.macros, consensus=merged)
        vm.exec(self.program)
        return bytes(vm.out)
