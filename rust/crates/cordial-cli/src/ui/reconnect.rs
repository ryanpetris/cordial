//! Follows an adapter through the USB reconnect that saving a configuration interface change
//! causes. The adapter leaves USB and returns, possibly on another port because its USB serial
//! number gains or loses a suffix, so it is found again by its adapter ID and reopened without
//! reporting the expected loss. An adapter that doesn't return in time is reported as lost.
use crate::{
    controller::{Command, Outcome, SessionId, Ticket},
    error::Error,
    profiles,
};
use cordial_client::serial::PortInfo;
use cordial_protocol as p;
use std::time::{Duration, Instant};

/// How long the adapter may take to return, matching the desktop app.
const WINDOW: Duration = Duration::from_secs(15);
/// How often the ports are listed while waiting.
const POLL: Duration = Duration::from_millis(500);

/// Whether running `command` may reconnect USB.
pub(crate) fn may_reconnect(command: &Command) -> bool {
    match command {
        Command::Interface { .. } => true,
        Command::AdapterSave(update) => !update.interfaces.is_empty(),
        _ => false,
    }
}

pub(crate) struct Reconnect {
    /// The adapter ID that begins its USB serial number.
    id: String,
    /// The session the save ran on.
    session: SessionId,
    /// The save.
    ticket: Ticket,
    /// The configuration interfaces before the save.
    before: Vec<p::ConfigurationInterfaceSupport>,
    stage: Stage,
}

enum Stage {
    /// The save is running.
    Saving,
    /// The save reconnects USB, so the connection is expected to drop before this time.
    Saved(Instant),
    /// The connection dropped, and the adapter may return until `until`.
    Lost {
        error: Error,
        until: Instant,
        /// When the ports may be listed again.
        next: Instant,
        listing: bool,
    },
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
    /// Follows a save that may reconnect USB, sent to the adapter with `status`.
    pub(crate) fn new(session: SessionId, ticket: Ticket, status: &p::Status) -> Self {
        Self {
            id: status.id.clone(),
            session,
            ticket,
            before: status.configuration_interfaces.clone(),
            stage: Stage::Saving,
        }
    }

    /// Applies a command's result. False once no reconnect is expected: the save changed nothing
    /// that reconnects USB, or failed while the adapter stayed connected.
    pub(crate) fn done(
        &mut self,
        ticket: Ticket,
        result: &Result<Outcome, Error>,
        available: bool,
    ) -> bool {
        if ticket != self.ticket || !matches!(self.stage, Stage::Saving) {
            return true;
        }
        match result {
            Ok(Outcome::AdapterSaved(after) | Outcome::Interface(_, after))
                if profiles::reconnected(&self.before, &after.configuration_interfaces) =>
            {
                self.stage = Stage::Saved(Instant::now() + WINDOW);
                true
            }
            Ok(_) => false,
            // A save cut short by the connection dropping may have been stored.
            Err(_) => !available,
        }
    }

    /// Whether losing `session` is the expected reconnect, which starts the wait for the adapter.
    pub(crate) fn lost(&mut self, session: SessionId, error: &Error) -> bool {
        let now = Instant::now();
        let expected = session == self.session
            && match self.stage {
                Stage::Saving => true,
                Stage::Saved(until) => now < until,
                Stage::Lost { .. } => false,
            };
        if expected {
            self.stage = Stage::Lost {
                error: error.clone(),
                until: now + WINDOW,
                next: now,
                listing: false,
            };
        }
        expected
    }

    /// Whether the adapter is being waited for.
    pub(crate) fn waiting(&self) -> bool {
        matches!(self.stage, Stage::Lost { .. })
    }

    /// Whether a save's reconnect was expected but the connection never dropped.
    pub(crate) fn lapsed(&self) -> bool {
        matches!(self.stage, Stage::Saved(until) if Instant::now() >= until)
    }

    pub(crate) fn step(&mut self) -> Step {
        let Stage::Lost {
            error,
            until,
            next,
            listing,
        } = &mut self.stage
        else {
            return Step::Wait;
        };
        let now = Instant::now();
        if now >= *until {
            Step::GiveUp(error.clone())
        } else if *listing || now < *next {
            Step::Wait
        } else {
            *listing = true;
            *next = now + POLL;
            Step::List
        }
    }

    /// The adapter's port among `ports`, the result of a listing.
    pub(crate) fn listed(&mut self, ports: &[PortInfo]) -> Option<String> {
        if let Stage::Lost { listing, .. } = &mut self.stage {
            *listing = false;
        }
        ports
            .iter()
            .find(|p| p.is_adapter(&self.id))
            .map(|p| p.port.clone())
    }

    /// Reopening failed, as when the port is listed before it can be opened: keeps looking.
    pub(crate) fn retry(&mut self) {
        if let Stage::Lost { next, listing, .. } = &mut self.stage {
            *listing = false;
            *next = Instant::now() + POLL;
        }
    }

    /// Ends the current wait now, as if its time had run out.
    #[cfg(test)]
    pub(crate) fn expire(&mut self) {
        let past = Instant::now() - Duration::from_millis(1);
        match &mut self.stage {
            Stage::Saving => {}
            Stage::Saved(until) => *until = past,
            Stage::Lost { until, next, .. } => {
                *until = past;
                *next = past;
            }
        }
    }

    /// Lets the ports be listed again now.
    #[cfg(test)]
    pub(crate) fn hurry(&mut self) {
        if let Stage::Lost { next, .. } = &mut self.stage {
            *next = Instant::now() - Duration::from_millis(1);
        }
    }
}
