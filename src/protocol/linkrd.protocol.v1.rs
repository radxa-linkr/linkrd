#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Envelope {
    #[prost(uint32, tag = "1")]
    pub protocol_major: u32,
    #[prost(bytes = "vec", tag = "2")]
    pub session_id: ::prost::alloc::vec::Vec<u8>,
    #[prost(uint64, tag = "3")]
    pub message_id: u64,
    #[prost(message, optional, tag = "4")]
    pub reply_to: ::core::option::Option<MessageRef>,
    #[prost(oneof = "envelope::Payload", tags = "10000, 10010, 10020, 10030, 11110, 11111")]
    pub payload: ::core::option::Option<envelope::Payload>,
}
pub mod envelope {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Payload {
        #[prost(message, tag = "10000")]
        Hello(super::Hello),
        #[prost(message, tag = "10010")]
        ClipboardSetRequest(super::ClipboardSetRequest),
        #[prost(message, tag = "10020")]
        ClipboardSetResponse(super::ClipboardSetResponse),
        #[prost(message, tag = "10030")]
        ClipboardChanged(super::ClipboardChanged),
        #[prost(message, tag = "11110")]
        Ping(super::Ping),
        #[prost(message, tag = "11111")]
        Pong(super::Pong),
    }
}
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct MessageRef {
    #[prost(bytes = "vec", tag = "1")]
    pub session_id: ::prost::alloc::vec::Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub message_id: u64,
}
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct Hello {
    #[prost(string, repeated, tag = "1")]
    pub capabilities: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(uint32, tag = "2")]
    pub max_frame_bytes: u32,
    #[prost(uint32, tag = "3")]
    pub max_clipboard_bytes: u32,
}
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClipboardSetRequest {
    #[prost(message, optional, tag = "1")]
    pub content: ::core::option::Option<ClipboardContent>,
}
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct ClipboardSetResponse {
    #[prost(oneof = "clipboard_set_response::Result", tags = "1, 2")]
    pub result: ::core::option::Option<clipboard_set_response::Result>,
}
pub mod clipboard_set_response {
    #[derive(Clone, PartialEq, Eq, Hash, ::prost::Oneof)]
    pub enum Result {
        #[prost(message, tag = "1")]
        Applied(super::ClipboardApplied),
        #[prost(message, tag = "2")]
        Error(super::ProtocolError),
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, ::prost::Message)]
pub struct ClipboardApplied {
    #[prost(uint64, tag = "1")]
    pub revision: u64,
}
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClipboardChanged {
    #[prost(uint64, tag = "1")]
    pub revision: u64,
    #[prost(message, optional, tag = "2")]
    pub caused_by: ::core::option::Option<MessageRef>,
    #[prost(oneof = "clipboard_changed::State", tags = "3, 4")]
    pub state: ::core::option::Option<clipboard_changed::State>,
}
pub mod clipboard_changed {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum State {
        #[prost(message, tag = "3")]
        Content(super::ClipboardContent),
        #[prost(message, tag = "4")]
        Unavailable(super::ProtocolError),
    }
}
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClipboardContent {
    #[prost(message, repeated, tag = "1")]
    pub representations: ::prost::alloc::vec::Vec<Representation>,
}
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct Representation {
    #[prost(string, tag = "1")]
    pub mime_type: ::prost::alloc::string::String,
    #[prost(bytes = "vec", tag = "2")]
    pub data: ::prost::alloc::vec::Vec<u8>,
}
#[derive(Clone, PartialEq, Eq, Hash, ::prost::Message)]
pub struct ProtocolError {
    #[prost(enumeration = "ErrorCode", tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub detail: ::prost::alloc::string::String,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, ::prost::Message)]
pub struct Ping {
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, ::prost::Message)]
pub struct Pong {
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum ErrorCode {
    Unspecified = 0,
    NoText = 1,
    TooLarge = 2,
    Unsupported = 3,
    Busy = 4,
    AccessDenied = 5,
    ReadFailed = 6,
    WriteFailed = 7,
    InvalidRequest = 8,
}
impl ErrorCode {
    pub fn as_str_name(&self) -> &'static str {
        match self {
            Self::Unspecified => "ERROR_CODE_UNSPECIFIED",
            Self::NoText => "ERROR_CODE_NO_TEXT",
            Self::TooLarge => "ERROR_CODE_TOO_LARGE",
            Self::Unsupported => "ERROR_CODE_UNSUPPORTED",
            Self::Busy => "ERROR_CODE_BUSY",
            Self::AccessDenied => "ERROR_CODE_ACCESS_DENIED",
            Self::ReadFailed => "ERROR_CODE_READ_FAILED",
            Self::WriteFailed => "ERROR_CODE_WRITE_FAILED",
            Self::InvalidRequest => "ERROR_CODE_INVALID_REQUEST",
        }
    }
    pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
        match value {
            "ERROR_CODE_UNSPECIFIED" => Some(Self::Unspecified),
            "ERROR_CODE_NO_TEXT" => Some(Self::NoText),
            "ERROR_CODE_TOO_LARGE" => Some(Self::TooLarge),
            "ERROR_CODE_UNSUPPORTED" => Some(Self::Unsupported),
            "ERROR_CODE_BUSY" => Some(Self::Busy),
            "ERROR_CODE_ACCESS_DENIED" => Some(Self::AccessDenied),
            "ERROR_CODE_READ_FAILED" => Some(Self::ReadFailed),
            "ERROR_CODE_WRITE_FAILED" => Some(Self::WriteFailed),
            "ERROR_CODE_INVALID_REQUEST" => Some(Self::InvalidRequest),
            _ => None,
        }
    }
}
