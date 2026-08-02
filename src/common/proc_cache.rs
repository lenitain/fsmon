//! Proc connector integration for proc-tree (event-driven migration,
//! RUN-23): cn_proc netlink events feed the core's lifecycle protocol
//! directly — no polling, no recursive /proc scanning. `/proc` is only
//! touched on demand for metadata the event stream cannot carry (cmdline,
//! uid).

use proc_connector::{NetlinkMessageIter, ProcConnector, ProcEvent as PcEvent};
use proc_tree::{
    BootstrapBuilder, ClockDomain, EventMeta, ExitScope, IdentityEvidence, LifecycleEvent,
    ProcessMetadata, ProcessTracker, SourceCapabilities, SourceHealth, SourceId, Tgid, Tid,
    TrackerConfig,
};

/// Capability contract for the cn_proc source (registered once).
pub fn source_capabilities() -> SourceCapabilities {
    SourceCapabilities {
        // No start_ticks on the wire: identity rests on continuity only.
        identity: IdentityEvidence::Weak,
        // PROC_EVENT_FORK carries the full TID/TGID four-tuple.
        thread_granularity: proc_tree::Granularity::Thread,
        order: proc_tree::OrderDomain::PerCpuSequence,
        clock: ClockDomain::BootMonotonic,
        metadata: false,
    }
}

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

/// Bounded process history budget (replaces the 0.5 TTL semantics: the
/// event-driven tracker keeps a bounded tombstone history instead of a
/// time-based cache).
pub const PROC_HISTORY_CAP: usize = 65536;

/// Build the initial topology: register the cn_proc source, then adopt a
/// one-shot `/proc` snapshot as the bootstrap baseline (not a poll loop —
/// events maintain the tree from here on). Returns the tracker and the
/// registered source id.
pub fn init_tracker(config: TrackerConfig, proc_root: &std::path::Path) -> (ProcessTracker, SourceId) {
    let mut tracker = ProcessTracker::new(config);
    let src = tracker.register_source(source_capabilities());
    let (entries, meta) = crate::common::proc_scan::scan_once(proc_root);
    let builder = BootstrapBuilder::new(config);
    let tracker = match builder.build(entries, &meta) {
        Ok((tracker, _report, _)) => tracker,
        Err(_) => {
            // Event-only start: honest degradation (continuity starts with
            // the first applied event).
            tracker
        }
    };
    // Enrich the baseline with comm/uid so cmd filters work immediately
    // (one /proc read per process, bootstrap only).
    let baseline: Vec<_> = {
        let view = tracker.view();
        tracker
            .live_keys()
            .map(|k| (k, view.get(k).map(|v| v.tgid().0)))
            .collect()
    };
    let mut tracker = tracker;
    for (key, tgid) in baseline {
        if let Some(tgid) = tgid
            && let Some(meta) = proc_tree::read_metadata(proc_root, tgid)
        {
            tracker.enrich(key, Some(meta));
        }
    }
    (tracker, src)
}

/// Per-CPU sequence tracking (RUN-23, proc-connector v0.3): `cn_msg.seq`
/// is monotonic within one CPU; a jump quantifies lost events. The mapper
/// keeps the last seen seq per CPU across drains and reports the gap as a
/// health signal.
#[derive(Default)]
pub struct EventMapper {
    per_cpu_last: std::collections::HashMap<u32, u32>,
}

impl EventMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the per-CPU sequence and build the event cursor
    /// `(cpu << 32) | seq`. Returns `Some(health)` when the sequence
    /// jumped (events lost on that CPU).
    pub fn cursor(&mut self, cpu: u32, seq: u32) -> (u64, Option<SourceHealth>) {
        let mut health = None;
        if let Some(&last) = self.per_cpu_last.get(&cpu) {
            if seq > last + 1 {
                health = Some(SourceHealth::Gapped {
                    from: Some(last as u64 + 1),
                    to: Some(seq as u64 - 1),
                });
            }
        }
        self.per_cpu_last.insert(cpu, seq);
        (((cpu as u64) << 32) | seq as u64, health)
    }
}

