//! The app's error: a local message, or a Dongle error with the command it answered.
use cordial_protocol as p;
use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct Error {
    /// A complete local message, or the error code's token for a Dongle error.
    pub message: String,
    pub dongle: Option<p::Error>,
    /// The command the Dongle answered, for explanations specific to it.
    pub command: Option<&'static str>,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            dongle: None,
            command: None,
        }
    }

    /// A Dongle error answering `command`.
    pub fn dongle(error: p::Error, command: Option<&'static str>) -> Self {
        Self {
            message: crate::model::code_token(error.code()),
            dongle: Some(error),
            command,
        }
    }

    /// A Dongle error with only a code, as a pairing failure carries it.
    pub fn code(code: p::ErrorCode, command: Option<&'static str>) -> Self {
        Self::dongle(
            p::Error {
                code: code as i32,
                ..Default::default()
            },
            command,
        )
    }

    pub fn from_client(error: cordial_client::Error, command: &'static str) -> Self {
        use cordial_client::Error as E;
        match error {
            E::Dongle(e) => Self::dongle(e, Some(command)),
            E::UnexpectedResponse => Self::new("the adapter returned an unexpected result"),
            E::Timeout => Self::new("the adapter didn't respond in time"),
            E::TooLong => Self::new("the request was too large for the adapter"),
            E::Io(e) => Self::new(format!("the serial connection to the adapter failed: {e}")),
            E::Protocol => Self::new("the adapter sent a message Cordial couldn't read"),
            E::Closed => Self::new("the connection to the adapter closed"),
        }
    }

    /// The Dongle's error code, when the Dongle refused.
    pub fn code_of(&self) -> Option<p::ErrorCode> {
        self.dongle.as_ref().map(p::Error::code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::new(value.to_string())
    }
}
