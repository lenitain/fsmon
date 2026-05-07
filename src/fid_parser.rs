use fanotify::low_level::{
    FAN_ACCESS, FAN_ATTRIB, FAN_CLOSE_NOWRITE, FAN_CLOSE_WRITE, FAN_CREATE, FAN_DELETE,
    FAN_DELETE_SELF, FAN_MODIFY, FAN_MOVE_SELF, FAN_MOVED_FROM, FAN_MOVED_TO, FAN_OPEN,
    FAN_OPEN_EXEC,
};
use dashmap::DashMap;
use smallvec::SmallVec;
use std::fs;
use std::path::PathBuf;

use crate::EventType;

/// Handle key type: fsid + file_handle bytes, stack-allocated if ≤128 bytes
pub type HandleKey = SmallVec<[u8; 128]>;

// ---- FID event parsing required kernel structures and constants ----

/// fanotify_event_info_header.info_type
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;



/// fanotify_event_metadata (matches kernel structure)
#[repr(C)]
pub struct FanMetadata {
    event_len: u32,
    vers: u8,
    reserved: u8,
    metadata_len: u16,
    pub mask: u64,
    pub fd: i32,
    pub pid: i32,
}

/// fanotify_event_info_header
#[repr(C)]
struct FanInfoHeader {
    info_type: u8,
    pad: u8,
    len: u16,
}

pub const META_SIZE: usize = std::mem::size_of::<FanMetadata>();
const INFO_HDR_SIZE: usize = std::mem::size_of::<FanInfoHeader>();
const FSID_SIZE: usize = 8; // __kernel_fsid_t = { i32 val[2]; }
const FH_HDR_SIZE: usize = 8; // file_handle: handle_bytes(u32) + handle_type(i32)

/// Event parsed from FID buffer
pub struct FidEvent {
    pub mask: u64,
    pub pid: i32,
    pub path: PathBuf,
    /// DFID_NAME directory handle key (fsid + file_handle), for cache lookup
    pub dfid_name_handle: Option<HandleKey>,
    /// DFID_NAME filename
    pub dfid_name_filename: Option<String>,
    /// Self handle key from DFID/FID record (fsid + file_handle), for cache building
    pub self_handle: Option<HandleKey>,
}

// ---- Event type mapping (1:1 with fanotify event bits) ----

const EVENT_BITS: [(u64, EventType); 13] = [
    (FAN_ACCESS, EventType::Access),
    (FAN_MODIFY, EventType::Modify),
    (FAN_CLOSE_WRITE, EventType::CloseWrite),
    (FAN_CLOSE_NOWRITE, EventType::CloseNowrite),
    (FAN_OPEN, EventType::Open),
    (FAN_OPEN_EXEC, EventType::OpenExec),
    (FAN_ATTRIB, EventType::Attrib),
    (FAN_CREATE, EventType::Create),
    (FAN_DELETE, EventType::Delete),
    (FAN_DELETE_SELF, EventType::DeleteSelf),
    (FAN_MOVED_FROM, EventType::MovedFrom),
    (FAN_MOVED_TO, EventType::MovedTo),
    (FAN_MOVE_SELF, EventType::MoveSelf),
];

pub fn mask_to_event_types(mask: u64) -> SmallVec<[EventType; 8]> {
    EVENT_BITS
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, event_type)| *event_type)
        .collect()
}

// ---- FID event reading and parsing ----

