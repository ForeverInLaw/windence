use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context as _, Result};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    runtime,
    time::timeout,
};

use super::{ACTIVATE_ACKNOWLEDGMENT, ACTIVATE_MESSAGE, ACTIVATION_TIMEOUT, Duration};

pub enum Instance {
    Primary(Arc<InstanceLifecycle>),
    Secondary,
}

pub struct InstanceLifecycle {
    activations: async_channel::Receiver<()>,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
}

impl InstanceLifecycle {
    pub fn acquire() -> Result<Instance> {
        // Named pipes are machine-global while the Unix transport lives in
        // the per-user cache directory; keep the pipe name per-user so two
        // accounts on one machine do not hand activations to each other.
        let user = std::env::var("USERNAME").unwrap_or_default();
        let sanitized: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        Self::acquire_at(&format!("cadence-activation-{sanitized}"))
    }

    fn acquire_at(pipe_name: &str) -> Result<Instance> {
        // Pipe objects bind to the IO driver of the runtime they are created
        // in, so the acquisition runtime is handed to the listener thread
        // and keeps driving the server for the process's lifetime.
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("could not start lifecycle runtime")?;
        match Self::claim_pipe(&rt, pipe_name) {
            Ok(server) => Self::start_primary(pipe_name.to_owned(), rt, server),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                if Self::request_activation(pipe_name).is_ok() {
                    return Ok(Instance::Secondary);
                }

                // The pipe is owned by a process that does not speak our
                // protocol. Unlike a Unix socket there is no stale file to
                // clean up - the kernel drops pipes with their owner - so
                // refuse rather than steal the name.
                Err(error).with_context(|| {
                    format!("could not claim lifecycle pipe \\\\.\\pipe\\{pipe_name}")
                })
            }
            Err(error) => Err(error).with_context(|| {
                format!("could not create lifecycle pipe \\\\.\\pipe\\{pipe_name}")
            }),
        }
    }

    /// The first server instance claims the name; later instances are
    /// created without that flag as each client connects.
    fn claim_pipe(rt: &runtime::Runtime, pipe_name: &str) -> io::Result<NamedPipeServer> {
        let name = format!(r"\\.\pipe\{pipe_name}");
        rt.block_on(async move { ServerOptions::new().first_pipe_instance(true).create(name) })
    }

    fn start_primary(
        pipe_name: String,
        rt: runtime::Runtime,
        server: NamedPipeServer,
    ) -> Result<Instance> {
        let (activation_tx, activation_rx) = async_channel::bounded(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let listener_shutdown = shutdown.clone();
        let listener_thread = thread::Builder::new()
            .name("cadence-activation".into())
            .spawn(move || listener_loop(pipe_name, rt, server, activation_tx, listener_shutdown))
            .context("could not start lifecycle listener")?;

        Ok(Instance::Primary(Arc::new(Self {
            activations: activation_rx,
            shutdown,
            listener_thread: Some(listener_thread),
        })))
    }

    fn request_activation(pipe_name: &str) -> Result<()> {
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("could not start activation runtime")?;
        rt.block_on(async move {
            timeout(ACTIVATION_TIMEOUT, async {
                let mut client = ClientOptions::new().open(format!(r"\\.\pipe\{pipe_name}"))?;
                client.write_all(ACTIVATE_MESSAGE).await?;
                client.flush().await?;
                let mut acknowledgment = [0; ACTIVATE_ACKNOWLEDGMENT.len()];
                client.read_exact(&mut acknowledgment).await?;
                match acknowledgment == ACTIVATE_ACKNOWLEDGMENT {
                    true => Ok(()),
                    false => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid activation acknowledgment",
                    )),
                }
            })
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "activation handshake timed out",
                ))
            })
        })
        .context("could not activate the running instance")
    }

    pub fn take_activation(&self) -> bool {
        let mut activated = false;
        while self.activations.try_recv().is_ok() {
            activated = true;
        }
        activated
    }

    pub fn activation_receiver(&self) -> async_channel::Receiver<()> {
        self.activations.clone()
    }
}

impl Drop for InstanceLifecycle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(listener_thread) = self.listener_thread.take() {
            let _ = listener_thread.join();
        }
    }
}

/// Accepts activation clients until shut down. The loop polls the shutdown
/// flag between connections instead of blocking forever on `connect`, so a
/// dropped `InstanceLifecycle` always joins its thread promptly.
fn listener_loop(
    pipe_name: String,
    rt: runtime::Runtime,
    server: NamedPipeServer,
    activation_tx: async_channel::Sender<()>,
    shutdown: Arc<AtomicBool>,
) {
    rt.block_on(async move {
        let mut server = server;
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                connected = server.connect() => {
                    if connected.is_err() {
                        break;
                    }
                }
            }
            // The connected instance is now bound to the client; take it
            // aside and put a fresh listening instance in its place at once,
            // so the next launch always finds an open pipe.
            let mut client = server;
            match ServerOptions::new().create(format!(r"\\.\pipe\{pipe_name}")) {
                Ok(next) => server = next,
                Err(_) => {
                    let _ = timeout(ACTIVATION_TIMEOUT, answer(&mut client, &activation_tx)).await;
                    break;
                }
            }
            let _ = timeout(ACTIVATION_TIMEOUT, answer(&mut client, &activation_tx)).await;
        }
    });
}

async fn answer(server: &mut NamedPipeServer, activation_tx: &async_channel::Sender<()>) {
    let mut message = [0; ACTIVATE_MESSAGE.len()];
    if server.read_exact(&mut message).await.is_ok()
        && message == ACTIVATE_MESSAGE
        && server.write_all(ACTIVATE_ACKNOWLEDGMENT).await.is_ok()
        && server.flush().await.is_ok()
    {
        let _ = activation_tx.try_send(());
    }
}
