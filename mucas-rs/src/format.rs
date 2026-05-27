//! .mucas file format — v0.1 (extended in v0.11 with optional consensus section)
//!
//! Binary layout (all multi-byte integers are little-endian):
//!
//!   Offset  Size  Field
//!   ------  ----  -----
//!   0       4     Magic: "MCAS" = [0x4D, 0x43, 0x41, 0x53]
//!   4       1     Version: 0x01
//!   5       1     Flags: 0x00 = baseline; 0x01 = FLAG_HAS_CONSENSUS
//!   6       4     payload_len: u32 — decompressed payload byte count
//!   10      …     zlib-compressed payload (see below)
//!
//! Decompressed payload when flags == 0x00 (baseline):
//!   sub_count: u32
//!   for each sub (sorted by ascending ID):
//!     id: u32, body_len: u32, body[body_len]
//!   main program bytes (all remaining)
//!
//! Decompressed payload when flags & 0x01 == 0x01 (HAS_CONSENSUS):
//!   consensus_count: u32
//!   for each pattern:
//!     hash_len: u8, hash[hash_len], pat_len: u32, pat[pat_len]
//!   sub_count: u32
//!   for each sub (sorted by ascending ID):
//!     id: u32, body_len: u32, body[body_len]
//!   main program bytes (all remaining)

use crate::{Consensus, Subs};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub const MAGIC:             [u8; 4] = [0x4D, 0x43, 0x41, 0x53]; // "MCAS"
pub const VERSION:           u8      = 0x01;
const FLAG_HAS_CONSENSUS:    u8      = 0x01;

// ---------------------------------------------------------------------------
// MucasFile
// ---------------------------------------------------------------------------

/// A .mucas archive: μCAS program + subroutine table + optional consensus dictionary.
#[derive(Debug)]
pub struct MucasFile {
    pub program:   Vec<u8>,
    pub subs:      Subs,
    pub flags:     u8,
    /// REF hash → pattern bytes (non-empty only when FLAG_HAS_CONSENSUS is set).
    pub consensus: Consensus,
}

impl MucasFile {
    /// Construct with no consensus (backward-compatible, flags=0x00).
    pub fn new(program: Vec<u8>, subs: Subs) -> Self {
        MucasFile { program, subs, flags: 0x00, consensus: Consensus::new() }
    }

    /// Construct with an embedded consensus dictionary.
    /// If `consensus` is empty, behaves identically to `new()`.
    pub fn new_with_consensus(program: Vec<u8>, subs: Subs, consensus: Consensus) -> Self {
        let flags = if consensus.is_empty() { 0x00 } else { FLAG_HAS_CONSENSUS };
        MucasFile { program, subs, flags, consensus }
    }

    /// Serialize to .mucas bytes (header + zlib-compressed payload).
    pub fn encode(&self) -> Vec<u8> {
        let payload     = self.build_payload();
        let payload_len = payload.len() as u32;
        let compressed  = compress_zlib(&payload);

        let mut out = Vec::with_capacity(10 + compressed.len());
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.flags);
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// Parse from .mucas bytes.
    pub fn decode(data: &[u8]) -> Result<Self, FormatError> {
        if data.len() < 10 { return Err(FormatError::TooShort); }
        if data[0..4] != MAGIC   { return Err(FormatError::BadMagic); }
        if data[4]    != VERSION { return Err(FormatError::UnsupportedVersion(data[4])); }

        let flags       = data[5];
        let payload_len = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;

        let payload = decompress_zlib(&data[10..])
            .map_err(|_| FormatError::DecompressFailed)?;
        if payload.len() != payload_len {
            return Err(FormatError::LengthMismatch {
                expected: payload_len,
                actual:   payload.len(),
            });
        }

        let mut pos = 0usize;

        let consensus = if flags & FLAG_HAS_CONSENSUS != 0 {
            let (c, new_pos) = parse_consensus(&payload, pos)?;
            pos = new_pos;
            c
        } else {
            Consensus::new()
        };

        let (subs, prog_start) = parse_subs_at(&payload, pos)?;
        let program = payload[prog_start..].to_vec();
        Ok(MucasFile { program, subs, flags, consensus })
    }

    /// Encoded file size / raw program size (lower = better compression).
    pub fn compression_ratio(&self) -> f64 {
        let raw = self.program.len()
            + self.subs.values().map(|v| v.len()).sum::<usize>();
        if raw == 0 { return 1.0; }
        self.encode().len() as f64 / raw as f64
    }

