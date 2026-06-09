mod core;

pub use core::Query;

#[cfg(test)]
mod tests {
    use crate::common::EventType;
    use crate::common::query::core::Query;
    use crate::common::utils::{TimeFilter, TimeOp};
    use crate::common::{FileEvent, parse_log_line_jsonl};
    use chrono::Utc;
    use std::fs;
    use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};

    fn create_log_file(dir: &Path, events: &[FileEvent]) -> PathBuf {
        let path = dir.join("test.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for event in events {
            writeln!(f, "{}", event.to_jsonl_string()).unwrap();
        }
        path
    }

    // ---- basic ----

    #[test]
    fn test_read_events_basic() {
        let dir = std::env::temp_dir().join("fsmon_query_test_basic");
        fs::create_dir_all(&dir).unwrap();
        let events = vec![
            FileEvent {
                time: Utc::now(),
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/test"),
                pid: 100,
                cmd: "touch".into(),
                user: "root".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: Utc::now(),
                event_type: EventType::Modify,
                path: PathBuf::from("/tmp/test"),
                pid: 200,
                cmd: "vim".into(),
                user: "root".into(),
                file_size: 100,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, None, vec![], false);
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].pid, 100);
        assert_eq!(result[1].pid, 200);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_empty_file() {
        let dir = std::env::temp_dir().join("fsmon_query_test_empty");
        fs::create_dir_all(&dir).unwrap();
        let log_path = create_log_file(&dir, &[]);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, None, vec![], false);
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert!(result.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_log_files_by_cmd() {
        let dir = std::env::temp_dir().join("fsmon_query_test_resolve_cmd");
        fs::create_dir_all(&dir).unwrap();
        // Create a cmd-based log file
        let log_path = dir.join("openclaw_log.jsonl");
        let mut f = fs::File::create(&log_path).unwrap();
        writeln!(f, "{{\"time\":\"2025-01-01T00:00:00Z\",\"event_type\":\"CREATE\",\"path\":\"/a\",\"pid\":1,\"cmd\":\"openclaw\",\"user\":\"r\",\"file_size\":0,\"ppid\":0,\"tgid\":0,\"chain\":\"\"}}").unwrap();

        let q = Query::new(dir.clone(), Some("openclaw".into()), None, vec![], false);
        let files = q.resolve_log_files().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("openclaw_log.jsonl"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_log_files_nonexistent_cmd() {
        let dir = std::env::temp_dir().join("fsmon_query_test_nonexistent_cmd");
        fs::create_dir_all(&dir).unwrap();
        let q = Query::new(dir.clone(), Some("nonexistent".into()), None, vec![], false);
        let files = q.resolve_log_files().unwrap();
        assert!(
            files.is_empty(),
            "nonexistent cmd should yield no log files"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- time filter operators ----

    #[test]
    fn test_time_filter_gt() {
        let dir = std::env::temp_dir().join("fsmon_query_test_gt");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let events = vec![
            FileEvent {
                time: now - chrono::Duration::hours(2),
                event_type: EventType::Create,
                path: PathBuf::from("/a"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now - chrono::Duration::minutes(30),
                event_type: EventType::Create,
                path: PathBuf::from("/b"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            None,
            vec![TimeFilter {
                op: TimeOp::Gt,
                time: now - chrono::Duration::hours(1),
            }],
            false,
        );
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/b"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_time_filter_ge() {
        let dir = std::env::temp_dir().join("fsmon_query_test_ge");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let cutoff = now - chrono::Duration::hours(1);
        let events = vec![
            FileEvent {
                time: cutoff,
                event_type: EventType::Create,
                path: PathBuf::from("/a"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/b"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            None,
            vec![TimeFilter {
                op: TimeOp::Ge,
                time: cutoff,
            }],
            false,
        );
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_time_filter_lt() {
        let dir = std::env::temp_dir().join("fsmon_query_test_lt");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let events = vec![
            FileEvent {
                time: now - chrono::Duration::hours(2),
                event_type: EventType::Create,
                path: PathBuf::from("/a"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/b"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            None,
            vec![TimeFilter {
                op: TimeOp::Lt,
                time: now - chrono::Duration::hours(1),
            }],
            false,
        );
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/a"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_time_filter_le() {
        let dir = std::env::temp_dir().join("fsmon_query_test_le");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let cutoff = now - chrono::Duration::hours(1);
        let events = vec![
            FileEvent {
                time: cutoff,
                event_type: EventType::Create,
                path: PathBuf::from("/a"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/b"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            None,
            vec![TimeFilter {
                op: TimeOp::Le,
                time: cutoff,
            }],
            false,
        );
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/a"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_time_filter_eq() {
        let dir = std::env::temp_dir().join("fsmon_query_test_eq");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let events = vec![
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/a"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now - chrono::Duration::hours(1),
                event_type: EventType::Create,
                path: PathBuf::from("/b"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            None,
            vec![TimeFilter {
                op: TimeOp::Eq,
                time: now,
            }],
            false,
        );
        let result = q.read_events_from(&log_path, None, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/a"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- path filters ----

    #[test]
    fn test_path_filter_prefix() {
        let dir = std::env::temp_dir().join("fsmon_query_test_path_filter");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let events = vec![
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/test"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now,
                event_type: EventType::Create,
                path: PathBuf::from("/var/log/syslog"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(
            log_dir,
            None,
            Some(vec![PathBuf::from("/tmp")]),
            vec![],
            false,
        );
        let mut result = q.read_events_from(&log_path, None, None).unwrap();
        result.retain(|event| {
            q.path_filters()
                .unwrap()
                .iter()
                .any(|pf| event.path.starts_with(pf))
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, PathBuf::from("/tmp/test"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- binary search ----

    #[test]
    fn test_binary_search_finds_first_event() {
        let dir = std::env::temp_dir().join("fsmon_query_test_binary_search");
        fs::create_dir_all(&dir).unwrap();
        let base_time = Utc::now() - chrono::Duration::hours(10);
        let events: Vec<FileEvent> = (0..100)
            .map(|i| FileEvent {
                time: base_time + chrono::Duration::minutes(i),
                event_type: EventType::Create,
                path: PathBuf::from(format!("/file{}", i)),
                pid: i as u32,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            })
            .collect();
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, None, vec![], false);
        let file_len = fs::metadata(&log_path).unwrap().len();
        let target_time = base_time + chrono::Duration::minutes(50);
        let pos = q
            .find_first_event_after(file_len, &log_path, target_time)
            .unwrap();
        // Read from that position and verify first event is >= target_time
        let mut reader = BufReader::new(fs::File::open(&log_path).unwrap());
        reader.seek(SeekFrom::Start(pos)).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let event = parse_log_line_jsonl(line.trim()).unwrap();
        assert!(event.time >= target_time);
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- execute_changes ----

    #[test]
    fn test_execute_changes_dedup() {
        let dir = std::env::temp_dir().join("fsmon_query_test_changes");
        fs::create_dir_all(&dir).unwrap();
        let now = Utc::now();
        let events = vec![
            FileEvent {
                time: now - chrono::Duration::hours(1),
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/test"),
                pid: 1,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now,
                event_type: EventType::Modify,
                path: PathBuf::from("/tmp/test"),
                pid: 2,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 100,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
            FileEvent {
                time: now - chrono::Duration::minutes(30),
                event_type: EventType::Create,
                path: PathBuf::from("/tmp/other"),
                pid: 3,
                cmd: "c".into(),
                user: "u".into(),
                file_size: 0,
                ppid: 0,
                tgid: 0,
                chain: String::new(),
            },
        ];
        let log_path = create_log_file(&dir, &events);
        let log_dir = log_path.parent().unwrap().to_path_buf();
        let q = Query::new(log_dir, None, None, vec![], false);
        let result = q.read_events_from(&log_path, None, None).unwrap();
        // Should dedup by path, keeping latest event per path
        let mut latest_by_path: std::collections::HashMap<PathBuf, FileEvent> =
            std::collections::HashMap::new();
        for event in result {
            match latest_by_path.entry(event.path.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if event.time > entry.get().time {
                        entry.insert(event);
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(event);
                }
            }
        }
        assert_eq!(latest_by_path.len(), 2);
        assert!(latest_by_path.contains_key(&PathBuf::from("/tmp/test")));
        assert!(latest_by_path.contains_key(&PathBuf::from("/tmp/other")));
        // The latest event for /tmp/test should be the Modify event
        let test_event = latest_by_path.get(&PathBuf::from("/tmp/test")).unwrap();
        assert_eq!(test_event.event_type, EventType::Modify);
        let _ = fs::remove_dir_all(&dir);
    }
}
