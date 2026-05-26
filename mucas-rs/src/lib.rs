//! μCAS VM — Rust reference implementation (v0.1)
//!
//! Aligned to MUCAS_SPEC_v0.1 and the Python reference vm.py.
//!
//! Instruction encoding (§3):
//!   LIT  0x00  <len:LEB128> <bytes[len]>
//!   CPY  0x01  <offset:LEB128> <length:LEB128>
//!   MAP  0x02  <transform_id:u8> <len:LEB128>        ← in-place transform on last N output bytes
//!   LOOP 0x03  <count:LEB128> <body_len:LEB128> <body[body_len]>
//!   CALL 0x04  <macro_id:LEB128>
//!   REF  0x05  <hash_len:u8> <hash[hash_len]>
//!   ELIT 0x06  <encoded_len:LEB128> <raw_len:LEB128> <zlib_payload[encoded_len]>
//!   SCAN 0x07  <count:LEB128> <template_id:LEB128> <param_stream_len:LEB128> <param_stream[param_stream_len]>
//!   NEXT 0x08  (reads next length-prefixed field from the enclosing SCAN's parameter stream)
//!   HALT 0xFF

pub mod lz;
pub mod synth;
pub mod sched;
pub mod format;
pub mod pipeline;

use std::collections::HashMap;

pub const MAX_CALL_DEPTH: usize = 16;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024; // 256 MiB hard limit

// ---------------------------------------------------------------------------
// Opcode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Lit  = 0x00,
    Cpy  = 0x01,
    Map  = 0x02,
    Loop = 0x03,
    Call = 0x04,
    Ref  = 0x05,
    Elit = 0x06,
    Scan = 0x07,
    Next = 0x08,
    Halt = 0xFF,
}

impl TryFrom<u8> for Opcode {
    type Error = VmError;
    fn try_from(b: u8) -> Result<Self, VmError> {
        match b {
            0x00 => Ok(Opcode::Lit),
            0x01 => Ok(Opcode::Cpy),
            0x02 => Ok(Opcode::Map),
            0x03 => Ok(Opcode::Loop),
            0x04 => Ok(Opcode::Call),
            0x05 => Ok(Opcode::Ref),
            0x06 => Ok(Opcode::Elit),
            0x07 => Ok(Opcode::Scan),
            0x08 => Ok(Opcode::Next),
            0xFF => Ok(Opcode::Halt),
            _    => Err(VmError::UnknownOpcode(b)),
        }
    }
}

// ---------------------------------------------------------------------------
// MAP transform IDs (MUCAS_SPEC_v0.1 §3.3 / §4.2)
//
// MAP operates IN-PLACE: it takes the last `len` bytes already in the output
// buffer, applies the transform, and writes the result back.
// ---------------------------------------------------------------------------

/// Built-in MAP transforms (spec §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Transform {
    IncU8       = 0x00, // each byte += 1 (mod 256)
    DecU8       = 0x01, // each byte -= 1 (mod 256)
    IncU32      = 0x02, // LE uint32 += 1 (mod 2³²); len must be 4
    DecU32      = 0x03, // LE uint32 -= 1 (mod 2³²); len must be 4
    DeltaU8     = 0x04, // prefix-sum: out[i] = Σ in[0..i] mod 256
    DeltaU32    = 0x05, // prefix-sum on LE uint32 array; len must be multiple of 4
    ByteswapU32 = 0x06, // reverse byte order of each u32; len must be 4
    ZigzagU32   = 0x07, // ZigZag decode: n → (n>>1) XOR -(n&1); len must be 4
}

impl TryFrom<u8> for Transform {
    type Error = VmError;
    fn try_from(b: u8) -> Result<Self, VmError> {
        match b {
            0x00 => Ok(Transform::IncU8),
            0x01 => Ok(Transform::DecU8),
            0x02 => Ok(Transform::IncU32),
            0x03 => Ok(Transform::DecU32),
            0x04 => Ok(Transform::DeltaU8),
            0x05 => Ok(Transform::DeltaU32),
            0x06 => Ok(Transform::ByteswapU32),
            0x07 => Ok(Transform::ZigzagU32),
            _    => Err(VmError::UnknownMapTransform(b)),
        }
    }
}

