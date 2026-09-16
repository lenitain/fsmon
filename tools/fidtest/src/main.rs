//! Demonstrate that fanotify-fid silently discards info records it does not know,
//! using its OWN public API. No root, no live kernel events needed.
use fanotify_fid::consts::*;
use fanotify_fid::parse::parse_fid_events;

const META_SIZE: usize = 24;
const INFO_HDR: usize = 4;
const FSID: usize = 8;

fn metadata(mask: u64, pid: i32, event_len: usize) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(event_len as u32).to_ne_bytes());
    b.push(1); // vers
    b.push(0); // reserved
    b.extend_from_slice(&(META_SIZE as u16).to_ne_bytes());
    b.extend_from_slice(&mask.to_ne_bytes());
    b.extend_from_slice(&(-1i32).to_ne_bytes()); // FAN_NOFD
    b.extend_from_slice(&pid.to_ne_bytes());
    b
}

fn file_handle(payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    b.extend_from_slice(&1i32.to_ne_bytes());
    b.extend_from_slice(payload);
    b
}

/// A DFID_NAME-shaped record with an arbitrary info_type — this is exactly the
/// layout the kernel uses for OLD_DFID_NAME / NEW_DFID_NAME.
fn dfid_name_record(info_type: u8, name: &str, handle_payload: &[u8]) -> Vec<u8> {
    let fh = file_handle(handle_payload);
    let nb = name.as_bytes();
    let padded = (nb.len() + 1 + 3) & !3;
    let mut nm = nb.to_vec();
    nm.push(0);
    nm.resize(padded, 0);

    let payload_len = FSID + fh.len() + nm.len();
    let mut b = Vec::new();
    b.push(info_type);
    b.push(0);
    b.extend_from_slice(&((INFO_HDR + payload_len) as u16).to_ne_bytes());
    b.extend_from_slice(&100i32.to_ne_bytes()); // fsid
    b.extend_from_slice(&200i32.to_ne_bytes());
    b.extend_from_slice(&fh);
    b.extend_from_slice(&nm);
    b
}

fn build(records: Vec<Vec<u8>>, mask: u64) -> (Vec<u8>, usize) {
    let body: usize = records.iter().map(|r| r.len()).sum();
    let total = META_SIZE + body;
    let mut buf = metadata(mask, 4242, total);
    for r in records {
        buf.extend_from_slice(&r);
    }
    (buf, total)
}

fn report(label: &str, buf: &[u8], note: &str) {
    let events = parse_fid_events(buf, &[]);
    println!("\n── {label} ──");
    println!("  {note}");
    println!("  events parsed: {}", events.len());
    for e in &events {
        println!("    mask        = {:#x} {:?}", e.mask(), e.event_names().collect::<Vec<_>>());
        println!("    pid         = {}", e.pid());
        println!("    path        = {:?}", e.path());
        println!("    dfid_name   = {:?}", e.dfid_name_filename());
        println!("    self_handle = {}", if e.self_handle().is_some() { "Some" } else { "None" });
    }
}

fn main() {
    println!("=== what fanotify-fid does with info records it does not recognise ===");

    // Control: an ordinary DFID_NAME event. This is the case the crate handles.
    let (buf, _) = build(
        vec![dfid_name_record(FAN_EVENT_INFO_TYPE_DFID_NAME, "control.txt", &[1, 2, 3, 4])],
        FAN_CREATE,
    );
    report("CONTROL: create (type 2 DFID_NAME)", &buf, "expected: path/name present");

    // FAN_RENAME: kernel emits OLD_DFID_NAME (10) then NEW_DFID_NAME (12).
    let (buf, _) = build(
        vec![
            dfid_name_record(10u8 /* OLD_DFID_NAME; no crate constant */, "old-name.txt", &[1, 2, 3, 4]),
            dfid_name_record(12u8 /* NEW_DFID_NAME; no crate constant */, "new-name.txt", &[5, 6, 7, 8]),
        ],
        FAN_RENAME,
    );
    report(
        "RENAME (types 10 + 12)",
        &buf,
        "the crate exports FAN_RENAME and its builder can enable it",
    );

    // FAN_REPORT_PIDFD: kernel appends a PIDFD record (type 4).
    let mut pidfd = Vec::new();
    pidfd.push(4u8 /* PIDFD; no crate constant */);
    pidfd.push(0);
    pidfd.extend_from_slice(&(INFO_HDR as u16 + 4).to_ne_bytes());
    pidfd.extend_from_slice(&7i32.to_ne_bytes()); // the pidfd
    let (buf, _) = build(
        vec![dfid_name_record(FAN_EVENT_INFO_TYPE_DFID_NAME, "x.txt", &[1, 2, 3, 4]), pidfd],
        FAN_CREATE,
    );
    report(
        "FAN_REPORT_PIDFD (type 4)",
        &buf,
        "builder.rs exposes report_pidfd(); parser drops the pidfd",
    );

    // FAN_FS_ERROR: kernel appends an ERROR record (type 5).
    let mut err = Vec::new();
    err.push(5u8 /* ERROR; no crate constant */);
    err.push(0);
    err.extend_from_slice(&(INFO_HDR as u16 + 8).to_ne_bytes());
    err.extend_from_slice(&(-5i32).to_ne_bytes()); // error
    err.extend_from_slice(&3u32.to_ne_bytes()); // error_count
    let (buf, _) = build(vec![err], FAN_FS_ERROR);
    report(
        "FAN_FS_ERROR (type 5)",
        &buf,
        "error code and error_count are dropped",
    );

    // A future info type the crate has never heard of.
    let (buf, _) = build(
        vec![dfid_name_record(99, "future.txt", &[1, 2, 3, 4])],
        FAN_CREATE,
    );
    report(
        "UNKNOWN future type (99)",
        &buf,
        "silently ignored — caller gets no signal that data was dropped",
    );
}
