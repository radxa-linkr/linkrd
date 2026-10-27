use crate::{
    clipboard::ClipboardBackend,
    framing::{FrameReader, encode_frame},
    protocol::{Envelope, HELLO_MAX_BYTES, MAX_FRAME_BYTES},
    session::Session,
};
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 8;
const TICK: Duration = Duration::from_millis(20);

#[derive(Clone)]
pub struct ConnectionOptions {
    pub handshake_timeout: Duration,
    pub frame_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub refresh_interval: Duration,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(5),
            frame_timeout: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(15),
            refresh_interval: Duration::from_secs(1),
        }
    }
}

pub fn run_connection<R: Read + Send, W: Write + Send>(
    reader: R,
    mut writer: W,
    clipboard: &mut impl ClipboardBackend,
    dirty: &AtomicBool,
    shutdown: &AtomicBool,
    options: ConnectionOptions,
) -> Result<()> {
    let connection_id = Uuid::new_v4();
    let _span = tracing::info_span!("connection", session_id = %connection_id).entered();
    let live = AtomicBool::new(true);
    let ready = AtomicBool::new(false);
    let (incoming_tx, incoming_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (outgoing_tx, outgoing_rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE_CAPACITY);
    let (failure_tx, failure_rx) = mpsc::sync_channel::<String>(2);

    thread::scope(|scope| {
        let reader_failure = failure_tx.clone();
        let live_ref = &live;
        let ready_ref = &ready;
        let reader_thread = scope.spawn(move || {
            let mut frames = FrameReader::new(reader, HELLO_MAX_BYTES, options.frame_timeout);
            let mut first = true;
            while live_ref.load(Ordering::Acquire) && !shutdown.load(Ordering::Acquire) {
                match frames.poll() {
                    Ok(Some(message)) => {
                        if incoming_tx.try_send(message).is_err() {
                            let _ = reader_failure.try_send("incoming queue full or closed".into());
                            live_ref.store(false, Ordering::Release);
                            break;
                        }
                        if first {
                            while !ready_ref.load(Ordering::Acquire)
                                && live_ref.load(Ordering::Acquire)
                                && !shutdown.load(Ordering::Acquire)
                            {
                                thread::sleep(TICK);
                            }
                            frames.set_max_bytes(MAX_FRAME_BYTES);
                            first = false;
                        }
                    }
                    Ok(None) => (),
                    Err(error) => {
                        let _ = reader_failure.try_send(error.to_string());
                        live_ref.store(false, Ordering::Release);
                    }
                }
            }
        });
        let writer_live = &live;
        let writer_thread = scope.spawn(move || {
            while writer_live.load(Ordering::Acquire) && !shutdown.load(Ordering::Acquire) {
                match outgoing_rx.recv_timeout(TICK) {
                    Ok(frame) => {
                        if let Err(error) = write_with_deadline(
                            &mut writer,
                            &frame,
                            options.frame_timeout,
                            writer_live,
                            shutdown,
                        ) {
                            let _ = failure_tx.try_send(format!("ACM write failed: {error}"));
                            writer_live.store(false, Ordering::Release);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        let result = (|| -> Result<()> {
            let mut session = Session::new(*connection_id.as_bytes());
            let send = |message: &Envelope, limit: usize| -> Result<()> {
                outgoing_tx
                    .try_send(encode_frame(message, limit)?)
                    .map_err(|_| {
                        anyhow::anyhow!("outgoing queue full or closed; reconnect to refresh state")
                    })?;
                crate::logging::record_outgoing(message);
                Ok(())
            };
            send(&session.hello(), HELLO_MAX_BYTES)?;
            let started = Instant::now();
            let mut last_hello = started;
            let mut last_refresh = started;
            let mut last_ping = started;
            let mut ping_started = None;
            loop {
                if shutdown.load(Ordering::Acquire) {
                    return Ok(());
                }
                if let Ok(failure) = failure_rx.try_recv() {
                    bail!("{failure}");
                }
                if !live.load(Ordering::Acquire) {
                    bail!("ACM worker stopped");
                }
                if !session.is_ready() && started.elapsed() >= options.handshake_timeout {
                    bail!("Hello handshake timed out");
                }
                match incoming_rx.recv_timeout(TICK) {
                    Ok(message) => {
                        crate::logging::record_incoming(&message);
                        let was_ready = session.is_ready();
                        let messages = session.receive(message, clipboard)?;
                        if !was_ready && session.is_ready() {
                            send(&session.hello(), HELLO_MAX_BYTES)?;
                            tracing::info!("protocol session ready");
                            ready.store(true, Ordering::Release);
                            dirty.store(true, Ordering::Release);
                            last_ping = Instant::now();
                        }
                        for message in messages {
                            send(&message, session.max_send_bytes())?;
                        }
                        if !session.awaiting_pong() {
                            ping_started = None;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if shutdown.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        if let Ok(failure) = failure_rx.try_recv() {
                            bail!("{failure}");
                        }
                        bail!("ACM reader stopped");
                    }
                }
                if !session.is_ready() {
                    if last_hello.elapsed() >= Duration::from_secs(1) {
                        send(&session.hello(), HELLO_MAX_BYTES)?;
                        last_hello = Instant::now();
                    }
                    continue;
                }
                if ping_started
                    .is_some_and(|start: Instant| start.elapsed() >= options.heartbeat_timeout)
                {
                    bail!("peer heartbeat timed out");
                }
                if !session.awaiting_pong() && last_ping.elapsed() >= options.heartbeat_interval {
                    send(&session.ping()?, session.max_send_bytes())?;
                    last_ping = Instant::now();
                    ping_started = Some(last_ping);
                }
                if dirty.swap(false, Ordering::AcqRel)
                    || last_refresh.elapsed() >= options.refresh_interval
                {
                    if let Some(message) = session.observe(clipboard)? {
                        send(&message, session.max_send_bytes())?;
                    }
                    last_refresh = Instant::now();
                }
            }
        })();
        let stopping = Instant::now();
        tracing::debug!(
            shutdown = shutdown.load(Ordering::Acquire),
            "stopping ACM workers"
        );
        live.store(false, Ordering::Release);
        drop(outgoing_tx);
        reader_thread
            .join()
            .map_err(|_| anyhow::anyhow!("ACM reader panicked"))?;
        tracing::debug!(
            elapsed_ms = stopping.elapsed().as_millis() as u64,
            "ACM reader joined"
        );
        writer_thread
            .join()
            .map_err(|_| anyhow::anyhow!("ACM writer panicked"))?;
        tracing::debug!(
            elapsed_ms = stopping.elapsed().as_millis() as u64,
            "ACM writer joined"
        );
        if let Err(error) = &result {
            tracing::warn!(error = %format_args!("{error:#}"), "protocol session ended");
        } else {
            tracing::debug!("protocol session stopped");
        }
        result.context("device session ended")
    })
}

fn write_with_deadline(
    writer: &mut impl Write,
    mut bytes: &[u8],
    timeout: Duration,
    live: &AtomicBool,
    shutdown: &AtomicBool,
) -> std::io::Result<()> {
    let started = Instant::now();
    while !bytes.is_empty() {
        if !live.load(Ordering::Acquire) || shutdown.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "ACM write cancelled",
            ));
        }
        if started.elapsed() >= timeout {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "frame write deadline exceeded",
            ));
        }
        let count = match writer.write(&bytes[..bytes.len().min(16 * 1024)]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "ACM write made no progress",
                ));
            }
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(TICK.min(timeout.saturating_sub(started.elapsed())));
                continue;
            }
            Err(error) => return Err(error),
        };
        bytes = &bytes[count..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, ErrorKind};

    struct PausingWriter {
        calls: usize,
        bytes: Vec<u8>,
    }
    impl Write for PausingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            match self.calls {
                2 => Err(ErrorKind::WouldBlock.into()),
                3 => Err(ErrorKind::TimedOut.into()),
                4 => Err(ErrorKind::Interrupted.into()),
                _ => {
                    let n = bytes.len().min(3);
                    self.bytes.extend_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_write_retries_preserve_the_frame_without_duplicate_bytes() {
        let mut writer = PausingWriter {
            calls: 0,
            bytes: Vec::new(),
        };
        write_with_deadline(
            &mut writer,
            b"a complete frame",
            Duration::from_secs(1),
            &AtomicBool::new(true),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(writer.bytes, b"a complete frame");
    }

    struct FullWriter;
    impl Write for FullWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(ErrorKind::WouldBlock.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn backpressure_stops_at_frame_deadline_or_shutdown() {
        let started = Instant::now();
        let error = write_with_deadline(
            &mut FullWriter,
            b"frame",
            Duration::from_millis(60),
            &AtomicBool::new(true),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        let error = write_with_deadline(
            &mut FullWriter,
            b"frame",
            Duration::from_secs(5),
            &AtomicBool::new(true),
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
    }
}
