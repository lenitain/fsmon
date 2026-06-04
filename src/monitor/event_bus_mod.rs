use tokio::sync::broadcast;

use crate::FileEvent;
use crate::monitor::channel::EventSender;

/// Manages event distribution: internal channel and broadcast stream.
pub struct EventBus {
    /// Internal event sender for reader tasks
    pub event_tx: Option<EventSender>,
    /// Unified event stream: broadcast channel for all consumers.
    /// Carries (FileEvent, cmd_name) — cmd_name is for file routing.
    pub event_stream_tx: Option<broadcast::Sender<(FileEvent, String)>>,
}

impl EventBus {
    pub fn new(subscribe_buf: Option<usize>) -> Self {
        let event_stream_tx = subscribe_buf.map(|buf| {
            let (tx, _) = broadcast::channel(buf);
            tx
        });
        
        Self {
            event_tx: None,
            event_stream_tx,
        }
    }

    /// Set the internal event sender.
    pub fn set_event_tx(&mut self, tx: EventSender) {
        self.event_tx = Some(tx);
    }

    /// Get a reference to the event sender, if set.
    pub fn event_tx(&self) -> Option<&EventSender> {
        self.event_tx.as_ref()
    }

    /// Get a reference to the broadcast sender, if set.
    pub fn event_stream_tx(&self) -> Option<&broadcast::Sender<(FileEvent, String)>> {
        self.event_stream_tx.as_ref()
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> Option<broadcast::Receiver<(FileEvent, String)>> {
        self.event_stream_tx.as_ref().map(|tx| tx.subscribe())
    }

    /// Send an event to the broadcast stream.
    pub fn broadcast(&self, event: FileEvent, cmd: String) -> Result<(), broadcast::error::SendError<(FileEvent, String)>> {
        if let Some(ref tx) = self.event_stream_tx {
            tx.send((event, cmd))?;
        }
        Ok(())
    }
}
