//! mucas CLI — compress, decompress, and benchmark files with the μCAS pipeline.
//!
//! Commands:
//!   mucas compress   <input>      <output.mucas>
//!   mucas decompress <input.mucas> <output>
//!   mucas bench      <file>

use mucas::{VmState, Consensus};
use mucas::pipeline::Pipeline;
use mucas::format::{MucasFile, compress_zlib};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("compress")   => compress_cmd(&args),
        Some("decompress") => decompress_cmd(&args),
        Some("bench")      => bench_cmd(&args),
        _                  => { usage(); return; }
    };
    if let Err(msg) = result {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!("mucas v0.1  —  μCAS adaptive compressor");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  mucas compress   <input>       <output.mucas>");
    eprintln!("  mucas decompress <input.mucas> <output>");
    eprintln!("  mucas bench      <file>");
}

// ---------------------------------------------------------------------------
// compress
// ---------------------------------------------------------------------------

fn compress_cmd(args: &[String]) -> Result<(), String> {
    let input  = args.get(2).ok_or("missing input path")?;
    let output = args.get(3).ok_or("missing output path")?;

    let data = std::fs::read(input).map_err(|e| e.to_string())?;

    let (prog, _class) = Pipeline::new().compress(&data);
    let (prog_bytes, subs) = prog.to_bytes();
    let encoded = MucasFile::new(prog_bytes, subs).encode();

    std::fs::write(output, &encoded).map_err(|e| e.to_string())?;

    let ratio = encoded.len() as f64 / data.len().max(1) as f64 * 100.0;
    eprintln!("{} → {} bytes  ({:.1}%)", data.len(), encoded.len(), ratio);
    Ok(())
}

// ---------------------------------------------------------------------------
// decompress
// ---------------------------------------------------------------------------

fn decompress_cmd(args: &[String]) -> Result<(), String> {
    let input  = args.get(2).ok_or("missing .mucas path")?;
    let output = args.get(3).ok_or("missing output path")?;

    let raw  = std::fs::read(input).map_err(|e| e.to_string())?;
    let file = MucasFile::decode(&raw).map_err(|e| e.to_string())?;

    let mut vm = VmState::new();
    vm.exec(&file.program, &file.subs, &Consensus::new())
        .map_err(|e| format!("{e:?}"))?;

    std::fs::write(output, &vm.output).map_err(|e| e.to_string())?;
    eprintln!("{} bytes → {}", vm.output.len(), output);
    Ok(())
}

// ---------------------------------------------------------------------------
// bench
// ---------------------------------------------------------------------------

fn bench_cmd(args: &[String]) -> Result<(), String> {
    let input = args.get(2).ok_or("missing file path")?;
    let data  = std::fs::read(input).map_err(|e| e.to_string())?;
    let n     = data.len();

    if n == 0 {
        eprintln!("(empty file — nothing to benchmark)");
        return Ok(());
    }

    let result = Pipeline::new().compress_verbose(&data);

    let (prog_bytes, subs) = result.program.to_bytes();
    let mucas_size = MucasFile::new(prog_bytes, subs).encode().len();
    let zlib_size  = compress_zlib(&data).len();

    let pct = |size: usize| size as f64 / n as f64 * 100.0;

    let separator = "─".repeat(50);
    println!("File:        {input}");
    println!("Input size:  {n} bytes");
    println!("Data class:  {:?}", result.data_class);
    println!("Synth path:  {}", if result.used_hybrid_path { "hybrid" } else { "lz-first" });
    println!("{separator}");
    println!("LZ ratio:    {:.2}%  ({} bytes)",
        result.lz_ratio * 100.0,
        (result.lz_ratio * n as f64) as usize);
    println!("Synth ratio: {:.2}%  ({} bytes)  [gain {:+.2}%]",
        result.synth_ratio * 100.0,
        (result.synth_ratio * n as f64) as usize,
        result.synth_gain * 100.0);
    println!("{separator}");
    println!(".mucas size: {:.2}%  ({mucas_size} bytes)", pct(mucas_size));
    println!("zlib(input): {:.2}%  ({zlib_size} bytes)", pct(zlib_size));
    println!("{separator}");

    let delta_pct = (zlib_size as f64 - mucas_size as f64) / zlib_size as f64 * 100.0;
    if delta_pct > 0.0 {
        println!("μCAS beats zlib by {delta_pct:.1}%");
    } else if delta_pct < -0.5 {
        println!("μCAS is {:.1}% larger than zlib (synthesis overhead > gain)", -delta_pct);
    } else {
        println!("μCAS ≈ zlib (< 0.5% difference)");
    }

    // Verify round-trip correctness.
    if !result.program.verify_round_trip(&data) {
        return Err("ROUND-TRIP FAILED — synthesized program does not reconstruct input".into());
    }
    println!("Round-trip:  OK ✓");
    Ok(())
}
