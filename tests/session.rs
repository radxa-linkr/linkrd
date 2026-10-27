use clipboard_changed::State;
use clipboard_set_response::Result as SetResult;
use envelope::Payload;
use linkrd::{clipboard::ClipboardBackend, protocol::*, session::Session};
use prost::Message;

#[derive(Default)]
struct FakeClipboard {
    text: Option<String>,
    writes: usize,
    fail_read: bool,
    fail_write: bool,
}
impl ClipboardBackend for FakeClipboard {
    fn read_text(&mut self) -> Result<Option<String>, ProtocolError> {
        if self.fail_read {
            Err(failure(ErrorCode::ReadFailed))
        } else {
            Ok(self.text.clone())
        }
    }
    fn write_text(&mut self, text: &str) -> Result<(), ProtocolError> {
        if self.fail_write {
            return Err(failure(ErrorCode::WriteFailed));
        }
        self.writes += 1;
        self.text = Some(text.into());
        Ok(())
    }
}
fn failure(code: ErrorCode) -> ProtocolError {
    ProtocolError {
        code: code as i32,
        detail: "failed".into(),
    }
}
fn peer(id: u64, payload: Payload) -> Envelope {
    Envelope {
        protocol_major: 1,
        session_id: vec![2; 16],
        message_id: id,
        reply_to: None,
        payload: Some(payload),
    }
}
fn set(id: u64, text: &str) -> Envelope {
    peer(
        id,
        Payload::ClipboardSetRequest(ClipboardSetRequest {
            content: Some(text_content(text.into())),
        }),
    )
}
fn connect(clipboard: &mut FakeClipboard) -> (Session, Vec<Envelope>) {
    let mut session = Session::new([1; 16]);
    let messages = session
        .receive(peer(1, Payload::Hello(hello())), clipboard)
        .unwrap();
    (session, messages)
}
fn changed(message: &Envelope) -> &ClipboardChanged {
    match message.payload.as_ref().unwrap() {
        Payload::ClipboardChanged(event) => event,
        _ => panic!("expected change"),
    }
}
fn outcome(message: &Envelope) -> &SetResult {
    match message.payload.as_ref().unwrap() {
        Payload::ClipboardSetResponse(response) => response.result.as_ref().unwrap(),
        _ => panic!("expected response"),
    }
}
fn has_error(message: &Envelope, code: ErrorCode) -> bool {
    matches!(outcome(message), SetResult::Error(error) if error.code == code as i32)
}

#[test]
fn initial_snapshot_and_automatic_changes_are_deduplicated() {
    let mut clipboard = FakeClipboard {
        text: Some("初始 📋\ntext".into()),
        ..Default::default()
    };
    let (mut session, initial) = connect(&mut clipboard);
    assert_eq!(initial.len(), 1);
    assert_eq!(changed(&initial[0]).revision, 1);
    assert!(
        matches!(&changed(&initial[0]).state, Some(State::Content(content)) if content == &text_content("初始 📋\ntext".into()))
    );
    assert!(session.observe(&mut clipboard).unwrap().is_none());
    clipboard.text = Some("new".into());
    assert_eq!(
        changed(&session.observe(&mut clipboard).unwrap().unwrap()).revision,
        2
    );
    assert_eq!(clipboard.writes, 0);
}

#[test]
fn remote_write_reports_result_once_without_echo_and_preserves_new_local_copy() {
    let mut clipboard = FakeClipboard::default();
    let (mut session, _) = connect(&mut clipboard);
    let request = set(2, "主控机文本");
    let messages = session.receive(request.clone(), &mut clipboard).unwrap();
    assert_eq!(messages.len(), 2);
    let event = changed(&messages[0]);
    assert_eq!(
        event.caused_by,
        Some(MessageRef {
            session_id: vec![2; 16],
            message_id: 2
        })
    );
    assert_eq!(
        outcome(&messages[1]),
        &SetResult::Applied(ClipboardApplied {
            revision: event.revision
        })
    );
    assert_eq!(messages[1].reply_to, event.caused_by);
    assert!(messages[0].message_id < messages[1].message_id);
    assert!(session.observe(&mut clipboard).unwrap().is_none());
    clipboard.text = Some("local user copied again".into());
    assert!(session.observe(&mut clipboard).unwrap().is_some());
    let duplicate = session.receive(request, &mut clipboard).unwrap();
    assert_eq!(duplicate.len(), 1);
    assert_eq!(outcome(&duplicate[0]), outcome(&messages[1]));
    assert_eq!(clipboard.writes, 1);
    assert_eq!(clipboard.text.as_deref(), Some("local user copied again"));
}

