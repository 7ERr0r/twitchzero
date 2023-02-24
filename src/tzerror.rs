#[derive(Debug, derive_more::Display)]
pub enum TwitchzeroError {
    // #[display(fmt = "file ffplay.exe is not a file")]
    // FileIsNotFFplay,
    #[display(fmt = "TCPConnectError: {}", _0)]
    TCPConnectError(std::io::Error),

    #[cfg(target_os = "linux")]
    #[display(fmt = "UnixConnectError: {}", _0)]
    UnixConnectError(std::io::Error),

    #[display(fmt = "FileCreateError: {}", _0)]
    FileCreateError(std::io::Error),

    #[display(fmt = "NoUnixSocketError: target_os != linux")]
    NoUnixSocketError,

    #[display(fmt = "SegmentDataSendError: can't send")]
    SegmentDataSendError,

    #[display(fmt = "SegmentSendError")]
    SegmentSendError,

    #[display(fmt = "SegmentJoinSendError")]
    SegmentJoinSendError,
}
impl std::error::Error for TwitchzeroError {}
