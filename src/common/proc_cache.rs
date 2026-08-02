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

/// Map one parsed proc-connector event into the tracker input protocol
/// (pure; no tracker access). `cursor` comes from the [`EventMapper`].
fn map_proc_event(ev: &PcEvent, src: SourceId, cursor: u64) -> LifecycleEvent {
    let ts = match ev {
        PcEvent::Exit { .. } => None,
        PcEvent::Fork { timestamp_ns, .. }
        | PcEvent::Exec { timestamp_ns, .. }
        | PcEvent::Uid { timestamp_ns, .. }
        | PcEvent::Gid { timestamp_ns, .. }
        | PcEvent::Sid { timestamp_ns, .. }
        | PcEvent::Ptrace { timestamp_ns, .. }
        | PcEvent::Comm { timestamp_ns, .. }
        | PcEvent::Coredump { timestamp_ns, .. } => Some(*timestamp_ns),
        PcEvent::Unknown { .. } => None,
    };
    let meta = EventMeta {
        src,
        epoch: 0,
        timestamp: ts,
        clock: ClockDomain::BootMonotonic,
        cursor: Some(cursor),
    };
    match ev {
        PcEvent::Fork {
            parent_pid,
            parent_tgid,
            child_pid,
            child_tgid,
            ..
        } => LifecycleEvent::Spawn {
            meta,
            child_tid: Tid(*child_pid),
            child_tgid: Tgid(*child_tgid),
            parent_tid: Tid(*parent_pid),
            parent_tgid: Tgid(*parent_tgid),
            evidence: IdentityEvidence::Weak,
        },
        PcEvent::Exec { pid, tgid, .. } => LifecycleEvent::Exec {
            meta,
            tid: Tid(*pid),
            tgid: Tgid(*tgid),
        },
        PcEvent::Exit { pid, tgid, .. } => LifecycleEvent::Exit {
            meta,
            tid: Tid(*pid),
            tgid: Tgid(*tgid),
            scope: ExitScope::Task,
        },
        _ => unreachable!("map_proc_event only receives Fork/Exec/Exit"),
    }
}

