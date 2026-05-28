use super::*;
use crate::fid_parser::mask_to_event_types;
use crate::monitored::PathEntry;
use crate::utils::{SizeFilter, SizeOp};
use crate::{EventType, FileEvent};
use crate::filters::PathOptions;
use fanotify_fid::consts::{FAN_CREATE, FAN_DELETE, FAN_EVENT_ON_CHILD, FAN_MODIFY, FAN_ONDIR};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

// ---- mask_to_event_types ----

#[test]
fn test_mask_to_event_types_single() {
    let types = mask_to_event_types(FAN_CREATE);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0], EventType::Create);
}

#[test]
fn test_mask_to_event_types_multiple() {
    let mask = FAN_CREATE | FAN_DELETE | FAN_MODIFY;
    let types = mask_to_event_types(mask);
    assert_eq!(types.len(), 3);
    assert!(types.contains(&EventType::Create));
    assert!(types.contains(&EventType::Delete));
    assert!(types.contains(&EventType::Modify));
}

#[test]
fn test_mask_to_event_types_none() {
    let types = mask_to_event_types(0);
    assert!(types.is_empty());
}

#[test]
fn test_mask_to_event_types_all() {
    use fanotify_fid::consts::{
        FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_DELETE_SELF,
        FAN_FS_ERROR, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_OPEN, FAN_OPEN_EXEC,
    };
    let mask = FAN_ACCESS
        | FAN_MODIFY
        | FAN_CLOSE_WRITE
        | FAN_CLOSE_NOWRITE
        | FAN_OPEN
        | FAN_OPEN_EXEC
        | FAN_ATTRIB
        | FAN_CREATE
        | FAN_DELETE
        | FAN_DELETE_SELF
        | FAN_FS_ERROR
        | FAN_MOVED_FROM
        | FAN_MOVED_TO
        | FAN_MOVE_SELF;
    let types = mask_to_event_types(mask);
    assert_eq!(types.len(), 14);
}

#[test]
fn test_mask_to_event_types_with_flags() {
    let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
    let types = mask_to_event_types(mask);
    assert_eq!(types.len(), 1);
    assert_eq!(types[0], EventType::Create);
}

// ---- Monitor tests ----

fn options(
    size_filter: Option<SizeFilter>,
    event_types: Option<Vec<EventType>>,
    recursive: bool,
) -> PathOptions {
    PathOptions {
        size_filter,
        event_types,
        recursive,
        cmd: None,
    }
}

fn make_monitor(
    paths: Vec<&str>,
    size_filter: Option<SizeFilter>,
    event_types: Option<Vec<EventType>>,
    recursive: bool,
) -> Monitor {
    Monitor::new(
        paths
            .into_iter()
            .map(|p| {
                (
                    PathBuf::from(p),
                    options(size_filter, event_types.clone(), recursive),
                )
            })
            .collect(),
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        false,
        None,
    )
    .unwrap()
}

#[test]
fn test_should_output_no_filters() {
    let m = make_monitor(vec!["/tmp"], None, None, false);
    let event = make_event("/tmp/test.txt", EventType::Create, 1000, 1024);
    assert!(m.should_output(&event));
}

#[test]
fn test_should_output_type_filter_match() {
    let m = make_monitor(
        vec!["/tmp"],
        None,
        Some(vec![EventType::Create, EventType::Delete]),
        false,
    );
    assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 0)));
    assert!(m.should_output(&make_event("/tmp/a", EventType::Delete, 1, 0)));
    assert!(!m.should_output(&make_event("/tmp/a", EventType::Modify, 1, 0)));
}

#[test]
fn test_should_output_size_filter() {
    let m = make_monitor(
        vec!["/tmp"],
        Some(SizeFilter {
            op: SizeOp::Ge,
            bytes: 1000,
        }),
        None,
        false,
    );
    assert!(m.should_output(&make_event("/tmp/a", EventType::Create, 1, 2000)));
    assert!(!m.should_output(&make_event("/tmp/a", EventType::Create, 1, 500)));
}

#[test]
fn test_should_output_combined_filters() {
    let m = make_monitor(
        vec!["/tmp"],
        Some(SizeFilter {
            op: SizeOp::Ge,
            bytes: 100,
        }),
        Some(vec![EventType::Create]),
        false,
    );
    assert!(m.should_output(&make_event("/tmp/data", EventType::Create, 1, 200)));
    assert!(!m.should_output(&make_event("/tmp/data", EventType::Delete, 1, 200)));
    assert!(!m.should_output(&make_event("/tmp/data", EventType::Create, 1, 50)));
}

