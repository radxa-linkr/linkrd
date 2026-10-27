use anyhow::{Context, Result};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use std::time::Duration;

pub fn close(reader: Box<dyn SerialPort>, writer: Box<dyn SerialPort>) {
    let started = std::time::Instant::now();
    tracing::debug!("discarding pending ACM output before close");
    if let Err(error) = writer.clear(serialport::ClearBuffer::Output) {
        tracing::warn!(%error, "cannot discard pending ACM output; driver close may wait");
    }
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ACM output discard finished; closing handles"
    );
    drop(reader);
    drop(writer);
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ACM handles closed"
    );
}

pub fn open(name: &str, baud: u32) -> Result<Box<dyn SerialPort>> {
    let builder = serialport::new(name, baud)
        .data_bits(DataBits::Eight)
        .stop_bits(StopBits::One)
        .parity(Parity::None)
        .flow_control(FlowControl::None)
        .timeout(Duration::from_millis(250));
    #[cfg(unix)]
    {
        let port = builder
            .open_native()
            .with_context(|| format!("cannot open {name}"))?;
        nonblocking(&port).context("cannot enable nonblocking ACM I/O")?;
        Ok(Box::new(port))
    }
    #[cfg(not(unix))]
    {
        builder
            .open()
            .with_context(|| format!("cannot open {name}"))
    }
}

#[cfg(unix)]
fn nonblocking(port: &serialport::TTYPort) -> std::io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    use std::os::fd::{AsRawFd, BorrowedFd};

    let fd = unsafe { BorrowedFd::borrow_raw(port.as_raw_fd()) };
    let flags = fcntl_getfl(fd)?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        clipboard::ClipboardBackend,
        protocol::ProtocolError,
        transport::{ConnectionOptions, run_connection},
    };
    use std::{
        io::{self, Write},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Instant,
    };

    struct EmptyClipboard;
    impl ClipboardBackend for EmptyClipboard {
        fn read_text(&mut self) -> Result<Option<String>, ProtocolError> {
            Ok(None)
        }
        fn write_text(&mut self, _: &str) -> Result<(), ProtocolError> {
            panic!("unexpected write")
        }
    }

    struct ObservedWriter(serialport::TTYPort, mpsc::SyncSender<()>);
    impl Write for ObservedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.1.try_send(());
            self.0.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    #[test]
    fn shutdown_joins_serial_workers_with_a_full_real_tty_buffer() {
        let (peer, mut port) = serialport::TTYPort::pair().unwrap();
        nonblocking(&port).unwrap();
        let mut writer = port.try_clone_native().unwrap();
        writer.set_timeout(Duration::from_millis(250)).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let (writing_tx, writing_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut filled = 0;
            loop {
                match writer.write(&[0x55; 64 * 1024]) {
                    Ok(0) => panic!("tty write made no progress"),
                    Ok(n) => filled += n,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("fill tty: {error}"),
                }
                assert!(filled < 16 * 1024 * 1024, "tty buffer never filled");
            }
            assert!(filled > 0);
            let mut writer = ObservedWriter(writer, writing_tx);
            let result = run_connection(
                &mut port,
                &mut writer,
                &mut EmptyClipboard,
                &AtomicBool::new(false),
                &worker_shutdown,
                ConnectionOptions::default(),
            );
            close(Box::new(port), Box::new(writer.0));
            let _ = done_tx.send(result);
        });
        writing_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("writer did not reach a full tty");
        let started = Instant::now();
        shutdown.store(true, Ordering::Release);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("shutdown blocked on tty I/O")
            .unwrap();
        worker.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "slow shutdown: {:?}",
            started.elapsed()
        );
        drop(peer);
    }
}
