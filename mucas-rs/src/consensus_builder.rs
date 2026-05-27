//! ConsensusBuilder — cross-file pattern frequency analysis for REF synthesis.
//!
//! Usage (--deep pack flow):
//!   1. For every compressible file, call `builder.feed(data)`.
//!   2. Call `builder.build()` → `Consensus` (HashMap<hash_bytes, pattern_bytes>).
//!   3. Pass the `Consensus` to `ArchiveWriter::with_consensus_options(...)`.
//!      Each entry's MucasFile will embed only the patterns it actually uses.
//!
//! Pattern selection:
//!   - Candidate lengths: 8, 16, 32, 64 bytes (4 fixed sizes).
//!   - Sliding window with stride = len/2 to keep extraction O(n) per file.
//!   - Each distinct pattern counted once per file (not per occurrence).
//!   - Entropy filter: Shannon entropy of pattern ≥ H_THRESHOLD (default 2.5 b/B).
//!   - Coverage filter: appears in ≥ max(2, files_seen/10) distinct files.
//!   - Top N_PATTERNS (default 50) by file-coverage, then by length.
//!   - Hashes: sequential 1-byte keys [0x00]..[0xFE]; [0xFF, k] for k≥255.

use std::collections::{HashMap, HashSet};
use crate::Consensus;

const CANDIDATE_LENGTHS: &[usize] = &[8, 16, 32, 64];
const H_THRESHOLD:   f64   = 2.5;
const N_PATTERNS:    usize = 50;
const MIN_COVERAGE:  usize = 2;

/// Files ≤ this size are skipped during the consensus scan pass.
/// Zlib's 32 KB sliding window already captures all within-file repetition for
/// files this small, so cross-file REF cannot improve on Zlib for them.
pub const MIN_FEED_SIZE: usize = 32 * 1024;

pub struct ConsensusBuilder {
    /// pattern_bytes → number of distinct files containing this pattern.
    counts:      HashMap<Vec<u8>, usize>,
    files_seen:  usize,
}

impl Default for ConsensusBuilder {
    fn default() -> Self {
        ConsensusBuilder { counts: HashMap::new(), files_seen: 0 }
    }
}

impl ConsensusBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn files_seen(&self) -> usize { self.files_seen }

    /// Feed one file's raw bytes into the builder.
    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() { return; }
        self.files_seen += 1;

        let mut seen_in_file: HashSet<Vec<u8>> = HashSet::new();
        for &len in CANDIDATE_LENGTHS {
            if len > data.len() { continue; }
            let stride = (len / 2).max(1);
            let mut i = 0;
            while i + len <= data.len() {
                let pat = data[i..i + len].to_vec();
                if seen_in_file.insert(pat.clone()) {
                    *self.counts.entry(pat).or_insert(0) += 1;
                }
                i += stride;
            }
        }
    }

    /// Build the `Consensus` from patterns passing coverage + entropy filters.
    pub fn build(self) -> Consensus {
        if self.files_seen == 0 { return Consensus::new(); }

        let min_cov = MIN_COVERAGE.max(self.files_seen / 10);
        let mut candidates: Vec<(Vec<u8>, usize)> = self.counts
            .into_iter()
            .filter(|(pat, count)| *count >= min_cov && pattern_entropy(pat) >= H_THRESHOLD)
            .collect();

        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.len().cmp(&a.0.len())));
        candidates.truncate(N_PATTERNS);

        let mut consensus = Consensus::new();
        for (i, (pattern, _)) in candidates.into_iter().enumerate() {
            consensus.insert(encode_hash(i), pattern);
        }
        consensus
    }
}

/// Compact sequential hash key: [i] for i < 255, [0xFF, i-255] otherwise.
fn encode_hash(i: usize) -> Vec<u8> {
    if i < 255 { vec![i as u8] }
    else        { vec![0xFF, (i - 255) as u8] }
}

// ---------------------------------------------------------------------------
// REF gain estimator
// ---------------------------------------------------------------------------

/// Minimum net raw-byte gain required before writing a consensus section.
/// Below this threshold the consensus overhead exceeds expected REF savings.
pub const GAIN_THRESHOLD: isize = 1024;

/// Upper-bound estimate of the net archive-level REF gain in raw (pre-Zlib) bytes.
///
/// Assumes each pattern matches exactly once per file (greedy, non-overlapping).
/// Over-estimates actual savings because overlapping patterns and files without a
/// match reduce real gain.  Used as a fast go/no-go filter — not a precise forecast.
///
/// Returns positive when REF is expected to help, negative when it hurts.
pub fn estimate_ref_net_gain(consensus: &Consensus, files_seen: usize) -> isize {
    // Per-pattern saving: replacing a LIT span with a REF saves (pat_len - REF_size) bytes.
    // REF token encoding: opcode(1) + hash_len(1) + hash → 2 + hash.len() bytes.
    let per_file_savings: usize = consensus
        .iter()
        .map(|(h, p)| p.len().saturating_sub(2 + h.len()))
        .sum();
    let gross = per_file_savings * files_seen;

    // Consensus section on-disk cost: section_len(4) + count(4) + per-entry overhead.
    let overhead: usize = 4 + 4
        + consensus.iter().map(|(h, p)| 1 + h.len() + 4 + p.len()).sum::<usize>();

    gross as isize - overhead as isize
}