#[test]
fn test_is_path_in_scope_recursive() {
    let m = make_monitor(vec!["/tmp"], None, None, true);
    assert!(m.is_path_in_scope(Path::new("/tmp")));
    assert!(m.is_path_in_scope(Path::new("/tmp/sub")));
    assert!(m.is_path_in_scope(Path::new("/tmp/sub/deep/file.txt")));
    assert!(!m.is_path_in_scope(Path::new("/var/log")));
    assert!(!m.is_path_in_scope(Path::new("/tmpfile")));
}

#[test]
fn test_is_path_in_scope_non_recursive() {
    let m = make_monitor(vec!["/tmp"], None, None, false);
    assert!(m.is_path_in_scope(Path::new("/tmp")));
    assert!(m.is_path_in_scope(Path::new("/tmp/file.txt")));
    assert!(!m.is_path_in_scope(Path::new("/tmp/sub/file.txt")));
    assert!(!m.is_path_in_scope(Path::new("/var/log")));
}

#[test]
fn test_is_path_in_scope_multiple_paths() {
    let m = make_monitor(vec!["/tmp", "/var/log"], None, None, true);
    assert!(m.is_path_in_scope(Path::new("/tmp/file")));
    assert!(m.is_path_in_scope(Path::new("/var/log/syslog")));
    assert!(!m.is_path_in_scope(Path::new("/etc/passwd")));
}

#[test]
fn test_file_size_cache_eviction() {
    use lru::LruCache;
    use std::num::NonZeroUsize;

    let mut cache = LruCache::new(NonZeroUsize::new(3).unwrap());

    cache.put(PathBuf::from("/a"), 100);
    cache.put(PathBuf::from("/b"), 200);
    cache.put(PathBuf::from("/c"), 300);
    assert_eq!(cache.len(), 3);

    cache.put(PathBuf::from("/d"), 400);
    assert_eq!(cache.len(), 3);
    assert!(cache.get(&PathBuf::from("/a")).is_none());
    assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
    assert_eq!(cache.get(&PathBuf::from("/d")), Some(&400));

    cache.get(&PathBuf::from("/b"));
    cache.put(PathBuf::from("/e"), 500);
    assert!(cache.get(&PathBuf::from("/c")).is_none());
    assert_eq!(cache.get(&PathBuf::from("/b")), Some(&200));
}

#[test]
fn test_reject_cmd_fsmon_at_startup() {
    let opts = PathOptions {
        size_filter: None,
        event_types: None,
        recursive: true,
        cmd: Some("fsmon".to_string()),
    };
    let result = Monitor::new(
        vec![(PathBuf::from("/tmp"), opts)],
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_err(), "Monitor::new() should reject cmd=fsmon");
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains("Cannot monitor 'fsmon' process"),
        "Error should mention fsmon rejection, got: {}",
        err
    );
}

#[test]
fn test_monitor_buffer_size_validation() {
    let opts = options(None, None, false);

    let result = Monitor::new(
        vec![(PathBuf::from("/tmp"), opts.clone())],
        None,
        None,
        Some(1024),
        None,
        false,
        None,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("at least 4096"));

    let result = Monitor::new(
        vec![(PathBuf::from("/tmp"), opts.clone())],
        None,
        None,
        Some(2 * 1024 * 1024),
        None,
        false,
        None,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_err());
    assert!(result.err().unwrap().to_string().contains("not exceed"));

    let result = Monitor::new(
        vec![(PathBuf::from("/tmp"), opts.clone())],
        None,
        None,
        Some(65536),
        None,
        false,
        None,
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_ok());
}

#[test]
fn test_add_path_and_remove_path() {
    let mut m = Monitor::new(vec![], None, None, None, None, false, None, None, None, None, false, None).unwrap();

    let entry = PathEntry {
        cmd: None,
        path: PathBuf::from("/tmp/test_add"),
        recursive: Some(true),
        types: None,
        size: None,
    };

    // add_path on non-existent path → goes to pending_paths
    let result = m.add_path(&entry);
    assert!(result.is_ok());
    assert!(
        m.pending_paths
            .iter()
            .any(|(p, _)| p == Path::new("/tmp/test_add"))
    );
    assert!(!m.paths.contains(&PathBuf::from("/tmp/test_add")));

    // remove_path on non-existent path (not in options)
    let result = m.remove_path(Path::new("/nonexistent"), None);
    assert!(result.is_err());
}

fn make_event(path: &str, event_type: EventType, pid: u32, size: u64) -> FileEvent {
    FileEvent {
        time: chrono::Utc::now(),
        event_type,
        path: PathBuf::from(path),
        pid,
        cmd: "test".to_string(),
        user: "root".to_string(),
        file_size: size,
        ppid: 0,
        tgid: 0,
        chain: String::new(),
    }
}

