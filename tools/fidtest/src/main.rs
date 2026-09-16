//! Show what fanotify-fid does with the info records it parses, using its OWN
//! public API. No root, no live kernel events needed.
//!
//! Originally written to *demonstrate* that 0.7.0 silently discarded records it
//! did not recognise (see PRIVILEGE-SEPARATION-PLAN.md §5.6b).  It compiles
//! against both 0.7.0 and 0.7.1: the `records` section prints the typed
//! accessors only when the crate provides them, so the same one-line
//! `Cargo.toml` switch shows the before and after.
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

// Info types as the kernel UAPI header defines them. 0.7.1 exports constants
// for all of these; 0.7.0 exported only 1/2/3.  Raw values are used so this
// harness compiles against both, which is what makes the before/after
// comparison possible at all.
const PIDFD: u8 = 4;
const ERROR: u8 = 5;
const MNT: u8 = 7;
/// The rename record types (`FAN_EVENT_INFO_TYPE_OLD_DFID_NAME` in 0.7.1).
const OLD_DFID_NAME: u8 = 10;
/// (`FAN_EVENT_INFO_TYPE_NEW_DFID_NAME` in 0.7.1).
const NEW_DFID_NAME: u8 = 12;

fn build(records: Vec<Vec<u8>>, mask: u64) -> Vec<u8> {
    let body: usize = records.iter().map(|r| r.len()).sum();
    let mut buf = metadata(mask, 4242, META_SIZE + body);
    for r in records {
        buf.extend_from_slice(&r);
    }
    buf
}

/// Set by `build.rs` from the *resolved* fanotify-fid version: true for
/// 0.7.1 or later, which is when info record types 4/5/6/7/10/12 became
/// reachable.
#[cfg(fanotify_fid_has_records)]
const HAS_RECORDS: bool = true;
#[cfg(not(fanotify_fid_has_records))]
const HAS_RECORDS: bool = false;

/// Report one synthetic buffer.
///
/// `pidfd_sent` is the descriptor this buffer carried, if any: the harness
/// checks the event ended up owning *that* descriptor, which is the only way to
/// show the record is honoured rather than merely not-an-error.
fn report(label: &str, buf: &[u8], note: &str, pidfd_sent: Option<i32>) {
    let events = parse_fid_events(buf, &[]);
    println!("\n── {label} ──");
    println!("  {note}");
    println!("  events parsed: {}", events.len());
    for e in &events {
        println!(
            "    mask        = {:#x} {:?}",
            e.mask(),
            e.event_names().collect::<Vec<_>>()
        );
        println!("    pid         = {}", e.pid());
        println!("    path        = {:?}", e.path());
        println!("    dfid_name   = {:?}", e.dfid_name_filename());
        println!(
            "    self_handle = {}",
            if e.self_handle().is_some() {
                "Some"
            } else {
                "None"
            }
        );

        #[cfg(fanotify_fid_has_records)]
        {
            if let Some(raw) = pidfd_sent {
                // A real descriptor is used, not a placeholder, so this proves
                // ownership: the event must hold the very same descriptor.
                let holds_it = e
                    .pidfd()
                    .is_some_and(|fd| std::os::fd::AsRawFd::as_raw_fd(&fd) == raw);
                println!(
                    "    pidfd       = {}",
                    if holds_it {
                        format!("Some (original fd {raw} is owned by the event)")
                    } else {
                        "None (RECORD LOST!)".to_string()
                    }
                );
            }
            println!("    fs_error    = {:?}", e.fs_error());
            println!(
                "    rename_from = {:?}",
                e.rename_source().map(|s| s.name.as_str())
            );
            println!(
                "    rename_to   = {:?}",
                e.rename_target().map(|s| s.name.as_str())
            );
            println!("    unparsed    = {:?}", e.unknown_info_records().len());
        }

        #[cfg(not(fanotify_fid_has_records))]
        {
            let _ = pidfd_sent;
            println!("    (0.7.0 has accessors only for types 1/2/3 —");
            println!("     pidfd, fs_error, rename sides and unparsed records");
            println!("     are unreachable from the public API)");
        }
    }
}