/// Shannon entropy of `data` in bits/byte.
pub fn pattern_entropy(data: &[u8]) -> f64 {
    if data.is_empty() { return 0.0; }
    let mut freq = [0u32; 256];
    for &b in data { freq[b as usize] += 1; }
    let n = data.len() as f64;
    freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| { let p = c as f64 / n; -p * p.log2() })
        .sum()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_feed_yields_empty_consensus() {
        let b = ConsensusBuilder::new();
        assert!(b.build().is_empty());
    }

    #[test]
    fn single_file_below_coverage_threshold() {
        let mut b = ConsensusBuilder::new();
        // MIN_COVERAGE is 2; a single file can never meet it.
        b.feed(b"LOGENTRY: 2024-01-01 some structured log data here!!");
        assert!(b.build().is_empty(), "single-file feed must not produce consensus");
    }

    #[test]
    fn two_files_with_shared_pattern_yield_consensus() {
        let pattern = b"STRUCTURED_LOG_ENTRY_PREFIX:".repeat(3); // 84 bytes, used as source
        let shared  = &pattern[..16]; // 16-byte candidate length
        let data1: Vec<u8> = shared.iter().cloned().cycle().take(512).collect();
        let data2: Vec<u8> = shared.iter().cloned().cycle().take(512).collect();

        let mut b = ConsensusBuilder::new();
        b.feed(&data1);
        b.feed(&data2);
        let c = b.build();
        // At least one pattern should be extracted
        assert!(!c.is_empty(), "two files with shared 16-byte pattern must produce consensus");
    }

    #[test]
    fn entropy_filter_rejects_low_entropy_patterns() {
        // All-zero pattern has entropy 0 — must be filtered out.
        let zeros: Vec<u8> = vec![0u8; 64];
        let mut b = ConsensusBuilder::new();
        b.feed(&zeros);
        b.feed(&zeros);
        b.feed(&zeros);
        assert!(b.build().is_empty(), "all-zero pattern must be filtered by entropy");
    }

    #[test]
    fn pattern_entropy_sanity() {
        // Uniform random-like pattern has entropy ≈ 8 b/B; all-same has 0.
        assert!(pattern_entropy(&[0u8; 8]) < 0.01);
        // High-entropy pattern (0,1,2,...,255 repeated)
        let hi: Vec<u8> = (0u8..=255).collect();
        assert!(pattern_entropy(&hi) > 7.9);
    }

    #[test]
    fn build_returns_at_most_n_patterns() {
        // Feed 60 distinct 8-byte patterns appearing in 5 files each.
        let mut b = ConsensusBuilder::new();
        for _ in 0..5 {
            let mut data: Vec<u8> = Vec::new();
            for i in 0u8..60 {
                // each 8-byte pattern: [i, i^0xAA, i^0x55, i^0x33, i^0xCC, i^0xFF, 0x01, 0x02]
                data.push(i);
                data.push(i ^ 0xAA);
                data.push(i ^ 0x55);
                data.push(i ^ 0x33);
                data.push(i ^ 0xCC);
                data.push(i ^ 0xFF);
                data.push(0x01);
                data.push(0x02);
            }
            b.feed(&data);
        }
        let c = b.build();
        assert!(c.len() <= N_PATTERNS, "consensus must not exceed N_PATTERNS={N_PATTERNS}");
    }

    // --- estimate_ref_net_gain ---

    #[test]
    fn empty_consensus_has_negative_net_gain() {
        let c = Consensus::new();
        // overhead = 4+4 = 8 bytes, gross = 0 → net = -8
        assert!(estimate_ref_net_gain(&c, 10) < 0);
    }

    #[test]
    fn single_short_pattern_few_files_is_negative() {
        // 8-byte pattern, hash=[0x00] (1 byte): REF saves 8-(2+1)=5 bytes per match.
        // gross = 5 × 3 files = 15; overhead = 4+4+(1+1+4+8) = 22 → net = -7
        let mut c = Consensus::new();
        c.insert(vec![0x00], b"ABCDEFGH".to_vec()); // 8-byte pattern
        let net = estimate_ref_net_gain(&c, 3);
        assert!(net < 0, "short pattern in few files should be net-negative, got {net}");
    }

    #[test]
    fn large_pattern_many_files_exceeds_threshold() {
        // 64-byte pattern, hash=[0x00]: REF saves 64-3=61 bytes/file.
        // gross = 61 × 40 = 2440; overhead = 4+4+(1+1+4+64) = 78 → net = 2362
        let mut c = Consensus::new();
        c.insert(vec![0x00], (0u8..64).collect::<Vec<u8>>());
        let net = estimate_ref_net_gain(&c, 40);
        assert!(net >= GAIN_THRESHOLD, "64B pattern × 40 files should exceed threshold, got {net}");
    }

    #[test]
    fn estimate_matches_observed_corpus_order_of_magnitude() {
        // Simulate the log corpus: 18 patterns of 64B + 32 patterns of 32B, 40 files.
        let mut c = Consensus::new();
        for i in 0u8..18 {
            c.insert(vec![i], (0u8..64).collect::<Vec<u8>>());
        }
        for i in 18u8..50 {
            c.insert(vec![i], (0u8..32).collect::<Vec<u8>>());
        }
        let net = estimate_ref_net_gain(&c, 40);
        // Actual observed gain was ~7716 B; estimate should be > GAIN_THRESHOLD.
        assert!(net >= GAIN_THRESHOLD,
            "corpus-like consensus should exceed threshold, got {net}");
        // Estimate is an upper bound, so it should exceed observed ~7716 B.
        assert!(net > 7000, "estimate should be above observed gain, got {net}");
    }
}
