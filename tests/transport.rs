use envelope::Payload;
use linkrd::{
    clipboard::ClipboardBackend,
    framing::{FrameReader, encode_frame, write_frame},
    protocol::*,
    transport::{ConnectionOptions, run_connection},
};
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct ClipboardData {
    text: Option<String>,
    writes: usize,
}
#[derive(Clone, Default)]
struct MemoryClipboard(Arc<Mutex<ClipboardData>>);
impl ClipboardBackend for MemoryClipboard {
    fn read_text(&mut self) -> Result<Option<String>, ProtocolError> {
        Ok(self.0.lock().unwrap().text.clone())
    }
    fn write_text(&mut self, text: &str) -> Result<(), ProtocolError> {
        let mut state = self.0.lock().unwrap();
        state.text = Some(text.into());
        state.writes += 1;
        Ok(())
    }
}
struct StopOnDrop<'a>(&'a AtomicBool);
impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let a = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (b, _) = listener.accept().unwrap();
    for stream in [&a, &b] {
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
    }
    (a, b)
}
fn peer(id: u64, payload: Payload) -> Envelope {
    Envelope {
        protocol_major: 1,
        session_id: vec![9; 16],
        message_id: id,
        reply_to: None,
        payload: Some(payload),
    }
}
fn next(reader: &mut FrameReader<TcpStream>) -> Envelope {
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if let Some(message) = reader.poll().unwrap() {
            return message;
        }
    }
    panic!("timed out waiting for peer message");
}
fn connect_peer(writer: &mut TcpStream, reader: &mut FrameReader<TcpStream>) {
    assert!(matches!(next(reader).payload, Some(Payload::Hello(_))));
    write_frame(writer, &peer(1, Payload::Hello(hello())), HELLO_MAX_BYTES).unwrap();
    assert!(matches!(next(reader).payload, Some(Payload::Hello(_))));
    assert!(matches!(
        next(reader).payload,
        Some(Payload::ClipboardChanged(_))
    ));
}

#[test]
fn discarded_early_hello_is_repeated_before_initial_clipboard() {
    let (device, mut peer_io) = pair();
    let mut reader = FrameReader::new(
        peer_io.try_clone().unwrap(),
        MAX_FRAME_BYTES,
        Duration::from_secs(1),
    );
    let mut clipboard = MemoryClipboard::default();
    let dirty = AtomicBool::new(false);
    let shutdown = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_connection(
                device.try_clone().unwrap(),
                device,
                &mut clipboard,
                &dirty,
                &shutdown,
                ConnectionOptions::default(),
            )
        });
        let _stop = StopOnDrop(&shutdown);
        let discarded = next(&mut reader);
        assert!(matches!(discarded.payload, Some(Payload::Hello(_))));
        write_frame(
            &mut peer_io,
            &peer(1, Payload::Hello(hello())),
            HELLO_MAX_BYTES,
        )
        .unwrap();
        let greeting = next(&mut reader);
        assert_eq!(greeting, discarded);
        let snapshot = next(&mut reader);
        assert!(matches!(
            snapshot.payload,
            Some(Payload::ClipboardChanged(ClipboardChanged {
                revision: 1,
                ..
            }))
        ));
        assert_eq!(snapshot.session_id, greeting.session_id);
        write_frame(
            &mut peer_io,
            &peer(1, Payload::Hello(hello())),
            HELLO_MAX_BYTES,
        )
        .unwrap();
        write_frame(
            &mut peer_io,
            &peer(2, Payload::Ping(Ping {})),
            MAX_FRAME_BYTES,
        )
        .unwrap();
        assert!(matches!(next(&mut reader).payload, Some(Payload::Pong(_))));
        shutdown.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
    });
}

