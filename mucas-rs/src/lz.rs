//! LZ77 encoder with hash chains — Phase 2 compression baseline.
//!
//! Outputs a stream of `LzToken`s that map directly to μCAS CPY / LIT
//! instructions.  The `LzAnalysis` struct is the interface consumed by all
//! Phase-2 synthesizer modules (macro extractor, pattern miner, MAP detector).

use super::{encode_leb128, VmState, Subs, Consensus};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

pub const WINDOW_SIZE: usize = 1 << 15;  // 32 KB sliding window
pub const MIN_MATCH:   usize = 4;        // minimum back-reference length
pub const MAX_MATCH:   usize = 258;      // maximum back-reference length (DEFLATE compat)

const HASH_BITS: usize = 16;
const HASH_SIZE: usize = 1 << HASH_BITS; // 64 K hash buckets
const HASH_MASK: usize = HASH_SIZE - 1;
const MAX_CHAIN: usize = 64;             // maximum chain hops per position

// ---------------------------------------------------------------------------
// Token type
// ---------------------------------------------------------------------------

/// A single LZ parse token, annotated with its source position in the input.
#[derive(Debug, Clone)]
pub enum LzToken {
    /// Raw literal bytes covering `input[start .. start+data.len()]`.
    Literal { start: usize, data: Vec<u8> },
    /// Back-reference covering `input[start .. start+length]`.
    Match   { start: usize, offset: usize, length: usize },
}

impl LzToken {
    pub fn start(&self) -> usize {
        match self {
            LzToken::Literal { start, .. } | LzToken::Match { start, .. } => *start,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            LzToken::Literal { data, .. } => data.len(),
            LzToken::Match { length, .. } => *length,
        }
    }

    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

// ---------------------------------------------------------------------------
// LzAnalysis — the Phase-2 interface
// ---------------------------------------------------------------------------

/// Complete LZ parse of an input buffer.
///
/// This is the central data structure for Phase-2 synthesis: every
/// higher-level module (REF applicability, LOOP detector, MAP detector)
/// receives an `LzAnalysis` and annotates or replaces its tokens.
pub struct LzAnalysis {
    pub tokens:    Vec<LzToken>,
    pub input_len: usize,
}

impl LzAnalysis {
    /// Convert to a minimal μCAS program containing only LIT and CPY.
    pub fn to_program(&self) -> Vec<u8> {
        let mut prog = Vec::new();
        for tok in &self.tokens {
            match tok {
                LzToken::Literal { data, .. } => {
                    prog.push(0x00); // LIT
                    prog.extend(encode_leb128(data.len() as u32));
                    prog.extend_from_slice(data);
                }
                LzToken::Match { offset, length, .. } => {
                    prog.push(0x01); // CPY
                    prog.extend(encode_leb128(*offset as u32));
                    prog.extend(encode_leb128(*length as u32));
                }
            }
        }
        prog
    }

    /// Run the program through the μCAS VM and verify it reconstructs `original`.
    pub fn verify_round_trip(&self, original: &[u8]) -> bool {
        let prog = self.to_program();
        let mut vm = VmState::new();
        let subs: Subs = HashMap::new();
        let consensus: Consensus = HashMap::new();
        vm.exec(&prog, &subs, &consensus).is_ok() && vm.output == original
    }

    /// Raw program size / input size (lower is better, < 1.0 means compressed).
    pub fn ratio(&self) -> f64 {
        if self.input_len == 0 { return 1.0; }
        self.to_program().len() as f64 / self.input_len as f64
    }

    /// Fraction of input bytes covered by Literal tokens.
    pub fn literal_fraction(&self) -> f64 {
        if self.input_len == 0 { return 1.0; }
        let lit: usize = self.tokens.iter().filter_map(|t| {
            if let LzToken::Literal { data, .. } = t { Some(data.len()) } else { None }
        }).sum();
        lit as f64 / self.input_len as f64
    }

    /// Iterate literal regions as `(start, end)` half-open intervals.
    pub fn literal_regions(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.tokens.iter().filter_map(|t| {
            if let LzToken::Literal { start, data } = t {
                Some((*start, start + data.len()))
            } else {
                None
            }
        })
    }

    /// Iterate match regions as `(start, offset, length)`.
    pub fn match_regions(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.tokens.iter().filter_map(|t| {
            if let LzToken::Match { start, offset, length } = t {
                Some((*start, *offset, *length))
            } else {
                None
            }
        })
    }

