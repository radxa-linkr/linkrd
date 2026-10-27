include!("protocol/linkrd.protocol.v1.rs");

pub const PROTOCOL_MAJOR: u32 = 1;
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";
pub const WATCH_TEXT: &str = "clipboard.watch.text.v1";
pub const WRITE_TEXT: &str = "clipboard.write.text.v1";
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
pub const HELLO_MAX_BYTES: usize = 4096;
pub const MIN_FRAME_BYTES: usize = 1024;
pub const CONTENT_OVERHEAD: usize = 512;

pub fn hello() -> Hello {
    Hello {
        capabilities: [WATCH_TEXT, WRITE_TEXT].map(str::to_owned).to_vec(),
        max_frame_bytes: MAX_FRAME_BYTES as u32,
        max_clipboard_bytes: MAX_CLIPBOARD_BYTES as u32,
    }
}

pub fn text_content(text: String) -> ClipboardContent {
    ClipboardContent {
        representations: vec![Representation {
            mime_type: TEXT_MIME.into(),
            data: text.into_bytes(),
        }],
    }
}

pub(crate) fn error(code: ErrorCode, detail: &'static str) -> ProtocolError {
    ProtocolError {
        code: code as i32,
        detail: detail.into(),
    }
}
