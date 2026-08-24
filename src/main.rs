mod app;

fn main() {
    // Logs go to the console that launched the exe. Defaults to info; set
    // RUST_LOG=debug (or trace) for the full firehose.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    app::run();
}
