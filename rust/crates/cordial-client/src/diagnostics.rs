//! Callbacks for original protocol responses and events.
use crate::protocol as p;
use std::sync::Arc;

/// Original protocol messages received from the adapter.
pub enum Diagnostic<'a> {
    /// The original response and the request it answers.
    Response(&'a p::Request, &'a p::Response),
    /// The original adapter event.
    Event(&'a p::Event),
}

/// A diagnostic callback on the serial reader. It must return without waiting for output.
pub type Sink = Arc<dyn for<'a> Fn(Diagnostic<'a>) + Send + Sync>;