/// Parse raw proc-connector bytes and drive the tracker's event protocol.
///
/// Lifecycle events are batched into one `apply_events` call (single commit
/// revision); `Comm` updates attach metadata immediately; per-CPU sequence
/// jumps degrade continuity via `report_health` (quantified gap); netlink
/// overrun reports a separate health signal.
pub fn handle_proc_events(
    tracker: &mut ProcessTracker,
    src: SourceId,
    mapper: &mut EventMapper,
    data: &[u8],
    n: usize,
) {
    let mut events: Vec<LifecycleEvent> = Vec::new();
    let mut health: Option<SourceHealth> = None;
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(PcEvent::Fork {
                cpu,
                seq,
                parent_pid,
                parent_tgid,
                child_pid,
                child_tgid,
                timestamp_ns,
            })) => {
                let (cursor, h) = mapper.cursor(cpu, seq);
                health = health.or(h);
                events.push(LifecycleEvent::Spawn {
                    meta: EventMeta {
                        src,
                        epoch: 0,
                        timestamp: Some(timestamp_ns),
                        clock: ClockDomain::BootMonotonic,
                        cursor: Some(cursor),
                    },
                    child_tid: Tid(child_pid),
                    child_tgid: Tgid(child_tgid),
                    parent_tid: Tid(parent_pid),
                    parent_tgid: Tgid(parent_tgid),
                    evidence: IdentityEvidence::Weak,
                });
            }
            Ok(Some(PcEvent::Exec {
                cpu,
                seq,
                pid,
                tgid,
                timestamp_ns,
            })) => {
                let (cursor, h) = mapper.cursor(cpu, seq);
                health = health.or(h);
                events.push(LifecycleEvent::Exec {
                    meta: EventMeta {
                        src,
                        epoch: 0,
                        timestamp: Some(timestamp_ns),
                        clock: ClockDomain::BootMonotonic,
                        cursor: Some(cursor),
                    },
                    tid: Tid(pid),
                    tgid: Tgid(tgid),
                });
            }
            Ok(Some(PcEvent::Exit {
                cpu,
                seq,
                pid,
                tgid,
                ..
            })) => {
                let (cursor, h) = mapper.cursor(cpu, seq);
                health = health.or(h);
                events.push(LifecycleEvent::Exit {
                    meta: EventMeta {
                        src,
                        epoch: 0,
                        timestamp: None,
                        clock: ClockDomain::BootMonotonic,
                        cursor: Some(cursor),
                    },
                    tid: Tid(pid),
                    tgid: Tgid(tgid),
                    scope: ExitScope::Task,
                });
            }
            Ok(Some(PcEvent::Comm {
                cpu,
                seq,
                pid,
                tgid,
                comm,
                ..
            })) => {
                let (_, h) = mapper.cursor(cpu, seq);
                health = health.or(h);
                // Enrichment: the comm change attaches to the current
                // generation without touching the event batch.
                let comm = String::from_utf8_lossy(&comm)
                    .trim_matches('\0')
                    .to_string();
                if !comm.is_empty()
                    && let Some(key) = tracker.current(Tgid(tgid))
                {
                    tracker.enrich(
                        key,
                        Some(ProcessMetadata {
                            comm: Some(comm),
                            uid: None,
                        }),
                    );
                }
                let _ = pid;
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(proc_connector::Error::Overrun) => {
                eprintln!("[WARNING] proc connector overrun — some events may have been lost");
                health = health.or(Some(SourceHealth::Overrun { count: 0 }));
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
        let _ = tracker.apply_events(src, &events);
    }
    if let Some(h) = health {
        tracker.report_health(src, h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapper_detects_per_cpu_gap() {
        let mut m = EventMapper::new();
        // First message on a CPU: no baseline, no gap.
        let (c1, h1) = m.cursor(0, 5);
        assert_eq!(c1, 5);
        assert!(h1.is_none());
        // Jump 5 -> 9: seqs 6..=8 were lost on CPU 0.
        let (c2, h2) = m.cursor(0, 9);
        assert_eq!(c2, 9);
        assert_eq!(
            h2,
            Some(SourceHealth::Gapped {
                from: Some(6),
                to: Some(8)
            })
        );
        // A different CPU has an independent sequence.
        let (_, h3) = m.cursor(1, 1);
        assert!(h3.is_none());
        // Redelivery (seq <= last) is not a gap; the core's dedupe handles it.
        let (_, h4) = m.cursor(0, 7);
        assert!(h4.is_none());
    }
}
