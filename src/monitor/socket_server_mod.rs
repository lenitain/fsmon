use tokio::net::UnixListener;

/// Manages the Unix socket listener for client connections.
pub struct SocketServer {
    /// Unix socket listener for accepting client connections
    pub listener: Option<UnixListener>,
}

impl SocketServer {
    pub fn new(listener: Option<UnixListener>) -> Self {
        Self { listener }
    }

    /// Check if the socket server has a listener.
    pub fn has_listener(&self) -> bool {
        self.listener.is_some()
    }

    /// Take ownership of the listener (e.g., to move into the event loop).
    pub fn take_listener(&mut self) -> Option<UnixListener> {
        self.listener.take()
    }

    /// Get a reference to the listener.
    pub fn listener(&self) -> Option<&UnixListener> {
        self.listener.as_ref()
    }
}
