//! The app's error: a local message, or a Dongle error with the command it answered.
use cordial_protocol as p;
use std::{fmt, io, sync::Arc};

pub type Result<T> = std::result::Result<T, Error>;

const CLOSED: &str = "the connection to the adapter closed";
const TIMEOUT: &str = "the adapter didn't respond in time";
const IO: &str = "the serial connection to the adapter failed: ";
pub(crate) const UNEXPECTED: &str = "the adapter returned an unexpected result";
const PROTOCOL: &str = "the adapter sent a message Cordial couldn't read";

#[derive(Clone, Debug)]
pub struct Error {
    /// A complete local message, or the error code's token for a Dongle error.
    pub message: String,
    pub dongle: Option<p::Error>,
    /// The command the Dongle answered, for explanations specific to it.
    pub command: Option<&'static str>,
    /// Native handshake metadata for a compatibility refusal.
    pub native_hello: Option<Arc<cordial_client::compatibility::NativeHello>>,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            dongle: None,
            command: None,
            native_hello: None,
        }
    }

    /// A Dongle error answering `command`.
    pub fn dongle(error: p::Error, command: Option<&'static str>) -> Self {
        Self {
            message: crate::model::code_token(error.code()),
            dongle: Some(error),
            command,
            native_hello: None,
        }
    }

    /// A refusal Cordial makes before sending, worded as the Dongle's error with `code`: nothing
    /// took effect.
    pub fn code(code: p::ErrorCode, command: Option<&'static str>) -> Self {
        Self::refusal(
            p::Error {
                code: code as i32,
                ..Default::default()
            },
            command,
        )
    }

    /// A refusal Cordial makes before sending, worded as the Dongle's `error`.
    pub fn refusal(error: p::Error, command: Option<&'static str>) -> Self {
        Self::dongle(
            p::Error {
                outcome: p::ErrorOutcome::NoEffect as i32,
                ..error
            },
            command,
        )
    }

    pub fn from_client(error: cordial_client::Error, command: &'static str) -> Self {
        use cordial_client::Error as E;
        match error {
            E::Incompatible(hello) => {
                let message = E::Incompatible(hello.clone()).to_string();
                Self {
                    native_hello: Some(Arc::new(hello)),
                    ..Self::new(message)
                }
            }
            E::Handshake(error) => Self::new(format!(
                "the adapter couldn't complete the connection: {}",
                Self::from_client(Arc::unwrap_or_clone(error), command).message
            )),
            E::NotSent(error) => Self::new(format!(
                "the adapter request wasn't sent: {}",
                Self::from_client(Arc::unwrap_or_clone(error), command).message
            )),
            E::Dongle(e) => Self::dongle(e, Some(command)),
            E::UnexpectedResponse => Self::new(UNEXPECTED),
            E::Timeout => Self::new(TIMEOUT),
            E::TooLong => Self::new("the request was too large for the adapter"),
            E::Io(e) => Self::new(format!("{IO}{e}")),
            E::Protocol => Self::new(PROTOCOL),
            E::Closed => Self::new(CLOSED),
        }
    }

    /// Whether a local failure came after the request may have reached the adapter, so a change
    /// it carried may or may not have been saved: the connection closed or failed, the adapter
    /// didn't answer in time, or its answer couldn't be read.
    pub fn unconfirmed(&self) -> bool {
        self.dongle.is_none()
            && (matches!(
                self.message.as_str(),
                CLOSED | TIMEOUT | UNEXPECTED | PROTOCOL
            ) || self.message.starts_with(IO))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_and_unsent_failures_never_report_an_uncertain_write() {
        for error in [
            cordial_client::Error::Handshake(Arc::new(cordial_client::Error::Timeout)),
            cordial_client::Error::NotSent(Arc::new(cordial_client::Error::Closed)),
        ] {
            assert!(!Error::from_client(error, "save").unconfirmed());
        }
        assert!(Error::from_client(cordial_client::Error::Closed, "save").unconfirmed());
        let native = cordial_client::compatibility::NativeHello::decode(&[8, 2]).unwrap();
        let error = Error::from_client(cordial_client::Error::Incompatible(native.clone()), "save");
        assert_eq!(error.native_hello.as_deref(), Some(&native));
        assert!(!error.unconfirmed());
        assert_eq!(
            crate::ui::text::sentence(&crate::ui::text::error_words(&error)),
            "This adapter requires a newer version of Cordial. Update Cordial to connect."
        );
    }
}