    fn build_payload(&self) -> Vec<u8> {
        let mut p = Vec::new();

        if !self.consensus.is_empty() {
            let mut sorted_c: Vec<(&Vec<u8>, &Vec<u8>)> = self.consensus.iter().collect();
            sorted_c.sort_by_key(|(h, _)| *h);
            p.extend_from_slice(&(sorted_c.len() as u32).to_le_bytes());
            for (hash, pattern) in &sorted_c {
                p.push(hash.len() as u8);
                p.extend_from_slice(hash);
                p.extend_from_slice(&(pattern.len() as u32).to_le_bytes());
                p.extend_from_slice(pattern);
            }
        }

        let mut sorted_s: Vec<(u32, &Vec<u8>)> =
            self.subs.iter().map(|(&id, b)| (id, b)).collect();
        sorted_s.sort_by_key(|(id, _)| *id);
        p.extend_from_slice(&(sorted_s.len() as u32).to_le_bytes());
        for (id, body) in sorted_s {
            p.extend_from_slice(&id.to_le_bytes());
            p.extend_from_slice(&(body.len() as u32).to_le_bytes());
            p.extend_from_slice(body);
        }
        p.extend_from_slice(&self.program);
        p
    }
}

// ---------------------------------------------------------------------------
// Payload parsers
// ---------------------------------------------------------------------------

fn parse_consensus(payload: &[u8], mut pos: usize) -> Result<(Consensus, usize), FormatError> {
    if pos + 4 > payload.len() { return Err(FormatError::TooShort); }
    let count = u32::from_le_bytes(payload[pos..pos+4].try_into().unwrap()) as usize;
    pos += 4;

    let mut consensus = Consensus::new();
    for _ in 0..count {
        if pos >= payload.len() { return Err(FormatError::TooShort); }
        let hash_len = payload[pos] as usize;
        pos += 1;
        if pos + hash_len > payload.len() { return Err(FormatError::TooShort); }
        let hash = payload[pos..pos + hash_len].to_vec();
        pos += hash_len;

        if pos + 4 > payload.len() { return Err(FormatError::TooShort); }
        let pat_len = u32::from_le_bytes(payload[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + pat_len > payload.len() { return Err(FormatError::TooShort); }
        let pattern = payload[pos..pos + pat_len].to_vec();
        pos += pat_len;

        consensus.insert(hash, pattern);
    }
    Ok((consensus, pos))
}

fn parse_subs_at(payload: &[u8], mut pos: usize) -> Result<(Subs, usize), FormatError> {
    if pos + 4 > payload.len() { return Err(FormatError::TooShort); }
    let sub_count = u32::from_le_bytes(payload[pos..pos+4].try_into().unwrap()) as usize;
    pos += 4;
    let mut subs = Subs::new();

    for _ in 0..sub_count {
        if pos + 8 > payload.len() { return Err(FormatError::TooShort); }
        let id       = u32::from_le_bytes(payload[pos..pos+4].try_into().unwrap());
        let body_len = u32::from_le_bytes(payload[pos+4..pos+8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + body_len > payload.len() { return Err(FormatError::TooShort); }
        subs.insert(id, payload[pos..pos + body_len].to_vec());
        pos += body_len;
    }
    Ok((subs, pos))
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum FormatError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u8),
    DecompressFailed,
    LengthMismatch { expected: usize, actual: usize },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::TooShort =>
                write!(f, "file too short to contain MCAS header"),
            FormatError::BadMagic =>
                write!(f, "bad magic: not a .mucas file"),
            FormatError::UnsupportedVersion(v) =>
                write!(f, "unsupported .mucas version 0x{v:02X}"),
            FormatError::DecompressFailed =>
                write!(f, "zlib decompression failed"),
            FormatError::LengthMismatch { expected, actual } =>
                write!(f, "decompressed length {actual} != declared {expected}"),
        }
    }
}

impl std::error::Error for FormatError {}

// ---------------------------------------------------------------------------
// Compression helpers (pub so main.rs can compute baselines)
// ---------------------------------------------------------------------------

pub fn compress_zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
    enc.write_all(data).expect("zlib encode write");
    enc.finish().expect("zlib encode finish")
}