/// Read and parse FID format events from fanotify fd
///
/// Uses two-pass processing + persistent cache:
/// 1. First pass: Parse all events, try to resolve file handles
/// 2. Second pass: Use persistent cache to recover child file paths for events that failed due to directory deletion
/// 3. Update newly resolved directory info to persistent cache
pub fn read_fid_events(
    fan_fd: i32,
    mount_fds: &[i32],
    dir_cache: &DashMap<HandleKey, PathBuf>,
    buf: &mut Vec<u8>,
) -> Vec<FidEvent> {
    let n = unsafe { libc::read(fan_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };

    if n <= 0 {
        return vec![];
    }

    let n = n as usize;
    let mut events = Vec::new();
    let mut offset = 0;

    // ---- First pass: Parse events and extract handle data ----

    while offset + META_SIZE <= n {
        let meta = unsafe { &*(buf.as_ptr().add(offset) as *const FanMetadata) };
        let event_len = meta.event_len as usize;

        if event_len < META_SIZE || offset + event_len > n {
            break;
        }

        let mut path = PathBuf::new();
        let mut dfid_name_handle: Option<HandleKey> = None;
        let mut dfid_name_filename: Option<String> = None;
        let mut self_handle: Option<HandleKey> = None;

        let mut info_off = offset + meta.metadata_len as usize;
        let event_end = offset + event_len;

        while info_off + INFO_HDR_SIZE <= event_end {
            let hdr = unsafe { &*(buf.as_ptr().add(info_off) as *const FanInfoHeader) };
            let info_len = hdr.len as usize;

            if info_len < INFO_HDR_SIZE || info_off + info_len > event_end {
                break;
            }

            match hdr.info_type {
                FAN_EVENT_INFO_TYPE_DFID_NAME => {
                    if let Some((key, filename, resolved)) =
                        extract_dfid_name(buf, info_off, info_len, mount_fds)
                    {
                        dfid_name_handle = Some(key);
                        dfid_name_filename = Some(filename);
                        if let Some(p) = resolved {
                            path = p;
                        }
                    }
                }
                FAN_EVENT_INFO_TYPE_FID | FAN_EVENT_INFO_TYPE_DFID => {
                    if let Some((key, resolved)) = extract_fid(buf, info_off, info_len, mount_fds) {
                        self_handle = Some(key);
                        if path.as_os_str().is_empty()
                            && let Some(p) = resolved
                        {
                            path = p;
                        }
                    }
                }
                _ => {}
            }

            info_off += info_len;
        }

        // In FID mode, fd should be -1, but defensively close it
        if meta.fd >= 0 {
            let _ = nix::unistd::close(meta.fd);
        }

        events.push(FidEvent {
            mask: meta.mask,
            pid: meta.pid,
            path,
            dfid_name_handle,
            dfid_name_filename,
            self_handle,
        });

        offset += event_len;
    }

    // ---- Second pass: Use persistent cache to recover child file paths for deleted directories ----
    // First update cache from successfully resolved events in this batch, then use cache to recover failed events
    // Iterate until no new paths are resolved (handles multi-level nested deletion)

    loop {
        // Update persistent cache from successfully resolved events
        for ev in events.iter() {
            if ev.path.as_os_str().is_empty() {
                continue;
            }

            // Cache self handle → path
            if let Some(ref key) = ev.self_handle {
                dir_cache
                    .entry(key.clone())
                    .or_insert_with(|| ev.path.clone());
            }

            // Cache DFID_NAME directory handle → directory path
            if let (Some(key), Some(filename)) = (&ev.dfid_name_handle, &ev.dfid_name_filename) {
                let dir_path = if !filename.is_empty() {
                    ev.path.parent().map(|p| p.to_path_buf())
                } else {
                    Some(ev.path.clone())
                };
                if let Some(dp) = dir_path {
                    dir_cache.entry(key.clone()).or_insert(dp);
                }
            }
        }

        // Try to recover empty path events using cache
        let mut made_progress = false;
        for ev in events.iter_mut() {
            if !ev.path.as_os_str().is_empty() {
                continue;
            }
            if let (Some(key), Some(filename)) = (&ev.dfid_name_handle, &ev.dfid_name_filename)
                && let Some(dir_path) = dir_cache.get(key)
            {
                ev.path = if filename.is_empty() {
                    dir_path.clone()
                } else {
                    dir_path.join(filename)
                };
                made_progress = true;
            }
            if ev.path.as_os_str().is_empty()
                && let Some(ref key) = ev.self_handle
                && let Some(cached_path) = dir_cache.get(key)
            {
                ev.path = cached_path.clone();
                made_progress = true;
            }
        }

        if !made_progress {
            break;
        }
    }

    events
}

/// Parse DFID_NAME info record: extract directory handle key, filename, and try to resolve path
///
/// Returns (handle_key, filename, resolved_path)
/// handle_key = fsid + file_handle bytes, uniquely identifies a directory
/// Even if open_by_handle_at fails (directory deleted), still returns handle_key and filename
///
/// Memory layout: InfoHeader(4) | fsid(8) | file_handle(8+N) | filename(null-terminated, padded)
fn extract_dfid_name(
    buf: &[u8],
    info_off: usize,
    info_len: usize,
    mount_fds: &[i32],
) -> Option<(HandleKey, String, Option<PathBuf>)> {
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[fh_off..fh_off + 4].try_into().ok()?) as usize;
    let fh_total = FH_HDR_SIZE + handle_bytes;
    let name_off = fh_off + fh_total;

    if name_off > record_end {
        return None;
    }

    // Extract null-terminated filename
    let name_bytes = &buf[name_off..record_end];
    let name = name_bytes.split(|&b| b == 0).next().unwrap_or(&[]);
    let filename = std::str::from_utf8(name).ok()?.to_string();

    // Cache key: file_handle bytes (uniquely identifies the directory inode within the same filesystem)
    let key = HandleKey::from_slice(&buf[fh_off..fh_off + fh_total]);

    // Try to resolve directory handle
    let dir_path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);
    let full_path = dir_path.map(|dp| {
        if filename.is_empty() {
            dp
        } else {
            dp.join(&filename)
        }
    });

    Some((key, filename, full_path))
}

