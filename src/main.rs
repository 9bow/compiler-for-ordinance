use std::path::PathBuf;

use clap::Parser;

/// Compile cached ordinance XML files into a bare Git repository.
/// This is a placeholder — actual compilation logic is deferred to Phase 4.
#[derive(Debug, Parser)]
#[command(name = "compiler-for-ordinance")]
#[command(about = "Compile cached ordinance XML into a bare Git repository (placeholder)")]
struct Cli {
    /// Path to the .cache/ordinance directory
    #[arg(long, default_value = ".cache/ordinance")]
    cache_dir: PathBuf,

    /// Output bare repository path
    #[arg(long, default_value = "output.git")]
    output_dir: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    println!("compiler-for-ordinance: placeholder (not yet implemented)");
    Ok(())
}
