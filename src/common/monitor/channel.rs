use crate::common::fid_parser::DecodedEvent;

/// One reader's decoded batch, as it crosses to the processor task.
///
/// The reader resolves and decodes — see [`DecodedEvent`] — so what travels here
/// is a record per event with a path already on it, not a kernel event that every
/// stage downstream would have to re-interpret.
pub(crate) type DecodedBatch = Vec<DecodedEvent>;

/// Bounded or unbounded sender for the event channel.
pub(crate) enum EventSender {
    Unbounded(tokio::sync::mpsc::UnboundedSender<DecodedBatch>),
    Bounded(tokio::sync::mpsc::Sender<DecodedBatch>),
}

impl std::fmt::Debug for EventSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventSender::Unbounded(_) => write!(f, "EventSender::Unbounded(...)"),
            EventSender::Bounded(_) => write!(f, "EventSender::Bounded(...)"),
        }
    }
}

impl Clone for EventSender {
    fn clone(&self) -> Self {
        match self {
            EventSender::Unbounded(tx) => EventSender::Unbounded(tx.clone()),
            EventSender::Bounded(tx) => EventSender::Bounded(tx.clone()),
        }
    }
}

/// Bounded or unbounded receiver for the event channel.
pub(crate) enum EventReceiver {
    Unbounded(tokio::sync::mpsc::UnboundedReceiver<DecodedBatch>),
    Bounded(tokio::sync::mpsc::Receiver<DecodedBatch>),
}

impl std::fmt::Debug for EventReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventReceiver::Unbounded(_) => write!(f, "EventReceiver::Unbounded(...)"),
            EventReceiver::Bounded(_) => write!(f, "EventReceiver::Bounded(...)"),
        }
    }
}

impl EventReceiver {
    pub(crate) async fn recv(&mut self) -> Option<DecodedBatch> {
        match self {
            EventReceiver::Unbounded(rx) => rx.recv().await,
            EventReceiver::Bounded(rx) => rx.recv().await,
        }
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<DecodedBatch, tokio::sync::mpsc::error::TryRecvError> {
        match self {
            EventReceiver::Unbounded(rx) => rx.try_recv(),
            EventReceiver::Bounded(rx) => rx.try_recv(),
        }
    }
}
