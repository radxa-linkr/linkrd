use crate::{
    clipboard::ClipboardBackend,
    protocol::{self, *},
};
use clipboard_changed::State;
use clipboard_set_response::Result as SetResult;
use envelope::Payload;
use prost::Message;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("invalid protocol message: {0}")]
    Invalid(&'static str),
    #[error("protocol counter exhausted; reconnect required")]
    Exhausted,
}

struct Peer {
    id: Vec<u8>,
    hello: Hello,
    max_frame: usize,
    max_text: usize,
    write_text: bool,
}

struct CachedWrite {
    id: u64,
    request: ClipboardSetRequest,
    result: SetResult,
}

pub struct Session {
    id: Vec<u8>,
    next_message: u64,
    peer: Option<Peer>,
    highest_received: u64,
    revision: u64,
    last_state: Option<State>,
    last_write: Option<CachedWrite>,
    pending_ping: Option<u64>,
}

impl Session {
    pub fn new(id: [u8; 16]) -> Self {
        Self {
            id: id.to_vec(),
            next_message: 2,
            peer: None,
            highest_received: 0,
            revision: 0,
            last_state: None,
            last_write: None,
            pending_ping: None,
        }
    }

    pub fn hello(&self) -> Envelope {
        Envelope {
            protocol_major: PROTOCOL_MAJOR,
            session_id: self.id.clone(),
            message_id: 1,
            reply_to: None,
            payload: Some(Payload::Hello(protocol::hello())),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.peer.is_some()
    }

    pub fn max_send_bytes(&self) -> usize {
        self.peer
            .as_ref()
            .map_or(HELLO_MAX_BYTES, |peer| peer.max_frame)
    }

    pub fn awaiting_pong(&self) -> bool {
        self.pending_ping.is_some()
    }

    pub fn ping(&mut self) -> Result<Envelope, SessionError> {
        if !self.is_ready() || self.pending_ping.is_some() {
            return Err(SessionError::Invalid("cannot send Ping now"));
        }
        let ping = self.envelope(Payload::Ping(Ping {}), None)?;
        self.pending_ping = Some(ping.message_id);
        Ok(ping)
    }

    pub fn receive(
        &mut self,
        message: Envelope,
        clipboard: &mut impl ClipboardBackend,
    ) -> Result<Vec<Envelope>, SessionError> {
        if message.protocol_major != PROTOCOL_MAJOR
            || message.session_id.len() != 16
            || message.session_id.iter().all(|byte| *byte == 0)
            || message.message_id == 0
        {
            return Err(SessionError::Invalid("version, session ID or message ID"));
        }
        if message.encoded_len() > MAX_FRAME_BYTES {
            return Err(SessionError::Invalid("message exceeds local frame limit"));
        }
        if let Some(Payload::Hello(hello)) = &message.payload {
            if message.reply_to.is_some() || message.encoded_len() > HELLO_MAX_BYTES {
                return Err(SessionError::Invalid("invalid Hello envelope"));
            }
            if let Some(peer) = &self.peer {
                if peer.id != message.session_id || peer.hello != *hello {
                    return Err(SessionError::Invalid("peer changed during session"));
                }
                self.highest_received = self.highest_received.max(message.message_id);
                return Ok(Vec::new());
            }
            if message.session_id == self.id
                || !(MIN_FRAME_BYTES..=MAX_FRAME_BYTES).contains(&(hello.max_frame_bytes as usize))
                || !(1..=MAX_CLIPBOARD_BYTES).contains(&(hello.max_clipboard_bytes as usize))
                || !hello.capabilities.iter().any(|cap| cap == WATCH_TEXT)
            {
                return Err(SessionError::Invalid(
                    "unsupported Hello capabilities or limits",
                ));
            }
            let max_frame = hello.max_frame_bytes as usize;
            self.peer = Some(Peer {
                id: message.session_id.clone(),
                hello: hello.clone(),
                max_frame,
                max_text: (hello.max_clipboard_bytes as usize).min(max_frame - CONTENT_OVERHEAD),
                write_text: hello.capabilities.iter().any(|cap| cap == WRITE_TEXT),
            });
            self.highest_received = message.message_id;
            if let Some(peer) = &self.peer {
                tracing::debug!(
                    max_frame = peer.max_frame,
                    max_text = peer.max_text,
                    "peer limits negotiated"
                );
            }
            return Ok(self.observe(clipboard)?.into_iter().collect());
        }
        let Some(peer) = &self.peer else {
            return Err(SessionError::Invalid("business message before Hello"));
        };
        if message.session_id != peer.id {
            return Err(SessionError::Invalid("stale peer session"));
        }
        let reference = MessageRef {
            session_id: message.session_id,
            message_id: message.message_id,
        };
        let payload = message
            .payload
            .ok_or(SessionError::Invalid("missing or unknown payload"))?;
        if !matches!(payload, Payload::Pong(_)) && message.reply_to.is_some() {
            return Err(SessionError::Invalid("unexpected reply_to"));
        }
        if message.message_id <= self.highest_received {
            if let Payload::ClipboardSetRequest(request) = payload {
                let result = match &self.last_write {
                    Some(cached)
                        if cached.id == message.message_id && cached.request == request =>
                    {
                        cached.result.clone()
                    }
                    _ => SetResult::Error(error(
                        ErrorCode::InvalidRequest,
                        "stale or reused request ID",
                    )),
                };
                return Ok(vec![self.response(reference, result)?]);
            }
            return Err(SessionError::Invalid("replayed message ID"));
        }
        self.highest_received = message.message_id;
        match payload {
            Payload::ClipboardSetRequest(request) => self.apply(reference, request, clipboard),
            Payload::Ping(_) => Ok(vec![
                self.envelope(Payload::Pong(Pong {}), Some(reference))?,
            ]),
            Payload::Pong(_) => {
                let expected = self.pending_ping.map(|message_id| MessageRef {
                    session_id: self.id.clone(),
                    message_id,
                });
                if expected.is_none() || message.reply_to != expected {
                    return Err(SessionError::Invalid("unexpected Pong correlation"));
                }
                self.pending_ping = None;
                Ok(Vec::new())
            }
            Payload::Hello(_) => Err(SessionError::Invalid("unexpected Hello")),
            Payload::ClipboardSetResponse(_) | Payload::ClipboardChanged(_) => {
                Err(SessionError::Invalid("message has wrong direction"))
            }
        }
    }

    pub fn observe(
        &mut self,
        clipboard: &mut impl ClipboardBackend,
    ) -> Result<Option<Envelope>, SessionError> {
        if self.peer.is_none() {
            return Ok(None);
        }
        let peer = self.peer.as_ref().expect("handshake checked");
        let state = match clipboard.read_text() {
            Ok(Some(text)) if text.len() > peer.max_text => State::Unavailable(error(
                ErrorCode::TooLarge,
                "clipboard text exceeds negotiated limit",
            )),
            Ok(Some(text)) if text.contains('\0') => State::Unavailable(error(
                ErrorCode::Unsupported,
                "embedded NUL is not supported in v1 text",
            )),
            Ok(Some(text)) => State::Content(text_content(text)),
            Ok(None) => State::Unavailable(error(ErrorCode::NoText, "clipboard has no text")),
            Err(failure) => State::Unavailable(failure),
        };
        if self.last_state.as_ref() == Some(&state) {
            return Ok(None);
        }
        self.publish(state, None).map(Some)
    }

    fn apply(
        &mut self,
        reference: MessageRef,
        request: ClipboardSetRequest,
        clipboard: &mut impl ClipboardBackend,
    ) -> Result<Vec<Envelope>, SessionError> {
        let value = self.validate_write(&request);
        let mut changed = None;
        let result = match value {
            Err(failure) => SetResult::Error(failure),
            Ok(value) => {
                if self.revision == u64::MAX {
                    return Err(SessionError::Exhausted);
                }
                let applied = clipboard.write_text(value);
                match applied {
                    Err(failure) => SetResult::Error(failure),
                    Ok(()) => {
                        let state = State::Content(text_content(value.to_owned()));
                        changed = Some(self.publish(state, Some(reference.clone()))?);
                        SetResult::Applied(ClipboardApplied {
                            revision: self.revision,
                        })
                    }
                }
            }
        };
        if request.encoded_len() <= MAX_CLIPBOARD_BYTES + CONTENT_OVERHEAD {
            self.last_write = Some(CachedWrite {
                id: reference.message_id,
                request,
                result: result.clone(),
            });
        } else {
            self.last_write = None;
        }
        let response = self.response(reference, result)?;
        let mut messages = Vec::with_capacity(2);
        if let Some(changed) = changed {
            messages.push(changed);
        }
        messages.push(response);
        Ok(messages)
    }

    fn validate_write<'a>(
        &self,
        request: &'a ClipboardSetRequest,
    ) -> Result<&'a str, ProtocolError> {
        let Some(peer) = &self.peer else {
            return Err(error(ErrorCode::InvalidRequest, "handshake required"));
        };
        let Some(content) = &request.content else {
            return Err(error(
                ErrorCode::InvalidRequest,
                "missing clipboard content",
            ));
        };
        if !peer.write_text {
            return Err(error(
                ErrorCode::Unsupported,
                "text writing was not negotiated",
            ));
        }
        let [representation] = content.representations.as_slice() else {
            return Err(error(
                ErrorCode::InvalidRequest,
                "v1 requires one representation",
            ));
        };
        if representation.mime_type != TEXT_MIME {
            return Err(error(ErrorCode::Unsupported, "unsupported MIME type"));
        }
        if representation.data.len() > peer.max_text {
            return Err(error(ErrorCode::TooLarge, "text exceeds negotiated limit"));
        }
        let text = std::str::from_utf8(&representation.data)
            .map_err(|_| error(ErrorCode::InvalidRequest, "text is not UTF-8"))?;
        if text.contains('\0') {
            return Err(error(
                ErrorCode::Unsupported,
                "embedded NUL is not supported in v1 text",
            ));
        }
        Ok(text)
    }

    fn publish(
        &mut self,
        state: State,
        caused_by: Option<MessageRef>,
    ) -> Result<Envelope, SessionError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(SessionError::Exhausted)?;
        self.last_state = Some(state.clone());
        self.envelope(
            Payload::ClipboardChanged(ClipboardChanged {
                revision: self.revision,
                caused_by,
                state: Some(state),
            }),
            None,
        )
    }

    fn response(
        &mut self,
        reference: MessageRef,
        result: SetResult,
    ) -> Result<Envelope, SessionError> {
        self.envelope(
            Payload::ClipboardSetResponse(ClipboardSetResponse {
                result: Some(result),
            }),
            Some(reference),
        )
    }

    fn envelope(
        &mut self,
        payload: Payload,
        reply_to: Option<MessageRef>,
    ) -> Result<Envelope, SessionError> {
        let message_id = self.next_message;
        self.next_message = self
            .next_message
            .checked_add(1)
            .ok_or(SessionError::Exhausted)?;
        Ok(Envelope {
            protocol_major: PROTOCOL_MAJOR,
            session_id: self.id.clone(),
            message_id,
            reply_to,
            payload: Some(payload),
        })
    }
}
