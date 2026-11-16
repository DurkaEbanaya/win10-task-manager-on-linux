// Copyright 2026 Mission Center Developers
// SPDX-License-Identifier: GPL-3.0-or-later

//! Offline reference facts, never measurements or replacements for native counters.
//! Reviewed 2026-11-16; sources and limitations in docs/hardware-monitoring.md.

/// Exact identity only. Never apply base-M5 facts to Pro/Max/Ultra, a VM, or a
/// similarly named GPU. Native values stay separate and retain precedence.
pub fn is_base_m5(cpu: &mc_magpie::magpie_types::cpu::Cpu) -> bool {
    cpu.darwin_cpu == Some(true)
        && cpu.is_virtual_machine == Some(false)
        && cpu.name.as_deref().map(str::trim) == Some("Apple M5")
}

/// Published approximate SLC capacity, not CPU L3 or a native measurement.
/// Source/method limitations are recorded in docs/hardware-monitoring.md.
pub fn slc_estimate_bytes(cpu: &mc_magpie::magpie_types::cpu::Cpu) -> Option<u64> {
    is_base_m5(cpu).then_some(32 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_requires_exact_native_identity_and_non_guest() {
        let mut cpu = mc_magpie::magpie_types::cpu::Cpu {
            name: Some("Apple M5".into()),
            darwin_cpu: Some(true),
            is_virtual_machine: Some(false),
            ..Default::default()
        };
        assert!(is_base_m5(&cpu));
        assert_eq!(slc_estimate_bytes(&cpu), Some(32 * 1024 * 1024));
        for name in [
            "Apple M5 Pro",
            "Apple M5 Max",
            "Apple M5 Ultra",
            "Apple M6",
            "M5",
        ] {
            cpu.name = Some(name.into());
            assert!(!is_base_m5(&cpu));
            assert_eq!(slc_estimate_bytes(&cpu), None);
        }
        cpu.name = Some("Apple M5".into());
        cpu.is_virtual_machine = None;
        assert!(!is_base_m5(&cpu));
        cpu.is_virtual_machine = Some(true);
        assert!(!is_base_m5(&cpu));
        cpu.is_virtual_machine = Some(false);
        cpu.darwin_cpu = None;
        assert!(!is_base_m5(&cpu));
        assert_eq!(cpu.base_freq_khz, None);
        assert_eq!(cpu.current_frequency_mhz, 0);
    }
}
