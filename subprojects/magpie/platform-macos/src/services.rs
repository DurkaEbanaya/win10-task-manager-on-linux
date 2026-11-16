/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Read-only jobs visible to the daemon's launchd bootstrap namespace.
//! Do not confuse this inventory with all system launch daemons, and do not
//! map systemd enable/start semantics to launchd without a separate contract.
use magpie_platform::services::{Service, ServiceCache};
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Services {
    user: HashMap<u64, Service>,
    system: HashMap<u64, Service>,
}
impl ServiceCache for Services {
    fn new() -> Self {
        Self {
            user: HashMap::new(),
            system: HashMap::new(),
        }
    }
    fn refresh(&mut self) {
        let Ok(mut child) = Command::new("/bin/launchctl")
            .arg("list")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            self.user.clear();
            return;
        };
        let mut bytes = vec![];
        let mut stdout = child.stdout.take().expect("piped stdout");
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        let nonblocking =
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0u8; 8192];
        let complete = loop {
            if !nonblocking || Instant::now() >= deadline || bytes.len() > 2 * 1024 * 1024 {
                break false;
            }
            match stdout.read(&mut buffer) {
                Ok(0) => break true,
                Ok(len) => bytes.extend_from_slice(&buffer[..len]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(_) => break false,
            }
        };
        if !complete {
            let _ = child.kill();
            let _ = child.wait();
            self.user.clear();
            return;
        }
        let success = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        if !success {
            self.user.clear();
            return;
        }
        self.user = String::from_utf8_lossy(&bytes)
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut columns = line.splitn(3, '\t');
                let pid = columns.next()?.trim().parse::<u32>().ok();
                let exit = columns.next()?.trim().parse::<i32>().ok()?;
                let name = columns.next()?.trim();
                if name.is_empty() {
                    return None;
                }
                // Stable FNV identity; never use an untrusted label as shell input.
                let id = name.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
                    (hash ^ byte as u64).wrapping_mul(0x100000001b3)
                });
                Some((
                    id,
                    Service {
                        id,
                        name: name.into(),
                        description: Some(
                            "launchd · current bootstrap namespace (read-only)".into(),
                        ),
                        enabled: true,
                        running: pid.is_some(),
                        failed: pid.is_none() && exit != 0,
                        pid,
                        group: Some("launchd".into()),
                        actions_supported: Some(false),
                        enabled_valid: Some(false),
                        ..Default::default()
                    },
                ))
            })
            .collect();
    }
    fn user_entries(&self) -> &HashMap<u64, Service> {
        &self.user
    }
    fn system_entries(&self) -> &HashMap<u64, Service> {
        &self.system
    }
}
