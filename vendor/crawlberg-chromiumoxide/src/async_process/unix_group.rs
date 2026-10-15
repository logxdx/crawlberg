use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[path = "unix_group_drain.rs"]
mod drain;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const DROP_TIMEOUT: Duration = Duration::from_secs(2);
const OWNER_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
// ~keep Poll the guardian's current PPID: a fork can retain pipe descriptors after its parent dies.
// ~keep Foreground probes/timers are reaped before the guardian kills its held group; no cached
// ~keep auxiliary PID is signalled after a shell could have reaped and recycled it.
#[cfg(any(not(target_os = "linux"), test))]
const GUARDIAN_SCRIPT: &str = "trap 'kill -s KILL -- -$$' TERM; printf .; \
    while owner=$(/bin/ps -o ppid= -p \"$$\") && [ \"$owner\" -eq \"$1\" ]; \
    do /bin/sleep 1 || break; done; kill -s KILL -- -$$";

// ~keep Linux procfs provides the live parent without requiring procps or BusyBox ps options.
#[cfg(target_os = "linux")]
const LINUX_GUARDIAN_SCRIPT: &str = "trap 'kill -9 -$$' TERM; printf .; \
    while owner= && [ -r /proc/$$/status ]; do \
    while read -r key value rest; do case \"$key\" in PPid:) owner=$value; break;; esac; \
    done < /proc/$$/status; [ \"$owner\" = \"$1\" ] || break; \
    /bin/sleep 1 || break; done; kill -9 -$$";

#[derive(Debug)]
pub(super) struct Group {
    owner: u32,
    id: u32,
    child: Mutex<Option<Child>>,
}

impl Group {
    pub(super) fn start() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let group = Self::start_script(LINUX_GUARDIAN_SCRIPT)?;
            validate_timer("/bin/sleep")?;
            let status = std::fs::read_to_string(format!("/proc/{}/status", group.id))?;
            if status_parent(&status) != Some(group.owner) {
                return Err(io::Error::other(
                    "guardian procfs parent did not identify this process as its owner",
                ));
            }
            Ok(group)
        }
        #[cfg(not(target_os = "linux"))]
        Self::start_with_tools("/bin/ps", "/bin/sleep")
    }

    #[cfg(any(not(target_os = "linux"), test))]
    pub(super) fn start_with_tools(probe: &str, timer: &str) -> io::Result<Self> {
        let group = Self::start_script(GUARDIAN_SCRIPT)?;
        validate_timer(timer).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not start browser guardian timer using {timer}: {error}"),
            )
        })?;
        validate_owner(probe, group.id, group.owner).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not establish browser guardian ownership using {probe}: {error}"),
            )
        })?;
        Ok(group)
    }

    fn start_script(script: &str) -> io::Result<Self> {
        // ~keep The guardian starts first and retains its own group ID against reuse.
        // ~keep The acknowledgement socket closes before Chrome starts; no ownership pipe survives a fork.
        let (mut ready, child_ready) = std::os::unix::net::UnixStream::pair()?;
        ready.set_read_timeout(Some(OWNER_PROBE_TIMEOUT))?;
        let child = Command::new("/bin/sh")
            .args([
                "-c",
                script,
                "crawlberg-browser-guardian",
                &std::process::id().to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(std::os::fd::OwnedFd::from(child_ready)))
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()?;
        let id = child.id();
        let group = Self {
            owner: std::process::id(),
            id,
            child: Mutex::new(Some(child)),
        };
        acknowledge_ready(&mut ready)?;
        Ok(group)
    }

    pub(super) fn id(&self) -> u32 {
        self.id
    }

    pub(super) fn signal(&self) -> io::Result<()> {
        if self.owner != std::process::id() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "browser group belongs to another process",
            ));
        }
        let child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if child.is_none() {
            return Ok(());
        }
        signal_group(self.id, libc::SIGTERM)
    }

    pub(super) fn stop(&self, limit: Duration) -> bool {
        if self.owner != std::process::id() {
            return false;
        }
        let deadline = Instant::now() + limit;
        let mut child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if child.is_none() {
            return true;
        }
        if drain::guardian_exited(self.id).is_err() || signal_group(self.id, libc::SIGTERM).is_err() {
            return false;
        }
        let mut killed_remaining = false;
        loop {
            match drain::guardian_exited(self.id) {
                Ok(true) => {
                    // ~keep WNOWAIT retains the guardian's PID: no group signal can reach a reused group ID.
                    if !killed_remaining {
                        if signal_group(self.id, libc::SIGKILL).is_err() {
                            return false;
                        }
                        killed_remaining = true;
                    }
                    match drain::has_live_members(self.id) {
                        Ok(false) => {
                            let reaped = child.as_mut().is_some_and(|guardian| guardian.wait().is_ok());
                            if reaped {
                                child.take();
                            }
                            return reaped;
                        }
                        Err(_) | Ok(true) => {}
                    }
                }
                Err(_) => return false,
                Ok(false) => {}
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.owner == std::process::id() && !self.stop(DROP_TIMEOUT) {
            tracing::warn!(
                group = self.id,
                "browser guardian did not stop within the cleanup deadline"
            );
        }
    }
}

#[allow(unsafe_code)]
fn signal_group(id: u32, signal: i32) -> io::Result<()> {
    let id = i32::try_from(id).map_err(|_| io::Error::other("browser group ID is too large"))?;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: the mutex retains the unreaped guardian, so this group ID cannot be recycled.
    // ~keep TERM lets the guardian reap its foreground helper before its KILL trap.
    if unsafe { kill(-id, signal) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(any(not(target_os = "linux"), test))]
fn validate_owner(probe: &str, guardian: u32, expected: u32) -> io::Result<()> {
    use std::io::Read;
    let mut child = Command::new(probe)
        .args(["-o", "ppid=", "-p", &guardian.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let status = wait_probe(&mut child)?;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output)?;
    }
    if !status.success() || output.trim().parse::<u32>() != Ok(expected) {
        return Err(io::Error::other(
            "guardian PPID probe did not identify this process as its owner",
        ));
    }
    Ok(())
}

fn acknowledge_ready(ready: &mut std::os::unix::net::UnixStream) -> io::Result<()> {
    use std::io::Read;
    let mut acknowledgement = [0u8; 1];
    ready.read_exact(&mut acknowledgement).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("browser guardian did not acknowledge startup within its deadline: {error}"),
        )
    })?;
    if acknowledgement != [b'.'] {
        return Err(io::Error::other("invalid browser guardian startup acknowledgement"));
    }
    Ok(())
}

