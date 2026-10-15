use std::io;

#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
#[allow(unsafe_code)]
pub(super) fn guardian_exited(pid: u32) -> io::Result<bool> {
    let pid = libc::id_t::try_from(pid).map_err(|_| io::Error::other("guardian PID is too large"))?;
    // SAFETY: zero initializes writable signal information; waitid fills it without reaping. ~keep
    let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: P_PID selects this owned guardian, and WNOWAIT preserves its PID against reuse. ~keep
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut status,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful waitid initialized the signal information, including a zero PID when still running. ~keep
    Ok(unsafe { status.si_pid() } != 0)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(super) fn guardian_exited(_pid: u32) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no authoritative browser group census on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn has_live_members(group: u32) -> io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if pid == group {
            continue;
        }
        let Some(stat) = read_stat(&entry.path().join("stat"))? else {
            continue;
        };
        let (state, found_group) = process_state_and_group(&stat)?;
        if found_group != group {
            continue;
        }
        if state != b'Z' && state != b'X' {
            return Ok(true);
        }
        // ~keep Linux can mark the main thread zombie before its other threads finish their writes.
        let tasks = match std::fs::read_dir(entry.path().join("task")) {
            Ok(tasks) => tasks,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for task in tasks {
            if let Some(stat) = read_stat(&task?.path().join("stat"))? {
                let (state, _) = process_state_and_group(&stat)?;
                if state != b'Z' && state != b'X' {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_stat(path: &std::path::Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_state_and_group(stat: &[u8]) -> io::Result<(u8, u32)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "incomplete process group record");
    let name_end = stat.iter().rposition(|byte| *byte == b')').ok_or_else(invalid)?;
    let mut fields = stat[name_end + 1..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    let state = fields
        .next()
        .and_then(|value| value.first().copied())
        .ok_or_else(invalid)?;
    let group = fields
        .nth(1)
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid)?;
    Ok((state, group))
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(super) fn has_live_members(group: u32) -> io::Result<bool> {
    let group = libc::pid_t::try_from(group).map_err(|_| io::Error::other("browser group ID is too large"))?;
    // ~keep The kernel's size is an estimate; a full buffer is incomplete and must never prove an empty group.
    // SAFETY: a null buffer asks libproc only for its required buffer size. ~keep
    let required = unsafe { libc::proc_listpgrppids(group, std::ptr::null_mut(), 0) };
    if required <= 0 {
        return Err(io::Error::last_os_error());
    }
    const GROUP_LIST_HEADROOM: usize = 16;
    // ~keep Unlike proc_listpids, proc_listpgrppids returns a PID count, not a byte count.
    let count =
        usize::try_from(required).map_err(|_| io::Error::other("invalid process group size"))? + GROUP_LIST_HEADROOM;
    let mut members = vec![0 as libc::pid_t; count];
    let bytes = i32::try_from(std::mem::size_of_val(members.as_slice()))
        .map_err(|_| io::Error::other("process group list is too large"))?;
    // SAFETY: members is aligned writable PID storage of exactly bytes bytes. ~keep
    let written = unsafe { libc::proc_listpgrppids(group, members.as_mut_ptr().cast(), bytes) };
    if written <= 0 || written as usize >= count {
        return Err(io::Error::other("incomplete browser group census"));
    }
    for &pid in &members[..written as usize] {
        if pid > 0 && pid != group && macos_member_alive(pid, group)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn macos_member_alive(pid: libc::pid_t, group: libc::pid_t) -> io::Result<bool> {
    // SAFETY: zero initializes the writable native process record. ~keep
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size =
        i32::try_from(std::mem::size_of_val(&info)).map_err(|_| io::Error::other("process record is too large"))?;
    // SAFETY: info is aligned native storage; arg 1 includes zombie records for an explicit status check. ~keep
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            1,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if written == size {
        return Ok(info.pbi_pgid == group as u32 && info.pbi_status != libc::SZOMB);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(false);
    }
    Err(error)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(super) fn has_live_members(_group: u32) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no authoritative browser group census on this platform",
    ))
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn a_process_name_with_parentheses_and_non_utf8_does_not_hide_its_group() {
        assert_eq!(
            process_state_and_group(b"12 (a)\xff) S 5 17 0").expect("process record"),
            (b'S', 17)
        );
        assert!(process_state_and_group(b"12 (incomplete) S 5").is_err());
    }
}