// ---------------------------------------------------------------------------
// Error codes (MUCAS_SPEC_v0.1 §5.4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    UnknownOpcode(u8),        // 0x01
    CpyUnderflow,             // 0x02  offset > current output length
    LoopBodyOverrun,          // 0x03  body_len extends past program boundary
    OutputOverflow,           // 0x04  output > MAX_OUTPUT_BYTES
    CallDepthExceeded,        // 0x05  call depth > MAX_CALL_DEPTH
    CallCycle(u32),           // 0x06  macro refers to itself or an ancestor
    UnknownRef,               // 0x07  hash not found in consensus
    RefHashMismatch,          // 0x08  external UFC snapshot hash mismatch
    ConsensusUnavailable,     // 0x09  required UFC version not provided
    UnknownMapTransform(u8),  // 0x0A  transform_id not in defined set
    ScanNoParamStream,        // 0x0B  NEXT executed outside of a SCAN context
    ScanParamExhausted,       // 0x0C  NEXT called but parameter stream is exhausted
    TruncatedProgram,         // 0xFF  unexpected end of bytecode
}

impl VmError {
    pub fn code(&self) -> u8 {
        match self {
            VmError::UnknownOpcode(_)      => 0x01,
            VmError::CpyUnderflow          => 0x02,
            VmError::LoopBodyOverrun       => 0x03,
            VmError::OutputOverflow        => 0x04,
            VmError::CallDepthExceeded     => 0x05,
            VmError::CallCycle(_)          => 0x06,
            VmError::UnknownRef            => 0x07,
            VmError::RefHashMismatch       => 0x08,
            VmError::ConsensusUnavailable  => 0x09,
            VmError::UnknownMapTransform(_)=> 0x0A,
            VmError::ScanNoParamStream     => 0x0B,
            VmError::ScanParamExhausted    => 0x0C,
            VmError::TruncatedProgram      => 0xFF,
        }
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VmError(0x{:02X}): {:?}", self.code(), self)
    }
}

impl std::error::Error for VmError {}

// ---------------------------------------------------------------------------
// Public type aliases
// ---------------------------------------------------------------------------

/// Macro table: macro_id → bytecode body.
pub type Subs = HashMap<u32, Vec<u8>>;

/// Consensus library: hash_bytes → pattern bytes.
/// Use single-byte sequential IDs for session-local consensus, e.g. `vec![0u8]`.
pub type Consensus = HashMap<Vec<u8>, Vec<u8>>;

// ---------------------------------------------------------------------------
// VM state
// ---------------------------------------------------------------------------

pub struct VmState {
    pub output: Vec<u8>,
    call_depth: usize,
    active_calls: Vec<u32>,         // cycle detection for CALL
    param_stack: Vec<(Vec<u8>, usize)>, // (param_stream, cursor) stack for SCAN/NEXT
}

impl Default for VmState {
    fn default() -> Self {
        VmState {
            output: Vec::new(),
            call_depth: 0,
            active_calls: Vec::new(),
            param_stack: Vec::new(),
        }
    }
}

impl VmState {
    pub fn new() -> Self { Self::default() }

    pub fn exec(
        &mut self,
        prog: &[u8],
        subs: &Subs,
        consensus: &Consensus,
    ) -> Result<(), VmError> {
        self.run(prog, subs, consensus)
    }

