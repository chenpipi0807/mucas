//! mucas CLI — μCAS adaptive compressor and archiver
//!
//! Commands:
//!   mucas pack    <source_dir>   [-o output.mcar]  [--max-memory MiB]
//!   mucas unpack  <input.mcar>   [-o output_dir]
//!   mucas list    <input.mcar>
//!   mucas check   <input.mcar>
//!   mucas compress   <input>      <output.mucas>    (single-file, legacy)
//!   mucas decompress <input.mucas> <output>          (single-file, legacy)
//!   mucas bench   <file>

use indicatif::{ProgressBar, ProgressStyle};
use mucas::{VmState, Consensus};
use mucas::pipeline::Pipeline;
use mucas::format::{MucasFile, compress_zlib};
use mucas::archive::{
    ArchiveWriter, ArchiveReader, list_archive,
    DEFAULT_MAX_MEMORY, Method,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("pack")       => pack_cmd(&args),
        Some("unpack")     => unpack_cmd(&args),
        Some("list")       => list_cmd(&args),
        Some("check")      => check_cmd(&args),
        Some("compress")   => compress_cmd(&args),
        Some("decompress") => decompress_cmd(&args),
        Some("bench")      => bench_cmd(&args),
        // Legacy aliases kept for compatibility.
        Some("archive")    => pack_cmd(&args),
        Some("extract")    => unpack_cmd(&args),
        _                  => { usage(); return; }
    };
    if let Err(msg) = result {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
}

fn usage() {
    eprintln!("mucas v0.9  —  μCAS adaptive compressor & archiver");
    eprintln!();
    eprintln!("Archive commands:");
    eprintln!("  mucas pack    <source_dir>   [-o out.mcar] [--max-memory MiB]");
    eprintln!("  mucas unpack  <input.mcar>   [-o output_dir]");
    eprintln!("  mucas list    <input.mcar>");
    eprintln!("  mucas check   <input.mcar>");
    eprintln!();
    eprintln!("Single-file commands:");
    eprintln!("  mucas compress   <input>       <output.mucas>");
    eprintln!("  mucas decompress <input.mucas> <output>");
    eprintln!("  mucas bench      <file>");
}

// ---------------------------------------------------------------------------
// pack  (directory → .mcar)
// ---------------------------------------------------------------------------

fn pack_cmd(args: &[String]) -> Result<(), String> {
    // Accept both new ('pack') and legacy ('archive') command name.
    let source_dir = args.get(2).ok_or("missing source directory")?;

    // Parse flags: -o <output>, --max-memory <MiB>
    let output     = flag_value(args, "-o")
        .unwrap_or_else(|| format!("{}.mcar", source_dir.trim_end_matches(['/', '\\'])));
    let max_memory = parse_max_memory(args).unwrap_or(DEFAULT_MAX_MEMORY);

    // --- Pass 1: collect paths (no file content) ---
    let mut paths: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_paths(
        std::path::Path::new(source_dir),
        std::path::Path::new(source_dir),
        &mut paths,
    ).map_err(|e| e.to_string())?;

    if paths.is_empty() {
        eprintln!("(no files found in {source_dir})");
        return Ok(());
    }

    let total = paths.len() as u32;

    // --- Progress bar ---
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:40.green/white}] {pos}/{len}  {msg}"
        ).unwrap().progress_chars("█▉▊▋▌▍▎▏ "),
    );
    pb.set_message(format!("packing {} → {}", source_dir, output));

    // --- Pass 2: stream-compress each file ---
    let out_file = std::fs::File::create(&output).map_err(|e| e.to_string())?;
    let mut writer = ArchiveWriter::with_options(out_file, total, max_memory)
        .map_err(|e| e.to_string())?;

    for (_i, (rel_path, abs_path)) in paths.iter().enumerate() {
        writer.add_file_path(rel_path, abs_path).map_err(|e| e.to_string())?;

        let method_label = match writer.last_method {
            Some(Method::Store) => "STORE",
            Some(Method::Zlib)  => "ZLIB ",
            Some(Method::MuCAS) => "μCAS ",
            None => "     ",
        };
        pb.set_message(format!("[{method_label}] {}", truncate(rel_path, 40)));
        pb.inc(1);
    }

    // Capture stats before finish() consumes the writer.
    let store_n    = writer.stats.store_count;
    let zlib_n     = writer.stats.zlib_count;
    let mucas_n    = writer.stats.mucas_count;
    let total_orig = writer.stats.total_original;
    let total_comp = writer.stats.total_compressed;

    writer.finish().map_err(|e| e.to_string())?;
    pb.finish_and_clear();

    let ratio = if total_orig > 0 { total_comp as f64 / total_orig as f64 * 100.0 } else { 100.0 };
    let saved  = 100.0 - ratio;
    eprintln!("Packed {total} file(s)  {} → {}  ({ratio:.1}%  saved {saved:.1}%)",
        human_size(total_orig), human_size(total_comp));
    eprintln!("  Store: {store_n}  Zlib: {zlib_n}  μCAS: {mucas_n}");
    eprintln!("  Output: {output}");
    Ok(())
}

// ---------------------------------------------------------------------------
// unpack  (.mcar → directory)
// ---------------------------------------------------------------------------

