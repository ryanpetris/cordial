//! Bluetooth link observations and pairing prompts shared by the backends.
use serde::{Deserialize, Serialize};

/// Observed properties of the current Bluetooth link, never requested policy.
/// None means the backend cannot report that property. Key size is in bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectionSecurity {
    pub encrypted: Option<bool>,
    pub authenticated: Option<bool>,
    pub secure_connections: Option<bool>,
    pub key_size: Option<u8>,
    /// The backend set up the link with a saved bond. Backends refuse links without one.
    pub bonded: Option<bool>,
}

/// Discovery hint from advertised BLE Appearance or Classic Class of Device.
/// Unknown includes devices whose advertised metadata does not identify an input type.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Unknown,
    Keyboard,
    Mouse,
    KeyboardMouse,
}

/// How pairing authenticates: the user enters, confirms or reads a code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptMethod {
    ConfirmPasskey,
    EnterPasskey,
    EnterPin,
    DisplayPasskey,
    DisplayPin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairAction {
    Accept,
    Reject,
}

impl PromptMethod {
    /// The code is shown for the user to type on the device; no reply is needed.
    pub fn display(self) -> bool {
        matches!(self, Self::DisplayPasskey | Self::DisplayPin)
    }
    /// Whether a reply is well formed: entry methods accept with a passkey of six digits or a PIN
    /// of 1 to 16 printable ASCII characters; confirmation and rejection carry no value.
    pub fn valid_reply(self, action: PairAction, value: Option<&str>) -> bool {
        if self.display() {
            return false;
        }
        match (action, self, value) {
            (PairAction::Accept, Self::EnterPasskey, Some(v)) => {
                v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit())
            }
            (PairAction::Accept, Self::EnterPin, Some(v)) => {
                !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| (0x20..=0x7e).contains(&b))
            }
            (PairAction::Accept, Self::EnterPasskey | Self::EnterPin, _) => false,
            (_, _, None) => true,
            _ => false,
        }
    }
}