    fn run(
        &mut self,
        prog: &[u8],
        subs: &Subs,
        consensus: &Consensus,
    ) -> Result<(), VmError> {
        let mut ip = 0usize;

        macro_rules! need {
            ($n:expr) => {
                if ip + $n > prog.len() {
                    return Err(VmError::TruncatedProgram);
                }
            };
        }
        macro_rules! read_byte {
            () => {{ need!(1); let b = prog[ip]; ip += 1; b }};
        }
        macro_rules! read_leb {
            () => {{ let (v, c) = decode_leb128(&prog[ip..])?; ip += c; v }};
        }

        loop {
            if ip >= prog.len() { break; }
            let op = Opcode::try_from(read_byte!())?;

            match op {
                Opcode::Halt => break,

                // LIT: push n raw bytes to output
                Opcode::Lit => {
                    let len = read_leb!() as usize;
                    need!(len);
                    self.emit(&prog[ip..ip + len])?;
                    ip += len;
                }

                // CPY: back-reference copy.  src = out.len() - offset; copy `length` bytes.
                Opcode::Cpy => {
                    let offset = read_leb!() as usize;
                    let length = read_leb!() as usize;
                    let out_len = self.output.len();
                    if offset == 0 || offset > out_len {
                        return Err(VmError::CpyUnderflow);
                    }
                    let src = out_len - offset;
                    for i in 0..length {
                        let b = self.output[src + i];
                        self.emit_byte(b)?;
                    }
                }

                // MAP: apply transform in-place to the last `len` bytes of output.
                Opcode::Map => {
                    let transform = Transform::try_from(read_byte!())?;
                    let len = read_leb!() as usize;
                    self.exec_map(transform, len)?;
                }

                // LOOP: repeat inline body `count` times.
                Opcode::Loop => {
                    let count    = read_leb!() as usize;
                    let body_len = read_leb!() as usize;
                    if ip + body_len > prog.len() {
                        return Err(VmError::LoopBodyOverrun);
                    }
                    let body = prog[ip..ip + body_len].to_vec();
                    ip += body_len;
                    for _ in 0..count {
                        self.run(&body, subs, consensus)?;
                    }
                }

                // CALL: invoke a named macro.
                Opcode::Call => {
                    let macro_id = read_leb!();
                    if self.call_depth >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    if self.active_calls.contains(&macro_id) {
                        return Err(VmError::CallCycle(macro_id));
                    }
                    let body = subs.get(&macro_id)
                        .ok_or(VmError::UnknownRef)?
                        .clone();
                    self.call_depth += 1;
                    self.active_calls.push(macro_id);
                    let result = self.run(&body, subs, consensus);
                    self.active_calls.pop();
                    self.call_depth -= 1;
                    result?;
                }

                // REF: look up hash in consensus library and append pattern.
                Opcode::Ref => {
                    need!(1);
                    let hash_len = prog[ip] as usize;
                    ip += 1;
                    need!(hash_len);
                    let hash = prog[ip..ip + hash_len].to_vec();
                    ip += hash_len;
                    let pattern = consensus.get(&hash).ok_or(VmError::UnknownRef)?.clone();
                    self.emit(&pattern)?;
                }

                // ELIT: zlib-compressed literal block.
                Opcode::Elit => {
                    let encoded_len = read_leb!() as usize;
                    let _raw_len    = read_leb!();
                    need!(encoded_len);
                    let compressed = prog[ip..ip + encoded_len].to_vec();
                    ip += encoded_len;
                    let decompressed = zlib_decompress(&compressed)?;
                    self.emit(&decompressed)?;
                }

                // SCAN: parameterized-template loop.
                // Executes the template sub `count` times; each NEXT in the template
                // reads the next length-prefixed field from `param_stream`.
                Opcode::Scan => {
                    let count       = read_leb!() as usize;
                    let template_id = read_leb!();
                    let param_len   = read_leb!() as usize;
                    need!(param_len);
                    let param_data  = prog[ip..ip + param_len].to_vec();
                    ip += param_len;

                    if self.call_depth >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let body = subs.get(&template_id)
                        .ok_or(VmError::UnknownRef)?
                        .clone();

                    self.call_depth += 1;
                    self.param_stack.push((param_data, 0));
                    let mut result = Ok(());
                    for _ in 0..count {
                        result = self.run(&body, subs, consensus);
                        if result.is_err() { break; }
                    }
                    self.param_stack.pop();
                    self.call_depth -= 1;
                    result?;
                }

                // NEXT: emit next length-prefixed field from the enclosing SCAN's param stream.
                Opcode::Next => {
                    let (param_data, pos) = self.param_stack.last_mut()
                        .ok_or(VmError::ScanNoParamStream)?;
                    if *pos >= param_data.len() {
                        return Err(VmError::ScanParamExhausted);
                    }
                    let (len, consumed) = decode_leb128(&param_data[*pos..])?;
                    *pos += consumed;
                    let len = len as usize;
                    if *pos + len > param_data.len() {
                        return Err(VmError::TruncatedProgram);
                    }
                    let field = param_data[*pos..*pos + len].to_vec();
                    *pos += len;
                    self.emit(&field)?;
                }
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------

    fn exec_map(&mut self, transform: Transform, len: usize) -> Result<(), VmError> {
        if len > self.output.len() {
            return Err(VmError::CpyUnderflow);
        }
        let start = self.output.len() - len;

        match transform {
            Transform::IncU8 => {
                for b in &mut self.output[start..] { *b = b.wrapping_add(1); }
            }
            Transform::DecU8 => {
                for b in &mut self.output[start..] { *b = b.wrapping_sub(1); }
            }
            Transform::IncU32 => {
                if len != 4 { return Err(VmError::UnknownMapTransform(0x02)); }
                let v = u32::from_le_bytes(self.output[start..start+4].try_into().unwrap());
                self.output[start..start+4].copy_from_slice(&v.wrapping_add(1).to_le_bytes());
            }
            Transform::DecU32 => {
                if len != 4 { return Err(VmError::UnknownMapTransform(0x03)); }
                let v = u32::from_le_bytes(self.output[start..start+4].try_into().unwrap());
                self.output[start..start+4].copy_from_slice(&v.wrapping_sub(1).to_le_bytes());
            }
            Transform::DeltaU8 => {
                // Prefix-sum (delta decode): out[i] = sum(in[0..=i]) mod 256
                let mut acc = 0u8;
                for b in &mut self.output[start..] {
                    acc = acc.wrapping_add(*b);
                    *b = acc;
                }
            }
            Transform::DeltaU32 => {
                if len % 4 != 0 { return Err(VmError::UnknownMapTransform(0x05)); }
                let mut acc = 0u32;
                for s in (start..start+len).step_by(4) {
                    let v = u32::from_le_bytes(self.output[s..s+4].try_into().unwrap());
                    acc = acc.wrapping_add(v);
                    self.output[s..s+4].copy_from_slice(&acc.to_le_bytes());
                }
            }
            Transform::ByteswapU32 => {
                if len != 4 { return Err(VmError::UnknownMapTransform(0x06)); }
                self.output[start..start+4].reverse();
            }
            Transform::ZigzagU32 => {
                if len != 4 { return Err(VmError::UnknownMapTransform(0x07)); }
                let n = u32::from_le_bytes(self.output[start..start+4].try_into().unwrap());
                let v = ((n >> 1) as i32) ^ -((n & 1) as i32);
                self.output[start..start+4].copy_from_slice(&v.to_le_bytes());
            }
        }
        Ok(())
    }

    #[inline]
    fn emit(&mut self, bytes: &[u8]) -> Result<(), VmError> {
        if self.output.len() + bytes.len() > MAX_OUTPUT_BYTES {
            return Err(VmError::OutputOverflow);
        }
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    #[inline]
    fn emit_byte(&mut self, b: u8) -> Result<(), VmError> {
        if self.output.len() >= MAX_OUTPUT_BYTES {
            return Err(VmError::OutputOverflow);
        }
        self.output.push(b);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LEB128 codec
// ---------------------------------------------------------------------------

pub fn decode_leb128(data: &[u8]) -> Result<(u32, usize), VmError> {
    let mut result = 0u32;
    let mut shift  = 0u32;
    for (i, &b) in data.iter().enumerate() {
        if i >= 5 { return Err(VmError::TruncatedProgram); }
        result |= ((b & 0x7F) as u32) << shift;
        shift += 7;
        if b & 0x80 == 0 { return Ok((result, i + 1)); }
    }
    Err(VmError::TruncatedProgram)
}

pub fn encode_leb128(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 { byte |= 0x80; }
        out.push(byte);
        if value == 0 { break; }
    }
    out
}

// ---------------------------------------------------------------------------
// DEFLATE helper (for ELIT)
// ---------------------------------------------------------------------------

fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, VmError> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut dec = ZlibDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(|_| VmError::TruncatedProgram)?;
    Ok(out)
}

pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

// ---------------------------------------------------------------------------
// Program builder
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct ProgramBuilder {
    buf:  Vec<u8>,
    subs: Subs,
}

impl ProgramBuilder {
    pub fn new() -> Self { Self::default() }

    pub fn lit(mut self, data: &[u8]) -> Self {
        self.buf.push(0x00);
        self.buf.extend(encode_leb128(data.len() as u32));
        self.buf.extend_from_slice(data);
        self
    }

    pub fn cpy(mut self, offset: u32, length: u32) -> Self {
        self.buf.push(0x01);
        self.buf.extend(encode_leb128(offset));
        self.buf.extend(encode_leb128(length));
        self
    }

    /// MAP: apply `transform` in-place to last `len` output bytes.
    pub fn map(mut self, transform: Transform, len: u32) -> Self {
        self.buf.push(0x02);
        self.buf.push(transform as u8);
        self.buf.extend(encode_leb128(len));
        self
    }

    pub fn loop_(mut self, count: u32, body: Vec<u8>) -> Self {
        self.buf.push(0x03);
        self.buf.extend(encode_leb128(count));
        self.buf.extend(encode_leb128(body.len() as u32));
        self.buf.extend(body);
        self
    }

    pub fn call(mut self, macro_id: u32) -> Self {
        self.buf.push(0x04);
        self.buf.extend(encode_leb128(macro_id));
        self
    }

    /// REF: emit a consensus pattern identified by `hash` bytes.
    /// For session-local IDs use a 1-byte slice, e.g. `&[7u8]`.
    pub fn ref_(mut self, hash: &[u8]) -> Self {
        self.buf.push(0x05);
        self.buf.push(hash.len() as u8);
        self.buf.extend_from_slice(hash);
        self
    }

    pub fn elit(mut self, raw: &[u8]) -> Self {
        let compressed = zlib_compress(raw);
        self.buf.push(0x06);
        self.buf.extend(encode_leb128(compressed.len() as u32));
        self.buf.extend(encode_leb128(raw.len() as u32));
        self.buf.extend(compressed);
        self
    }

    /// SCAN: parameterized-template loop.  `params` is the flat list of field values
    /// in row-major, column-major order (all variable fields from row 0, then row 1, …).
    pub fn scan(mut self, count: u32, template_id: u32, params: &[Vec<u8>]) -> Self {
        let mut stream = Vec::new();
        for p in params {
            stream.extend(encode_leb128(p.len() as u32));
            stream.extend_from_slice(p);
        }
        self.buf.push(0x07);
        self.buf.extend(encode_leb128(count));
        self.buf.extend(encode_leb128(template_id));
        self.buf.extend(encode_leb128(stream.len() as u32));
        self.buf.extend(stream);
        self
    }

    /// NEXT: read next field from the enclosing SCAN's parameter stream.
    pub fn next(mut self) -> Self { self.buf.push(0x08); self }

    pub fn halt(mut self) -> Self { self.buf.push(0xFF); self }

    pub fn add_sub(mut self, id: u32, body: Vec<u8>) -> Self {
        self.subs.insert(id, body);
        self
    }

    pub fn build(self) -> (Vec<u8>, Subs) { (self.buf, self.subs) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run(prog: &[u8]) -> Result<Vec<u8>, VmError> {
        let mut vm = VmState::new();
        vm.exec(prog, &HashMap::new(), &HashMap::new())?;
        Ok(vm.output)
    }

    // --- LEB128 ---

    #[test]
    fn leb128_roundtrip() {
        for v in [0u32, 1, 127, 128, 255, 300, 16383, 16384, u32::MAX] {
            let enc = encode_leb128(v);
            let (dec, _) = decode_leb128(&enc).unwrap();
            assert_eq!(dec, v);
        }
    }

    // --- LIT ---

    #[test]
    fn lit_round_trip() {
        let data = b"Hello, world!";
        let prog = ProgramBuilder::new().lit(data).build().0;
        assert_eq!(run(&prog).unwrap(), data);
    }

    // --- CPY ---

    #[test]
    fn cpy_duplicate() {
        // LIT "ab" + CPY(2, 2) → "abab"
        let prog = ProgramBuilder::new().lit(b"ab").cpy(2, 2).build().0;
        assert_eq!(run(&prog).unwrap(), b"abab");
    }

    #[test]
    fn cpy_overlap_run() {
        // LIT "a" + CPY(1, 4) → "aaaaa"
        let prog = ProgramBuilder::new().lit(b"a").cpy(1, 4).build().0;
        assert_eq!(run(&prog).unwrap(), b"aaaaa");
    }

    #[test]
    fn cpy_underflow_error() {
        let prog = ProgramBuilder::new().cpy(5, 3).build().0;
        assert_eq!(run(&prog).unwrap_err().code(), 0x02);
    }

    // --- MAP (in-place transform on last N output bytes) ---

    #[test]
    fn map_inc_u8() {
        // LIT [10, 20, 30] → MAP INC_U8 3 → [11, 21, 31]
        let prog = ProgramBuilder::new()
            .lit(&[10u8, 20, 30])
            .map(Transform::IncU8, 3)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![11, 21, 31]);
    }

    #[test]
    fn map_dec_u8() {
        let prog = ProgramBuilder::new()
            .lit(&[11u8, 21, 31])
            .map(Transform::DecU8, 3)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![10, 20, 30]);
    }

    #[test]
    fn map_delta_u8_prefix_sum() {
        // LIT [5, 3, 2, 1] → MAP DELTA_U8 4 → [5, 8, 10, 11]
        let prog = ProgramBuilder::new()
            .lit(&[5u8, 3, 2, 1])
            .map(Transform::DeltaU8, 4)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![5, 8, 10, 11]);
    }

    #[test]
    fn map_delta_u8_arithmetic_sequence() {
        // Delta-encode [65,66,67,68,69] = [65,1,1,1,1], then decode with MAP DELTA_U8
        let prog = ProgramBuilder::new()
            .lit(&[65u8, 1, 1, 1, 1])
            .map(Transform::DeltaU8, 5)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![65, 66, 67, 68, 69]);
    }

    #[test]
    fn map_inc_u32() {
        // [0xFF, 0x00, 0x00, 0x00] (=255 LE) → INC_U32 → 256 LE
        let prog = ProgramBuilder::new()
            .lit(&[0xFF, 0x00, 0x00, 0x00])
            .map(Transform::IncU32, 4)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn map_byteswap_u32() {
        let prog = ProgramBuilder::new()
            .lit(&[0x01, 0x02, 0x03, 0x04])
            .map(Transform::ByteswapU32, 4)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn map_zigzag_u32() {
        // encoded=2 → decoded=1 ((2>>1)=1, (2&1)=0, 1 XOR 0=1)
        let prog = ProgramBuilder::new()
            .lit(&[2u8, 0, 0, 0])
            .map(Transform::ZigzagU32, 4)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![1, 0, 0, 0]);
    }

    // --- LOOP ---

    #[test]
    fn loop_repeat() {
        let body = ProgramBuilder::new().lit(b"x").build().0;
        let prog = ProgramBuilder::new().loop_(3, body).build().0;
        assert_eq!(run(&prog).unwrap(), b"xxx");
    }

    #[test]
    fn loop_body_overrun() {
        // body_len claims more bytes than are present
        let prog = vec![0x03, 0x03, 0x0F, 0x00, 0x00]; // LOOP 3 body_len=15 (only 2 body bytes)
        assert_eq!(run(&prog).unwrap_err().code(), 0x03);
    }

    // --- CALL ---

    #[test]
    fn call_basic() {
        let (sub_body, _) = ProgramBuilder::new().lit(b"world").build();
        let (prog, mut subs) = ProgramBuilder::new().lit(b"hello ").call(0).build();
        subs.insert(0, sub_body);
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &HashMap::new()).unwrap();
        assert_eq!(vm.output, b"hello world");
    }

    #[test]
    fn call_depth_exceeded() {
        let (body, _) = ProgramBuilder::new().call(0).build();
        let mut subs = HashMap::new();
        subs.insert(0u32, body);
        let (prog, _) = ProgramBuilder::new().call(0).build();
        let mut vm = VmState::new();
        let err = vm.exec(&prog, &subs, &HashMap::new()).unwrap_err();
        assert!(matches!(err, VmError::CallCycle(0) | VmError::CallDepthExceeded));
    }

    // --- REF ---

    #[test]
    fn ref_lookup_single_byte_hash() {
        let mut consensus = HashMap::new();
        consensus.insert(vec![7u8], b"pattern".to_vec());
        let (prog, subs) = ProgramBuilder::new().ref_(&[7u8]).build();
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &consensus).unwrap();
        assert_eq!(vm.output, b"pattern");
    }

