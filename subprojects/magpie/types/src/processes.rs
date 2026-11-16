/* src/processes.proto
 *
 * Copyright 2025 Mission Center Developers
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::collections::HashMap;

include!(concat!(env!("OUT_DIR"), "/magpie.processes.rs"));

impl ProcessUsageStats {
    pub fn merge(&mut self, other: &Self) {
        self.cpu_usage += other.cpu_usage;
        self.memory_usage += other.memory_usage;
        self.swap_usage += other.swap_usage;
        self.disk_usage += other.disk_usage;
        self.network_usage += other.network_usage;
        self.gpu_usage += other.gpu_usage;
        self.gpu_memory_usage += other.gpu_memory_usage;
        // Legacy None means available; a group is unavailable if any member is.
        for (valid, other) in [
            (&mut self.cpu_valid, other.cpu_valid),
            (&mut self.memory_valid, other.memory_valid),
            (&mut self.swap_valid, other.swap_valid),
            (&mut self.disk_valid, other.disk_valid),
            (&mut self.network_valid, other.network_valid),
        ] {
            if *valid == Some(false) || other == Some(false) {
                *valid = Some(false);
            } else if other == Some(true) {
                *valid = Some(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_member_invalidates_group_without_changing_legacy_samples() {
        let mut aggregate = ProcessUsageStats::default();
        aggregate.merge(&ProcessUsageStats {
            cpu_usage: 2.,
            ..Default::default()
        });
        assert_eq!(aggregate.cpu_usage, 2.);
        assert_eq!(aggregate.cpu_valid, None);
        aggregate.merge(&ProcessUsageStats {
            cpu_usage: 3.,
            cpu_valid: Some(false),
            memory_valid: Some(true),
            ..Default::default()
        });
        assert_eq!(aggregate.cpu_valid, Some(false));
        assert_eq!(aggregate.memory_valid, Some(true));
        aggregate.merge(&ProcessUsageStats {
            cpu_valid: Some(true),
            ..Default::default()
        });
        assert_eq!(aggregate.cpu_valid, Some(false));
    }
}

impl AsRef<Process> for Process {
    fn as_ref(&self) -> &Process {
        self
    }
}

impl AsMut<Process> for Process {
    fn as_mut(&mut self) -> &mut Process {
        self
    }
}

impl Process {
    pub fn merged_usage_stats(&self, processes: &HashMap<u32, Process>) -> ProcessUsageStats {
        let mut usage_stats = ProcessUsageStats::default();
        for child_pid in &self.children {
            if let Some(child) = processes.get(child_pid) {
                let child_usage_stats = child.merged_usage_stats(processes);
                usage_stats.merge(&child_usage_stats);
            }
        }
        usage_stats.merge(&self.usage_stats);
        usage_stats
    }
}
