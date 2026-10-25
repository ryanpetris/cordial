//! Follows an adapter through a USB reconnect. The adapter reconnects USB whenever the interfaces
//! it exposes change, after a save or on its own as devices connect and disconnect, so every loss
//! of the open adapter is followed. The adapter leaves USB and returns, possibly on another port
//! because its USB serial number gains or loses a suffix, so it is found again by its adapter ID
//! and reopened without reporting the loss. An adapter that doesn't return in time is reported as
//! lost.
use crate::error::Error;
use cordial_client::serial::PortInfo;
use std::time::{Duration, Instant};

/// How long the adapter may take to return, matching the desktop app.
const WINDOW: Duration = Duration::from_secs(15);
/// How often the ports are listed while waiting.
const POLL: Duration = Duration::from_millis(500);

pub(crate) struct Reconnect {
    /// The adapter ID that begins its USB serial number.
    pub(crate) id: String,
    /// Why the connection dropped, reported if the adapter doesn't return.
    error: Error,
    until: Instant,
    /// When the ports may be listed again.
    next: Instant,
    listing: bool,
}

/// What waiting for the adapter does next.
pub(crate) enum Step {
    Wait,
    /// List the ports, and pass the result to [`Reconnect::listed`].
    List,
    /// The adapter didn't return: report its loss.
    GiveUp(Error),
}

impl Reconnect {
    /// Waits for the adapter with ID `id`, whose connection dropped with `error`. None when the ID
    /// is unknown, since the adapter couldn't be recognized.
    pub(crate) fn new(id: &str, error: &Error) -> Option<Self> {
        let now = Instant::now();
        (!id.is_empty()).then(|| Self {
            id: id.to_owned(),
            error: error.clone(),
            until: now + WINDOW,
            next: now,
            listing: false,
        })
    }

    pub(crate) fn step(&mut self) -> Step {
        let now = Instant::now();
        if now >= self.until {
            Step::GiveUp(self.error.clone())
        } else if self.listing || now < self.next {
            Step::Wait
        } else {
            self.listing = true;
            self.next = now + POLL;
            Step::List
        }
    }

    /// The adapter's port among `ports`, the result of a listing.
    pub(crate) fn listed(&mut self, ports: &[PortInfo]) -> Option<String> {
        self.listing = false;
        ports
            .iter()
            .find(|p| p.is_adapter(&self.id))
            .map(|p| p.port.clone())
    }

    /// Reopening failed, as when the port is listed before it can be opened: keeps looking.
    pub(crate) fn retry(&mut self) {
        self.listing = false;
        self.next = Instant::now() + POLL;
    }

    /// Ends the wait now, as if its time had run out.
    #[cfg(test)]
    pub(crate) fn expire(&mut self) {
        let past = Instant::now() - Duration::from_millis(1);
        self.until = past;
        self.next = past;
    }

    /// Lets the ports be listed again now.
    #[cfg(test)]
    pub(crate) fn hurry(&mut self) {
        self.next = Instant::now() - Duration::from_millis(1);
    }
}
