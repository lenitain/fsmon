//! Proc connector integration for proc-tree.
//!
//! Only two things live here:
//! 1. Constants (cache sizing)
//! 2. Raw proc-connector byte parsing → proc_tree::ProcEvent conversion

use proc_connector::{NetlinkMessageIter, ProcConnector, ProcEvent as PcEvent};

pub use proc_tree::{
    DefaultStore, ProcEvent, ProcessInfo, ProcessLink, ProcessStore, read_proc_start_time_ns,
};

/// Time-to-live for process store entries (in seconds).
pub const PROC_STORE_TTL_SECS: u64 = 600;

/// Try to create a proc connector for receiving process events.
///
/// Returns `None` if the connector cannot be created or set to non-blocking mode.
pub fn try_create_connector() -> Option<ProcConnector> {
    let conn = match ProcConnector::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[WARNING] Failed to create proc connector: {e}. \
                       Process tree tracking will be unavailable."
            );
            return None;
        }
    };
    if let Err(e) = conn.set_nonblocking() {
        eprintln!("[WARNING] Failed to set proc connector non-blocking: {e}");
        return None;
    }
    Some(conn)
}

/// Parse raw proc-connector bytes and delegate to proc_tree::handle_events.
///
/// Returns RAII guards for exited processes. Each guard automatically removes
/// Returns exited PIDs. Process info stays in store — caller decides when to remove.
pub fn handle_proc_events(store: &DefaultStore, data: &[u8], n: usize) -> Vec<u32> {
    let mut events: Vec<ProcEvent> = Vec::new();
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(PcEvent::Exec {
                pid, timestamp_ns, ..
            })) => {
                events.push(ProcEvent::Exec { pid, timestamp_ns });
            }
            Ok(Some(PcEvent::Fork {
                child_pid,
                parent_pid,
                timestamp_ns,
                ..
            })) => {
                events.push(ProcEvent::Fork {
                    child_pid,
                    parent_pid,
                    timestamp_ns,
                });
            }
            Ok(Some(PcEvent::Exit { pid, .. })) => {
                events.push(ProcEvent::Exit { pid });
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(proc_connector::Error::Overrun) => {
                eprintln!("[WARNING] proc connector overrun — some events may have been lost");
            }
            Err(proc_connector::Error::Truncated) => {
                eprintln!("[WARNING] proc connector truncated message, continuing...");
            }
            Err(e) => {
                eprintln!("proc connector parse error: {e}");
            }
        }
    }
    if !events.is_empty() {
        proc_tree::handle_events(store, &events)
    } else {
        Vec::new()
    }
}
