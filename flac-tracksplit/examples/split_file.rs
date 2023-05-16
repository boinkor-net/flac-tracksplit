use flac_tracksplit::split_one_file;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

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
    split_one_file("test.flac").expect("Correctly split");
}
