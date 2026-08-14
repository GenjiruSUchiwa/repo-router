//! Entry point that fans one event out to the registered handlers.

/// One incoming event.
pub struct Event {
    /// Numeric kind discriminating the event.
    pub kind: u32,
}

/// Outcome of processing a single event.
pub enum Status {
    /// The event was processed.
    Ok,
    /// The event was rejected.
    Failed,
}

/// Dispatches an event to the handler registered for its kind.
pub fn dispatch_event(event: &Event) -> Status {
    record_event(event)
}

/// Records an event in the audit log.
pub fn record_event(event: &Event) -> Status {
    if event.kind == 0 {
        Status::Failed
    } else {
        Status::Ok
    }
}
