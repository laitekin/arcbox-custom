//! Framed machine sessions without registering HV descriptors with kqueue.
//!
//! Dedicated blocking workers own one reader and one writer. Bounded channels
//! bridge them to async session flow control; dropping either endpoint shuts
//! down the socket and wakes pending poll/read/write operations.

use std::time::{Duration, Instant};

use arcbox_transport::error::{Result, TransportError};
use arcbox_transport::vsock::{BlockingVsockShutdown, VsockReceiver, VsockSender};
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use super::transport::{AgentTransport, BLOCKING_RPC_TIMEOUT};

pub(super) enum Sender {
    Async(VsockSender),
    Blocking {
        frames: mpsc::Sender<(Bytes, oneshot::Sender<Result<()>>)>,
        _shutdown: BlockingVsockShutdown,
    },
}

pub(super) enum Receiver {
    Async(VsockReceiver),
    Blocking {
        frames: mpsc::Receiver<Result<Bytes>>,
        _shutdown: BlockingVsockShutdown,
    },
}

pub(super) fn split(transport: AgentTransport) -> Result<(Sender, Receiver)> {
    match transport {
        AgentTransport::Async(transport) => {
            let (sender, receiver) = transport.into_split()?;
            Ok((Sender::Async(sender), Receiver::Async(receiver)))
        }
        AgentTransport::Blocking(mut reader) => {
            let mut writer = reader.try_clone()?;
            let sender_shutdown = reader.shutdown_handle()?;
            let receiver_shutdown = reader.shutdown_handle()?;
            let (write_tx, mut write_rx) = mpsc::channel::<(Bytes, oneshot::Sender<Result<()>>)>(1);
            let (read_tx, read_rx) = mpsc::channel(1);
            tokio::task::spawn_blocking(move || {
                while let Some((frame, ack)) = write_rx.blocking_recv() {
                    let result = writer.send(&frame, Instant::now() + BLOCKING_RPC_TIMEOUT);
                    let failed = result.is_err();
                    let _ = ack.send(result);
                    if failed {
                        break;
                    }
                }
            });
            tokio::task::spawn_blocking(move || {
                loop {
                    // An interactive shell can be idle for hours. Cancellation
                    // uses shutdown, never retrying a partially consumed frame.
                    let result = reader.recv(Instant::now() + Duration::from_secs(24 * 60 * 60));
                    let failed = result.is_err();
                    if read_tx.blocking_send(result).is_err() || failed {
                        break;
                    }
                }
            });
            Ok((
                Sender::Blocking {
                    frames: write_tx,
                    _shutdown: sender_shutdown,
                },
                Receiver::Blocking {
                    frames: read_rx,
                    _shutdown: receiver_shutdown,
                },
            ))
        }
    }
}

impl Sender {
    pub(super) async fn send(&mut self, frame: Bytes) -> Result<()> {
        match self {
            Self::Async(sender) => sender.send(frame).await,
            Self::Blocking { frames, .. } => {
                let (tx, rx) = oneshot::channel();
                frames
                    .send((frame, tx))
                    .await
                    .map_err(|_| TransportError::NotConnected)?;
                rx.await.map_err(|_| TransportError::NotConnected)?
            }
        }
    }
}

impl Receiver {
    pub(super) async fn recv(&mut self) -> Result<Bytes> {
        match self {
            Self::Async(receiver) => receiver.recv().await,
            Self::Blocking { frames, .. } => {
                frames.recv().await.ok_or(TransportError::NotConnected)?
            }
        }
    }
}
