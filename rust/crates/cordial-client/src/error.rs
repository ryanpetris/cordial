use cordial_protocol as p;
use std::{fmt, io, sync::Arc};

/// Why a request did not return a result.
#[derive(Clone, Debug)]
pub enum Error {
    /// The connection closed before this request could be written.
    NotSent(Arc<Error>),
    /// No ordinary request was sent because negotiation failed.
    Handshake(Arc<Error>),
    /// A valid hello identifies a revision without an implemented compatibility path.
    Incompatible(crate::compatibility::NativeHello),
    /// The Dongle answered with an error.
    Dongle(p::Error),
    /// The response carries a different result from the one the command returns.
    UnexpectedResponse,
    /// No response arrived in time. The connection is closed, and later requests fail with this
    /// error.
    Timeout,
    /// The request is longer than [`p::MAX_REQUEST_BYTES`] once encoded. Nothing was sent.
    TooLong,
    /// Reading from or writing to the stream failed. The connection is closed.
    Io(Arc<io::Error>),
    /// The Dongle sent a frame that exceeds the client's receive limit or is not a valid message,
    /// or a response arrived with no request waiting for it. The connection is closed.
    Protocol,
    /// The connection is closed: the stream ended or [`Connection::close`] was called.
    ///
    /// [`Connection::close`]: crate::Connection::close
    Closed,
}

impl Error {
    /// The Dongle's error code, when the Dongle answered with an error.
    pub fn code(&self) -> Option<p::ErrorCode> {
        match self {
            Self::Dongle(error) => Some(error.code()),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSent(error) => write!(f, "the adapter request wasn't sent: {error}"),
            Self::Handshake(error) => write!(f, "the adapter handshake failed: {error}"),
            Self::Incompatible(hello) => f.write_str(match hello.compatibility() {
                crate::compatibility::Compatibility::Newer => "this adapter requires a newer version of Cordial. Update Cordial to connect",
                _ => "Cordial doesn't support this adapter's protocol. Update the adapter firmware to connect",
            }),
            Self::Dongle(error) => write!(f, "dongle error {}", error.code().as_str_name()),
            Self::UnexpectedResponse => f.write_str("unexpected response"),
            Self::Timeout => f.write_str("request timed out"),
            Self::TooLong => f.write_str("request too long"),
            Self::Io(error) => write!(f, "serial I/O failed: {error}"),
            Self::Protocol => f.write_str("invalid frame from dongle"),
            Self::Closed => f.write_str("connection closed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error.as_ref()),
            Self::Handshake(error) | Self::NotSent(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(Arc::new(error))
    }
}