/// Parse raw proc-connector bytes and drive the tracker's event protocol.
///
/// Lifecycle events are batched into one `apply_events` call (single commit
/// revision); `Comm` updates attach metadata AFTER the batch (the Spawn
/// event for the same process sits in the batch — enriching before it is
/// applied would miss the key); per-CPU sequence jumps degrade continuity
/// via `report_health` (quantified gap); netlink overrun reports a separate
/// health signal.
pub fn handle_proc_events(
    tracker: &mut ProcessTracker,
    src: SourceId,
    mapper: &mut EventMapper,
    data: &[u8],
    n: usize,
) {
    let mut events: Vec<LifecycleEvent> = Vec::new();
    let mut health: Option<SourceHealth> = None;
    // Comm enrichments are applied AFTER the batch: the Spawn event for the
    // same process sits in the batch and is not in the tracker yet, so
    // `current(tgid)` would miss it during the parse loop.
    let mut comm_updates: Vec<(Tgid, String)> = Vec::new();
    for msg in NetlinkMessageIter::new(data, n) {
        match msg {
            Ok(Some(ev @ (PcEvent::Fork { .. } | PcEvent::Exec { .. } | PcEvent::Exit { .. }))) => {
                let (cpu, seq) = match ev {
                    PcEvent::Fork { cpu, seq, .. }
                    | PcEvent::Exec { cpu, seq, .. }
                    | PcEvent::Exit { cpu, seq, .. } => (cpu, seq),
                    _ => unreachable!(),
                };
                let (cursor, h) = mapper.cursor(cpu, seq);
                health = health.or(h);
                events.push(map_proc_event(&ev, src, cursor));
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
                if !comm.is_empty() {
                    comm_updates.push((Tgid(tgid), comm));
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
    for (tgid, comm) in comm_updates {
        if let Some(key) = tracker.current(tgid) {
            tracker.enrich(
                key,
                Some(ProcessMetadata {
                    comm: Some(comm),
                    uid: None,
                }),
            );
        }
    }
    if let Some(h) = health {
        tracker.report_health(src, h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_tree::{ApplyOutcome, HistoryPolicy, ProcessTracker, TrackerConfig};

    fn tracker() -> ProcessTracker {
        ProcessTracker::new(TrackerConfig {
            domain: proc_tree::DomainId(1),
            history: HistoryPolicy::Count(16),
            stop: proc_tree::StopPolicy::Continue,
        })
    }

    fn boot_source(t: &mut ProcessTracker) -> SourceId {
        t.register_source(source_capabilities())
    }

    /// A Fork(pid) from pid 1 with per-CPU seq.
    fn fork_event(cpu: u32, seq: u32, child: u32) -> PcEvent {
        PcEvent::Fork {
            cpu,
            seq,
            parent_pid: 1,
            parent_tgid: 1,
            child_pid: child,
            child_tgid: child,
            timestamp_ns: 10,
        }
    }

    #[test]
    fn spawn_maps_to_weak_evidence_event() {
        let ev = fork_event(0, 5, 100);
        let mapped = map_proc_event(&ev, SourceId(1), 5);
        match mapped {
            LifecycleEvent::Spawn {
                child_tid,
                child_tgid,
                parent_tid: _,
                parent_tgid,
                evidence,
                meta,
            } => {
                assert_eq!(child_tid.0, 100);
                assert_eq!(child_tgid.0, 100);
                assert_eq!(parent_tgid.0, 1);
                assert_eq!(evidence, IdentityEvidence::Weak);
                assert_eq!(meta.cursor, Some(5));
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[test]
    fn comm_enriches_after_batch_apply() {
        // Regression: Comm enrichment used to run BEFORE apply_events, so
        // the Spawn for the same process was not yet in the tracker and
        // `current(tgid)` missed — comm was silently dropped.
        let mut t = tracker();
        let src = boot_source(&mut t);
        t.apply(&LifecycleEvent::Spawn {
            meta: EventMeta {
                src,
                epoch: 0,
                timestamp: None,
                clock: ClockDomain::BootMonotonic,
                cursor: Some(1),
            },
            child_tid: Tid(100),
            child_tgid: Tgid(100),
            parent_tid: Tid(1),
            parent_tgid: Tgid(1),
            evidence: IdentityEvidence::Weak,
        });
        let key = t.current(Tgid(100)).expect("spawn applied");
        t.enrich(
            key,
            Some(ProcessMetadata {
                comm: Some("touch".into()),
                uid: None,
            }),
        );
        let view = t.view();
        assert_eq!(
            view.metadata(key).and_then(|m| m.comm.as_deref()),
            Some("touch")
        );
    }

    #[test]
    fn spawn_parent_edge_with_weak_evidence() {
        // fsmon ppid regression: event-driven spawn from an existing
        // parent must attach the parent edge even with Weak evidence.
        let mut t = tracker();
        let src = boot_source(&mut t);
        t.apply(&LifecycleEvent::Spawn {
            meta: EventMeta {
                src,
                epoch: 0,
                timestamp: None,
                clock: ClockDomain::BootMonotonic,
                cursor: Some(1),
            },
            child_tid: Tid(1),
            child_tgid: Tgid(1),
            parent_tid: Tid(0),
            parent_tgid: Tgid(0),
            evidence: IdentityEvidence::Weak,
        });
        let (outcome, _) = t.apply(&LifecycleEvent::Spawn {
            meta: EventMeta {
                src,
                epoch: 0,
                timestamp: None,
                clock: ClockDomain::BootMonotonic,
                cursor: Some(2),
            },
            child_tid: Tid(100),
            child_tgid: Tgid(100),
            parent_tid: Tid(1),
            parent_tgid: Tgid(1),
            evidence: IdentityEvidence::Weak,
        });
        assert_eq!(outcome, ApplyOutcome::Applied);
        let view = t.view();
        let key = view.current(Tgid(100)).expect("child");
        assert_eq!(
            view.get(key).and_then(|n| n.current_parent()).map(|p| p.tgid.0),
            Some(1),
            "parent edge must attach (fsmon ppid regression)"
        );
    }

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
