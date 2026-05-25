"""
μCAS — Micro Compression Assembly
Public API for the reference implementation.
"""
from .vm import MuCASVM, encode_leb128, read_leb128, TRANSFORMS
from .compress import (naive_compress, smart_compress, benchmark,
                       encode_loop, _emit_literal,
                       parse_program_coverage, lz_analyze)
from .consensus import (build_consensus, build_cross_file_consensus,
                        compress_ref_lz, decompress_ref,
                        find_matches, emit_ref, compute_rai,
                        byte_entropy, predict_cross_ref_benefit,
                        RAI_H_THRESHOLD, CROSS_COVERAGE_THRESHOLD)
from .format import UfiFile, EMBEDDED, EXTERNAL, HYBRID
from .corpus import UfcFile

__all__ = [
    # VM
    'MuCASVM', 'encode_leb128', 'read_leb128', 'TRANSFORMS',
    # Compressors
    'naive_compress', 'smart_compress', 'benchmark', 'encode_loop',
    # Consensus + RAI
    'build_consensus', 'build_cross_file_consensus',
    'compress_ref_lz', 'decompress_ref',
    'find_matches', 'emit_ref', 'compute_rai',
    'byte_entropy', 'predict_cross_ref_benefit',
    'RAI_H_THRESHOLD', 'CROSS_COVERAGE_THRESHOLD',
    # Analysis
    'parse_program_coverage', 'lz_analyze',
    # File formats
    'UfiFile', 'EMBEDDED', 'EXTERNAL', 'HYBRID',
    'UfcFile',
]
