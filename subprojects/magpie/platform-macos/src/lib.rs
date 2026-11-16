/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

//! Native, unprivileged macOS telemetry. Only the read-only launchd inventory
//! uses a bounded launchctl subprocess on a daemon worker.

#[cfg(any(target_os = "macos", test))]
mod counters;
#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
mod services;
#[cfg(target_os = "macos")]
pub use native::DataCacheTypes;
