/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Native, unprivileged macOS telemetry. Read-only launchd and Mach-port counts
//! use bounded launchctl/top subprocesses on daemon workers.

#[cfg(any(target_os = "macos", test))]
mod counters;
#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "macos")]
mod native;
#[cfg(any(target_os = "macos", test))]
mod ports;
#[cfg(target_os = "macos")]
mod report;
#[cfg(target_os = "macos")]
mod sensors;
#[cfg(target_os = "macos")]
mod services;
#[cfg(target_os = "macos")]
mod smc;
#[cfg(target_os = "macos")]
pub use native::DataCacheTypes;
