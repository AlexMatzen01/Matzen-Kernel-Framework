//! MFKE assembler binary wrapper - calls python or native logic
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Simple: if invoked as mfke-asm, assemble
    // But for now this binary is placeholder: it prints help
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: mfke-asm <input.asm> <output.mfke>");
        eprintln!("  Assembles MFKE bytecode. See tools/mfke_asm.py for full impl.");
        eprintln!("  Example: python tools/mfke_asm.py sdk/template/src/main.asm app.mfke");
        std::process::exit(1);
    }
    // Try to delegate to python script if exists
    let input = Path::new(&args[1]);
    let output = Path::new(&args[2]);
    let py = Path::new("tools/mfke_asm.py");
    if py.exists() {
        let status = std::process::Command::new("python")
            .args([py.to_string_lossy().to_string(), input.to_string_lossy().to_string(), output.to_string_lossy().to_string()])
            .status();
        match status {
            Ok(s) if s.success() => return,
            Ok(s) => { eprintln!("python assembler failed: {}", s); std::process::exit(1); }
            Err(e) => { eprintln!("failed to run python: {}", e); }
        }
    }
    // fallback: pure rust assembler minimal
    eprintln!("Fallback: trying internal assemble");
    let text = fs::read_to_string(input).expect("read input");
    // minimal: use simplfs_host bundle? Actually just assemble via logic below
    // For now error
    eprintln!("No python and no internal asm yet. Install python.");
    std::process::exit(1);
}
