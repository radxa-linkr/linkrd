use crate::protocol::{ErrorCode, ProtocolError, error};
use clipboard_rs::{
    Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext,
    ContentFormat, WatcherShutdown,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

fn with_platform_pool<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| f())
    }
    #[cfg(not(target_os = "macos"))]
    {
        f()
    }
}

struct ChangeSignal(Arc<AtomicBool>);
impl ClipboardHandler for ChangeSignal {
    fn on_clipboard_change(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct ClipboardMonitor {
    shutdown: Option<WatcherShutdown>,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardMonitor {
    pub fn start(dirty: Arc<AtomicBool>) -> anyhow::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || {
                let watcher = with_platform_pool(ClipboardWatcherContext::new);
                match watcher {
                    Ok(mut watcher) => {
                        let shutdown = watcher
                            .add_handler(ChangeSignal(dirty))
                            .get_shutdown_channel();
                        if ready_tx.send(Ok(shutdown)).is_ok() {
                            watcher.start_watch();
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(anyhow::anyhow!(
                            "cannot create clipboard watcher: {error}"
                        )));
                    }
                }
            })?;
        match ready_rx.recv() {
            Ok(Ok(shutdown)) => Ok(Self {
                shutdown: Some(shutdown),
                thread: Some(thread),
            }),
            result => {
                let _ = thread.join();
                match result {
                    Ok(Err(error)) => Err(error),
                    _ => anyhow::bail!("clipboard watcher exited during initialization"),
                }
            }
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for ClipboardMonitor {
    fn drop(&mut self) {
        drop(self.shutdown.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("clipboard watcher exited with an error");
        }
    }
}

pub trait ClipboardBackend {
    fn read_text(&mut self) -> Result<Option<String>, ProtocolError>;
    fn write_text(&mut self, text: &str) -> Result<(), ProtocolError>;
}

pub struct SystemClipboard(ClipboardContext);

impl SystemClipboard {
    pub fn new() -> anyhow::Result<Self> {
        with_platform_pool(ClipboardContext::new)
            .map(Self)
            .map_err(|e| anyhow::anyhow!("cannot open user clipboard: {e}"))
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read_text(&mut self) -> Result<Option<String>, ProtocolError> {
        with_platform_pool(|| {
            self.0
                .available_formats()
                .map_err(|_| error(ErrorCode::ReadFailed, "cannot enumerate clipboard formats"))?;
            if self.0.has(ContentFormat::Files) || !self.0.has(ContentFormat::Text) {
                return Ok(None);
            }
            self.0
                .get_text()
                .map(Some)
                .map_err(|_| error(ErrorCode::ReadFailed, "cannot read clipboard text"))
        })
    }

    fn write_text(&mut self, text: &str) -> Result<(), ProtocolError> {
        with_platform_pool(|| {
            self.0
                .set_text(text.to_owned())
                .map_err(|_| error(ErrorCode::WriteFailed, "cannot write clipboard text"))
        })
    }
}
