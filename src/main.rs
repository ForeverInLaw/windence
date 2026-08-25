// A release build is a window, not a command. Without this Windows gives
// the exe a console as well, and the app opens with a black box of log
// lines beside it. Development builds keep the console: that is where the
// logs are read from while working.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

use file_rotate::{ContentLimit, FileRotate, compression::Compression, suffix::AppendCount};
use spotify_gpui_client::storage::Store;

/// How large the log grows before it rolls over, and how many rolled files
/// are kept. Two megabytes is a long session of `info` lines, and three
/// files cap the whole lot at six.
const LOG_SIZE_LIMIT: usize = 2 * 1024 * 1024;
const LOG_FILES_KEPT: usize = 3;

/// The default level, and the one crate held below it.
///
/// `rspotify_http` logs every request with its headers, and those headers
/// carry the account's bearer token. The log is written to disk and is
/// meant to be sent on when something goes wrong, so the token must not be
/// in it. `RUST_LOG` still overrides this for anyone who needs the traffic
/// and knows what the lines hold.
const LOG_DEFAULT_FILTER: &str = "info,rspotify_http=warn";

fn main() {
    init_logging();
    app::run();
}

/// Points the log somewhere useful. A development build writes to the
/// console it was launched from. A release build has no console, so it
/// writes beside the database instead, where the listener can find it and
/// send it on.
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(LOG_DEFAULT_FILTER),
    );
    if !cfg!(debug_assertions)
        && let Some(log) = rotating_log()
    {
        builder.target(env_logger::Target::Pipe(Box::new(log)));
    }
    builder.init();
}

/// Opens `cadence.log` next to the database, rolling it over once it grows
/// past the limit. Returns `None` if the directory cannot be made, which
/// leaves the logs unwritten rather than stopping the app from starting.
fn rotating_log() -> Option<FileRotate<AppendCount>> {
    let directory = Store::data_dir().ok()?;
    std::fs::create_dir_all(&directory).ok()?;
    Some(FileRotate::new(
        directory.join("cadence.log"),
        AppendCount::new(LOG_FILES_KEPT),
        ContentLimit::Bytes(LOG_SIZE_LIMIT),
        Compression::None,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// The limit is a byte count, not a line count — the two are easy to
    /// swap, and swapping them means the log never rolls over at all.
    #[test]
    fn the_log_rolls_over_once_it_passes_the_size_limit() {
        let directory = std::env::temp_dir().join("cadence-log-rotation-test");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cadence.log");

        let mut log = FileRotate::new(
            &path,
            AppendCount::new(LOG_FILES_KEPT),
            ContentLimit::Bytes(LOG_SIZE_LIMIT),
            Compression::None,
            None,
        );
        let line = vec![b'x'; 64 * 1024];
        for _ in 0..40 {
            log.write_all(&line).unwrap();
        }
        log.flush().unwrap();

        assert!(
            path.with_extension("log.1").exists(),
            "passing the limit must leave a rolled file behind"
        );
        assert!(
            std::fs::metadata(&path).unwrap().len() < LOG_SIZE_LIMIT as u64,
            "the live log must start over rather than keep growing"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }
}
