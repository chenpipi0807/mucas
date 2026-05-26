//! AdaptiveScheduler — classifies data and dispatches to the right
//! compression path (full synthesis vs. LZ-only fallback).
//!
//! Decision table (heuristic v1, replaceable with a micro-model):
//!
//!   StructuredLog    → LZ + PatternSynthesizer (LOOP / MAP / Macro)
//!   SemiStructured   → LZ + PatternSynthesizer
//!   UnstructuredText → LZ only (prose: structural repetition < threshold)
//!   Binary           → LZ only (already-compressed or encrypted content)

use crate::lz::{LzEncoder, LzAnalysis};
use crate::synth::{PatternSynthesizer, SynthProgram, shannon_entropy_byte};

// ---------------------------------------------------------------------------
// Per-class synthesizer factory
// ---------------------------------------------------------------------------

/// Return a PatternSynthesizer tuned for `class`, or `None` if synthesis
/// should be skipped (Binary path).
pub fn synthesizer_for(class: DataClass) -> Option<PatternSynthesizer> {
    match class {
        // Full synthesis: LOOP + MAP + Macro + SCAN all enabled.
        DataClass::StructuredLog | DataClass::JsonArray | DataClass::SemiStructured =>
            Some(PatternSynthesizer::new()),

        // Conservative: only MAP (catches numeric sequences / timestamps).
        // LOOP and Macro are disabled to avoid structure false-positives in prose.
        DataClass::UnstructuredText =>
            Some(PatternSynthesizer { enable_loop: false, enable_macro: false, enable_map: true, enable_scan: false }),

        // LZ-only — no synthesis overhead on already-compressed or encrypted data.
        DataClass::Binary =>
            None,
    }
}

// ---------------------------------------------------------------------------
// DataClass
// ---------------------------------------------------------------------------

/// Heuristic classification assigned to each input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClass {
    /// Repetitive, homogeneous log / record data: full synthesis on.
    StructuredLog,
    /// NDJSON or JSON-array: full synthesis with JSON SCAN enabled.
    JsonArray,
    /// Semi-structured (JSON, XML, config): synthesis on with REF later.
    SemiStructured,
    /// Natural language prose: LZ-only (low structural repetition).
    UnstructuredText,
    /// Binary / already-compressed: LZ-only.
    Binary,
}

impl DataClass {}

// ---------------------------------------------------------------------------
// ClassMetrics
// ---------------------------------------------------------------------------

/// Observable metrics computed from the data and its LzAnalysis.
#[derive(Debug)]
pub struct ClassMetrics {
    /// Fraction of input bytes in LIT (literal) tokens — higher means harder to compress.
    pub literal_fraction:  f64,
    /// Average match length across all CPY tokens — higher means more compressible.
    pub avg_match_len:     f64,
    /// Mean Jaccard 4-gram similarity between adjacent lines — higher means more structured.
    pub line_similarity:   f64,
    /// Shannon entropy of individual bytes — near 8.0 for random/compressed data.
    pub byte_entropy:      f64,
    /// Fraction of bytes that form valid UTF-8 character sequences (0=binary, 1=valid text).
    pub utf8_valid_ratio:  f64,
}

impl ClassMetrics {
    pub fn compute(data: &[u8], analysis: &LzAnalysis) -> Self {
        ClassMetrics {
            literal_fraction:  analysis.literal_fraction(),
            avg_match_len:     avg_match_len(analysis),
            line_similarity:   line_similarity(data),
            byte_entropy:      shannon_entropy_byte(data),
            utf8_valid_ratio:  utf8_valid_ratio(data),
        }
    }
}

// ---------------------------------------------------------------------------
// Heuristic classifier
// ---------------------------------------------------------------------------

/// Classify `data` using heuristic rules on `ClassMetrics`.
///
/// The thresholds below are calibrated on the μCAS benchmark corpus.
/// Each branch is independently falsifiable: change the data, change the class.
pub fn classify(metrics: &ClassMetrics) -> DataClass {
    classify_from_metrics(metrics, None)
}

/// Full classifier that also accepts the raw first bytes for JSON heuristics.
pub fn classify_with_data(metrics: &ClassMetrics, data: &[u8]) -> DataClass {
    classify_from_metrics(metrics, Some(data))
}

