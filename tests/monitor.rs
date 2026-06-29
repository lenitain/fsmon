//! P1 — Event parsing and serialization tests.

use fsmon::common::{EventType, FileEvent, parse_log_line_jsonl};

#[test]
fn parse_valid_jsonl_event() {
    let json = r#"{"time":"2026-06-01T12:00:00Z","event_type":"CREATE","path":"/tmp/test.txt","pid":1234,"comm":"touch","cmd":"touch","user":"pilot","file_size":0,"ppid":100,"tgid":1234,"chain":[]}"#;
    let ev = parse_log_line_jsonl(json).unwrap();
    assert_eq!(ev.event_type, EventType::Create);
    assert_eq!(ev.path.to_string_lossy(), "/tmp/test.txt");
    assert_eq!(ev.pid, 1234);
    assert_eq!(ev.cmd, "touch");
    assert_eq!(ev.user, "pilot");
}

#[test]
fn parse_invalid_jsonl_returns_none() {
    assert!(parse_log_line_jsonl("not json").is_none());
    assert!(parse_log_line_jsonl("").is_none());
}

#[test]
fn parse_whitespace_only_returns_none() {
    assert!(parse_log_line_jsonl("   \n").is_none());
    assert!(parse_log_line_jsonl("\t  ").is_none());
}

#[test]
fn event_jsonl_round_trip() {
    use chrono::Utc;
    let ev = FileEvent {
        time: Utc::now(),
        event_type: EventType::Modify,
        path: std::path::PathBuf::from("/tmp/test"),
        pid: 42,
        comm: "test".into(),
        cmd: "vim".into(),
        user: "root".into(),
        file_size: 1024,
        ppid: 1,
        tgid: 42,
        chain: Vec::new(),
    };
    let json = ev.to_jsonl_string();
    let parsed = parse_log_line_jsonl(&json).unwrap();
    assert_eq!(parsed.path, ev.path);
    assert_eq!(parsed.pid, ev.pid);
    assert_eq!(parsed.cmd, ev.cmd);
    assert_eq!(parsed.event_type, ev.event_type);
    assert_eq!(parsed.file_size, ev.file_size);
}

#[test]
fn jsonl_local_time_has_offset_not_z() {
    use chrono::Utc;
    let ev = FileEvent {
        time: Utc::now(),
        event_type: EventType::Create,
        path: std::path::PathBuf::from("/tmp/x"),
        pid: 1,
        comm: "test".into(),
        cmd: "x".into(),
        user: "x".into(),
        file_size: 0,
        ppid: 0,
        tgid: 0,
        chain: Vec::new(),
    };
    let local = ev.to_jsonl_string_local();
    // local time must have + or - offset, not Z
    assert!(
        local.contains('+') || local.contains('-'),
        "local time should have timezone offset, got: {}",
        &local[..local.len().min(120)]
    );
    assert!(
        !local.contains("\"time\":\"")
            || !local[local.find("\"time\":\"").unwrap()..].contains('Z'),
        "local time should not end with Z"
    );
}

#[test]
fn jsonl_normal_uses_utc() {
    use chrono::Utc;
    let ev = FileEvent {
        time: Utc::now(),
        event_type: EventType::Create,
        path: std::path::PathBuf::from("/tmp/x"),
        pid: 1,
        comm: "test".into(),
        cmd: "x".into(),
        user: "x".into(),
        file_size: 0,
        ppid: 0,
        tgid: 0,
        chain: Vec::new(),
    };
    let normal = ev.to_jsonl_string();
    assert!(normal.contains('Z'), "normal time should use UTC Z suffix");
}

#[test]
fn event_type_all_covers_14_types() {
    assert_eq!(EventType::ALL.len(), 14);
}

#[test]
fn event_type_to_string_round_trip() {
    let all_types = [
        EventType::Access,
        EventType::Modify,
        EventType::CloseWrite,
        EventType::CloseNowrite,
        EventType::Open,
        EventType::OpenExec,
        EventType::Attrib,
        EventType::Create,
        EventType::Delete,
        EventType::DeleteSelf,
        EventType::MovedFrom,
        EventType::MovedTo,
        EventType::MoveSelf,
        EventType::FsError,
    ];
    for et in &all_types {
        let s = et.to_string();
        let parsed: EventType = s.parse().unwrap();
        assert_eq!(*et, parsed, "round-trip failed for {:?}", et);
    }
}

#[test]
fn event_type_display_matches_serialized() {
    // Verify Display output matches serde serialization format (SCREAMING_SNAKE_CASE)
    assert_eq!(EventType::Create.to_string(), "CREATE");
    assert_eq!(EventType::Modify.to_string(), "MODIFY");
    assert_eq!(EventType::Delete.to_string(), "DELETE");
    assert_eq!(EventType::MovedFrom.to_string(), "MOVED_FROM");
}

#[test]
fn event_type_parse_case_insensitive() {
    assert_eq!("modify".parse::<EventType>().unwrap(), EventType::Modify);
    assert_eq!("CREATE".parse::<EventType>().unwrap(), EventType::Create);
    assert_eq!("delete".parse::<EventType>().unwrap(), EventType::Delete);
}

#[test]
fn event_type_parse_invalid_returns_err() {
    assert!("INVALID".parse::<EventType>().is_err());
    assert!("".parse::<EventType>().is_err());
}

#[test]
fn file_event_all_fields_present_in_json() {
    use chrono::Utc;
    let ev = FileEvent {
        time: Utc::now(),
        event_type: EventType::Create,
        path: std::path::PathBuf::from("/a/b"),
        pid: 99,
        comm: "test".into(),
        cmd: "test_cmd".into(),
        user: "test_user".into(),
        file_size: 42,
        ppid: 7,
        tgid: 99,
        chain: Vec::new(),
    };
    let json = ev.to_jsonl_string();
    assert!(json.contains("\"time\":\""));
    assert!(json.contains("\"event_type\":\"CREATE\""));
    assert!(json.contains("\"path\":\"/a/b\""));
    assert!(json.contains("\"pid\":99"));
    assert!(json.contains("\"cmd\":\"test_cmd\""));
    assert!(json.contains("\"user\":\"test_user\""));
    assert!(json.contains("\"file_size\":42"));
    assert!(json.contains("\"ppid\":7"));
    assert!(json.contains("\"tgid\":99"));
    assert!(json.contains("\"chain\":[]"));
}
