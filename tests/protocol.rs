use envelope::Payload;
use linkrd::{clipboard::ClipboardBackend, protocol::*, session::Session};
use prost::Message;

#[derive(Default)]
struct Clipboard {
    writes: usize,
    text: Option<String>,
}

impl ClipboardBackend for Clipboard {
    fn read_text(&mut self) -> Result<Option<String>, ProtocolError> {
        Ok(self.text.clone())
    }
    fn write_text(&mut self, text: &str) -> Result<(), ProtocolError> {
        self.writes += 1;
        self.text = Some(text.into());
        Ok(())
    }
}

#[test]
fn existing_linkr_go_frames_match_both_text_directions_and_write_confirmation() {
    use linkrd::framing::{FrameReader, encode_frame};
    use std::{io::Cursor, time::Duration};

    let decode = |bytes: &[u8]| {
        FrameReader::new(Cursor::new(bytes), MAX_FRAME_BYTES, Duration::from_secs(1))
            .poll()
            .unwrap()
            .unwrap()
    };
    let mut session = Session::new([1; 16]);
    let mut clipboard = Clipboard::default();
    session
        .receive(
            decode(include_bytes!("fixtures/linkr-hello.frame")),
            &mut clipboard,
        )
        .unwrap();
    let messages = session
        .receive(
            decode(include_bytes!("fixtures/linkr-text.frame")),
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(
        clipboard.text.as_deref(),
        Some("Linkr → Linkrd 文本同步\nsecond line")
    );
    assert_eq!(clipboard.writes, 1);
    assert_eq!(messages.len(), 2);
    assert_eq!(
        encode_frame(&messages[0], MAX_FRAME_BYTES).unwrap(),
        include_bytes!("fixtures/linkr-changed.frame")
    );
    assert_eq!(
        encode_frame(&messages[1], MAX_FRAME_BYTES).unwrap(),
        include_bytes!("fixtures/linkr-applied.frame")
    );
    assert!(session.observe(&mut clipboard).unwrap().is_none());
}

fn peer(id: u64, payload: Option<Payload>) -> Envelope {
    Envelope {
        protocol_major: 1,
        session_id: vec![2; 16],
        message_id: id,
        payload,
        ..Default::default()
    }
}

#[test]
fn handshake_advertises_only_text_and_accepts_a_peer_with_extra_capabilities() {
    assert_eq!(hello().capabilities, [WATCH_TEXT, WRITE_TEXT]);
    let mut session = Session::new([1; 16]);
    let mut clipboard = Clipboard::default();
    let mut full_peer = hello();
    full_peer.capabilities.extend([
        "transfer.chunked.v1".into(),
        "clipboard.write.image.png.v1".into(),
    ]);
    session
        .receive(peer(1, Some(Payload::Hello(full_peer))), &mut clipboard)
        .unwrap();
    assert!(session.is_ready());
    assert_eq!(clipboard.writes, 0);
}

#[test]
fn removed_transfer_payload_cannot_apply_clipboard_content() {
    let mut session = Session::new([1; 16]);
    let mut clipboard = Clipboard::default();
    session
        .receive(peer(1, Some(Payload::Hello(hello()))), &mut clipboard)
        .unwrap();
    let mut bytes = peer(2, None).encode_to_vec();
    bytes.extend_from_slice(&[0xc2, 0xf3, 0x04, 0x00]);
    let request = Envelope::decode(bytes.as_slice()).unwrap();
    assert!(request.payload.is_none());
    assert!(session.receive(request, &mut clipboard).is_err());
    assert_eq!(clipboard.writes, 0);
}

#[test]
fn removed_transfer_manifest_does_not_clear_the_clipboard() {
    let mut session = Session::new([1; 16]);
    let mut clipboard = Clipboard::default();
    session
        .receive(peer(1, Some(Payload::Hello(hello()))), &mut clipboard)
        .unwrap();
    let content = ClipboardContent::decode(&[0x12, 0x00][..]).unwrap();
    let responses = session
        .receive(
            peer(
                2,
                Some(Payload::ClipboardSetRequest(ClipboardSetRequest {
                    content: Some(content),
                })),
            ),
            &mut clipboard,
        )
        .unwrap();
    assert_eq!(clipboard.writes, 0);
    assert!(
        matches!(&responses[0].payload, Some(Payload::ClipboardSetResponse(ClipboardSetResponse {
        result: Some(clipboard_set_response::Result::Error(error))
    })) if error.code == ErrorCode::InvalidRequest as i32)
    );
}
