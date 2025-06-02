use std::path::PathBuf;

use anyhow::Context;
use bytesize::ByteSize;
use clap::{Parser, Subcommand};
use flac_tracksplit::{extract_sample_range, get_sample_rate, split_one_file};
use rayon::prelude::*;
use tracing::error;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "flac-tracksplit", author, version, about = "Split FLAC files with embedded CUE sheets or extract time ranges", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Pathnames of .flac files (with embedded CUE sheets) to split into tracks.
    /// This is the legacy interface - if no subcommand is provided, this will be used.
    #[arg(value_name = "FILES")]
    paths: Vec<PathBuf>,

    /// Output directory into which to sort resulting per-track FLAC files.
    /// Tracks will be named according to this template:
    ///
    /// OUTPUT_DIR/<Album Artist>/<Release year> - <Album name>/<Trackno>.<Track title>.flac
    #[arg(long, default_value = "./")]
    output_dir: PathBuf,

    /// Number of 0-byte padding to add to the end of the metadata
    /// block. More padding allows larger additions to metadata
    /// without having to rewrite the whole file.
    #[arg(long, default_value = "2kB")]
    metadata_padding: ByteSize,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Extract a time range from a FLAC file
    Split {
        /// Input FLAC file
        input: PathBuf,

        /// Starting time in milliseconds
        #[arg(long = "from", value_name = "MS")]
        from_ms: u64,

        /// Ending time in milliseconds
        #[arg(long = "to", value_name = "MS")]
        to_ms: u64,

        /// Output FLAC file
        output: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    // Setup logging:
    let indicatif_layer = tracing_indicatif::IndicatifLayer::new();
    let filter = EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();
    let writer = indicatif_layer.get_stderr_writer();
    let app_log_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact()
        .with_writer(writer);
    tracing_subscriber::registry()
        .with(filter)
        .with(app_log_layer)
        .with(indicatif_layer)
        .init();

    let args = Args::parse();

    match args.command {
        Some(Commands::Split {
            input,
            from_ms,
            to_ms,
            output,
        }) => {
            // New split subcommand
            if from_ms >= to_ms {
                anyhow::bail!("from_ms ({}) must be less than to_ms ({})", from_ms, to_ms);
            }

            // Get sample rate to convert milliseconds to samples
            let sample_rate = get_sample_rate(&input)
                .with_context(|| format!("reading sample rate from {:?}", input))?;

            let from_sample = (from_ms * sample_rate) / 1000;
            let to_sample = (to_ms * sample_rate) / 1000;

            extract_sample_range(&input, from_sample, to_sample, &output).with_context(|| {
                format!(
                    "extracting {}ms to {}ms (samples {} to {}) from {:?} to {:?}",
                    from_ms, to_ms, from_sample, to_sample, input, output
                )
            })?;

            println!(
                "Successfully extracted {}ms to {}ms (samples {} to {}) from {:?} to {:?}",
                from_ms, to_ms, from_sample, to_sample, input, output
            );
            Ok(())
        }
        None => {
            // Legacy interface - split files with embedded CUE sheets
            if args.paths.is_empty() {
                eprintln!("Error: No input files provided. Use --help for usage information.");
                std::process::exit(1);
            }

            let base_path = args.output_dir.as_path();
            let metadata_padding: u32 = args
                .metadata_padding
                .as_u64()
                .try_into()
                .context("--metadata-padding should fit into a 32-bit unsigned int")?;
            if let Err(err) = args
                .paths
                .into_par_iter()
                .panic_fuse()
                .try_for_each(|path| {
                    split_one_file(&path, base_path, metadata_padding)
                        .map(|_| ())
                        .with_context(|| format!("splitting {:?}", path))
                })
            {
                error!(error = %err);
                Err(err)
            } else {
                Ok(())
            }
        }
    }
}
