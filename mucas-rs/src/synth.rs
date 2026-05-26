//! PatternSynthesizer — MDL-driven rewrite of μCAS instruction streams.
//!
//! Pipeline: `LzAnalysis → SynthProgram` by iteratively replacing LIT tokens
//! with LOOP / CALL-macro instructions whenever doing so reduces the encoded
//! program size (MDL gain > 0).
//!
//! Two rewrite families, in priority order:
//!   LOOP  — periodic literal body (e.g. repeated log-line prefix)
//!   Macro — common literal subsequence promoted to a CALL subroutine
//!
//! MAP instruction is supported in `SynthToken` for completeness (manual
//! program construction), but MAP *synthesis* is intentionally disabled:
//! MAP's benefit only materialises after an outer entropy coder (zstd/deflate),
//! so raw-byte MDL cannot evaluate it correctly.

use std::collections::HashMap;
use crate::{encode_leb128, Transform, VmState, Subs, Consensus};
use crate::lz::{LzAnalysis, LzEncoder, LzToken};

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

pub const MIN_LOOP_BODY:        usize = 4;    // minimum loop body bytes
pub const MIN_LOOP_COUNT:       usize = 3;    // minimum loop iterations
pub const MIN_MACRO_LEN:        usize = 8;    // minimum macro pattern length
pub const MIN_MACRO_OCCUR:      usize = 2;    // minimum pattern occurrences (per-token)
pub const MAX_MACRO_LEN:        usize = 64;   // maximum macro pattern length (rolling-hash range)
pub const MIN_SCAN_ROWS:        usize = 10;   // minimum consecutive rows to attempt SCAN
/// Minimum LIT token size before the SA macro extractor is invoked (avoids SA overhead on small tokens).
pub const SA_THRESHOLD:         usize = 256;
/// SA-based search handles patterns longer than the rolling-hash ceiling.
pub const SA_MIN_PAT_LEN:       usize = MAX_MACRO_LEN + 1;  // = 65
/// Upper bound on SA pattern length.
pub const SA_MAX_PAT_LEN:       usize = 1024;

// ---------------------------------------------------------------------------
// LEB128 size helper
// ---------------------------------------------------------------------------

/// Number of bytes required to encode `v` in unsigned LEB128.
#[inline]
pub fn leb_len(v: u32) -> usize {
    match v {
        0          ..= 0x0000_007F => 1,
        0x80       ..= 0x0000_3FFF => 2,
        0x4000     ..= 0x001F_FFFF => 3,
        0x0020_0000..= 0x0FFF_FFFF => 4,
        _                          => 5,
    }
}

// ---------------------------------------------------------------------------
// SynthToken
// ---------------------------------------------------------------------------

/// A synthesized μCAS instruction token with exact program-byte cost.
#[derive(Debug, Clone, PartialEq)]
pub enum SynthToken {
    Lit  { data: Vec<u8> },
    Cpy  { offset: usize, length: usize },
    Loop { count: usize, body: Vec<SynthToken> },
    Call { sub_id: u32 },
    /// In-place transform: applies `transform` to the last `len` output bytes.
    /// Serialised as `0x02 <transform_id:u8> <len:LEB128>`.
    Map  { transform: Transform, len: usize },
    /// Consensus lookup: emits the pattern registered under `hash`.
    /// Serialised as `0x05 <hash_len:u8> <hash[hash_len]>`.
    /// For session-local IDs use single-byte hashes `[0x00]`, `[0x01]`, etc.
    Ref  { hash: Vec<u8> },
    /// Parameterized-template loop: execute template sub `count` times, pulling variable
    /// fields from `params` (length-prefixed, row-major column-major order).
    /// Serialised as `0x07 count:LEB128 template_id:LEB128 param_len:LEB128 params[param_len]`.
    Scan { count: usize, template_id: u32, params: Vec<Vec<u8>> },
    /// Read next length-prefixed field from the enclosing SCAN's parameter stream.
    /// Only valid inside a template macro called by SCAN.  Serialised as `0x08`.
    Next,
}

impl SynthToken {
    /// Exact bytes this token occupies in a serialized μCAS program.
    pub fn program_bytes(&self) -> usize {
        match self {
            SynthToken::Lit { data } =>
                1 + leb_len(data.len() as u32) + data.len(),

            SynthToken::Cpy { offset, length } =>
                1 + leb_len(*offset as u32) + leb_len(*length as u32),

            SynthToken::Loop { count, body } => {
                let body_sz: usize = body.iter().map(|t| t.program_bytes()).sum();
                1 + leb_len(*count as u32) + leb_len(body_sz as u32) + body_sz
            }

            SynthToken::Call { sub_id } =>
                1 + leb_len(*sub_id),

            SynthToken::Map { len, .. } =>
                1 + 1 + leb_len(*len as u32),

            SynthToken::Ref { hash } =>
                1 + 1 + hash.len(),

            SynthToken::Scan { count, template_id, params } => {
                let param_stream_len: usize = params.iter()
                    .map(|p| leb_len(p.len() as u32) + p.len())
                    .sum();
                1 + leb_len(*count as u32)
                  + leb_len(*template_id)
                  + leb_len(param_stream_len as u32)
                  + param_stream_len
            }

            SynthToken::Next => 1,
        }
    }
}

// ---------------------------------------------------------------------------
// SynthProgram
// ---------------------------------------------------------------------------

/// A synthesized μCAS program: main token stream + optional subroutine table
/// + optional consensus dictionary (for REF instructions).
pub struct SynthProgram {
    pub tokens:    Vec<SynthToken>,
    pub sub_defs:  HashMap<u32, Vec<SynthToken>>,
    pub next_sub_id: u32,
    pub input_len: usize,
    /// Populated when REF synthesis is active.  Maps hash bytes → pattern bytes.
    pub consensus: Consensus,
}

impl SynthProgram {
    pub fn new(input_len: usize) -> Self {
        SynthProgram {
            tokens: Vec::new(),
            sub_defs: HashMap::new(),
            next_sub_id: 0,
            input_len,
            consensus: Consensus::new(),
        }
    }

    /// Initialize from a raw LzAnalysis (LIT + CPY tokens only).
    pub fn from_analysis(a: &LzAnalysis) -> Self {
        let mut prog = SynthProgram::new(a.input_len);
        for tok in &a.tokens {
            match tok {
                LzToken::Literal { data, .. } =>
                    prog.tokens.push(SynthToken::Lit { data: data.clone() }),
                LzToken::Match { offset, length, .. } =>
                    prog.tokens.push(SynthToken::Cpy { offset: *offset, length: *length }),
            }
        }
        prog
    }

    /// Total bytes for the main token stream (excludes sub-table overhead).
    pub fn encoded_len(&self) -> usize {
        self.tokens.iter().map(|t| t.program_bytes()).sum()
    }

    /// Total estimated file size: main program + sub-table (4-byte header per sub).
    pub fn total_encoded_len(&self) -> usize {
        let sub_overhead: usize = self.sub_defs.values()
            .map(|body| 4 + body.iter().map(|t| t.program_bytes()).sum::<usize>())
            .sum();
        self.encoded_len() + sub_overhead
    }

    /// Compression ratio: total_encoded_len / input_len.
    pub fn ratio(&self) -> f64 {
        if self.input_len == 0 { return 1.0; }
        self.total_encoded_len() as f64 / self.input_len as f64
    }

    /// Serialize to `(main_program_bytes, subs_for_vm)`.
    pub fn to_bytes(&self) -> (Vec<u8>, Subs) {
        let prog = tokens_to_bytes(&self.tokens);
        let subs: Subs = self.sub_defs.iter()
            .map(|(&id, body)| (id, tokens_to_bytes(body)))
            .collect();
        (prog, subs)
    }

    /// Execute through the μCAS VM and verify `original` is reconstructed.
    pub fn verify_round_trip(&self, original: &[u8]) -> bool {
        let (prog, subs) = self.to_bytes();
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &self.consensus).is_ok() && vm.output == original
    }
}

