use crate::protocol::{self, Envelope, ErrorCode, envelope::Payload};
use anyhow::{Context, Result};
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::{ErrorCounter, WorkerGuard};
use tracing_subscriber::{filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt};

mod rolling;
use rolling::SizeLimitedWriter;

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const MAX_LOG_FILES: usize = 7;

pub struct LoggingGuard {
    _worker: Option<WorkerGuard>,
    dropped: Option<ErrorCounter>,
}

impl Drop for LoggingGuard {
    fn drop(&mut self) {
        if let Some(counter) = &self.dropped {
            let dropped = counter.dropped_lines();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    "file log queue overflowed; some log writes were dropped"
                );
            }
        }
    }
}

pub fn default_log_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("linkrd")
        .join("logs")
}

fn file_appender(directory: &Path) -> Result<SizeLimitedWriter> {
    SizeLimitedWriter::new(directory, MAX_LOG_BYTES, MAX_LOG_FILES)
        .context("cannot initialize rolling log file")
}

pub fn init(level: LevelFilter, directory: Option<&Path>) -> Result<LoggingGuard> {
    let mut failure = None;
    let appender = directory.and_then(|directory| match file_appender(directory) {
        Ok(appender) => Some(appender),
        Err(error) => {
            failure = Some(error);
            None
        }
    });
    let mut guard = LoggingGuard {
        _worker: None,
        dropped: None,
    };
    let file = appender.map(|appender| {
        let (writer, worker) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(1024)
            .lossy(true)
            .finish(appender);
        guard.dropped = Some(writer.error_counter());
        guard._worker = Some(worker);
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
    });
    let terminal_color = std::io::stderr().is_terminal()
        && !std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
        && std::env::var_os("TERM").is_none_or(|value| value != "dumb");
    tracing_subscriber::registry()
        .with(filter_fn(|metadata| {
            metadata.target() == "linkrd" || metadata.target().starts_with("linkrd::")
        }))
        .with(level)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(terminal_color)
                .with_writer(std::io::stderr),
        )
        .with(file)
        .try_init()
        .map_err(|error| anyhow::anyhow!("cannot initialize logging: {error}"))?;
    if let Some(error) = failure {
        eprintln!("linkrd: {error:#}; continuing with stderr logging only");
    } else if let Some(directory) = directory {
        tracing::info!(directory = %directory.display(), max_file_bytes = MAX_LOG_BYTES, max_files = MAX_LOG_FILES, "file logging enabled (daily and size rotation)");
    }
    Ok(guard)
}

fn code_name(code: i32) -> &'static str {
    ErrorCode::try_from(code).map_or("UNKNOWN", |code| code.as_str_name())
}

pub(crate) fn record_incoming(message: &Envelope) {
    if matches!(message.payload, Some(Payload::ClipboardSetRequest(_))) {
        tracing::debug!(
            request_id = message.message_id,
            "text clipboard write received"
        );
    }
}

pub(crate) fn record_outgoing(message: &Envelope) {
    let request_id = message
        .reply_to
        .as_ref()
        .map(|reference| reference.message_id);
    match &message.payload {
        Some(Payload::ClipboardChanged(changed)) => match &changed.state {
            Some(protocol::clipboard_changed::State::Unavailable(error))
                if error.code == ErrorCode::NoText as i32 =>
            {
                tracing::debug!(
                    revision = changed.revision,
                    "clipboard has no supported content"
                );
            }
            Some(protocol::clipboard_changed::State::Unavailable(error)) => {
                tracing::warn!(
                    revision = changed.revision,
                    code = error.code,
                    error_name = code_name(error.code),
                    "clipboard unavailable"
                );
            }
            Some(protocol::clipboard_changed::State::Content(content)) => {
                tracing::debug!(
                    revision = changed.revision,
                    bytes = content
                        .representations
                        .iter()
                        .map(|r| r.data.len())
                        .sum::<usize>(),
                    remote_write = changed.caused_by.is_some(),
                    "clipboard update queued"
                );
            }
            None => (),
        },
        Some(Payload::ClipboardSetResponse(response)) => match &response.result {
            Some(protocol::clipboard_set_response::Result::Error(error)) => {
                tracing::warn!(
                    ?request_id,
                    code = error.code,
                    error_name = code_name(error.code),
                    "clipboard write rejected or failed"
                );
            }
            Some(protocol::clipboard_set_response::Result::Applied(applied)) => {
                tracing::info!(
                    ?request_id,
                    revision = applied.revision,
                    "clipboard write applied"
                );
            }
            None => (),
        },
        Some(Payload::Ping(_)) => {
            tracing::trace!(message_id = message.message_id, "heartbeat Ping queued")
        }
        Some(Payload::Pong(_)) => tracing::trace!(?request_id, "heartbeat Pong queued"),
        Some(Payload::Hello(_)) => tracing::debug!("Hello queued"),
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::*;

    #[test]
    fn text_and_peer_error_details_never_appear_in_protocol_logs() {
        let root = tempfile::tempdir().unwrap();
        let (writer, guard) = tracing_appender::non_blocking(file_appender(root.path()).unwrap());
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(LevelFilter::TRACE)
            .with_writer(writer)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let private = "PRIVATE_CLIPBOARD_DATA";
            record_incoming(&Envelope {
                message_id: 42,
                payload: Some(Payload::ClipboardSetRequest(ClipboardSetRequest {
                    content: Some(text_content(private.into())),
                })),
                ..Default::default()
            });
            for payload in [
                Payload::ClipboardChanged(ClipboardChanged {
                    revision: 1,
                    caused_by: None,
                    state: Some(clipboard_changed::State::Content(text_content(
                        private.into(),
                    ))),
                }),
                Payload::ClipboardSetResponse(ClipboardSetResponse {
                    result: Some(clipboard_set_response::Result::Error(ProtocolError {
                        code: ErrorCode::AccessDenied as i32,
                        detail: private.into(),
                    })),
                }),
            ] {
                record_outgoing(&Envelope {
                    payload: Some(payload),
                    reply_to: Some(MessageRef {
                        session_id: vec![2; 16],
                        message_id: 42,
                    }),
                    ..Default::default()
                });
            }
        });
        drop(guard);
        let log = std::fs::read_to_string(root.path().join("linkrd.log")).unwrap();
        assert!(log.contains("ERROR_CODE_ACCESS_DENIED"));
        assert!(log.contains("request_id=42"));
        assert!(log.contains("clipboard update queued"));
        assert!(!log.contains("PRIVATE_CLIPBOARD_DATA"));
        assert!(!log.contains('\u{1b}'));
    }
}
