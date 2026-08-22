//! Single-instance lifecycle: one Cadence owns the session; extra launches
//! hand an "activate" nudge to the owner and exit. The transport is
//! per-platform (Unix domain sockets on macOS/Linux, named pipes on Windows)
//! but the protocol and public surface are identical.

use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Instance, InstanceLifecycle};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Instance, InstanceLifecycle};

/// The message a secondary launch sends to make the primary come forward.
const ACTIVATE_MESSAGE: &[u8] = b"activate\n";
/// What the primary answers once the nudge is queued.
const ACTIVATE_ACKNOWLEDGMENT: &[u8] = b"ok\n";
/// Both sides bound their handshake to this so a wedged peer cannot hang a
/// launch or the activation listener.
const ACTIVATION_TIMEOUT: Duration = Duration::from_millis(250);