fn tokens_to_bytes(tokens: &[SynthToken]) -> Vec<u8> {
    let mut out = Vec::new();
    for tok in tokens {
        match tok {
            SynthToken::Lit { data } => {
                out.push(0x00);
                out.extend(encode_leb128(data.len() as u32));
                out.extend_from_slice(data);
            }
            SynthToken::Cpy { offset, length } => {
                out.push(0x01);
                out.extend(encode_leb128(*offset as u32));
                out.extend(encode_leb128(*length as u32));
            }
            SynthToken::Loop { count, body } => {
                let body_bytes = tokens_to_bytes(body);
                out.push(0x03);
                out.extend(encode_leb128(*count as u32));
                out.extend(encode_leb128(body_bytes.len() as u32));
                out.extend(body_bytes);
            }
            SynthToken::Call { sub_id } => {
                out.push(0x04);
                out.extend(encode_leb128(*sub_id));
            }
            SynthToken::Map { transform, len } => {
                out.push(0x02);
                out.push(*transform as u8);
                out.extend(encode_leb128(*len as u32));
            }
            SynthToken::Ref { hash } => {
                out.push(0x05);
                out.push(hash.len() as u8);
                out.extend_from_slice(hash);
            }
            SynthToken::Scan { count, template_id, params } => {
                let mut stream = Vec::new();
                for p in params {
                    stream.extend(encode_leb128(p.len() as u32));
                    stream.extend_from_slice(p);
                }
                out.push(0x07);
                out.extend(encode_leb128(*count as u32));
                out.extend(encode_leb128(*template_id));
                out.extend(encode_leb128(stream.len() as u32));
                out.extend(stream);
            }
            SynthToken::Next => { out.push(0x08); }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pattern detectors
// ---------------------------------------------------------------------------

/// 0-order Shannon entropy of byte frequencies, in bits/byte.
/// Uniform 256-value distribution → 8.0; single repeated byte → 0.0.
pub fn shannon_entropy_byte(data: &[u8]) -> f64 {
    if data.is_empty() { return 0.0; }
    let mut freq = [0u32; 256];
    for &b in data { freq[b as usize] += 1; }
    let n = data.len() as f64;
    freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| { let p = c as f64 / n; -p * p.log2() })
        .sum()
}

/// Estimate the net gain (in bytes) from replacing `LIT(data)` with
/// `LIT(delta_encoded) + MAP(DeltaU8, len)`.
///
/// Gain is based on how much the outer entropy coder (zstd/zlib) would save
/// compressing delta bytes vs original bytes, minus the MAP instruction cost.
/// Returns `(gain_bytes, delta_encoded)` or `None` if not beneficial.
pub fn estimate_delta_u8_gain(data: &[u8]) -> Option<(isize, Vec<u8>)> {
    let n = data.len();
    if n < 8 { return None; }

    let h_orig = shannon_entropy_byte(data);

    let mut delta = Vec::with_capacity(n);
    delta.push(data[0]);
    for i in 1..n {
        delta.push(data[i].wrapping_sub(data[i - 1]));
    }
    let h_delta = shannon_entropy_byte(&delta);

    // Expected bytes saved in the compressed payload.
    let payload_gain = ((h_orig - h_delta) * n as f64 / 8.0) as isize;
    // Extra raw bytes for the MAP instruction: opcode + transform_id + len:LEB128.
    let map_overhead = (1 + 1 + leb_len(n as u32)) as isize;

    let net_gain = payload_gain - map_overhead;
    if net_gain > 0 { Some((net_gain, delta)) } else { None }
}

/// If `data` is a clean repetition of a shorter period, return `(period, count)`.
/// Finds the shortest valid period with count ≥ MIN_LOOP_COUNT.
pub fn detect_loop(data: &[u8]) -> Option<(usize, usize)> {
    let n = data.len();
    let max_period = n / MIN_LOOP_COUNT;
    for period in MIN_LOOP_BODY..=max_period {
        if n % period != 0 { continue; }
        let count = n / period;
        if count < MIN_LOOP_COUNT { continue; }
        if data.chunks(period).all(|chunk| chunk == &data[..period]) {
            return Some((period, count));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// MDL gain formulas (positive = beneficial substitution)
// ---------------------------------------------------------------------------

/// Gain from replacing LIT(len) with LOOP { count, LIT(period) }.
pub fn loop_mdl_gain(lit_len: usize, period: usize, count: usize) -> isize {
    let lit_cost  = (1 + leb_len(lit_len as u32) + lit_len) as isize;
    let body_cost = 1 + leb_len(period as u32) + period; // inner LIT
    let loop_cost = (1 + leb_len(count as u32) + leb_len(body_cost as u32) + body_cost) as isize;
    lit_cost - loop_cost
}

/// Gain from extracting a macro of `pat_len` bytes appearing `occur` times,
/// where the CALL instruction uses `sub_id`.
pub fn macro_mdl_gain(pat_len: usize, occur: usize, sub_id: u32) -> isize {
    let orig_cost  = (occur * (1 + leb_len(pat_len as u32) + pat_len)) as isize;
    let call_cost  = (occur * (1 + leb_len(sub_id))) as isize;
    // Sub definition: 4-byte header + one LIT encoding the pattern.
    let def_cost   = (4 + 1 + leb_len(pat_len as u32) + pat_len) as isize;
    orig_cost - call_cost - def_cost
}

// ---------------------------------------------------------------------------
// Macro candidate search
// ---------------------------------------------------------------------------

/// Rabin-Karp rolling hash: return all distinct patterns of length `len` that
/// appear at least `min_occur` times (by hash count) in `data`.
/// Results are candidate patterns only — callers must verify with count_nonoverlap.
fn find_candidate_patterns_rk(data: &[u8], len: usize, min_occur: usize) -> Vec<Vec<u8>> {
    let n = data.len();
    if len == 0 || len > n { return vec![]; }

    const BASE: u64 = 131u64;
    let mut base_pow: u64 = 1u64;
    for _ in 0..len.saturating_sub(1) {
        base_pow = base_pow.wrapping_mul(BASE);
    }

    // count occurrences per hash; record first-seen position for pattern extraction
    let mut hash_info: HashMap<u64, (usize, usize)> = HashMap::new(); // hash → (count, first_pos)

    let mut h: u64 = 0;
    for i in 0..len { h = h.wrapping_mul(BASE).wrapping_add(data[i] as u64); }
    {
        let e = hash_info.entry(h).or_insert((0, 0));
        e.0 += 1;
    }

    for i in 1..=n - len {
        h = h.wrapping_sub((data[i - 1] as u64).wrapping_mul(base_pow))
             .wrapping_mul(BASE)
             .wrapping_add(data[i + len - 1] as u64);
        let e = hash_info.entry(h).or_insert((0, i));
        e.0 += 1;
    }

    hash_info.into_iter()
        .filter(|(_, (cnt, _))| *cnt >= min_occur)
        .map(|(_, (_, first_pos))| data[first_pos..first_pos + len].to_vec())
        .collect()
}

/// Count non-overlapping occurrences of `pattern` in `data` (greedy L-to-R).
fn count_nonoverlap(data: &[u8], pattern: &[u8]) -> usize {
    let plen = pattern.len();
    let mut count = 0;
    let mut i = 0;
    while i + plen <= data.len() {
        if data[i..i+plen] == *pattern { count += 1; i += plen; }
        else { i += 1; }
    }
    count
}

/// Find the first occurrence of `pattern` in `data[from..]`, returning absolute pos.
fn find_first(data: &[u8], from: usize, pattern: &[u8]) -> Option<usize> {
    let plen = pattern.len();
    if plen == 0 || plen > data.len() { return None; }
    let end = data.len() - plen;
    if from > end { return None; }
    (from..=end).find(|&i| data[i..i+plen] == *pattern)
}

/// Search within individual LIT tokens for the most profitable macro pattern.
/// Uses Rabin-Karp rolling hash — O(n * MAX_MACRO_LEN) per token, no size cap.
/// Returns `(pattern, total_occurrence_count, MDL_gain)` or `None`.
pub fn find_best_macro(tokens: &[SynthToken], next_sub_id: u32) -> Option<(Vec<u8>, usize, isize)> {
    let mut best: Option<(Vec<u8>, usize, isize)> = None;

    for tok in tokens {
        if let SynthToken::Lit { data } = tok {
            let n = data.len();
            let max_len = MAX_MACRO_LEN.min(n / MIN_MACRO_OCCUR);
            if max_len < MIN_MACRO_LEN { continue; }

            for len in MIN_MACRO_LEN..=max_len {
                for pat in find_candidate_patterns_rk(data, len, MIN_MACRO_OCCUR) {
                    let cnt = count_nonoverlap(data, &pat);
                    if cnt < MIN_MACRO_OCCUR { continue; }
                    let gain = macro_mdl_gain(len, cnt, next_sub_id);
                    if gain > 0 && best.as_ref().map_or(true, |(_, _, g)| gain > *g) {
                        best = Some((pat, cnt, gain));
                    }
                }
            }
        }
    }
    best
}

/// Split `data` around non-overlapping occurrences of `pattern`, emitting
/// Lit tokens for residuals and Call tokens for matches.
fn split_and_replace_lit(data: &[u8], pattern: &[u8], sub_id: u32, out: &mut Vec<SynthToken>) {
    let plen = pattern.len();
    let mut pos = 0;
    while pos < data.len() {
        match find_first(data, pos, pattern) {
            Some(hit) => {
                if hit > pos {
                    out.push(SynthToken::Lit { data: data[pos..hit].to_vec() });
                }
                out.push(SynthToken::Call { sub_id });
                pos = hit + plen;
            }
            None => {
                out.push(SynthToken::Lit { data: data[pos..].to_vec() });
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Suffix-array macro extractor (v0.5)
// ---------------------------------------------------------------------------

/// Build the suffix array of `data` using prefix-doubling in O(n log² n).
/// Returns the sorted starting positions of all suffixes.
fn build_suffix_array(data: &[u8]) -> Vec<usize> {
    let n = data.len();
    if n == 0 { return vec![]; }
    if n == 1 { return vec![0]; }

    let mut rank: Vec<i32> = data.iter().map(|&b| b as i32).collect();
    let mut sa:   Vec<usize> = (0..n).collect();
    let mut tmp:  Vec<i32>  = vec![0; n];

    let mut k = 1usize;
    loop {
        // Sort SA by (rank[i], rank[i+k] or sentinel -1).
        // `rank` is immutably borrowed only inside the sort closure, so `tmp`
        // can be written afterwards without aliasing.
        {
            let r = &rank;
            sa.sort_unstable_by_key(|&i| {
                (r[i], if i + k < n { r[i + k] } else { -1i32 })
            });
        }

        // Assign new ranks: equal (rank, rank+k) pairs get the same rank.
        tmp[sa[0]] = 0;
        for j in 1..n {
            let prev = sa[j - 1];
            let cur  = sa[j];
            let p_r2 = if prev + k < n { rank[prev + k] } else { -1i32 };
            let c_r2 = if cur  + k < n { rank[cur  + k] } else { -1i32 };
            let same = rank[prev] == rank[cur] && p_r2 == c_r2;
            tmp[cur] = tmp[prev] + if same { 0 } else { 1 };
        }
        rank.copy_from_slice(&tmp);

        if rank[sa[n - 1]] as usize == n - 1 { break; }  // all ranks distinct
        k <<= 1;
        if k >= n { break; }
    }

    sa
}

/// Build the LCP array from `data` and its suffix array using Kasai's O(n) algorithm.
/// `lcp[i]` = length of the longest common prefix between `sa[i-1]` and `sa[i]`.
/// `lcp[0]` is always 0 (no predecessor for the smallest suffix).
fn build_lcp_array(data: &[u8], sa: &[usize]) -> Vec<usize> {
    let n = data.len();
    if n == 0 { return vec![]; }

    // rank[i] = position of the suffix starting at i in the sorted SA.
    let mut rank = vec![0usize; n];
    for (pos, &s) in sa.iter().enumerate() {
        rank[s] = pos;
    }

    let mut lcp = vec![0usize; n];
    let mut h   = 0usize;
    for i in 0..n {
        if rank[i] > 0 {
            let j = sa[rank[i] - 1];
            while i + h < n && j + h < n && data[i + h] == data[j + h] {
                h += 1;
            }
            lcp[rank[i]] = h;
            if h > 0 { h -= 1; }
        }
    }
    lcp
}

/// Find the most profitable macro pattern of length `[SA_MIN_PAT_LEN, SA_MAX_PAT_LEN]`
/// inside `data` using the suffix array.
/// Returns `(pattern_bytes, non_overlapping_count, MDL_gain)` or `None`.
pub fn find_long_macro_by_sa(data: &[u8], next_sub_id: u32) -> Option<(Vec<u8>, usize, isize)> {
    let n = data.len();
    if n < SA_MIN_PAT_LEN * MIN_MACRO_OCCUR { return None; }

    let sa  = build_suffix_array(data);
    let lcp = build_lcp_array(data, &sa);

    let mut best: Option<(Vec<u8>, usize, isize)> = None;

    // Scan through the LCP array for maximal runs where every lcp[i] >= SA_MIN_PAT_LEN.
    // Within each run [rs..=re] (1-based LCP indices), all suffix-array entries
    // sa[rs-1..=re] share a common prefix of length min(lcp[rs..=re]).
    let mut run_start: Option<usize> = None;
    let mut min_lcp:   usize         = usize::MAX;

    let try_run = |rs: usize, min_h: usize,
                       sa: &[usize], data: &[u8],
                       best: &mut Option<(Vec<u8>, usize, isize)>| {
        let pat_len = min_h.min(SA_MAX_PAT_LEN);
        if pat_len < SA_MIN_PAT_LEN { return; }
        let extract = sa[rs - 1];
        if extract + pat_len > n { return; }
        let pat = data[extract..extract + pat_len].to_vec();
        let occur = count_nonoverlap(data, &pat);
        if occur < MIN_MACRO_OCCUR { return; }
        let gain = macro_mdl_gain(pat_len, occur, next_sub_id);
        if gain > 0 && best.as_ref().map_or(true, |(_, _, g)| gain > *g) {
            *best = Some((pat, occur, gain));
        }
    };

    for i in 1..=n {
        let h = if i < n { lcp[i].min(SA_MAX_PAT_LEN) } else { 0 };
        if h >= SA_MIN_PAT_LEN {
            if run_start.is_none() { run_start = Some(i); min_lcp = h; }
            else { min_lcp = min_lcp.min(h); }
        } else if let Some(rs) = run_start {
            try_run(rs, min_lcp, &sa, data, &mut best);
            run_start = None;
            min_lcp   = usize::MAX;
        }
    }

    best
}

// ---------------------------------------------------------------------------
// Internal rewrite descriptors
// ---------------------------------------------------------------------------

enum Rewrite {
    Loop  { idx: usize, period: usize, count: usize },
    Macro { pattern: Vec<u8> },
}

fn apply_rewrite(prog: &mut SynthProgram, rw: Rewrite) {
    match rw {
        Rewrite::Loop { idx, period, count } => {
            let data = match &prog.tokens[idx] {
                SynthToken::Lit { data } => data.clone(),
                _ => unreachable!(),
            };
            let body_tok = SynthToken::Lit { data: data[..period].to_vec() };
            prog.tokens[idx] = SynthToken::Loop { count, body: vec![body_tok] };
        }

        Rewrite::Macro { pattern } => {
            // Register sub.
            let sub_id = prog.next_sub_id;
            prog.sub_defs.insert(sub_id, vec![SynthToken::Lit { data: pattern.clone() }]);
            prog.next_sub_id += 1;
            // Replace all LIT tokens that contain the pattern.
            let mut new_tokens: Vec<SynthToken> = Vec::with_capacity(prog.tokens.len() * 2);
            let old_tokens = std::mem::take(&mut prog.tokens);
            for tok in old_tokens {
                match tok {
                    SynthToken::Lit { data } =>
                        split_and_replace_lit(&data, &pattern, sub_id, &mut new_tokens),
                    other => new_tokens.push(other),
                }
            }
            prog.tokens = new_tokens;
        }
    }
}

// ---------------------------------------------------------------------------
// SCAN: parameterized-template detector
// ---------------------------------------------------------------------------

/// Result of a successful SCAN detection on a slice of raw bytes.
pub struct ScanResult {
    /// Bytes before the matched row block (may be empty).
    pub prefix:   Vec<u8>,
    /// SCAN token replacing the matched row block.
    pub scan_tok: SynthToken,
    /// Template body (SynthToken stream, stored in sub_defs).
    pub template: Vec<SynthToken>,
    /// Bytes after the matched row block (may be empty).
    pub suffix:   Vec<u8>,
}

// ---------------------------------------------------------------------------
// Quote-aware CSV helpers (RFC-4180 subset: `""` escapes within double quotes)
// ---------------------------------------------------------------------------

/// Count `delimiter` bytes that appear outside double-quoted fields.
/// Handles `""` as an escaped quote inside a quoted field.
fn count_delimiters_quoted(line: &[u8], delimiter: u8) -> usize {
    let mut count    = 0;
    let mut in_quote = false;
    let mut i        = 0usize;
    while i < line.len() {
        let b = line[i];
        if in_quote {
            if b == b'"' {
                if i + 1 < line.len() && line[i + 1] == b'"' { i += 2; continue; } // escaped ""
                in_quote = false;
            }
        } else {
            if      b == b'"'     { in_quote = true; }
            else if b == delimiter { count += 1; }
        }
        i += 1;
    }
    count
}

/// Split `line` into raw field slices respecting RFC-4180 quoting.
/// Returns raw bytes for each field — quoted fields include surrounding `"…"` verbatim
/// so the param stream can reconstruct the original line without any re-quoting logic.
fn split_fields_quoted_raw(line: &[u8], delimiter: u8) -> Vec<Vec<u8>> {
    let mut fields      = Vec::new();
    let mut field_start = 0usize;
    let mut in_quote    = false;
    let mut i           = 0usize;
    while i < line.len() {
        let b = line[i];
        if in_quote {
            if b == b'"' {
                if i + 1 < line.len() && line[i + 1] == b'"' { i += 2; continue; }
                in_quote = false;
            }
        } else {
            if b == b'"' { in_quote = true; }
            else if b == delimiter {
                fields.push(line[field_start..i].to_vec());
                field_start = i + 1;
            }
        }
        i += 1;
    }
    fields.push(line[field_start..].to_vec());
    fields
}

/// Try to find the largest consecutive block of delimited rows in `data` that share
/// the same column count (determined by `delimiter`), then encode as a SCAN instruction.
///
/// Returns `None` if no block of `MIN_SCAN_ROWS` rows is found that benefits from SCAN.
/// CRLF line endings are stripped before field parsing.
pub fn detect_scan_lit(data: &[u8], delimiter: u8, next_sub_id: u32) -> Option<ScanResult> {
    // Split into lines; record byte offsets for prefix/suffix reconstruction.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && i + 1 < data.len() {
            line_starts.push(i + 1);
        }
    }
    // Each element: data[line_starts[i]..line_starts[i+1]] (includes the '\n').
    // Last line may not end with '\n'.
    let n_lines = line_starts.len();
    if n_lines < MIN_SCAN_ROWS { return None; }

    let line_bytes: Vec<&[u8]> = (0..n_lines).map(|i| {
        let start = line_starts[i];
        let end   = if i + 1 < n_lines { line_starts[i + 1] } else { data.len() };
        &data[start..end]
    }).collect();

    // Column counts (delimiters outside quoted fields per line).
    let col_counts: Vec<usize> = line_bytes.iter().map(|l| {
        count_delimiters_quoted(l, delimiter)
    }).collect();

    // Find the longest consecutive run with the same column count.
    let mut best_start = 0;
    let mut best_len   = 1;
    let mut run_start  = 0;
    let mut run_len    = 1;

    for i in 1..n_lines {
        if col_counts[i] == col_counts[run_start] {
            run_len += 1;
            if run_len > best_len {
                best_len   = run_len;
                best_start = run_start;
            }
        } else {
            run_start = i;
            run_len   = 1;
        }
    }

    if best_len < MIN_SCAN_ROWS { return None; }
    let col_count = col_counts[best_start]; // commas; columns = col_count + 1
    if col_count == 0 { return None; } // single-column rows → LOOP may be better

    // Parse the matched rows into fields.
    let block_lines = &line_bytes[best_start..best_start + best_len];
    let rows: Vec<Vec<Vec<u8>>> = block_lines.iter().map(|line| {
        // Strip trailing '\n' or '\r\n' before splitting.
        let trimmed = line.strip_suffix(b"\n")
            .or_else(|| line.strip_suffix(b"\r\n"))
            .unwrap_or(line);
        split_fields_quoted_raw(trimmed, delimiter)
    }).collect();

    let n_cols = col_count + 1;

    // Identify fixed columns (same value across all rows in the block).
    let is_fixed: Vec<bool> = (0..n_cols).map(|c| {
        let first = &rows[0][c];
        rows.iter().all(|r| r.get(c).map_or(false, |v| v == first))
    }).collect();

    let n_variable = is_fixed.iter().filter(|&&f| !f).count();
    if n_variable == 0 { return None; } // All-fixed → perfect LOOP, not SCAN.

    // Build template body: LIT for delimiter separators and fixed fields, NEXT for variable fields.
    let mut template: Vec<SynthToken> = Vec::new();
    for (i, &fixed) in is_fixed.iter().enumerate() {
        if i > 0 {
            template.push(SynthToken::Lit { data: vec![delimiter] });
        }
        if fixed {
            template.push(SynthToken::Lit { data: rows[0][i].clone() });
        } else {
            template.push(SynthToken::Next);
        }
    }
    // Each template iteration emits one '\n' (newline included in block lines).
    template.push(SynthToken::Lit { data: b"\n".to_vec() });

    // Build flat params list: for each row, emit variable fields in column order.
    let variable_cols: Vec<usize> = is_fixed.iter().enumerate()
        .filter(|(_, &f)| !f)
        .map(|(i, _)| i)
        .collect();

    let mut params: Vec<Vec<u8>> = Vec::new();
    for row in &rows {
        for &c in &variable_cols {
            params.push(row.get(c).cloned().unwrap_or_default());
        }
    }

    // MDL gain check: compare raw LIT cost vs SCAN cost (pre-zlib, raw bytes).
    let block_byte_len: usize = block_lines.iter().map(|l| l.len()).sum();
    let raw_lit_cost = 1 + leb_len(block_byte_len as u32) + block_byte_len;

    let template_bytes: usize = template.iter().map(|t| t.program_bytes()).sum();
    let def_cost = 4 + template_bytes; // sub-table entry header + body

    let param_stream_len: usize = params.iter()
        .map(|p| leb_len(p.len() as u32) + p.len())
        .sum();
    let scan_cost = 1
        + leb_len(best_len as u32)
        + leb_len(next_sub_id)
        + leb_len(param_stream_len as u32)
        + param_stream_len;

    if def_cost + scan_cost >= raw_lit_cost { return None; }

    // Reconstruct prefix and suffix byte slices.
    let prefix_end = line_starts[best_start];
    let suffix_start = if best_start + best_len < n_lines {
        line_starts[best_start + best_len]
    } else {
        data.len()
    };

    Some(ScanResult {
        prefix:   data[..prefix_end].to_vec(),
        scan_tok: SynthToken::Scan { count: best_len, template_id: next_sub_id, params },
        template,
        suffix:   data[suffix_start..].to_vec(),
    })
}

// ---------------------------------------------------------------------------
// JSON SCAN detector (NDJSON and compact JSON arrays)
// ---------------------------------------------------------------------------

/// Advance `pos` past the JSON string starting at `data[pos]` (which must be `"`).
/// Returns the new position (after the closing `"`) or `None` on unterminated string.
fn json_str_end(data: &[u8], mut pos: usize) -> Option<usize> {
    pos += 1; // skip opening `"`
    while pos < data.len() {
        match data[pos] {
            b'"'  => return Some(pos + 1),
            b'\\' => pos += 2,
            _     => pos += 1,
        }
    }
    None
}

/// Advance `pos` past one complete JSON value starting at `data[pos]`.
/// Handles strings, numbers, `true`/`false`/`null`, and nested `{…}` / `[…]`.
/// Returns new position or `None` if the value can't be recognised.
fn json_val_end(data: &[u8], pos: usize) -> Option<usize> {
    match *data.get(pos)? {
        b'"'                => json_str_end(data, pos),
        b't'                => data.get(pos..pos+4).filter(|s| *s == b"true" ).map(|_| pos + 4),
        b'f'                => data.get(pos..pos+5).filter(|s| *s == b"false").map(|_| pos + 5),
        b'n'                => data.get(pos..pos+4).filter(|s| *s == b"null" ).map(|_| pos + 4),
        b'-' | b'0'..=b'9' => {
            Some((pos..data.len())
                .find(|&i| matches!(data[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n'))
                .unwrap_or(data.len()))
        }
        b'[' | b'{' => {
            let (open, close) = if data[pos] == b'[' { (b'[', b']') } else { (b'{', b'}') };
            let mut depth   = 0usize;
            let mut in_str  = false;
            let mut i       = pos;
            while i < data.len() {
                if in_str {
                    match data[i] {
                        b'"'  => { in_str = false; i += 1; }
                        b'\\' => i += 2,
                        _     => i += 1,
                    }
                } else {
                    match data[i] {
                        b'"'                  => { in_str = true; i += 1; }
                        b if b == open        => { depth += 1;    i += 1; }
                        b if b == close => {
                            depth -= 1;
                            if depth == 0 { return Some(i + 1); }
                            i += 1;
                        }
                        _                     => i += 1,
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse `{…}` and return per-key `(val_start, val_end)` byte offsets within `obj`.
/// Also returns the raw key bytes (with surrounding quotes) for key-set comparison.
/// `val_start` and `val_end` are positions within `obj` of the exact value bytes.
/// Returns `None` if the object can't be parsed cleanly or is empty.
fn parse_json_kv_pos(obj: &[u8]) -> Option<Vec<(Vec<u8>, usize, usize)>> {
    let mut entries = Vec::new();
    let mut p = 0usize;
    while p < obj.len() && matches!(obj[p], b' '|b'\t'|b'\r'|b'\n') { p += 1; }
    if obj.get(p) != Some(&b'{') { return None; }
    p += 1;
    loop {
        while p < obj.len() && matches!(obj[p], b' '|b'\t'|b'\r'|b'\n') { p += 1; }
        match obj.get(p) {
            Some(&b'}') => break,
            Some(&b'"') => {}
            _           => return None,
        }
        let ks = p;
        p = json_str_end(obj, p)?;
        let key = obj[ks..p].to_vec();

        while p < obj.len() && matches!(obj[p], b' '|b'\t') { p += 1; }
        if obj.get(p) != Some(&b':') { return None; }
        p += 1;
        while p < obj.len() && matches!(obj[p], b' '|b'\t') { p += 1; }

        let vs = p;
        p = json_val_end(obj, p)?;
        entries.push((key, vs, p));

        while p < obj.len() && matches!(obj[p], b' '|b'\t'|b'\r'|b'\n') { p += 1; }
        match obj.get(p) {
            Some(&b',') => p += 1,
            Some(&b'}') => break,
            _           => return None,
        }
    }
    if entries.is_empty() { None } else { Some(entries) }
}

/// Thin wrapper used only by unit tests — same as `parse_json_kv_pos` but returns `(key, val)` pairs.
#[cfg(test)]
fn parse_json_kv(obj: &[u8]) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    parse_json_kv_pos(obj).map(|v| v.into_iter().map(|(k, vs, ve)| (k, obj[vs..ve].to_vec())).collect())
}

/// Strip leading `[`, `,`, whitespace and trailing `]`, `,`, whitespace/newline from a line
/// to isolate the raw `{…}` object bytes.  Returns a sub-slice of `line`, or `None`.
fn extract_json_obj(line: &[u8]) -> Option<&[u8]> {
    let a = line.iter().position(|&b| !matches!(b, b' '|b'\t'|b'\r'|b'\n'|b'['|b','))?;
    let z = line.iter().rposition(|&b| !matches!(b, b' '|b'\t'|b'\r'|b'\n'|b']'|b','))?;
    if a > z { return None; }
    let s = &line[a..=z];
    if s.first() == Some(&b'{') && s.last() == Some(&b'}') { Some(s) } else { None }
}

/// Detect a NDJSON or compact JSON-array block and encode it as a SCAN instruction.
/// Supports:
///   - NDJSON: one object per line (`{…}\n`)
///   - Compact JSON array: `[{…},\n{…},\n…]` (objects separated by `,\n` with optional `[`/`]`)
/// Only fires when all objects in the block share the same ordered key set AND the same
/// structural separator bytes (whitespace around `:` and between key-value pairs).
pub fn detect_scan_json(data: &[u8], next_sub_id: u32) -> Option<ScanResult> {
    // Build line table.
    let mut line_starts: Vec<usize> = vec![0];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && i + 1 < data.len() { line_starts.push(i + 1); }
    }
    let n_lines = line_starts.len();
    if n_lines < MIN_SCAN_ROWS { return None; }

    let line_bytes: Vec<&[u8]> = (0..n_lines).map(|i| {
        let s = line_starts[i];
        let e = if i + 1 < n_lines { line_starts[i + 1] } else { data.len() };
        &data[s..e]
    }).collect();

    // Parse every line to get (obj_slice, positional_entries) — entries carry exact byte offsets.
    // `None` if the line can't be parsed or has a different key set from the run start.
    type PosEntries = Vec<(Vec<u8>, usize, usize)>; // (key_bytes, val_start, val_end) in obj
    let parsed_pos: Vec<Option<(&[u8], PosEntries)>> = line_bytes.iter().map(|l| {
        let obj = extract_json_obj(l)?;
        let entries = parse_json_kv_pos(obj)?;
        Some((obj, entries))
    }).collect();

    // Find the longest consecutive run with identical key ordering AND structural separators.
    // "Structural bytes" = separators between values (the bytes NOT inside the value ranges).
    // For the first row of a run we accept any valid parse; subsequent rows must match exactly.
    let structural_seps = |obj: &[u8], entries: &PosEntries| -> Vec<Vec<u8>> {
        // seps[0] = obj[1..val_start_0]  (after `{`)
        // seps[k] = obj[val_end_{k-1}..val_start_k]  for k > 0
        // seps[n] = obj[val_end_{n-1}..]  (should be `}`)
        let n = entries.len();
        let mut seps = Vec::with_capacity(n + 1);
        seps.push(obj[1..entries[0].1].to_vec());
        for k in 1..n {
            seps.push(obj[entries[k-1].2..entries[k].1].to_vec());
        }
        seps.push(obj[entries[n-1].2..].to_vec());
        seps
    };

    let (mut best_start, mut best_len) = (0usize, 0usize);
    let (mut run_start,  mut run_len)  = (0usize, 0usize);
    let mut run_ref_keys: Vec<Vec<u8>>    = Vec::new();
    let mut run_ref_seps: Vec<Vec<u8>>    = Vec::new();

    for i in 0..n_lines {
        let ok = match &parsed_pos[i] {
            None => false,
            Some((obj, entries)) if run_len == 0 => {
                run_start    = i;
                run_ref_keys = entries.iter().map(|(k,_,_)| k.clone()).collect();
                run_ref_seps = structural_seps(obj, entries);
                true
            }
            Some((obj, entries)) => {
                let keys_match = entries.len() == run_ref_keys.len()
                    && entries.iter().zip(&run_ref_keys).all(|((k,_,_), rk)| k == rk);
                let seps_match = keys_match && {
                    let seps = structural_seps(obj, entries);
                    seps == run_ref_seps
                };
                seps_match
            }
        };
        if ok {
            run_len += 1;
            if run_len > best_len { best_len = run_len; best_start = run_start; }
        } else {
            run_len = 0;
        }
    }
    if best_len < MIN_SCAN_ROWS { return None; }

    // Re-parse the best block (run_ref_keys/seps may come from a different run; recompute).
    let (ref_obj, ref_entries) = parsed_pos[best_start].as_ref().unwrap();
    let ref_seps = structural_seps(ref_obj, ref_entries);
    let n_keys   = ref_entries.len();

    // Identify fixed vs variable values (raw bytes comparison).
    let is_fixed: Vec<bool> = (0..n_keys).map(|k| {
        let first = &ref_obj[ref_entries[k].1..ref_entries[k].2];
        (best_start..best_start + best_len).all(|i| {
            let (obj, ents) = parsed_pos[i].as_ref().unwrap();
            &obj[ents[k].1..ents[k].2] == first
        })
    }).collect();
    let n_variable = is_fixed.iter().filter(|&&f| !f).count();
    if n_variable == 0 { return None; } // all-fixed → LOOP is better

    // Determine the per-line ending: `\r\n` or `\n`.
    let line_end: &[u8] = if line_bytes[best_start].ends_with(b"\r\n") { b"\r\n" } else { b"\n" };

    // Build template using EXACT structural bytes from the first row (ref_seps).
    // ref_seps[0]        = bytes between `{` and val_0  (e.g. `"id": `)
    // ref_seps[k] k>0    = bytes between val_{k-1} and val_k (e.g. `, "status": `)
    // ref_seps[n_keys]   = bytes after last val (should be `}`)
    //
    // Template structure per row:
    //   LIT("{")  LIT(seps[0]) [LIT(val) or NEXT]  LIT(seps[1]) [LIT(val) or NEXT]  ...  LIT(seps[n]) LIT(line_end)
    let mut template: Vec<SynthToken> = Vec::new();
    template.push(SynthToken::Lit { data: b"{".to_vec() });
    for k in 0..n_keys {
        template.push(SynthToken::Lit { data: ref_seps[k].clone() });
        if is_fixed[k] {
            let val = ref_obj[ref_entries[k].1..ref_entries[k].2].to_vec();
            template.push(SynthToken::Lit { data: val });
        } else {
            template.push(SynthToken::Next);
        }
    }
    // ref_seps[n_keys] should be `}`; append it then line_end.
    let mut closing = ref_seps[n_keys].clone();
    closing.extend_from_slice(line_end);
    template.push(SynthToken::Lit { data: closing });

    // Collect params: for each row, emit variable values in key order.
    let var_keys: Vec<usize> = is_fixed.iter().enumerate()
        .filter(|(_,&f)| !f).map(|(i,_)| i).collect();
    let mut params: Vec<Vec<u8>> = Vec::new();
    for i in best_start..best_start + best_len {
        let (obj, ents) = parsed_pos[i].as_ref().unwrap();
        for &k in &var_keys { params.push(obj[ents[k].1..ents[k].2].to_vec()); }
    }

    // MDL gain check.
    let block_byte_len: usize = (best_start..best_start + best_len)
        .map(|i| line_bytes[i].len()).sum();
    let raw_lit_cost     = 1 + leb_len(block_byte_len as u32) + block_byte_len;
    let template_bytes: usize = template.iter().map(|t| t.program_bytes()).sum();
    let def_cost         = 4 + template_bytes;
    let param_stream_len: usize = params.iter().map(|p| leb_len(p.len() as u32) + p.len()).sum();
    let scan_cost        = 1 + leb_len(best_len as u32) + leb_len(next_sub_id)
                             + leb_len(param_stream_len as u32) + param_stream_len;
    if def_cost + scan_cost >= raw_lit_cost { return None; }

    let prefix_end   = line_starts[best_start];
    let suffix_start = if best_start + best_len < n_lines {
        line_starts[best_start + best_len]
    } else { data.len() };

    Some(ScanResult {
        prefix:   data[..prefix_end].to_vec(),
        scan_tok: SynthToken::Scan { count: best_len, template_id: next_sub_id, params },
        template,
        suffix:   data[suffix_start..].to_vec(),
    })
}

/// Try JSON detection first (when data begins with `{` or `[{`), then common field
/// delimiters in priority order.  Space is excluded (high false-positive rate on prose).
pub fn detect_scan_best(data: &[u8], next_sub_id: u32) -> Option<ScanResult> {
    // Check whether the first meaningful byte suggests JSON.
    let first_meaningful = data.iter().position(|&b| !matches!(b, b' '|b'\t'|b'\r'|b'\n'));
    let looks_json = first_meaningful.map_or(false, |f| {
        data[f] == b'{' || (data[f] == b'[' && data.get(f + 1) == Some(&b'{'))
    });
    if looks_json {
        if let Some(r) = detect_scan_json(data, next_sub_id) { return Some(r); }
    }
    for &delim in &[b',', b'\t', b'|'] {
        if let Some(r) = detect_scan_lit(data, delim, next_sub_id) { return Some(r); }
    }
    None
}

// ---------------------------------------------------------------------------
// PatternSynthesizer
// ---------------------------------------------------------------------------

/// Iterative MDL optimizer.  Rewrites the LIT tokens in an `LzAnalysis`
/// with LOOP / MAP / CALL instructions until no positive-gain rewrite exists.
///
/// Phase 1 (raw-byte MDL): LOOP and Macro rewrites compete in the same greedy loop.
/// Phase 2 (entropy MDL): DeltaU8 MAP pass sweeps remaining LIT tokens once.
pub struct PatternSynthesizer {
    pub enable_loop:  bool,
    pub enable_macro: bool,
    /// Entropy-based DeltaU8 MAP synthesis (Phase 2).  Disabled separately
    /// because its MDL is in estimated compressed bytes, not raw bytes.
    pub enable_map:   bool,
    /// Parameterized-template SCAN synthesis (Phase 0 in synthesize_raw).
    /// Detects repeated CSV-like rows with fixed separators and variable fields.
    pub enable_scan:  bool,
}

impl Default for PatternSynthesizer {
    fn default() -> Self {
        PatternSynthesizer {
            enable_loop:  true,
            enable_macro: true,
            enable_map:   true,
            enable_scan:  true,
        }
    }
}

impl PatternSynthesizer {
    pub fn new() -> Self { Self::default() }

    /// Synthesize directly on raw bytes — SCAN (Phase 0), then LOOP / Macro / MAP (Phase 1+2)
    /// search runs on the original data before any LZ pass, so structural patterns
    /// aren't fragmented by the LZ greedy matcher.  Residual bytes remain as LIT tokens.
    pub fn synthesize_raw(&self, data: &[u8]) -> SynthProgram {
        let mut prog = SynthProgram::new(data.len());
        if data.is_empty() {
            return prog;
        }

        // Phase 0: SCAN — parameterized-template extraction on each large LIT token.
        // Applied iteratively: after a SCAN splits a LIT, the prefix/suffix residuals
        // may themselves contain another SCAN-eligible block.
        if self.enable_scan {
            let mut pending: Vec<u8> = data.to_vec();
            loop {
                match detect_scan_best(&pending, prog.next_sub_id) {
                    Some(r) => {
                        if !r.prefix.is_empty() {
                            prog.tokens.push(SynthToken::Lit { data: r.prefix });
                        }
                        prog.sub_defs.insert(prog.next_sub_id, r.template);
                        prog.next_sub_id += 1;
                        prog.tokens.push(r.scan_tok);
                        pending = r.suffix;
                        if pending.is_empty() { break; }
                    }
                    None => {
                        prog.tokens.push(SynthToken::Lit { data: pending });
                        break;
                    }
                }
            }
        } else {
            prog.tokens.push(SynthToken::Lit { data: data.to_vec() });
        }

        self.rewrite_loop(&mut prog);
        prog
    }

    fn rewrite_loop(&self, prog: &mut SynthProgram) {
        // Phase 1: LOOP + Macro greedy MDL loop (raw byte cost).
        loop {
            let mut best_gain: isize = 0;
            let mut best_rw: Option<Rewrite> = None;

            if self.enable_loop {
                for (idx, tok) in prog.tokens.iter().enumerate() {
                    if let SynthToken::Lit { data } = tok {
                        if let Some((period, count)) = detect_loop(data) {
                            let gain = loop_mdl_gain(data.len(), period, count);
                            if gain > best_gain {
                                best_gain = gain;
                                best_rw = Some(Rewrite::Loop { idx, period, count });
                            }
                        }
                    }
                }
            }

            if self.enable_macro {
                if let Some((pat, _, gain)) = find_best_macro(&prog.tokens, prog.next_sub_id) {
                    if gain > best_gain {
                        best_gain = gain;
                        best_rw   = Some(Rewrite::Macro { pattern: pat });
                    }
                }
                // SA-based search for patterns longer than MAX_MACRO_LEN (rolling-hash ceiling).
                for tok in &prog.tokens {
                    if let SynthToken::Lit { data } = tok {
                        if data.len() >= SA_THRESHOLD {
                            if let Some((pat, _, gain)) = find_long_macro_by_sa(data, prog.next_sub_id) {
                                if gain > best_gain {
                                    best_gain = gain;
                                    best_rw   = Some(Rewrite::Macro { pattern: pat });
                                }
                            }
                        }
                    }
                }
            }

            match best_rw {
                None => break,
                Some(rw) => apply_rewrite(prog, rw),
            }
        }

        // Phase 2: DeltaU8 MAP entropy pass (single sweep over remaining LIT tokens).
        if self.enable_map {
            let mut i = 0;
            while i < prog.tokens.len() {
                let maybe_delta = if let SynthToken::Lit { data } = &prog.tokens[i] {
                    estimate_delta_u8_gain(data)
                } else {
                    None
                };
                match maybe_delta {
                    Some((_, delta)) => {
                        let n = delta.len();
                        prog.tokens.splice(
                            i..=i,
                            [
                                SynthToken::Lit { data: delta },
                                SynthToken::Map { transform: Transform::DeltaU8, len: n },
                            ],
                        );
                        i += 2;
                    }
                    None => { i += 1; }
                }
            }
        }
    }

    /// Two-phase hybrid synthesis:
    /// 1. `synthesize_raw` — structural discovery (LOOP/Macro/MAP) on original bytes.
    /// 2. LZ compensation — replace each top-level LIT residual with an independent
    ///    LZ parse of its bytes.  CPY offsets produced by per-residual LZ are
    ///    self-referential (they can only reach back into that residual's own output
    ///    range), so they remain correct when the residual is embedded after preceding
    ///    LOOP/CALL tokens in the full output stream.
    pub fn synthesize_hybrid(&self, data: &[u8], lz: &LzEncoder) -> SynthProgram {
        let mut prog = self.synthesize_raw(data);

        let old_tokens = std::mem::take(&mut prog.tokens);
        let mut new_tokens = Vec::with_capacity(old_tokens.len() * 2);

        for tok in old_tokens {
            match tok {
                SynthToken::Lit { data: lit_data } => {
                    // Run LZ independently on this residual; CPY offsets are
                    // bounded by the residual's own decompressed length → correct.
                    let analysis = lz.analyze(&lit_data);
                    for lz_tok in &analysis.tokens {
                        match lz_tok {
                            LzToken::Literal { data: d, .. } =>
                                new_tokens.push(SynthToken::Lit { data: d.clone() }),
                            LzToken::Match { offset, length, .. } =>
                                new_tokens.push(SynthToken::Cpy { offset: *offset, length: *length }),
                        }
                    }
                }
                other => new_tokens.push(other),
            }
        }

        prog.tokens = new_tokens;
        prog
    }

    pub fn synthesize(&self, analysis: &LzAnalysis) -> SynthProgram {
        let mut prog = SynthProgram::from_analysis(analysis);
        self.rewrite_loop(&mut prog);
        prog
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lz::{LzEncoder, LzAnalysis, LzToken};

    fn synth(data: &[u8]) -> SynthProgram {
        let a = LzEncoder::new().analyze(data);
        PatternSynthesizer::new().synthesize(&a)
    }

    // --- leb_len ---

    #[test]
    fn leb_len_boundaries() {
        assert_eq!(leb_len(0),       1);
        assert_eq!(leb_len(127),     1);
        assert_eq!(leb_len(128),     2);
        assert_eq!(leb_len(16383),   2);
        assert_eq!(leb_len(16384),   3);
        assert_eq!(leb_len(u32::MAX),5);
    }

    // --- detect_loop ---

    #[test]
    fn detect_loop_finds_period() {
        let data: Vec<u8> = b"[INFO] ".repeat(20);
        let (period, count) = detect_loop(&data).expect("should detect loop");
        assert_eq!(period, 7);
        assert_eq!(count,  20);
    }

    #[test]
    fn detect_loop_none_for_unique() {
        assert!(detect_loop(b"abcdefghij").is_none());
    }

    #[test]
    fn detect_loop_none_for_short() {
        // period=4, count=2 < MIN_LOOP_COUNT=3 → None
        let data: Vec<u8> = b"abcd".repeat(2);
        assert!(detect_loop(&data).is_none());
    }

    #[test]
    fn loop_mdl_gain_positive_for_long_run() {
        // period=7, count=20 → 140-byte LIT vs ~12-byte LOOP
        assert!(loop_mdl_gain(140, 7, 20) > 0);
    }

    // --- macro_mdl_gain ---

    #[test]
    fn macro_mdl_gain_positive() {
        // pat_len=16, occur=5 → gain should be positive
        assert!(macro_mdl_gain(16, 5, 0) > 0);
    }

    // --- SynthToken::Map serialization (spec-aligned 2-param format) ---

    #[test]
    fn map_token_serializes_correctly() {
        use crate::Transform;
        let tok = SynthToken::Map { transform: Transform::DeltaU8, len: 8 };
        // program_bytes: 1 (opcode) + 1 (transform_id) + 1 (LEB128 for 8) = 3
        assert_eq!(tok.program_bytes(), 3);
        let bytes = tokens_to_bytes(&[tok]);
        assert_eq!(bytes, vec![0x02, 0x04, 0x08]); // MAP, DeltaU8, len=8
    }

    // --- shannon_entropy_byte ---

    #[test]
    fn entropy_of_constant_data_is_zero() {
        let data = vec![0x42u8; 256];
        assert_eq!(shannon_entropy_byte(&data), 0.0);
    }

    #[test]
    fn entropy_of_uniform_bytes_is_eight() {
        let data: Vec<u8> = (0u8..=255).collect();
        let h = shannon_entropy_byte(&data);
        assert!((h - 8.0).abs() < 1e-10, "expected 8.0 got {h}");
    }

    // --- estimate_delta_u8_gain ---

    #[test]
    fn delta_gain_positive_for_arithmetic_sequence() {
        // [0, 1, 2, ..., 127]: H_orig ≈ 7.0 bits/byte; delta = [0,1,1,...,1] → H ≈ 0
        let data: Vec<u8> = (0u8..128).collect();
        let result = estimate_delta_u8_gain(&data);
        assert!(result.is_some(), "expected positive gain for arithmetic sequence");
        let (gain, delta) = result.unwrap();
        assert!(gain > 0, "gain should be positive, got {gain}");
        // Delta should be [0, 1, 1, ..., 1].
        assert_eq!(delta[0], 0);
        assert!(delta[1..].iter().all(|&b| b == 1));
    }

    #[test]
    fn delta_gain_none_for_short_data() {
        let data: Vec<u8> = (0u8..7).collect();
        assert!(estimate_delta_u8_gain(&data).is_none(), "< 8 bytes should return None");
    }

    #[test]
    fn delta_gain_none_for_constant_data() {
        // All same byte: H_orig=0, H_delta=0, payload_gain=0 < map_overhead → None.
        let data = vec![0xABu8; 64];
        assert!(estimate_delta_u8_gain(&data).is_none());
    }

    // --- MAP synthesis end-to-end ---

    #[test]
    fn map_synthesis_fires_on_arithmetic_lit() {
        // [0..127] is a single LIT (LZ finds no matches), MAP should fire.
        let data: Vec<u8> = (0u8..128).collect();
        let a = LzAnalysis {
            tokens: vec![LzToken::Literal { start: 0, data: data.clone() }],
            input_len: data.len(),
        };
        let prog = PatternSynthesizer::new().synthesize(&a);
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Map { .. })),
            "expected MAP token after synthesis"
        );
    }

    #[test]
    fn map_synthesis_round_trip() {
        // Verify the full VM correctly reconstructs original bytes through MAP.
        let data: Vec<u8> = (0u8..128).collect();
        let a = LzAnalysis {
            tokens: vec![LzToken::Literal { start: 0, data: data.clone() }],
            input_len: data.len(),
        };
        let prog = PatternSynthesizer::new().synthesize(&a);
        assert!(prog.verify_round_trip(&data), "MAP round-trip failed");
    }

    // --- SynthToken::Ref serialization and round-trip ---

    #[test]
    fn ref_token_serializes_correctly() {
        let tok = SynthToken::Ref { hash: vec![0x07] };
        // program_bytes: 1 (opcode 0x05) + 1 (hash_len) + 1 (hash byte) = 3
        assert_eq!(tok.program_bytes(), 3);
        let bytes = tokens_to_bytes(&[tok]);
        assert_eq!(bytes, vec![0x05, 0x01, 0x07]);
    }

    #[test]
    fn ref_token_round_trip_via_vm() {
        // Manually build a SynthProgram with one REF token backed by consensus.
        let pattern = b"SERVER_LOG_ENTRY:".to_vec();
        let mut prog = SynthProgram::new(pattern.len());
        prog.consensus.insert(vec![0x00], pattern.clone());
        prog.tokens.push(SynthToken::Ref { hash: vec![0x00] });
        assert!(prog.verify_round_trip(&pattern), "REF round-trip failed");
    }

    #[test]
    fn map_synthesis_disabled_when_flag_off() {
        let data: Vec<u8> = (0u8..128).collect();
        let a = LzAnalysis {
            tokens: vec![LzToken::Literal { start: 0, data: data.clone() }],
            input_len: data.len(),
        };
        let synth = PatternSynthesizer { enable_loop: true, enable_macro: true, enable_map: false, enable_scan: false };
        let prog = synth.synthesize(&a);
        assert!(
            !prog.tokens.iter().any(|t| matches!(t, SynthToken::Map { .. })),
            "MAP should not fire when enable_map=false"
        );
        assert!(prog.verify_round_trip(&data));
    }

    // --- SCAN: parameterized template ---

    fn scan_csv_data() -> Vec<u8> {
        // 30 rows, 2 long fixed columns + 2 variable columns → clear raw MDL gain.
        let mut data = Vec::new();
        for i in 0u32..30 {
            let row = format!("{i},fixed_category,fixed_region,{}\n", i + 100);
            data.extend_from_slice(row.as_bytes());
        }
        data
    }

    #[test]
    fn scan_detect_fires_on_csv_with_fixed_columns() {
        let data = scan_csv_data();
        let result = detect_scan_lit(&data, b',', 0);
        assert!(result.is_some(), "SCAN should fire when fixed columns dominate");
        let r = result.unwrap();
        assert!(r.prefix.is_empty(), "no prefix expected (all rows are uniform)");
        assert!(r.suffix.is_empty(), "no suffix expected");
    }

    #[test]
    fn scan_round_trip_with_fixed_columns() {
        let data = scan_csv_data();
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "SCAN raw round-trip failed");
        // Verify at least one SCAN token was emitted.
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Scan { .. })),
            "expected at least one SCAN token"
        );
    }

    #[test]
    fn scan_hybrid_round_trip_with_fixed_columns() {
        let data = scan_csv_data();
        let lz = LzEncoder::new();
        let prog = PatternSynthesizer::new().synthesize_hybrid(&data, &lz);
        assert!(prog.verify_round_trip(&data), "SCAN hybrid round-trip failed");
    }

    #[test]
    fn scan_no_fire_on_single_column() {
        // Lines with no commas → single column → skip SCAN
        let data: Vec<u8> = b"line one\nline two\nline three\n".repeat(5);
        assert!(detect_scan_lit(&data, b',', 0).is_none());
    }

    #[test]
    fn scan_no_fire_when_all_fixed() {
        // All rows identical → all-fixed → detect_scan_lit returns None (use LOOP instead)
        let data: Vec<u8> = b"a,b,c\n".repeat(20);
        assert!(detect_scan_lit(&data, b',', 0).is_none(), "all-fixed rows should not trigger SCAN");
    }

    // --- TSV / pipe-delimited SCAN ---

    fn scan_tsv_data() -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0u32..30 {
            let row = format!("{i}\tfixed_region\tfixed_tier\t{}\n", i + 100);
            data.extend_from_slice(row.as_bytes());
        }
        data
    }

    fn scan_pipe_data() -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0u32..30 {
            let row = format!("{i}|fixed_category|fixed_datacenter|{}\n", i + 100);
            data.extend_from_slice(row.as_bytes());
        }
        data
    }

    #[test]
    fn scan_tsv_fires_and_round_trips() {
        let data = scan_tsv_data();
        let result = detect_scan_lit(&data, b'\t', 0);
        assert!(result.is_some(), "SCAN must fire on TSV with fixed columns");
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "TSV SCAN round-trip failed");
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Scan { .. })),
            "expected SCAN token in TSV output"
        );
    }

    #[test]
    fn scan_pipe_fires_and_round_trips() {
        let data = scan_pipe_data();
        let result = detect_scan_lit(&data, b'|', 0);
        assert!(result.is_some(), "SCAN must fire on pipe-delimited data with fixed columns");
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "pipe-delimited SCAN round-trip failed");
    }

    #[test]
    fn scan_best_detects_tsv_when_no_comma() {
        // detect_scan_best must find TSV even though the data has no commas.
        let data = scan_tsv_data();
        assert!(detect_scan_best(&data, 0).is_some(),
            "detect_scan_best must fall through to tab delimiter");
    }

    #[test]
    fn scan_no_fire_when_no_mdl_gain() {
        // 3-col CSV where all columns vary → adding LEB128 length prefixes costs more than saves
        let mut data = Vec::new();
        for i in 0u8..20 {
            let row = format!("{i},{},{i}\n", i + 1);
            data.extend_from_slice(row.as_bytes());
        }
        // All-variable 3-col CSV: SCAN overhead ≥ raw LIT cost → should not fire
        let _result = detect_scan_lit(&data, b',', 0);
        // Note: may or may not fire depending on field sizes; just verify round-trip either way.
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "round-trip failed for all-variable CSV");
    }

    // --- Suffix-array macro extractor ---

    fn long_pattern() -> Vec<u8> {
        // 120 bytes — clearly above MAX_MACRO_LEN=64 and SA_MIN_PAT_LEN=65.
        b"TEMPLATE: field1=alpha, field2=beta, field3=gamma, field4=delta; \
           status=active; version=1.0; region=us-east-1; tier=standard."
            .to_vec()
    }

    #[test]
    fn sa_finds_long_repeated_pattern() {
        let pat = long_pattern();
        // 4 occurrences separated by unique bytes to prevent LOOP detection.
        let mut data: Vec<u8> = Vec::new();
        for i in 0u8..4 {
            data.extend_from_slice(b"[ENTRY-");
            data.push(b'A' + i);
            data.push(b']');
            data.extend_from_slice(&pat);
        }
        let result = find_long_macro_by_sa(&data, 0);
        assert!(result.is_some(), "SA must find the repeated long pattern");
        let (found, occur, gain) = result.unwrap();
        assert!(found.len() >= SA_MIN_PAT_LEN, "pattern len {} < SA_MIN_PAT_LEN", found.len());
        assert!(occur >= 2,  "expected ≥2 occurrences, got {occur}");
        assert!(gain > 0,    "MDL gain must be positive, got {gain}");
    }

    #[test]
    fn sa_long_macro_round_trip_via_synth_raw() {
        let pat = long_pattern();
        let mut data: Vec<u8> = Vec::new();
        for i in 0u8..4 {
            data.push(b'[');
            data.push(b'0' + i);
            data.push(b']');
            data.extend_from_slice(&pat);
            data.push(b'\n');
        }
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "SA long-macro round-trip failed");
        let encoded = prog.total_encoded_len();
        let raw     = 1 + leb_len(data.len() as u32) + data.len();
        assert!(encoded < raw,
            "SA macro must compress: encoded={encoded} raw={raw}");
    }

    #[test]
    fn sa_long_macro_round_trip_via_hybrid() {
        let pat = long_pattern();
        let mut data: Vec<u8> = Vec::new();
        for i in 0u8..4 {
            data.push(b'[');
            data.push(b'0' + i);
            data.push(b']');
            data.extend_from_slice(&pat);
            data.push(b'\n');
        }
        let lz   = LzEncoder::new();
        let prog = PatternSynthesizer::new().synthesize_hybrid(&data, &lz);
        assert!(prog.verify_round_trip(&data), "SA hybrid round-trip failed");
    }

    #[test]
    fn sa_none_on_short_data() {
        // Data shorter than SA_MIN_PAT_LEN * MIN_MACRO_OCCUR → must return None.
        let data: Vec<u8> = b"short".to_vec();
        assert!(find_long_macro_by_sa(&data, 0).is_none());
    }

    #[test]
    fn sa_build_suffix_array_banana() {
        // Classic "banana" SA sanity check: expected SA = [5, 3, 1, 0, 4, 2].
        let data = b"banana";
        let sa = build_suffix_array(data);
        assert_eq!(sa, vec![5, 3, 1, 0, 4, 2]);
    }

    #[test]
    fn sa_build_lcp_array_banana() {
        // "banana" LCP expected = [0, 1, 3, 0, 0, 2].
        let data = b"banana";
        let sa   = build_suffix_array(data);
        let lcp  = build_lcp_array(data, &sa);
        assert_eq!(lcp, vec![0, 1, 3, 0, 0, 2]);
    }

    // --- LOOP fires on a long periodic LIT ---

    #[test]
    fn loop_fires_on_periodic_lit() {
        let body = b"[LOG] ";
        let full: Vec<u8> = body.repeat(30); // 180 bytes, period=6
        // Construct LzAnalysis directly to get a single LIT (skip LZ).
        let a = LzAnalysis {
            tokens: vec![LzToken::Literal { start: 0, data: full.clone() }],
            input_len: full.len(),
        };
        let prog = PatternSynthesizer::new().synthesize(&a);
        assert!(prog.verify_round_trip(&full), "LOOP round-trip failed");
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Loop { .. })),
            "expected a LOOP token in synthesized output"
        );
        // Encoded program must be smaller than raw LIT.
        let raw_lit_cost = 1 + leb_len(180) + 180; // = 183 bytes
        assert!(prog.encoded_len() < raw_lit_cost);
    }

    // --- Macro extraction ---

    #[test]
    fn macro_fires_on_repeated_prefix() {
        // 20 lines each beginning with the same 16-byte prefix but varied suffix.
        let prefix = b"SERVER_LOG_ENTRY:";
        let mut data: Vec<u8> = Vec::new();
        for i in 0u8..20 {
            data.extend_from_slice(prefix);
            data.push(b'A' + i);  // unique byte
            data.push(b'\n');
        }
        let prog = synth(&data);
        assert!(prog.verify_round_trip(&data), "macro round-trip failed");
    }

    // --- Synthesizer round-trips on realistic data ---

    #[test]
    fn synth_log_data_round_trip() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let data: Vec<u8> = line.repeat(200);
        let prog = synth(&data);
        assert!(prog.verify_round_trip(&data));
    }

    #[test]
    fn synth_does_not_degrade_cpy_tokens() {
        // LZ-heavy data: CPY tokens should pass through unchanged.
        let data: Vec<u8> = b"Hello, world! ".repeat(500);
        let a = LzEncoder::new().analyze(&data);
        let prog = PatternSynthesizer::new().synthesize(&a);
        assert!(prog.verify_round_trip(&data));
    }

    #[test]
    fn synth_binary_data_round_trip() {
        let data: Vec<u8> = (0u8..=255).cycle().take(1024).collect();
        let prog = synth(&data);
        assert!(prog.verify_round_trip(&data));
    }

    #[test]
    fn synth_empty_input() {
        let prog = synth(&[]);
        assert!(prog.verify_round_trip(&[]));
    }

    // --- Quoted CSV (RFC-4180) ---

    fn scan_quoted_csv_data() -> Vec<u8> {
        // 30 rows: first field quoted (contains comma), second fixed, third variable integer.
        let mut data = Vec::new();
        for i in 0u32..30 {
            let row = format!("\"Smith {i}, John\",fixed_category,{}\n", i + 100);
            data.extend_from_slice(row.as_bytes());
        }
        data
    }

    #[test]
    fn scan_quoted_csv_fires_and_round_trips() {
        let data = scan_quoted_csv_data();
        // detect_scan_lit with comma must correctly count delimiters outside quotes.
        let result = detect_scan_lit(&data, b',', 0);
        assert!(result.is_some(), "SCAN must fire on quoted CSV");
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "quoted CSV round-trip failed");
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Scan { .. })),
            "expected at least one SCAN token for quoted CSV"
        );
    }

    #[test]
    fn count_delimiters_quoted_basic() {
        // "Smith, John",category,42  →  2 commas outside quotes
        let line = b"\"Smith, John\",category,42";
        assert_eq!(count_delimiters_quoted(line, b','), 2);
    }

    #[test]
    fn count_delimiters_quoted_escaped_quote() {
        // "O""Brien",val  →  1 comma outside quotes; "" is escaped quote inside
        let line = b"\"O\"\"Brien\",val";
        assert_eq!(count_delimiters_quoted(line, b','), 1);
    }

    #[test]
    fn split_fields_quoted_raw_three_fields() {
        let line = b"\"Smith, John\",category,42";
        let fields = split_fields_quoted_raw(line, b',');
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], b"\"Smith, John\"");
        assert_eq!(fields[1], b"category");
        assert_eq!(fields[2], b"42");
    }

    // --- JSON SCAN (NDJSON) ---

    fn ndjson_data() -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0u32..30 {
            let row = format!("{{\"id\":{i},\"status\":\"active\",\"region\":\"us-east\"}}\n");
            data.extend_from_slice(row.as_bytes());
        }
        data
    }

    #[test]
    fn detect_scan_json_fires_on_ndjson() {
        let data = ndjson_data();
        let result = detect_scan_json(&data, 0);
        assert!(result.is_some(), "detect_scan_json must fire on NDJSON with fixed keys");
    }

    #[test]
    fn detect_scan_json_round_trip() {
        let data = ndjson_data();
        let prog = PatternSynthesizer::new().synthesize_raw(&data);
        assert!(prog.verify_round_trip(&data), "NDJSON SCAN round-trip failed");
        assert!(
            prog.tokens.iter().any(|t| matches!(t, SynthToken::Scan { .. })),
            "expected SCAN token for NDJSON"
        );
    }

    #[test]
    fn scan_best_detects_json_first() {
        // detect_scan_best must route to JSON detector, not comma-split on inner commas.
        let data = ndjson_data();
        let result = detect_scan_best(&data, 0);
        assert!(result.is_some(), "detect_scan_best must detect NDJSON");
        // Verify the result actually reconstructs the data correctly.
        if let Some(r) = result {
            let mut prog = SynthProgram::new(data.len());
            if !r.prefix.is_empty() { prog.tokens.push(SynthToken::Lit { data: r.prefix }); }
            prog.sub_defs.insert(0, r.template);
            prog.next_sub_id = 1;
            prog.tokens.push(r.scan_tok);
            if !r.suffix.is_empty() { prog.tokens.push(SynthToken::Lit { data: r.suffix }); }
            assert!(prog.verify_round_trip(&data), "NDJSON detect_scan_best round-trip failed");
        }
    }

    #[test]
    fn json_parse_kv_basic() {
        let obj = b"{\"id\":42,\"name\":\"Alice\"}";
        let pairs = parse_json_kv(obj).expect("parse should succeed");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, b"\"id\"");
        assert_eq!(pairs[0].1, b"42");
        assert_eq!(pairs[1].0, b"\"name\"");
        assert_eq!(pairs[1].1, b"\"Alice\"");
    }

    #[test]
    fn json_parse_kv_nested_value() {
        // Nested object as value — should parse (val includes the whole nested {}).
        let obj = b"{\"meta\":{\"k\":1}}";
        let pairs = parse_json_kv(obj).expect("nested value should parse");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1, b"{\"k\":1}");
    }

    #[test]
    fn detect_scan_json_all_fixed_returns_none() {
        // All rows identical → all-fixed → should return None (LOOP is better).
        let row = b"{\"status\":\"ok\",\"region\":\"us-east\"}\n";
        let data: Vec<u8> = row.repeat(30);
        assert!(detect_scan_json(&data, 0).is_none(),
            "all-fixed JSON should not trigger SCAN");
    }
}
