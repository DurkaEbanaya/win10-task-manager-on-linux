// Copyright 2026 Mission Center Developers
// SPDX-License-Identifier: GPL-3.0-or-later

//! Count occupied Mach IPC names using the Apple-signed top reader. Never a
//! Windows handle count; a truncated/denied/malformed sample is unavailable.

fn parse(output: &str) -> Option<u64> {
    let mut lines = output.lines();
    let reported = lines
        .next()?
        .strip_prefix("Processes: ")?
        .split_once(" total,")?
        .0
        .parse::<usize>()
        .ok()?;
    if reported == 0 || reported > 100_000 {
        return None;
    }
    let mut header = false;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for line in lines {
        let columns: Vec<_> = line.split_whitespace().collect();
        if !header {
            if columns == ["PID", "#PORTS"] {
                header = true;
            }
            continue;
        }
        if columns.is_empty() {
            continue;
        }
        if columns.len() != 2 {
            return None;
        }
        let pid = columns[0].parse::<u32>().ok()?;
        let value = columns[1].strip_suffix(['+', '-']).unwrap_or(columns[1]);
        let ports = value.parse::<u64>().ok()?;
        // top reports kernel_task as PID 0 with 0 ports; do not count inaccessible
        // userspace tasks as idle. This intentionally prefers unavailable.
        if !seen.insert(pid) || (pid != 0 && ports == 0) {
            return None;
        }
        total = total.checked_add(ports)?;
    }
    (header && seen.len() == reported).then_some(total)
}

#[cfg(target_os = "macos")]
pub struct Ports {
    last: Option<std::time::Instant>,
    value: Option<u64>,
}

#[cfg(target_os = "macos")]
impl Ports {
    pub fn new() -> Self {
        Self {
            last: None,
            value: None,
        }
    }

    pub fn sample(&mut self) -> Option<u64> {
        use std::time::{Duration, Instant};
        if self
            .last
            .is_none_or(|last| last.elapsed() >= Duration::from_secs(5))
        {
            self.value = read();
            self.last = Some(Instant::now());
        }
        self.value
    }
}

#[cfg(target_os = "macos")]
fn read() -> Option<u64> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("/usr/bin/top")
        .args(["-l", "1", "-n", "100000", "-R", "-F", "-stats", "pid,ports"])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Always reap, including failures and timeout. Never block a UI thread.
    let result = (|| {
        let mut pipe = child.stdout.take()?;
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = vec![];
        let mut buffer = [0u8; 8192];
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if bytes.len() + n > 4 * 1024 * 1024 {
                        return None;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
        loop {
            match child.try_wait().ok()? {
                Some(status) => {
                    return if status.success() {
                        parse(std::str::from_utf8(&bytes).ok()?)
                    } else {
                        None
                    }
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => return None,
            }
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_counts_include_kernel_zero_and_top_change_markers() {
        assert_eq!(
            parse("Processes: 3 total, 1 running\nCPU usage: 1%\nPID #PORTS\n0 0\n1 100+\n7 25-\n"),
            Some(125)
        );
    }
    #[test]
    fn incomplete_denied_duplicate_and_overflow_counts_are_unknown() {
        for text in [
            "Processes: 2 total, 1 running\nPID #PORTS\n1 5\n",
            "Processes: 1 total, 1 running\nPID #PORTS\n1 0\n",
            "Processes: 1 total, 1 running\nPID #PORTS\n1 N/A\n",
            "Processes: 2 total, 1 running\nPID #PORTS\n1 5\n1 6\n",
            "Processes: 2 total, 1 running\nPID #PORTS\n1 18446744073709551615\n2 1\n",
            "Processes: 1 total, 1 running\nPID #PORTS\n1 5 extra\n",
            "Processes: 1 total, 1 running\n1 5\n",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
