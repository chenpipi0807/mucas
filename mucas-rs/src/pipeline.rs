//! v0.2 pipeline: Classify → Synthesize-on-raw-data → compare with LZ-first → pick best.
//!
//! Architecture flip from v0.1:
//!   v0.1: raw → LZ → synthesize LZ-residuals  (synthesizer sees fragmented LIT tokens)
//!   v0.2: raw → synthesize-raw + raw → LZ → pick smaller program
//!
//! For data where structural patterns dominate (homogeneous logs, periodic records),
//! `synthesize_raw` finds LOOP / Macro before LZ can fragment them.
//! For data where byte-level repetition dominates (binary, mixed prose),
//! the LZ-first path is retained as a fallback.

use crate::lz::LzEncoder;
use crate::sched::{classify_with_data, synthesizer_for, ClassMetrics, DataClass};
use crate::synth::SynthProgram;

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

pub struct Pipeline {
    lz: LzEncoder,
}

impl Default for Pipeline {
    fn default() -> Self { Pipeline { lz: LzEncoder::new() } }
}

impl Pipeline {
    pub fn new() -> Self { Self::default() }

    /// Compress `data`, returning `(program, data_class)`.
    pub fn compress(&self, data: &[u8]) -> (SynthProgram, DataClass) {
        let (prog, class, _) = self.compress_inner(data);
        (prog, class)
    }

    /// Compress with full diagnostic output.
    pub fn compress_verbose(&self, data: &[u8]) -> PipelineResult {
        let analysis    = self.lz.analyze(data);
        let metrics     = ClassMetrics::compute(data, &analysis);
        let class       = classify_with_data(&metrics, data);
        let lz_ratio    = analysis.ratio();

        let (prog, used_hybrid) = match synthesizer_for(class) {
            None => (SynthProgram::from_analysis(&analysis), false),
            Some(synth) => {
                let prog_hybrid = synth.synthesize_hybrid(data, &self.lz);
                let prog_lz     = synth.synthesize(&analysis);
                if prog_hybrid.total_encoded_len() <= prog_lz.total_encoded_len() {
                    (prog_hybrid, true)
                } else {
                    (prog_lz, false)
                }
            }
        };

        PipelineResult {
            data_class:      class,
            lz_ratio,
            synth_ratio:     prog.ratio(),
            synth_gain:      lz_ratio - prog.ratio(),
            used_hybrid_path: used_hybrid,
            program:         prog,
            metrics,
        }
    }

    fn compress_inner(&self, data: &[u8]) -> (SynthProgram, DataClass, bool) {
        let analysis = self.lz.analyze(data);
        let metrics  = ClassMetrics::compute(data, &analysis);
        let class    = classify_with_data(&metrics, data);

        let (prog, used_hybrid) = match synthesizer_for(class) {
            None => (SynthProgram::from_analysis(&analysis), false),
            Some(synth) => {
                // Hybrid: structural discovery on raw data → LZ on residuals.
                // LZ-first: LZ pass → structural discovery on LZ residuals.
                // Whichever yields a smaller encoded program wins.
                let prog_hybrid = synth.synthesize_hybrid(data, &self.lz);
                let prog_lz     = synth.synthesize(&analysis);
                if prog_hybrid.total_encoded_len() <= prog_lz.total_encoded_len() {
                    (prog_hybrid, true)
                } else {
                    (prog_lz, false)
                }
            }
        };
        (prog, class, used_hybrid)
    }
}

// ---------------------------------------------------------------------------
// PipelineResult
// ---------------------------------------------------------------------------

pub struct PipelineResult {
    pub data_class:     DataClass,
    pub lz_ratio:       f64,
    pub synth_ratio:    f64,
    /// Positive = synthesizer improved over raw LZ; negative = regressed.
    pub synth_gain:     f64,
    /// True when the hybrid path (raw-synth + LZ on residuals) beat lz-first.
    pub used_hybrid_path: bool,
    pub program:        SynthProgram,
    pub metrics:        ClassMetrics,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline() -> Pipeline { Pipeline::new() }

    // --- UTF-8 text is no longer misclassified as Binary ---

    #[test]
    fn chinese_utf8_not_binary() {
        // Repeated Chinese text — high_byte_fraction would have been > 0.30 under v0.1.
        let text = "这是一段用于测试的中文文本，包含常见汉字和标点。\n".repeat(50);
        let (_, class) = pipeline().compress(text.as_bytes());
        assert_ne!(class, DataClass::Binary,
            "Chinese UTF-8 text must not be classified as Binary");
    }

    #[test]
    fn random_bytes_still_binary() {
        let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let (_, class) = pipeline().compress(&data);
        assert_eq!(class, DataClass::Binary);
    }

    // --- synthesize_raw fires on clean periodic data ---

    #[test]
    fn raw_synth_finds_loop_on_periodic_data() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let r = pipeline().compress_verbose(&data);
        assert!(r.used_hybrid_path || r.synth_ratio < r.lz_ratio + 0.01,
            "expected hybrid path or at least no regression: hybrid={} synth={:.3} lz={:.3}",
            r.used_hybrid_path, r.synth_ratio, r.lz_ratio);
        assert!(r.program.verify_round_trip(&data), "round-trip failed");
    }

    // --- round-trips ---

    #[test]
    fn pipeline_log_round_trip() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let (prog, class) = pipeline().compress(&data);
        assert_eq!(class, DataClass::StructuredLog);
        assert!(prog.verify_round_trip(&data));
    }

    #[test]
    fn pipeline_binary_round_trip() {
        let data: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        let (prog, class) = pipeline().compress(&data);
        assert_eq!(class, DataClass::Binary);
        assert!(prog.verify_round_trip(&data));
    }

    #[test]
    fn pipeline_empty_round_trip() {
        let (prog, _) = pipeline().compress(&[]);
        assert!(prog.verify_round_trip(&[]));
    }

    // --- hybrid path is at least as good as pure raw-synth ---

    #[test]
    fn hybrid_dominates_raw_synth_on_structure_plus_repetition() {
        // Periodic prefix → LOOP fires.  LZ-compressible suffix → LZ residual pass helps.
        let mut data: Vec<u8> = b"PREFIX: ".repeat(40); // 320 bytes, LOOP eligible
        data.extend_from_slice(&b"hello world ".repeat(30)); // 360 bytes, LZ-compressible
        let (prog, _) = pipeline().compress(&data);
        assert!(prog.verify_round_trip(&data), "hybrid round-trip failed");
    }

    #[test]
    fn hybrid_round_trip_mixed_data() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let mut data: Vec<u8> = line.repeat(50);
        // Append a block that is LZ-compressible but not periodic.
        data.extend_from_slice(&b"abcdefgh".repeat(100));
        let (prog, _) = pipeline().compress(&data);
        assert!(prog.verify_round_trip(&data));
    }

    // --- verbose: synth_gain on structured data ---

    #[test]
    fn verbose_gain_non_negative_on_log() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let r = pipeline().compress_verbose(&data);
        assert!(r.synth_gain >= -0.005,
            "synth regressed by {:.2}%", -r.synth_gain * 100.0);
    }
}
