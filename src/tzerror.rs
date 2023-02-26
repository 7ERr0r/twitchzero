#[derive(Debug, derive_more::Display)]
pub enum TwitchzeroError {
    // #[display(fmt = "file ffplay.exe is not a file")]
    // FileIsNotFFplay,
    #[display(fmt = "TCPConnectError: {}", _0)]
    TCPConnect(std::io::Error),

    #[cfg(target_os = "linux")]
    #[display(fmt = "UnixConnectError: {}", _0)]
    UnixConnect(std::io::Error),

    #[display(fmt = "FileCreateError: {}", _0)]
    FileCreate(std::io::Error),

    #[display(fmt = "NoUnixSocketError: target_os != linux")]
    NoUnixSocket,

    #[display(fmt = "SegmentDataSendError: can't send")]
    SegmentDataSend,

    #[display(fmt = "SegmentSendError")]
    SegmentSend,

    #[display(fmt = "SegmentJoinSendError")]
    SegmentJoinSend,
}
impl std::error::Error for TwitchzeroError {}