#[test]
fn byte_stream_handshake_auto_upload_manual_write_and_no_echo() {
    let (device, mut peer_io) = pair();
    let device_read = device.try_clone().unwrap();
    let mut peer_reader = FrameReader::new(
        peer_io.try_clone().unwrap(),
        MAX_FRAME_BYTES,
        Duration::from_secs(1),
    );
    let mut clipboard = MemoryClipboard::default();
    let clipboard_handle = clipboard.clone();
    let dirty = AtomicBool::new(false);
    let shutdown = AtomicBool::new(false);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_connection(
                device_read,
                device,
                &mut clipboard,
                &dirty,
                &shutdown,
                ConnectionOptions::default(),
            )
        });
        let _stop = StopOnDrop(&shutdown);
        connect_peer(&mut peer_io, &mut peer_reader);
        clipboard_handle.0.lock().unwrap().text = Some("被控机复制 📋".into());
        dirty.store(true, Ordering::Release);
        let automatic = next(&mut peer_reader);
        assert!(
            matches!(automatic.payload, Some(Payload::ClipboardChanged(ClipboardChanged { revision: 2, state: Some(clipboard_changed::State::Content(ref c)), .. })) if c == &text_content("被控机复制 📋".into()))
        );
        let request = peer(
            2,
            Payload::ClipboardSetRequest(ClipboardSetRequest {
                content: Some(text_content("browser text".into())),
            }),
        );
        let frame = encode_frame(&request, MAX_FRAME_BYTES).unwrap();
        for part in frame.chunks(3) {
            peer_io.write_all(part).unwrap();
        }
        assert!(matches!(
            next(&mut peer_reader).payload,
            Some(Payload::ClipboardChanged(_))
        ));
        let response = next(&mut peer_reader);
        assert_eq!(
            response.reply_to,
            Some(MessageRef {
                session_id: vec![9; 16],
                message_id: 2
            })
        );
        assert!(matches!(
            response.payload,
            Some(Payload::ClipboardSetResponse(ClipboardSetResponse {
                result: Some(clipboard_set_response::Result::Applied(_))
            }))
        ));
        assert_eq!(
            clipboard_handle.0.lock().unwrap().text.as_deref(),
            Some("browser text")
        );
        dirty.store(true, Ordering::Release);
        let until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < until {
            assert!(peer_reader.poll().unwrap().is_none());
        }
        write_frame(&mut peer_io, &request, MAX_FRAME_BYTES).unwrap();
        assert!(matches!(
            next(&mut peer_reader).payload,
            Some(Payload::ClipboardSetResponse(_))
        ));
        assert_eq!(clipboard_handle.0.lock().unwrap().writes, 1);
        shutdown.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
    });
}

#[test]
fn missed_watcher_signal_is_repaired_by_periodic_refresh() {
    let (device, mut peer_io) = pair();
    let device_read = device.try_clone().unwrap();
    let mut peer_reader = FrameReader::new(
        peer_io.try_clone().unwrap(),
        MAX_FRAME_BYTES,
        Duration::from_secs(1),
    );
    let mut clipboard = MemoryClipboard::default();
    let handle = clipboard.clone();
    let dirty = AtomicBool::new(false);
    let shutdown = AtomicBool::new(false);
    let options = ConnectionOptions {
        refresh_interval: Duration::from_millis(40),
        ..Default::default()
    };
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_connection(
                device_read,
                device,
                &mut clipboard,
                &dirty,
                &shutdown,
                options,
            )
        });
        let _stop = StopOnDrop(&shutdown);
        connect_peer(&mut peer_io, &mut peer_reader);
        handle.0.lock().unwrap().text = Some("changed without signal".into());
        assert!(matches!(
            next(&mut peer_reader).payload,
            Some(Payload::ClipboardChanged(ClipboardChanged {
                state: Some(clipboard_changed::State::Content(_)),
                ..
            }))
        ));
        shutdown.store(true, Ordering::Release);
        worker.join().unwrap().unwrap();
    });
}

#[test]
fn silent_peer_ends_session_on_heartbeat_deadline() {
    let (device, mut peer_io) = pair();
    let device_read = device.try_clone().unwrap();
    let mut peer_reader = FrameReader::new(
        peer_io.try_clone().unwrap(),
        MAX_FRAME_BYTES,
        Duration::from_secs(1),
    );
    let mut clipboard = MemoryClipboard::default();
    let dirty = AtomicBool::new(false);
    let shutdown = AtomicBool::new(false);
    let options = ConnectionOptions {
        heartbeat_interval: Duration::from_millis(30),
        heartbeat_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            run_connection(
                device_read,
                device,
                &mut clipboard,
                &dirty,
                &shutdown,
                options,
            )
        });
        let _stop = StopOnDrop(&shutdown);
        connect_peer(&mut peer_io, &mut peer_reader);
        assert!(matches!(
            next(&mut peer_reader).payload,
            Some(Payload::Ping(_))
        ));
        let error = worker.join().unwrap().unwrap_err();
        assert!(format!("{error:#}").contains("heartbeat timed out"));
    });
}

#[test]
fn missing_hello_and_corrupt_frames_cannot_write_clipboard() {
    for corrupt in [false, true] {
        let (device, mut peer_io) = pair();
        let mut clipboard = MemoryClipboard::default();
        let handle = clipboard.clone();
        if corrupt {
            let mut frame =
                encode_frame(&peer(1, Payload::Hello(hello())), HELLO_MAX_BYTES).unwrap();
            let last = frame.len() - 1;
            frame[last] ^= 1;
            peer_io.write_all(&frame).unwrap();
        }
        let options = ConnectionOptions {
            handshake_timeout: Duration::from_millis(100),
            ..Default::default()
        };
        let error = run_connection(
            device.try_clone().unwrap(),
            device,
            &mut clipboard,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            options,
        )
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains(if corrupt {
                "CRC"
            } else {
                "handshake timed out"
            }),
            "{text}"
        );
        assert_eq!(handle.0.lock().unwrap().writes, 0);
    }
}
