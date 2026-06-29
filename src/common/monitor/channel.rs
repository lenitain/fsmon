use fanotify_fid::types::FidEvent;

/// Bounded or unbounded sender for the event channel.
pub(crate) enum EventSender {
    Unbounded(tokio::sync::mpsc::UnboundedSender<Vec<FidEvent>>),
    Bounded(tokio::sync::mpsc::Sender<Vec<FidEvent>>),
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
    Unbounded(tokio::sync::mpsc::UnboundedReceiver<Vec<FidEvent>>),
    Bounded(tokio::sync::mpsc::Receiver<Vec<FidEvent>>),
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
    pub(crate) async fn recv(&mut self) -> Option<Vec<FidEvent>> {
        match self {
            EventReceiver::Unbounded(rx) => rx.recv().await,
            EventReceiver::Bounded(rx) => rx.recv().await,
        }
    }

    pub(crate) fn try_recv(
        &mut self,
    ) -> Result<Vec<FidEvent>, tokio::sync::mpsc::error::TryRecvError> {
        match self {
            EventReceiver::Unbounded(rx) => rx.try_recv(),
            EventReceiver::Bounded(rx) => rx.try_recv(),
        }
    }
}
