use prost::Message;
use std::{
    io::{self, Read, Write},
    time::{Duration, Instant},
};
use thiserror::Error;

use crate::protocol::{Envelope, MAX_FRAME_BYTES};

pub const MAGIC: [u8; 4] = *b"ACMP";
const HEADER_LEN: usize = 8;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("transport I/O: {0}")]
    Io(#[from] io::Error),
    #[error("invalid frame magic")]
    Magic,
    #[error("invalid payload length {0}")]
    Length(usize),
    #[error("frame CRC-32C mismatch")]
    Checksum,
    #[error("incomplete frame exceeded its deadline")]
    Deadline,
    #[error("invalid protobuf payload: {0}")]
    Decode(#[from] prost::DecodeError),
}

pub fn encode_frame(message: &Envelope, max_bytes: usize) -> Result<Vec<u8>, FrameError> {
    let length = message.encoded_len();
    if length == 0 || length > max_bytes.min(MAX_FRAME_BYTES) {
        return Err(FrameError::Length(length));
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + length + 4);
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&(length as u32).to_be_bytes());
    frame.extend_from_slice(&message.encode_to_vec());
    frame.extend_from_slice(&crc32c::crc32c(&frame).to_be_bytes());
    Ok(frame)
}

pub fn write_frame(
    writer: &mut impl Write,
    message: &Envelope,
    max_bytes: usize,
) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(message, max_bytes)?)?;
    Ok(())
}

pub struct FrameReader<R> {
    reader: R,
    header: [u8; HEADER_LEN],
    header_read: usize,
    body: Vec<u8>,
    body_read: usize,
    max_bytes: usize,
    deadline: Duration,
    started: Option<Instant>,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R, max_bytes: usize, deadline: Duration) -> Self {
        Self {
            reader,
            header: [0; HEADER_LEN],
            header_read: 0,
            body: Vec::new(),
            body_read: 0,
            max_bytes: max_bytes.min(MAX_FRAME_BYTES),
            deadline,
            started: None,
        }
    }

    pub fn set_max_bytes(&mut self, limit: usize) {
        self.max_bytes = limit.min(MAX_FRAME_BYTES);
    }

    pub fn poll(&mut self) -> Result<Option<Envelope>, FrameError> {
        if self
            .started
            .is_some_and(|start| start.elapsed() >= self.deadline)
        {
            return Err(FrameError::Deadline);
        }
        if self.header_read < HEADER_LEN {
            let count = read_available(&mut self.reader, &mut self.header[self.header_read..])?;
            if count == 0 {
                return Ok(None);
            }
            self.started.get_or_insert_with(Instant::now);
            self.header_read += count;
            if self.header_read < HEADER_LEN {
                return Ok(None);
            }
            if self.header[..4] != MAGIC {
                return Err(FrameError::Magic);
            }
            let length = u32::from_be_bytes([
                self.header[4],
                self.header[5],
                self.header[6],
                self.header[7],
            ]) as usize;
            if length == 0 || length > self.max_bytes {
                return Err(FrameError::Length(length));
            }
            self.body.resize(length + 4, 0);
        }
        let count = read_available(&mut self.reader, &mut self.body[self.body_read..])?;
        self.body_read += count;
        if self
            .started
            .is_some_and(|start| start.elapsed() >= self.deadline)
        {
            return Err(FrameError::Deadline);
        }
        if self.body_read < self.body.len() {
            return Ok(None);
        }
        let payload_len = self.body.len() - 4;
        let crc = &self.body[payload_len..];
        let expected = u32::from_be_bytes([crc[0], crc[1], crc[2], crc[3]]);
        let actual = crc32c::crc32c_append(crc32c::crc32c(&self.header), &self.body[..payload_len]);
        if actual != expected {
            return Err(FrameError::Checksum);
        }
        let message = Envelope::decode(&self.body[..payload_len])?;
        self.header_read = 0;
        self.body.clear();
        self.body_read = 0;
        self.started = None;
        Ok(Some(message))
    }
}

fn read_available(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    match reader.read(buffer) {
        Ok(0) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "ACM stream closed",
        )),
        Ok(count) => Ok(count),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(0)
        }
        Err(error) => Err(error),
    }
}