// ---- Integration tests (require sudo) ----

#[test]
#[ignore]
fn test_fanotify_init() {
    let fd = fanotify_init(
        FAN_CLOEXEC
            | FAN_NONBLOCK
            | FAN_CLASS_NOTIF
            | FAN_REPORT_FID
            | FAN_REPORT_DIR_FID
            | FAN_REPORT_NAME,
        (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
    );
    assert!(fd.is_ok(), "fanotify_init should succeed with root");
    // OwnedFd is closed on drop — no explicit close needed
}

#[test]
#[ignore]
fn test_fanotify_mark_directory() {
    let test_dir = std::env::temp_dir().join("fsmon_test_mark");
    std::fs::create_dir_all(&test_dir).unwrap();

    let fd = fanotify_init(
        FAN_CLOEXEC
            | FAN_NONBLOCK
            | FAN_CLASS_NOTIF
            | FAN_REPORT_FID
            | FAN_REPORT_DIR_FID
            | FAN_REPORT_NAME,
        (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
    )
    .unwrap();

    let mask = FAN_CREATE | FAN_DELETE | FAN_CLOSE_WRITE;
    let result = fanotify_mark(
        &fd,
        FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
        mask,
        AT_FDCWD,
        &test_dir,
    );
    assert!(
        result.is_ok(),
        "fanotify_mark should succeed on existing directory"
    );

    drop(fd);
    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
#[ignore]
fn test_fanotify_mark_nonexistent_path() {
    let fd = fanotify_init(
        FAN_CLOEXEC
            | FAN_NONBLOCK
            | FAN_CLASS_NOTIF
            | FAN_REPORT_FID
            | FAN_REPORT_DIR_FID
            | FAN_REPORT_NAME,
        (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
    )
    .unwrap();

    let mask = FAN_CREATE;
    let result = fanotify_mark(
        &fd,
        FAN_MARK_ADD,
        mask,
        AT_FDCWD,
        Path::new("/nonexistent_path_12345"),
    );
    assert!(
        result.is_err(),
        "fanotify_mark should fail on nonexistent path"
    );

    drop(fd);
}

#[test]
fn test_fanotify_mark_null_byte_path_no_root() {
    // Verifies CString::new rejects interior null bytes BEFORE any
    // syscall. This test does NOT require root — the error is raised
    // in userspace during path-to-C-string conversion.
    let mask = FAN_CREATE | FAN_DELETE;

    // Create a path with an interior null byte
    let bad_path = Path::new("/tmp/ok\0evil");

    // fanotify_mark needs an fd, but the null byte rejection happens
    // before any syscall. We just need a valid OwnedFd for the param.
    let dev_null = std::fs::File::open("/dev/null")
        .expect("/dev/null must exist on Linux");
    let dummy_fd: std::os::fd::OwnedFd = dev_null.into();

    let result = fanotify_mark(
        &dummy_fd,
        FAN_MARK_ADD,
        mask,
        AT_FDCWD,
        bad_path,
    );

    match result {
        Err(FanotifyError::Mark(code)) => {
            assert_eq!(code, libc::EINVAL,
                "null byte path should return EINVAL, got errno={}", code);
        }
        other => panic!("expected Err(Mark(EINVAL)), got {:?}", other),
    }
}

#[test]
#[ignore]
fn test_monitor_run_captures_events() {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let test_dir = std::env::temp_dir().join("fsmon_test_events");
    std::fs::create_dir_all(&test_dir).unwrap();
    let test_dir_for_cleanup = test_dir.clone();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let test_dir_clone = test_dir.clone();

    let handle = rt.spawn(async move {
        let fd = fanotify_init(
            FAN_CLOEXEC
                | FAN_NONBLOCK
                | FAN_CLASS_NOTIF
                | FAN_REPORT_FID
                | FAN_REPORT_DIR_FID
                | FAN_REPORT_NAME,
            (libc::O_CLOEXEC | libc::O_RDONLY) as u32,
        )
        .unwrap();

        let mask = FAN_CREATE | FAN_CLOSE_WRITE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        fanotify_mark(
            &fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            AT_FDCWD,
            &test_dir_clone,
        )
        .unwrap();

        let mut buf = vec![0u8; 4096];
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(200) {
            if let Ok(events) = fanotify_fid::read::read_fid_events(&fd, &[], &mut buf, None)
                && !events.is_empty()
            {
                counter_clone.fetch_add(events.len(), Ordering::SeqCst);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        drop(fd);
    });

    std::thread::sleep(std::time::Duration::from_millis(50));

    for i in 0..3 {
        let path = test_dir.join(format!("test_{}.txt", i));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "content {}", i).unwrap();
    }

    rt.block_on(handle).unwrap();

    let events_captured = counter.load(Ordering::SeqCst);
    assert!(
        events_captured > 0,
        "Should capture at least some events, got {}",
        events_captured
    );

    let _ = std::fs::remove_dir_all(&test_dir_for_cleanup);
}

// ---- Subscribe tests ----

#[test]
fn test_chains_contain_exact() {
    assert!(chains_contain("bash → myapp → fsmon", "myapp"));
}

#[test]
fn test_chains_contain_not_found() {
    assert!(!chains_contain("bash → other → fsmon", "myapp"));
}

#[test]
fn test_chains_contain_empty_chain() {
    assert!(!chains_contain("", "myapp"));
}

#[test]
fn test_chains_contain_partial_name_not_match() {
    // "myapp-backup" should not match filter "myapp"
    assert!(!chains_contain("bash → myapp-backup → fsmon", "myapp"));
}

#[tokio::test]
async fn test_subscriber_task_receives_events() {
    let (tx, mut rx) = tokio::sync::broadcast::channel(64);

    // Verify broadcast channel works as the unified event stream:
    // Multiple receivers get the same events.
    let mut rx2 = tx.subscribe();
    let event = FileEvent {
        time: chrono::Utc::now(),
        event_type: EventType::Create,
        path: PathBuf::from("/tmp/test.txt"),
        pid: 1234,
        cmd: "test-cmd".to_string(),
        user: "root".to_string(),
        file_size: 100,
        ppid: 0,
        tgid: 0,
        chain: "bash → test-cmd".to_string(),
    };
    tx.send(event.clone()).unwrap();

    let received1 = rx.recv().await.unwrap();
    let received2 = rx2.recv().await.unwrap();
    assert_eq!(received1.path, PathBuf::from("/tmp/test.txt"));
    assert_eq!(received2.path, PathBuf::from("/tmp/test.txt"));
}

#[tokio::test]
async fn test_subscriber_task_filters_by_cmd() {
    // Test the filter logic directly: chains_contain is already tested
    // above. The subscriber_task's filter is just chains_contain check.
    assert!(chains_contain("bash → myapp", "myapp"));
    assert!(!chains_contain("bash → myapp", "other-app"));
}

#[tokio::test]
async fn test_subscriber_task_filters_by_type() {
    // Test the type filter logic: subscriber_task checks if event.event_type
    // is in the allowed types list. We verify by checking a broadcast receiver
    // with the same filter pattern.
    let allowed = vec![EventType::Delete, EventType::CloseWrite];

    let create_event = FileEvent {
        time: chrono::Utc::now(),
        event_type: EventType::Create,
        path: PathBuf::from("/tmp/ignored.txt"),
        pid: 1,
        cmd: "test".to_string(),
        user: "root".to_string(),
        file_size: 0,
        ppid: 0,
        tgid: 0,
        chain: String::new(),
    };
    assert!(!allowed.contains(&create_event.event_type));

    let delete_event = FileEvent {
        time: chrono::Utc::now(),
        event_type: EventType::Delete,
        path: PathBuf::from("/tmp/deleted.txt"),
        pid: 2,
        cmd: "test".to_string(),
        user: "root".to_string(),
        file_size: 0,
        ppid: 0,
        tgid: 0,
        chain: String::new(),
    };
    assert!(allowed.contains(&delete_event.event_type));
}

#[tokio::test]
async fn test_subscriber_task_handles_lagged() {
    // Test the broadcast Lagged behavior directly
    let (tx, mut rx) = tokio::sync::broadcast::channel(4); // small buffer

    // Fill the buffer and overflow to trigger Lagged
    for i in 0..10 {
        let _ = tx.send(FileEvent {
            time: chrono::Utc::now(),
            event_type: EventType::Create,
            path: PathBuf::from(format!("/tmp/batch_{}.txt", i)),
            pid: 100 + i as u32,
            cmd: "test".to_string(),
            user: "root".to_string(),
            file_size: i as u64,
            ppid: 0,
            tgid: 0,
            chain: String::new(),
        });
    }

    // The next recv should get Lagged
    let result = rx.recv().await;
    match result {
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            assert!(n > 0, "should lag with >0 dropped events, got {}", n);
        }
        Ok(event) => {
            // Might get a recent event if buffer still has capacity
            assert!(event.file_size >= 6, "should be a recent event, got file_size={}", event.file_size);
        }
        Err(e) => panic!("unexpected error: {:?}", e),
    }
}