#[test]
fn invalid_text_and_missing_content_never_modify_clipboard() {
    let cases = [
        (None, ErrorCode::InvalidRequest),
        (
            Some(ClipboardContent {
                representations: vec![],
            }),
            ErrorCode::InvalidRequest,
        ),
        (
            Some(ClipboardContent {
                representations: vec![Representation {
                    mime_type: "image/png".into(),
                    data: vec![1],
                }],
            }),
            ErrorCode::Unsupported,
        ),
        (
            Some(ClipboardContent {
                representations: vec![Representation {
                    mime_type: TEXT_MIME.into(),
                    data: vec![0xff],
                }],
            }),
            ErrorCode::InvalidRequest,
        ),
        (Some(text_content("a\0b".into())), ErrorCode::Unsupported),
        (
            Some(text_content("x".repeat(MAX_CLIPBOARD_BYTES + 1))),
            ErrorCode::TooLarge,
        ),
    ];
    let mut clipboard = FakeClipboard {
        text: Some("keep".into()),
        ..Default::default()
    };
    let (mut session, _) = connect(&mut clipboard);
    for (index, (content, expected)) in cases.into_iter().enumerate() {
        let response = session
            .receive(
                peer(
                    index as u64 + 2,
                    Payload::ClipboardSetRequest(ClipboardSetRequest { content }),
                ),
                &mut clipboard,
            )
            .unwrap();
        assert_eq!(response.len(), 1);
        assert!(has_error(&response[0], expected));
    }
    assert_eq!(clipboard.writes, 0);
    assert_eq!(clipboard.text.as_deref(), Some("keep"));
}

#[test]
fn empty_text_is_valid_and_no_text_or_read_failure_never_clears_clipboard() {
    let mut clipboard = FakeClipboard::default();
    let (mut session, messages) = connect(&mut clipboard);
    assert!(
        matches!(changed(&messages[0]).state, Some(State::Unavailable(ref e)) if e.code == ErrorCode::NoText as i32)
    );
    let messages = session.receive(set(2, ""), &mut clipboard).unwrap();
    assert!(matches!(outcome(&messages[1]), SetResult::Applied(_)));
    assert_eq!(clipboard.text.as_deref(), Some(""));
    clipboard.fail_read = true;
    let event = session.observe(&mut clipboard).unwrap().unwrap();
    assert!(
        matches!(changed(&event).state, Some(State::Unavailable(ref e)) if e.code == ErrorCode::ReadFailed as i32)
    );
    assert!(session.observe(&mut clipboard).unwrap().is_none());
    clipboard.fail_read = false;
    assert!(session.observe(&mut clipboard).unwrap().is_some());
    assert_eq!(clipboard.writes, 1);
}