fn main() {
    println!("=== what fanotify-fid does with the info records in a FID event ===");
    if HAS_RECORDS {
        println!("fanotify-fid 0.7.1+: record types 4/5/6/7/10/12 are reachable");
    } else {
        println!("fanotify-fid 0.7.0: the audit's baseline — these records are");
        println!("dropped, and nothing in the public API says so");
    }

    // Control: an ordinary DFID_NAME event. This is the case 0.7.0 handles.
    let buf = build(
        vec![dfid_name_record(
            FAN_EVENT_INFO_TYPE_DFID_NAME,
            "control.txt",
            &[1, 2, 3, 4],
        )],
        FAN_CREATE,
    );
    report(
        "CONTROL: create (type 2 DFID_NAME)",
        &buf,
        "expected: name present",
        None,
    );

    // FAN_RENAME: kernel emits OLD_DFID_NAME (10) then NEW_DFID_NAME (12).
    let buf = build(
        vec![
            dfid_name_record(OLD_DFID_NAME, "old-name.txt", &[1, 2, 3, 4]),
            dfid_name_record(NEW_DFID_NAME, "new-name.txt", &[5, 6, 7, 8]),
        ],
        FAN_RENAME,
    );
    report(
        "RENAME (types 10 + 12)",
        &buf,
        "the crate exports FAN_RENAME and its builder can enable it",
        None,
    );

    // FAN_REPORT_PIDFD: kernel appends a PIDFD record (type 4) carrying a real
    // descriptor.  A harmless one is opened so the record is realistic.
    let real_fd = std::fs::File::open("/dev/null").unwrap();
    let fd_raw = std::os::fd::AsRawFd::as_raw_fd(&real_fd);
    let mut pidfd = Vec::new();
    pidfd.push(PIDFD);
    pidfd.push(0);
    pidfd.extend_from_slice(&(INFO_HDR as u16 + 4).to_ne_bytes());
    pidfd.extend_from_slice(&fd_raw.to_ne_bytes());
    let buf = build(
        vec![
            dfid_name_record(FAN_EVENT_INFO_TYPE_DFID_NAME, "x.txt", &[1, 2, 3, 4]),
            pidfd,
        ],
        FAN_CREATE,
    );
    drop(real_fd); // the record is the only owner now
    report(
        "FAN_REPORT_PIDFD (type 4)",
        &buf,
        "builder.rs exposes report_pidfd()",
        Some(fd_raw),
    );

    // FAN_FS_ERROR: kernel appends an ERROR record (type 5).
    let mut err = Vec::new();
    err.push(ERROR);
    err.push(0);
    err.extend_from_slice(&(INFO_HDR as u16 + 8).to_ne_bytes());
    err.extend_from_slice(&(-5i32).to_ne_bytes()); // error
    err.extend_from_slice(&3u32.to_ne_bytes()); // error_count
    let buf = build(vec![err], FAN_FS_ERROR);
    report("FAN_FS_ERROR (type 5)", &buf, "error + error_count", None);

    // A record type no released crate knows: RANGE (6) / MNT (7).
    let mut mnt = Vec::new();
    mnt.push(MNT);
    mnt.push(0);
    mnt.extend_from_slice(&(INFO_HDR as u16 + 8).to_ne_bytes());
    mnt.extend_from_slice(&4242u64.to_ne_bytes()); // mnt_id
    let buf = build(vec![mnt], FAN_CREATE);
    report("MNT (type 7)", &buf, "a mount record", None);

    // A future info type the crate has never heard of.
    let buf = build(
        vec![dfid_name_record(99, "future.txt", &[1, 2, 3, 4])],
        FAN_CREATE,
    );
    report(
        "UNKNOWN future type (99)",
        &buf,
        "a type no released kernel defines yet",
        None,
    );
}
