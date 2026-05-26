//! .mucas file format — v0.1
//!
//! Binary layout (all multi-byte integers are little-endian):
//!
//!   Offset  Size  Field
//!   ------  ----  -----
//!   0       4     Magic: "MCAS" = [0x4D, 0x43, 0x41, 0x53]
//!   4       1     Version: 0x01
//!   5       1     Flags (v0.1: reserved, must be 0x00)
//!   6       4     payload_len: u32 — decompressed payload byte count
//!   10      …     zlib-compressed payload (see below)
//!
//! Decompressed payload layout:
//!
//!   sub_count: u32
//!   for each sub (sorted by ascending ID):
//!     id:       u32
//!     body_len: u32
//!     body:     [u8; body_len]
//!   main program bytes (all remaining bytes)
//!
//! A payload with sub_count=0 is valid and common (LOOP/MAP programs need no subs).

use crate::Subs;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub const MAGIC:   [u8; 4] = [0x4D, 0x43, 0x41, 0x53]; // "MCAS"
pub const VERSION: u8 = 0x01;

// ---------------------------------------------------------------------------
// MucasFile
// ---------------------------------------------------------------------------

/// A .mucas archive: μCAS program + subroutine table, wrapped in MCAS format.
#[derive(Debug)]
pub struct MucasFile {
    pub program: Vec<u8>,
    pub subs:    Subs,
    pub flags:   u8,
}

impl MucasFile {
    pub fn new(program: Vec<u8>, subs: Subs) -> Self {
        MucasFile { program, subs, flags: 0x00 }
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
        if data[0..4] != MAGIC  { return Err(FormatError::BadMagic); }
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

        let (subs, prog_start) = parse_subs(&payload)?;
        let program = payload[prog_start..].to_vec();
        Ok(MucasFile { program, subs, flags })
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
        p.extend_from_slice(&(self.subs.len() as u32).to_le_bytes());

        let mut sorted: Vec<(u32, &Vec<u8>)> =
            self.subs.iter().map(|(&id, b)| (id, b)).collect();
        sorted.sort_by_key(|(id, _)| *id);

        for (id, body) in sorted {
            p.extend_from_slice(&id.to_le_bytes());
            p.extend_from_slice(&(body.len() as u32).to_le_bytes());
            p.extend_from_slice(body);
        }
        p.extend_from_slice(&self.program);
        p
    }
}

fn parse_subs(payload: &[u8]) -> Result<(Subs, usize), FormatError> {
    if payload.len() < 4 { return Err(FormatError::TooShort); }
    let sub_count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    let mut pos   = 4;
    let mut subs  = Subs::new();

    for _ in 0..sub_count {
        if pos + 8 > payload.len() { return Err(FormatError::TooShort); }
        let id       = u32::from_le_bytes(payload[pos..pos+4].try_into().unwrap());
        let body_len = u32::from_le_bytes(payload[pos+4..pos+8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + body_len > payload.len() { return Err(FormatError::TooShort); }
        subs.insert(id, payload[pos..pos+body_len].to_vec());
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
        assert_eq!(b[5], 0x00); // flags
    }

    #[test]
    fn round_trip_empty_program() {
        let encoded = MucasFile::new(vec![], no_subs()).encode();
        let decoded = MucasFile::decode(&encoded).unwrap();
        assert!(decoded.program.is_empty());
        assert!(decoded.subs.is_empty());
    }

    #[test]
    fn round_trip_program_with_subs() {
        // sub 0 = LIT("PREFIX:")
        let sub_body: Vec<u8> = ProgramBuilder::new().lit(b"PREFIX:").build().0;
        let subs: Subs = [(0u32, sub_body)].into_iter().collect();

        // main = CALL(0) LIT("suffix")
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
        // Two subs with non-sequential IDs to stress sort order.
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