/// Parse FID/DFID info record: extract self handle key and try to resolve path
///
/// Returns (handle_key, resolved_path)
///
/// Memory layout: InfoHeader(4) | fsid(8) | file_handle(8+N)
fn extract_fid(
    buf: &[u8],
    info_off: usize,
    info_len: usize,
    mount_fds: &[i32],
) -> Option<(HandleKey, Option<PathBuf>)> {
    let fsid_off = info_off + INFO_HDR_SIZE;
    let fh_off = fsid_off + FSID_SIZE;
    let record_end = info_off + info_len;

    if fh_off + FH_HDR_SIZE > record_end {
        return None;
    }

    let handle_bytes = u32::from_ne_bytes(buf[fh_off..fh_off + 4].try_into().ok()?) as usize;
    let fh_total = FH_HDR_SIZE + handle_bytes;

    if fh_off + fh_total > record_end {
        return None;
    }

    let key = HandleKey::from_slice(&buf[fh_off..fh_off + fh_total]);
    let path = resolve_file_handle(mount_fds, &buf[fh_off..fh_off + fh_total]);

    Some((key, path))
}

/// Resolve kernel file handle to path via open_by_handle_at
pub fn resolve_file_handle(mount_fds: &[i32], fh_data: &[u8]) -> Option<PathBuf> {
    if fh_data.len() < FH_HDR_SIZE {
        return None;
    }

    for &mfd in mount_fds {
        let fd = unsafe {
            libc::open_by_handle_at(
                mfd,
                fh_data.as_ptr() as *mut libc::file_handle,
                libc::O_PATH,
            )
        };

        if fd >= 0 {
            let result = fs::read_link(format!("/proc/self/fd/{}", fd));
            let _ = nix::unistd::close(fd);
            if let Ok(p) = result {
                return Some(p);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use fanotify::low_level::{FAN_CREATE, FAN_DELETE, FAN_EVENT_ON_CHILD, FAN_MODIFY, FAN_ONDIR};

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
            | FAN_MOVED_FROM
            | FAN_MOVED_TO
            | FAN_MOVE_SELF
;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 13);
    }

    #[test]
    fn test_mask_to_event_types_with_flags() {
        // FAN_EVENT_ON_CHILD and FAN_ONDIR are flags, not event types
        let mask = FAN_CREATE | FAN_EVENT_ON_CHILD | FAN_ONDIR;
        let types = mask_to_event_types(mask);
        assert_eq!(types.len(), 1);
        assert_eq!(types[0], EventType::Create);
    }
}