    #[test]
    fn ref_unknown_error() {
        let (prog, subs) = ProgramBuilder::new().ref_(&[42u8]).build();
        let err = VmState::new().exec(&prog, &subs, &HashMap::new()).unwrap_err();
        assert_eq!(err.code(), 0x07);
    }

    // --- ELIT ---

    #[test]
    fn elit_round_trip() {
        let data = b"Hello, ELIT world! This is a somewhat longer string to compress.";
        let prog = ProgramBuilder::new().elit(data).build().0;
        assert_eq!(run(&prog).unwrap(), data);
    }

    // --- error codes ---

    #[test]
    fn unknown_opcode_error() {
        assert_eq!(run(&[0xAB]).unwrap_err().code(), 0x01);
    }

    // --- composite round-trip ---

    #[test]
    fn hello_repeat_round_trip() {
        let chunk = b"Hello ";
        let mut prog = ProgramBuilder::new().lit(chunk);
        for _ in 1..1000 {
            prog = prog.cpy(chunk.len() as u32, chunk.len() as u32);
        }
        let (bytes, subs) = prog.build();
        let mut vm = VmState::new();
        vm.exec(&bytes, &subs, &HashMap::new()).unwrap();
        assert_eq!(vm.output, chunk.repeat(1000));
    }

    // --- SCAN / NEXT ---