    /// Number of back-reference tokens.
    pub fn match_count(&self) -> usize {
        self.tokens.iter().filter(|t| matches!(t, LzToken::Match { .. })).count()
    }

    /// Number of literal tokens.
    pub fn literal_count(&self) -> usize {
        self.tokens.iter().filter(|t| matches!(t, LzToken::Literal { .. })).count()
    }
}

// ---------------------------------------------------------------------------
// LzEncoder
// ---------------------------------------------------------------------------

/// Greedy LZ77 encoder (no lazy matching) backed by hash chains.
///
/// Suitable as a baseline for Phase-2 synthesis.  Lazy matching (+2–5% gain)
/// can be added later without changing the `LzAnalysis` interface.
pub struct LzEncoder {
    pub window:    usize,
    pub min_match: usize,
    pub max_match: usize,
    pub max_chain: usize,
}

impl Default for LzEncoder {
    fn default() -> Self {
        LzEncoder {
            window:    WINDOW_SIZE,
            min_match: MIN_MATCH,
            max_match: MAX_MATCH,
            max_chain: MAX_CHAIN,
        }
    }
}

impl LzEncoder {
    pub fn new() -> Self { Self::default() }

    /// Analyze `data` and return a complete `LzAnalysis`.
    pub fn analyze(&self, data: &[u8]) -> LzAnalysis {
        let n = data.len();
        // head[h] = most recent input position with hash h, or usize::MAX
        let mut head = vec![usize::MAX; HASH_SIZE];
        // prev[pos % window] = previous position in same hash chain
        let mut prev = vec![usize::MAX; self.window];

        let mut tokens: Vec<LzToken> = Vec::new();
        let mut lit_start = 0usize;
        let mut lit_buf: Vec<u8> = Vec::new();
        let mut i = 0usize;

        macro_rules! flush_lits {
            () => {
                if !lit_buf.is_empty() {
                    tokens.push(LzToken::Literal {
                        start: lit_start,
                        data: std::mem::take(&mut lit_buf),
                    });
                }
            };
        }

        while i < n {
            // Fewer than min_match bytes remain — can't form a hash, emit as literals.
            if i + self.min_match > n {
                if lit_buf.is_empty() { lit_start = i; }
                lit_buf.extend_from_slice(&data[i..]);
                break;
            }

            let h = hash4(data, i);

            // Search for the best match *before* inserting position i.
            let best = self.find_match(data, i, n, &head, &prev);

            // Insert position i into the hash chain.
            prev[i % self.window] = head[h];
            head[h] = i;

            match best {
                Some((offset, length)) => {
                    flush_lits!();
                    tokens.push(LzToken::Match { start: i, offset, length });

                    // Insert intermediate positions (skipped by the greedy advance).
                    for k in 1..length {
                        let pos = i + k;
                        if pos + self.min_match <= n {
                            let hk = hash4(data, pos);
                            prev[pos % self.window] = head[hk];
                            head[hk] = pos;
                        }
                    }
                    i += length;
                }
                None => {
                    if lit_buf.is_empty() { lit_start = i; }
                    lit_buf.push(data[i]);
                    i += 1;
                }
            }
        }

        flush_lits!();

        LzAnalysis { tokens, input_len: n }
    }

