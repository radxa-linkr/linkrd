use linkrd::{
    framing::{FrameError, FrameReader, encode_frame, write_frame},
    protocol::*,
};
use prost::Message;
use std::{
    collections::VecDeque,
    io::{self, Cursor, Read, Write},
    time::Duration,
};

fn message() -> Envelope {
    Envelope {
        protocol_major: 1,
        session_id: vec![1; 16],
        message_id: 1,
        reply_to: None,
        payload: Some(envelope::Payload::Ping(Ping {})),
    }
}

#[test]
fn interoperable_ping_golden_vector() {
    let hex = "41434d500000001a08011210010101010101010101010101010101011801b2b60500a1352338";
    let golden: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(encode_frame(&message(), MAX_FRAME_BYTES).unwrap(), golden);
    let mut reader = FrameReader::new(Cursor::new(golden), MAX_FRAME_BYTES, Duration::from_secs(1));
    assert_eq!(reader.poll().unwrap(), Some(message()));
}

struct Fragmented(VecDeque<io::Result<Vec<u8>>>);
impl Read for Fragmented {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut next = self
            .0
            .pop_front()
            .unwrap_or_else(|| Err(io::ErrorKind::TimedOut.into()))?;
        let count = output.len().min(next.len());
        output[..count].copy_from_slice(&next[..count]);
        next.drain(..count);
        if !next.is_empty() {
            self.0.push_front(Ok(next));
        }
        Ok(count)
    }
}

#[test]
fn every_split_boundary_survives_timeouts_and_following_frame() {
    let frame = encode_frame(&message(), MAX_FRAME_BYTES).unwrap();
    for split in 1..frame.len() {
        let source = Fragmented(VecDeque::from([
            Ok(frame[..split].to_vec()),
            Err(io::ErrorKind::TimedOut.into()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(frame[split..].to_vec()),
            Ok(frame.clone()),
        ]));
        let mut reader = FrameReader::new(source, MAX_FRAME_BYTES, Duration::from_secs(1));
        let mut received = vec![];
        for _ in 0..16 {
            if let Some(message) = reader.poll().unwrap() {
                received.push(message);
            }
            if received.len() == 2 {
                break;
            }
        }
        assert_eq!(received, vec![message(), message()], "split at {split}");
    }
}

#[test]
fn corrupt_frames_and_untrusted_lengths_are_rejected() {
    let mut frame = encode_frame(&message(), MAX_FRAME_BYTES).unwrap();
    frame[10] ^= 1;
    let mut reader = FrameReader::new(Cursor::new(frame), MAX_FRAME_BYTES, Duration::from_secs(1));
    assert!(matches!(reader.poll(), Err(FrameError::Checksum)));
    for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
        let mut header = b"ACMP".to_vec();
        header.extend_from_slice(&length.to_be_bytes());
        let mut reader =
            FrameReader::new(Cursor::new(header), MAX_FRAME_BYTES, Duration::from_secs(1));
        assert!(matches!(reader.poll(), Err(FrameError::Length(_))));
    }
    let mut reader = FrameReader::new(
        Cursor::new(b"BAD!\0\0\0\x01"),
        MAX_FRAME_BYTES,
        Duration::from_secs(1),
    );
    assert!(matches!(reader.poll(), Err(FrameError::Magic)));
}

#[test]
fn incomplete_frame_deadline_and_eof_are_distinct_from_idle() {
    let mut reader = FrameReader::new(
        Fragmented(VecDeque::from([Ok(b"A".to_vec())])),
        MAX_FRAME_BYTES,
        Duration::ZERO,
    );
    assert_eq!(reader.poll().unwrap(), None);
    assert!(matches!(reader.poll(), Err(FrameError::Deadline)));
    let mut reader = FrameReader::new(Cursor::new(b"A"), MAX_FRAME_BYTES, Duration::from_secs(1));
    assert_eq!(reader.poll().unwrap(), None);
    assert!(
        matches!(reader.poll(), Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof)
    );
}

#[test]
fn invalid_protobuf_and_outgoing_limits_are_rejected() {
    let mut frame = b"ACMP\0\0\0\x01\xff".to_vec();
    frame.extend_from_slice(&crc32c::crc32c(&frame).to_be_bytes());
    let mut reader = FrameReader::new(Cursor::new(frame), MAX_FRAME_BYTES, Duration::from_secs(1));
    assert!(matches!(reader.poll(), Err(FrameError::Decode(_))));
    assert!(matches!(
        encode_frame(&message(), message().encoded_len() - 1),
        Err(FrameError::Length(_))
    ));
}

#[test]
fn partial_writes_emit_one_complete_frame() {
    #[derive(Default)]
    struct ShortWriter(Vec<u8>);
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = bytes.len().min(3);
            self.0.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = ShortWriter::default();
    write_frame(&mut writer, &message(), MAX_FRAME_BYTES).unwrap();
    assert_eq!(writer.0, encode_frame(&message(), MAX_FRAME_BYTES).unwrap());
}