fn classify_from_metrics(metrics: &ClassMetrics, data: Option<&[u8]>) -> DataClass {
    // Binary: near-random byte distribution AND not valid UTF-8 text.
    if metrics.byte_entropy > 7.5 && metrics.utf8_valid_ratio < 0.70 {
        return DataClass::Binary;
    }

    // JSON array / NDJSON: first meaningful byte is `{` or `[{`.
    if let Some(d) = data {
        let first = d.iter().position(|&b| !matches!(b, b' '|b'\t'|b'\r'|b'\n'));
        let looks_json = first.map_or(false, |f| {
            d[f] == b'{' || (d[f] == b'[' && d.get(f + 1) == Some(&b'{'))
        });
        if looks_json && metrics.utf8_valid_ratio > 0.85 {
            return DataClass::JsonArray;
        }
    }

    // Structured log: high line-to-line similarity + LZ already handles most bytes.
    if metrics.line_similarity > 0.60 && metrics.literal_fraction < 0.45 {
        return DataClass::StructuredLog;
    }

    // Unstructured text: mostly literals, short matches, low byte diversity.
    if metrics.literal_fraction > 0.65 && metrics.avg_match_len < 10.0 {
        return DataClass::UnstructuredText;
    }

    // Default: treat as semi-structured — synthesizer is safe to run.
    DataClass::SemiStructured
}

// ---------------------------------------------------------------------------
// Metric helpers
// ---------------------------------------------------------------------------

fn avg_match_len(analysis: &LzAnalysis) -> f64 {
    let (sum, n) = analysis.match_regions()
        .fold((0usize, 0usize), |(s, c), (_, _, l)| (s + l, c + 1));
    if n == 0 { 0.0 } else { sum as f64 / n as f64 }
}

/// Fraction of code-points in `data` that are valid UTF-8 sequences.
/// ASCII bytes always count as valid. Returns 1.0 for empty input.
fn utf8_valid_ratio(data: &[u8]) -> f64 {
    if data.is_empty() { return 1.0; }
    let mut valid = 0usize;
    let mut total = 0usize;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        let seq_len: usize = if b < 0x80 { 1 }
            else if b >= 0xC2 && b <= 0xDF { 2 }
            else if b >= 0xE0 && b <= 0xEF { 3 }
            else if b >= 0xF0 && b <= 0xF4 { 4 }
            else { 0 }; // invalid start byte (0x80-0xC1, 0xF5-0xFF)

        if seq_len == 0 || i + seq_len > data.len() {
            total += 1;
            i += 1;
            continue;
        }
        let all_cont = (1..seq_len).all(|j| data[i + j] & 0xC0 == 0x80);
        total += 1;
        if all_cont {
            valid += 1;
            i += seq_len;
        } else {
            i += 1;
        }
    }
    if total == 0 { 1.0 } else { valid as f64 / total as f64 }
}

/// Mean Jaccard similarity of 4-gram sets between adjacent lines (split on '\n').
/// Range: [0, 1]. Identical lines → 1.0; completely disjoint → 0.0.
fn line_similarity(data: &[u8]) -> f64 {
    let lines: Vec<&[u8]> = data.split(|&b| b == b'\n')
        .filter(|l| l.len() >= 4)
        .collect();
    if lines.len() < 2 { return 0.0; }

    let total: f64 = lines.windows(2).map(|w| jaccard_4gram(w[0], w[1])).sum();
    total / (lines.len() - 1) as f64
}

fn jaccard_4gram(a: &[u8], b: &[u8]) -> f64 {
    use std::collections::HashSet;
    let sa: HashSet<[u8; 4]> = a.windows(4).map(|w| [w[0],w[1],w[2],w[3]]).collect();
    let sb: HashSet<[u8; 4]> = b.windows(4).map(|w| [w[0],w[1],w[2],w[3]]).collect();
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 1.0 } else { inter as f64 / union as f64 }
}

// ---------------------------------------------------------------------------
// AdaptiveScheduler
// ---------------------------------------------------------------------------

/// Top-level pipeline: classify → LZ → (optional) per-class PatternSynthesizer.
pub struct AdaptiveScheduler {
    lz: LzEncoder,
}

impl Default for AdaptiveScheduler {
    fn default() -> Self { AdaptiveScheduler { lz: LzEncoder::new() } }
}

impl AdaptiveScheduler {
    pub fn new() -> Self { Self::default() }

    /// Compress `data`, returning `(SynthProgram, DataClass)`.
    pub fn compress(&self, data: &[u8]) -> (SynthProgram, DataClass) {
        let analysis = self.lz.analyze(data);
        let class    = classify_with_data(&ClassMetrics::compute(data, &analysis), data);
        let prog = match synthesizer_for(class) {
            Some(synth) => synth.synthesize(&analysis),
            None        => SynthProgram::from_analysis(&analysis),
        };
        (prog, class)
    }