fn validate_timer(timer: &str) -> io::Result<()> {
    let mut child = Command::new(timer)
        .arg("0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if !wait_probe(&mut child)?.success() {
        return Err(io::Error::other("guardian timer refused its interval"));
    }
    Ok(())
}

fn wait_probe(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + OWNER_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "guardian PPID probe timed out"));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn status_parent(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse().ok())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    fn the_group_census_sees_a_live_member_and_excludes_its_guardian_and_bystander() {
        let group = Group::start_script("trap 'exit 0' TERM; printf .; while :; do :; done")
            .expect("guardian with no helper processes");
        let mut bystander = Command::new("/bin/sleep").arg("300").spawn().expect("unrelated process");
        let initially_empty = !drain::has_live_members(group.id()).expect("initial group census");
        let mut member = Command::new("/bin/sleep")
            .arg("300")
            .process_group(i32::try_from(group.id()).expect("group fits pid"))
            .spawn()
            .expect("owned group member");
        let member_visible = drain::has_live_members(group.id()).expect("live member census");
        member.kill().expect("stop owned member");
        member.wait().expect("reap owned member");
        let finally_empty = !drain::has_live_members(group.id()).expect("ended member census");
        let stopped = group.stop(DROP_TIMEOUT);
        let bystander_running = bystander.try_wait().expect("bystander status").is_none();
        let _ = bystander.kill();
        let _ = bystander.wait();

        assert!(initially_empty, "the guardian and unrelated process do not count as live group members");
        assert!(member_visible, "the census must actually inspect a live owned member");
        assert!(finally_empty, "a reaped member must leave no live group member");
        assert!(stopped, "the guardian-only group must stop");
        assert!(bystander_running, "the group stop must preserve unrelated processes");
    }

    #[test]
    fn a_guardian_exit_does_not_report_a_live_member_stopped() {
        let group = Group::start_script("trap 'exit 0' TERM; printf .; /bin/sleep 300")
            .expect("guardian that exits without killing its group");
        let (mut ready, member_ready) = std::os::unix::net::UnixStream::pair().expect("member readiness socket");
        ready.set_read_timeout(Some(OWNER_PROBE_TIMEOUT)).expect("readiness timeout");
        let mut member = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; printf .; exec /bin/sleep 300"])
            .stdout(std::process::Stdio::from(std::os::fd::OwnedFd::from(member_ready)))
            .process_group(i32::try_from(group.id()).expect("group fits pid"))
            .spawn()
            .expect("member that ignores TERM");
        acknowledge_ready(&mut ready).expect("member must install its TERM handler before stop");
        let mut bystander = Command::new("/bin/sleep").arg("300").spawn().expect("unrelated process");

        let stopped = group.stop(DROP_TIMEOUT);
        let member_ended = member.try_wait().expect("member status").is_some();
        let bystander_running = bystander.try_wait().expect("bystander status").is_none();
        let _ = member.kill();
        let _ = member.wait();
        let _ = bystander.kill();
        let _ = bystander.wait();

        assert!(stopped, "the owner must finish stopping its group");
        assert!(member_ended, "the guardian's exit alone must not report its live member stopped");
        assert!(bystander_running, "stopping the group must preserve unrelated processes");
    }

    #[test]
    fn an_inherited_group_refuses_signal_and_stop_without_touching_its_owner() {
        let mut group = Group::start().expect("guardian");
        let mut leaf = Command::new("/bin/sleep")
            .arg("30")
            .process_group(i32::try_from(group.id()).expect("group fits pid"))
            .spawn()
            .expect("group member");
        let owner = group.owner;
        group.owner = owner.wrapping_add(1);
        let refused = group.signal().expect_err("foreign process cannot signal this group");
        let stopped = group.stop(Duration::from_millis(100));
        let untouched = leaf.try_wait().expect("leaf status").is_none();
        group.owner = owner;
        assert!(group.stop(DROP_TIMEOUT), "actual owner must clean its group");
        leaf.wait().expect("reap group member");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!stopped, "foreign stop must refuse ownership");
        assert!(untouched, "neither foreign operation may signal the owner's member");
    }
}