pub fn decompress_zlib(data: &[u8]) -> Result<Vec<u8>, ()> {
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(|_| ())?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VmState, Consensus, ProgramBuilder};
    use std::collections::HashMap;

    fn no_subs() -> Subs { HashMap::new() }

    #[test]
    fn header_fields_correct() {
        let f = MucasFile::new(vec![0x00], no_subs());
        let b = f.encode();
        assert_eq!(&b[0..4], b"MCAS");
        assert_eq!(b[4], 0x01); // version
        assert_eq!(b[5], 0x00); // flags — no consensus
    }

    #[test]
    fn round_trip_empty_program() {
        let encoded = MucasFile::new(vec![], no_subs()).encode();
        let decoded = MucasFile::decode(&encoded).unwrap();
        assert!(decoded.program.is_empty());
        assert!(decoded.subs.is_empty());
        assert!(decoded.consensus.is_empty());
    }

    #[test]
    fn round_trip_program_with_subs() {
        let sub_body: Vec<u8> = ProgramBuilder::new().lit(b"PREFIX:").build().0;
        let subs: Subs = [(0u32, sub_body)].into_iter().collect();
        let main: Vec<u8> = ProgramBuilder::new().call(0).lit(b"suffix").build().0;

        let encoded = MucasFile::new(main.clone(), subs.clone()).encode();
        let decoded = MucasFile::decode(&encoded).unwrap();
        assert_eq!(decoded.program, main);
        assert_eq!(decoded.subs,    subs);

        let mut vm = VmState::new();
        vm.exec(&decoded.program, &decoded.subs, &Consensus::new()).unwrap();
        assert_eq!(vm.output, b"PREFIX:suffix");
    }

    #[test]
    fn round_trip_loop_program_no_subs() {
        let body: Vec<u8> = ProgramBuilder::new().lit(b"Hello").build().0;
        let prog: Vec<u8> = ProgramBuilder::new().loop_(3, body).build().0;
        let encoded = MucasFile::new(prog.clone(), no_subs()).encode();
        let decoded = MucasFile::decode(&encoded).unwrap();
        assert_eq!(decoded.program, prog);

        let mut vm = VmState::new();
        vm.exec(&decoded.program, &decoded.subs, &Consensus::new()).unwrap();
        assert_eq!(vm.output, b"HelloHelloHello");
    }

    #[test]
    fn compression_reduces_repetitive_program() {
        let line = b"2024-01-01T00:00:00Z INFO  server: request processed ok\n";
        let body: Vec<u8> = ProgramBuilder::new().lit(line).build().0;
        let prog: Vec<u8> = ProgramBuilder::new().loop_(200, body).build().0;
        let original_size = line.len() * 200;
        let mucas_size = MucasFile::new(prog, no_subs()).encode().len();
        let ratio = mucas_size as f64 / original_size as f64;
        assert!(ratio < 0.05, "expected < 5% of original, got {ratio:.2}");
    }

    // --- Consensus embedding ---

    #[test]
    fn consensus_round_trip_via_mucasfile() {
        let pattern = b"SERVER_LOG_ENTRY_PREFIX_DATA:".to_vec();
        let mut consensus = Consensus::new();
        consensus.insert(vec![0x00], pattern.clone());

        // Program: single REF token
        let prog = vec![0x05u8, 0x01, 0x00]; // REF hash_len=1 hash=[0x00]

        let encoded = MucasFile::new_with_consensus(prog.clone(), no_subs(), consensus.clone()).encode();

        // flags byte must be FLAG_HAS_CONSENSUS
        assert_eq!(encoded[5], 0x01, "flags should have FLAG_HAS_CONSENSUS set");

        let decoded = MucasFile::decode(&encoded).unwrap();
        assert_eq!(decoded.program, prog);
        assert_eq!(decoded.flags, 0x01);
        assert_eq!(decoded.consensus.get(&vec![0x00u8]).unwrap(), &pattern);

        // Execute through VM
        let mut vm = VmState::new();
        vm.exec(&decoded.program, &decoded.subs, &decoded.consensus).unwrap();
        assert_eq!(vm.output, pattern);
    }

    #[test]
    fn empty_consensus_produces_flag_zero() {
        let f = MucasFile::new_with_consensus(vec![], no_subs(), Consensus::new());
        let b = f.encode();
        assert_eq!(b[5], 0x00, "empty consensus must not set FLAG_HAS_CONSENSUS");
    }

    // --- Error cases ---

    #[test]
    fn decode_rejects_too_short() {
        assert_eq!(MucasFile::decode(&[]).unwrap_err(),     FormatError::TooShort);
        assert_eq!(MucasFile::decode(&[0; 9]).unwrap_err(), FormatError::TooShort);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut d = vec![0u8; 10];
        d[0..4].copy_from_slice(b"XXXX");
        assert_eq!(MucasFile::decode(&d).unwrap_err(), FormatError::BadMagic);
    }

    #[test]
    fn decode_rejects_unsupported_version() {
        let mut enc = MucasFile::new(vec![], no_subs()).encode();
        enc[4] = 0x02;
        assert_eq!(
            MucasFile::decode(&enc).unwrap_err(),
            FormatError::UnsupportedVersion(0x02)
        );
    }

    #[test]
    fn multiple_subs_round_trip_correctly() {
        let sub3: Vec<u8> = ProgramBuilder::new().lit(b"THREE").build().0;
        let sub1: Vec<u8> = ProgramBuilder::new().lit(b"ONE").build().0;
        let subs: Subs = [(3u32, sub3), (1u32, sub1)].into_iter().collect();
        let main: Vec<u8> = ProgramBuilder::new().call(1).call(3).build().0;

        let encoded = MucasFile::new(main.clone(), subs.clone()).encode();
        let decoded = MucasFile::decode(&encoded).unwrap();
        assert_eq!(decoded.subs, subs);

        let mut vm = VmState::new();
        vm.exec(&decoded.program, &decoded.subs, &Consensus::new()).unwrap();
        assert_eq!(vm.output, b"ONETHREE");
    }
}