    /// Compress with detailed diagnostic output.
    pub fn compress_verbose(&self, data: &[u8]) -> CompressResult {
        let analysis = self.lz.analyze(data);
        let metrics  = ClassMetrics::compute(data, &analysis);
        let class    = classify(&metrics);
        let lz_ratio = analysis.ratio();
        let prog = match synthesizer_for(class) {
            Some(synth) => synth.synthesize(&analysis),
            None        => SynthProgram::from_analysis(&analysis),
        };
        CompressResult {
            data_class:  class,
            lz_ratio,
            synth_ratio: prog.ratio(),
            synth_gain:  lz_ratio - prog.ratio(),
            program:     prog,
            metrics,
        }
    }
}

/// Full output from `compress_verbose`.
pub struct CompressResult {
    pub data_class:  DataClass,
    pub lz_ratio:    f64,
    pub synth_ratio: f64,
    /// Positive = synthesizer improved over raw LZ; negative = regressed.
    pub synth_gain:  f64,
    pub program:     SynthProgram,
    pub metrics:     ClassMetrics,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(data: &[u8]) -> ClassMetrics {
        let a = LzEncoder::new().analyze(data);
        ClassMetrics::compute(data, &a)
    }

    // --- line_similarity ---

    #[test]
    fn identical_lines_have_high_similarity() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(50);
        let sim = line_similarity(&data);
        assert!(sim > 0.90, "expected high similarity, got {sim:.3}");
    }

    #[test]
    fn diverse_lines_have_low_similarity() {
        let data = b"The quick brown fox jumps over the lazy dog.\n\
                     Pack my box with five dozen liquor jugs.\n\
                     How vexingly quick daft zebras jump!\n\
                     The five boxing wizards jump quickly.\n".repeat(5);
        let sim = line_similarity(&data);
        assert!(sim < 0.50, "expected low similarity, got {sim:.3}");
    }

    // --- classify ---

    #[test]
    fn classify_log_data() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let m = metrics(&data);
        let cls = classify(&m);
        assert_eq!(
            cls, DataClass::StructuredLog,
            "line_sim={:.2} lit_frac={:.2} avg_match={:.1}",
            m.line_similarity, m.literal_fraction, m.avg_match_len
        );
    }

    #[test]
    fn classify_binary_data() {
        let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let m = metrics(&data);
        let cls = classify(&m);
        assert_eq!(cls, DataClass::Binary);
    }

    #[test]
    fn classify_diverse_prose_not_structured_log() {
        let data = b"The quick brown fox jumps over the lazy dog.\n\
                     Pack my box with five dozen liquor jugs.\n\
                     How vexingly quick daft zebras jump!\n\
                     The five boxing wizards jump quickly.\n".repeat(10);
        let cls = classify(&metrics(&data));
        assert_ne!(cls, DataClass::StructuredLog);
    }

    // --- AdaptiveScheduler round-trips ---

    #[test]
    fn scheduler_log_round_trip() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let (prog, class) = AdaptiveScheduler::new().compress(&data);
        assert_eq!(class, DataClass::StructuredLog);
        assert!(prog.verify_round_trip(&data), "structured log round-trip failed");
    }

    #[test]
    fn scheduler_binary_round_trip() {
        let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let (prog, class) = AdaptiveScheduler::new().compress(&data);
        assert_eq!(class, DataClass::Binary);
        assert!(prog.verify_round_trip(&data), "binary round-trip failed");
    }

    #[test]
    fn scheduler_empty_round_trip() {
        let (prog, _) = AdaptiveScheduler::new().compress(&[]);
        assert!(prog.verify_round_trip(&[]));
    }

    // --- verbose output ---

    #[test]
    fn verbose_synth_does_not_degrade_log() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let r = AdaptiveScheduler::new().compress_verbose(&data);
        println!(
            "class={:?}  LZ={:.1}%  synth={:.1}%  gain={:+.1}%",
            r.data_class,
            r.lz_ratio * 100.0,
            r.synth_ratio * 100.0,
            r.synth_gain * 100.0,
        );
        // Synthesizer must not make things worse by more than 0.5%.
        assert!(
            r.synth_gain >= -0.005,
            "synthesizer degraded ratio by {:.2}%", -r.synth_gain * 100.0
        );
    }

    #[test]
    fn verbose_binary_path_skips_synthesizer() {
        // Binary path: synthesizer not invoked, lz_ratio == synth_ratio.
        let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let r = AdaptiveScheduler::new().compress_verbose(&data);
        assert_eq!(r.data_class, DataClass::Binary);
        // When synthesizer is skipped, synth_gain should be ~0.
        assert!((r.synth_gain).abs() < 1e-9, "expected no gain on binary path");
    }
}
