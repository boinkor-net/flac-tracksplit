use std::path::PathBuf;

use clap::Parser;
use flac_tracksplit::split_one_file;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about=None)]
struct Args {
    /// Pathnames of .flac files (with embedded CUE sheets) to split into tracks.
    paths: Vec<PathBuf>,

    /// Base path into which to sort resulting per-track FLAC files
    #[arg(long, default_value = "./")]
    base_path: PathBuf,
}

fn main() {
    // Setup logging:
    let indicatif_layer = tracing_indicatif::IndicatifLayer::new();
    let filter = EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();
    let writer = indicatif_layer.get_stderr_writer();
    let app_log_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact()
        .with_writer(writer.clone());
    tracing_subscriber::registry()
        .with(filter)
        .with(app_log_layer)
        .with(indicatif_layer)
        .init();

    let args = Args::parse();
    let base_path = args.base_path.as_path();
    for path in args.paths {
        split_one_file(path, base_path).expect("Correctly split");
    }
}