#[test]
fn failed_write_is_not_published_or_retried_as_a_new_operation() {
    let mut clipboard = FakeClipboard {
        text: Some("keep".into()),
        fail_write: true,
        ..Default::default()
    };
    let (mut session, _) = connect(&mut clipboard);
    let request = set(2, "bad");
    let failed = session.receive(request.clone(), &mut clipboard).unwrap();
    assert_eq!(failed.len(), 1);
    assert!(has_error(&failed[0], ErrorCode::WriteFailed));
    clipboard.fail_write = false;
    assert!(has_error(
        &session.receive(request, &mut clipboard).unwrap()[0],
        ErrorCode::WriteFailed
    ));
    assert_eq!(clipboard.writes, 0);
    assert!(session.observe(&mut clipboard).unwrap().is_none());
    assert_eq!(
        session
            .receive(set(3, "retry explicitly"), &mut clipboard)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn old_and_reused_request_ids_cannot_repeat_side_effects() {
    let mut clipboard = FakeClipboard::default();
    let (mut session, _) = connect(&mut clipboard);
    session.receive(set(2, "first"), &mut clipboard).unwrap();
    assert!(has_error(
        &session
            .receive(set(2, "different"), &mut clipboard)
            .unwrap()[0],
        ErrorCode::InvalidRequest
    ));
    session.receive(set(3, "second"), &mut clipboard).unwrap();
    assert!(has_error(
        &session.receive(set(2, "first"), &mut clipboard).unwrap()[0],
        ErrorCode::InvalidRequest
    ));
    assert_eq!(clipboard.writes, 2);
    assert_eq!(clipboard.text.as_deref(), Some("second"));
}

#[test]
fn handshake_and_session_validation_prevent_writes() {
    let mut clipboard = FakeClipboard::default();
    let mut session = Session::new([1; 16]);
    assert!(
        session
            .receive(set(2, "before hello"), &mut clipboard)
            .is_err()
    );
    let (mut session, _) = connect(&mut clipboard);
    let mut request = set(2, "stale");
    request.session_id = vec![3; 16];
    assert!(session.receive(request, &mut clipboard).is_err());
    let mut request = set(2, "wrong version");
    request.protocol_major = 2;
    assert!(session.receive(request, &mut clipboard).is_err());
    let mut request = set(2, "wrong reference");
    request.reply_to = Some(MessageRef {
        session_id: vec![1; 16],
        message_id: 1,
    });
    assert!(session.receive(request, &mut clipboard).is_err());
    assert_eq!(clipboard.writes, 0);
    assert!(
        session
            .receive(peer(1, Payload::Hello(hello())), &mut clipboard)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn negotiated_limits_apply_to_uploads_and_writes() {
    let mut clipboard = FakeClipboard {
        text: Some("x".repeat(600)),
        ..Default::default()
    };
    let mut session = Session::new([1; 16]);
    let mut greeting = hello();
    greeting.max_frame_bytes = MIN_FRAME_BYTES as u32;
    let messages = session
        .receive(peer(1, Payload::Hello(greeting)), &mut clipboard)
        .unwrap();
    assert!(messages[0].encoded_len() <= MIN_FRAME_BYTES);
    assert!(
        matches!(changed(&messages[0]).state, Some(State::Unavailable(ref e)) if e.code == ErrorCode::TooLarge as i32)
    );
    let response = session
        .receive(set(2, &"x".repeat(600)), &mut clipboard)
        .unwrap();
    assert!(has_error(&response[0], ErrorCode::TooLarge));
    let messages = session
        .receive(
            set(3, &"x".repeat(MIN_FRAME_BYTES - CONTENT_OVERHEAD)),
            &mut clipboard,
        )
        .unwrap();
    assert!(messages.iter().all(|m| m.encoded_len() <= MIN_FRAME_BYTES));
}

#[test]
fn malformed_hello_and_unnegotiated_write_are_rejected() {
    let mut clipboard = FakeClipboard::default();
    for limit in [0, 1, MAX_FRAME_BYTES as u32 + 1] {
        let mut session = Session::new([1; 16]);
        let mut greeting = hello();
        greeting.max_frame_bytes = limit;
        assert!(
            session
                .receive(peer(1, Payload::Hello(greeting)), &mut clipboard)
                .is_err()
        );
    }
    let mut session = Session::new([1; 16]);
    let mut greeting = hello();
    greeting.capabilities = vec![WATCH_TEXT.into()];
    session
        .receive(peer(1, Payload::Hello(greeting)), &mut clipboard)
        .unwrap();
    assert!(has_error(
        &session
            .receive(set(2, "not allowed"), &mut clipboard)
            .unwrap()[0],
        ErrorCode::Unsupported
    ));
    assert_eq!(clipboard.writes, 0);
}

#[test]
fn heartbeat_requires_matching_response_and_unknown_payload_is_not_success() {
    let mut clipboard = FakeClipboard::default();
    let (mut session, _) = connect(&mut clipboard);
    let ping = session.ping().unwrap();
    assert!(session.awaiting_pong());
    let mut pong = peer(2, Payload::Pong(Pong {}));
    pong.reply_to = Some(MessageRef {
        session_id: ping.session_id,
        message_id: ping.message_id,
    });
    session.receive(pong, &mut clipboard).unwrap();
    assert!(!session.awaiting_pong());
    let response = session
        .receive(peer(3, Payload::Ping(Ping {})), &mut clipboard)
        .unwrap();
    assert!(matches!(response[0].payload, Some(Payload::Pong(_))));
    let mut unknown = peer(4, Payload::Ping(Ping {}));
    unknown.payload = None;
    assert!(session.receive(unknown, &mut clipboard).is_err());
}

#[test]
fn new_session_publishes_fresh_snapshot_and_revision_one() {
    let mut clipboard = FakeClipboard::default();
    let (mut first, _) = connect(&mut clipboard);
    first
        .receive(set(2, "retained on reconnect"), &mut clipboard)
        .unwrap();
    let mut next = Session::new([3; 16]);
    let event = next
        .receive(peer(1, Payload::Hello(hello())), &mut clipboard)
        .unwrap()
        .remove(0);
    assert_eq!(event.session_id, vec![3; 16]);
    assert_eq!(changed(&event).revision, 1);
    assert!(
        matches!(&changed(&event).state, Some(State::Content(content)) if content == &text_content("retained on reconnect".into()))
    );
}