    fn find_match(
        &self,
        data:   &[u8],
        pos:    usize,
        n:      usize,
        head:   &[usize],
        prev:   &[usize],
    ) -> Option<(usize, usize)> {
        let h = hash4(data, pos);
        let mut best_len    = self.min_match - 1; // must strictly exceed this
        let mut best_offset = 0usize;
        let mut candidate   = head[h];
        let mut hops        = 0usize;

        while candidate != usize::MAX && hops < self.max_chain {
            // candidate should always be < pos (inserted before pos was processed)
            let dist = pos - candidate;
            if dist > self.window { break; }

            let max_len = (n - pos).min(self.max_match);
            let mlen = prefix_match_len(data, candidate, pos, max_len);

            if mlen > best_len {
                best_len    = mlen;
                best_offset = dist;
                if best_len == self.max_match { break; } // can't do better
            }

            candidate = prev[candidate % self.window];
            hops += 1;
        }

        if best_len >= self.min_match {
            Some((best_offset, best_len))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Low-level helpers
// ---------------------------------------------------------------------------

/// Fibonacci hash on the first 4 bytes at `pos`.  Produces a `HASH_BITS`-bit index.
#[inline(always)]
fn hash4(data: &[u8], pos: usize) -> usize {
    let v = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
    (v.wrapping_mul(0x9E37_79B9u32) >> (32 - HASH_BITS)) as usize & HASH_MASK
}

/// Length of the common prefix of `data[a..]` and `data[b..]`, up to `max`.
/// Caller guarantees `a < b` and `b + max <= data.len()`.
#[inline]
fn prefix_match_len(data: &[u8], a: usize, b: usize, max: usize) -> usize {
    // Since a < b, the binding constraint is data.len() - b.
    let limit = max.min(data.len() - b);
    let mut len = 0;
    while len < limit && data[a + len] == data[b + len] {
        len += 1;
    }
    len
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn enc() -> LzEncoder { LzEncoder::new() }

    // --- Round-trip correctness ---

    #[test]
    fn hello_repeat_round_trip() {
        let data: Vec<u8> = b"Hello ".repeat(1000);
        let a = enc().analyze(&data);
        assert!(a.verify_round_trip(&data), "round-trip failed");
    }

    #[test]
    fn log_line_round_trip() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(500);
        let a = enc().analyze(&data);
        assert!(a.verify_round_trip(&data));
    }

    #[test]
    fn short_input_round_trip() {
        for s in [b"".as_slice(), b"x", b"ab", b"abc", b"abcd", b"Hello, world!"] {
            let a = enc().analyze(s);
            assert!(a.verify_round_trip(s), "failed on {:?}", s);
        }
    }

    #[test]
    fn all_unique_bytes_round_trip() {
        let data: Vec<u8> = (0u8..=255).collect();
        let a = enc().analyze(&data);
        assert!(a.verify_round_trip(&data));
        // No matches possible (all 4-byte windows are unique in 0..255)
        assert_eq!(a.match_count(), 0);
    }

    #[test]
    fn tokens_cover_full_input() {
        for seed in [b"abcdefgh".repeat(200), b"xyzxyz".repeat(300)] {
            let a = enc().analyze(&seed);
            let covered: usize = a.tokens.iter().map(|t| t.len()).sum();
            assert_eq!(covered, seed.len(), "coverage mismatch");
        }
    }

    // --- Compression quality ---

    #[test]
    fn hello_repeat_compresses_well() {
        // First chunk as LIT + ~24 CPY(6, 258) tokens ≈ 1.7% raw ratio.
        let data: Vec<u8> = b"Hello ".repeat(1000);
        let a = enc().analyze(&data);
        assert!(a.ratio() < 0.10, "ratio = {:.1}%", a.ratio() * 100.0);
    }

    #[test]
    fn log_line_compresses_well() {
        // ~56-byte LIT then 499 CPY(56, 56) tokens ≈ 5.6% raw ratio.
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(500);
        let a = enc().analyze(&data);
        assert!(a.ratio() < 0.10, "ratio = {:.1}%", a.ratio() * 100.0);
    }

    // --- Overlap / run-length via CPY ---

    #[test]
    fn run_length_via_overlap_cpy() {
        // "aaa...a" (1000 bytes): after LIT "aaaa", CPY(1, 996) covers rest.
        // verify_round_trip exercises the VM's overlap-copy path.
        let data: Vec<u8> = vec![b'a'; 1000];
        let a = enc().analyze(&data);
        assert!(a.verify_round_trip(&data));
        assert!(a.ratio() < 0.02, "ratio = {:.1}%", a.ratio() * 100.0);
    }

    // --- Interface: LzAnalysis accessors ---

    #[test]
    fn match_regions_have_valid_geometry() {
        let data: Vec<u8> = b"xyzxyzxyz".repeat(100);
        let a = enc().analyze(&data);
        for (start, offset, length) in a.match_regions() {
            assert!(offset >= 1,              "zero offset at {start}");
            assert!(offset <= WINDOW_SIZE,    "offset exceeds window");
            assert!(length >= MIN_MATCH,      "match below min");
            assert!(start >= offset,          "source before start of input");
            assert!(start + length <= data.len(), "match past end of input");
        }
    }

    #[test]
    fn literal_and_match_fractions_sum_to_one() {
        let data: Vec<u8> = b"Hello world! ".repeat(300);
        let a = enc().analyze(&data);
        // Every input byte is either literal or matched — no gaps.
        let lit_bytes: usize = a.literal_regions().map(|(s,e)| e-s).sum();
        let match_bytes: usize = a.match_regions().map(|(_,_,l)| l).sum();
        assert_eq!(lit_bytes + match_bytes, data.len());
    }
}