    #[test]
    fn scan_basic_parameterized_rows() {
        // Template: NEXT "," NEXT "\n"  (two variable columns)
        // Rows: "a,b\n"  "c,d\n"
        let template = ProgramBuilder::new().next().lit(b",").next().lit(b"\n").build().0;
        let params: Vec<Vec<u8>> = vec![b"a".to_vec(), b"b".to_vec(),
                                        b"c".to_vec(), b"d".to_vec()];
        let (prog, mut subs) = ProgramBuilder::new().scan(2, 0, &params).build();
        subs.insert(0, template);
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &HashMap::new()).unwrap();
        assert_eq!(vm.output, b"a,b\nc,d\n");
    }

    #[test]
    fn scan_with_fixed_column_in_template() {
        // Template: NEXT ",FIXED," NEXT "\n"  — middle column is always "FIXED"
        let template = ProgramBuilder::new()
            .next().lit(b",FIXED,").next().lit(b"\n").build().0;
        let params: Vec<Vec<u8>> = vec![
            b"X1".to_vec(), b"Y1".to_vec(),
            b"X2".to_vec(), b"Y2".to_vec(),
        ];
        let (prog, mut subs) = ProgramBuilder::new().scan(2, 0, &params).build();
        subs.insert(0, template);
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &HashMap::new()).unwrap();
        assert_eq!(vm.output, b"X1,FIXED,Y1\nX2,FIXED,Y2\n");
    }

    #[test]
    fn scan_empty_field() {
        // Template: NEXT "," NEXT "\n" — first col empty in both rows
        let template = ProgramBuilder::new().next().lit(b",").next().lit(b"\n").build().0;
        let params: Vec<Vec<u8>> = vec![b"".to_vec(), b"val1".to_vec(),
                                        b"".to_vec(), b"val2".to_vec()];
        let (prog, mut subs) = ProgramBuilder::new().scan(2, 0, &params).build();
        subs.insert(0, template);
        let mut vm = VmState::new();
        vm.exec(&prog, &subs, &HashMap::new()).unwrap();
        assert_eq!(vm.output, b",val1\n,val2\n");
    }

    #[test]
    fn next_outside_scan_errors() {
        // NEXT with no enclosing SCAN → ScanNoParamStream
        let prog = vec![0x08u8];
        let err = VmState::new().exec(&prog, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert_eq!(err.code(), 0x0B);
    }

    // --- MAP + LIT pipeline (delta encoding use-case) ---

    #[test]
    fn delta_encode_in_loop() {
        // Emit [65, 1, 1, 1] × 5 times using LOOP, then MAP DELTA_U8 20 to decode.
        // Expected output: first row [65,66,67,68], then each subsequent [65,131,196,...]
        // (prefix sum continues across all rows)
        // Simpler: emit via LOOP [LIT [65,1,1,1]] × 1 then MAP DELTA_U8 4 → [65,66,67,68]
        let inner_body = ProgramBuilder::new().lit(&[65u8, 1, 1, 1]).build().0;
        let prog = ProgramBuilder::new()
            .loop_(1, inner_body)
            .map(Transform::DeltaU8, 4)
            .build().0;
        assert_eq!(run(&prog).unwrap(), vec![65, 66, 67, 68]);
    }
}