fn unpack_cmd(args: &[String]) -> Result<(), String> {
    let input      = args.get(2).ok_or("missing .mcar path")?;
    let output_dir = flag_value(args, "-o")
        .unwrap_or_else(|| {
            // Default: strip .mcar extension, or append _out.
            input.strip_suffix(".mcar")
                .unwrap_or(input)
                .to_string()
        });

    let f          = std::fs::File::open(input).map_err(|e| e.to_string())?;
    let mut reader = ArchiveReader::new(std::io::BufReader::new(f))
        .map_err(|e| e.to_string())?;

    let total = reader.entry_count();
    let base  = std::path::Path::new(&output_dir);

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{bar:40.blue/white}] {pos}/{len}  {msg}"
        ).unwrap().progress_chars("█▉▊▋▌▍▎▏ "),
    );
    pb.set_message(format!("unpacking → {output_dir}"));

    let mut i = 0u32;
    while let Some(entry) = reader.next_entry().map_err(|e| e.to_string())? {
        i += 1;
        let dest = base.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest, &entry.data).map_err(|e| e.to_string())?;
        pb.set_message(truncate(&entry.path, 50));
        pb.inc(1);
    }

    pb.finish_and_clear();
    eprintln!("Unpacked {i} file(s) → {output_dir}");
    Ok(())
}

// ---------------------------------------------------------------------------
// list  (print table of archive contents without extracting)
// ---------------------------------------------------------------------------

fn list_cmd(args: &[String]) -> Result<(), String> {
    let input = args.get(2).ok_or("missing .mcar path")?;
    let f     = std::fs::File::open(input).map_err(|e| e.to_string())?;
    let infos = list_archive(std::io::BufReader::new(f)).map_err(|e| e.to_string())?;

    let total_orig: u64 = infos.iter().map(|e| e.original_size).sum();
    let total_comp: u64 = infos.iter().map(|e| e.compressed_size).sum();

    println!("{:<10}  {:>10}  {:>10}  {:>6}  {}",
        "Method", "Original", "Compressed", "Ratio", "Path");
    println!("{}", "─".repeat(70));
    for e in &infos {
        let method = match e.method {
            Method::Store => "Store",
            Method::Zlib  => "Zlib ",
            Method::MuCAS => "μCAS ",
        };
        println!("{method:<10}  {:>10}  {:>10}  {:>5.1}%  {}",
            human_size(e.original_size),
            human_size(e.compressed_size),
            e.ratio() * 100.0,
            e.path);
    }
    println!("{}", "─".repeat(70));
    let ratio = if total_orig > 0 { total_comp as f64 / total_orig as f64 * 100.0 } else { 100.0 };
    println!("{:<10}  {:>10}  {:>10}  {:>5.1}%  ({} files)",
        "TOTAL",
        human_size(total_orig),
        human_size(total_comp),
        ratio,
        infos.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// check  (verify integrity without writing to disk)
// ---------------------------------------------------------------------------

fn check_cmd(args: &[String]) -> Result<(), String> {
    let input  = args.get(2).ok_or("missing .mcar path")?;
    let f      = std::fs::File::open(input).map_err(|e| e.to_string())?;
    let mut reader = ArchiveReader::new(std::io::BufReader::new(f))
        .map_err(|e| e.to_string())?;

    let total = reader.entry_count();
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.yellow} [{bar:40.yellow/white}] {pos}/{len}  {msg}"
        ).unwrap().progress_chars("█▉▊▋▌▍▎▏ "),
    );
    pb.set_message("verifying...");

    let mut i = 0u32;
    while let Some(entry) = reader.next_entry().map_err(|e| e.to_string())? {
        i += 1;
        pb.set_message(truncate(&entry.path, 50));
        pb.inc(1);
    }

    pb.finish_and_clear();
    eprintln!("OK — {i} file(s) verified in {input}");
    Ok(())
}

// ---------------------------------------------------------------------------
// compress / decompress  (single-file, legacy)
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

    if !result.program.verify_round_trip(&data) {
        return Err("ROUND-TRIP FAILED — synthesized program does not reconstruct input".into());
    }
    println!("Round-trip:  OK ✓");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_paths(
    root: &std::path::Path,
    dir:  &std::path::Path,
    out:  &mut Vec<(String, std::path::PathBuf)>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        if path.is_dir() {
            collect_paths(root, &path, out)?;
        } else {
            let rel     = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            out.push((rel_str, path));
        }
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1 << 30 { format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64) }
    else if bytes >= 1 << 20 { format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64) }
    else if bytes >= 1 << 10 { format!("{:.1} KB", bytes as f64 / (1u64 << 10) as f64) }
    else { format!("{bytes} B") }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { format!("…{}", &s[s.len().saturating_sub(max - 1)..]) }
}

fn parse_max_memory(args: &[String]) -> Option<usize> {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == "--max-memory" {
            return args[i + 1].parse::<usize>().ok().map(|mib| mib * 1024 * 1024);
        }
    }
    None
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    for i in 0..args.len().saturating_sub(1) {
        if args[i] == flag {
            return Some(args[i + 1].clone());
        }
    }
    None
}
