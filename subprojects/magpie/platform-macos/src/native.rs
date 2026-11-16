/* Copyright 2026 Mission Center Developers
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::{size_of, MaybeUninit};
use std::time::Instant;

use mach2::{mach_init::mach_host_self, mach_port::mach_port_deallocate, traps::mach_task_self};
use magpie_platform::processes::Process;
use magpie_platform::{
    about::*, apps::*, battery::*, cpu::*, disks::*, fan::*, gpus::*, memory::*, network::*,
    processes::*, services::*, Mutex,
};

use crate::counters::{cpu_delta, rate};

pub struct DataCacheTypes;
impl magpie_platform::DataCacheTypes for DataCacheTypes {
    type AboutCache = AboutCollector;
    type AppCache = AppsCollector;
    type BatteryCache = BatteryCollector;
    type CpuCache = CpuCollector;
    type DisksCache = DisksCollector;
    type DisksManager = Unavailable;
    type FanCache = Unavailable;
    type GpuCache = GpuCollector;
    type MemoryCache = MemoryCollector;
    type NetworkCache = NetworkCollector;
    type ProcessCache = ProcessCollector;
    type ProcessManager = ProcessActions;
    type ServiceCache = crate::services::Services;
    type ServicesManager = Unavailable;
}

fn sysctl<T: Copy>(name: &CStr) -> Option<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let mut len = size_of::<T>();
    // SAFETY: kernel writes at most len bytes to aligned storage of exactly that size.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && len == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn sysctl_string(name: &CStr) -> Option<String> {
    let mut bytes = vec![0u8; 4096];
    let mut len = bytes.len();
    // SAFETY: writable byte buffer; sysctl receives its actual capacity.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || len > bytes.len() {
        return None;
    }
    bytes.truncate(len);
    Some(
        String::from_utf8_lossy(&bytes)
            .trim_end_matches('\0')
            .to_owned(),
    )
}

fn pid_info<T>(pid: u32, flavor: i32) -> Option<T> {
    let pid = i32::try_from(pid).ok()?;
    let mut value = MaybeUninit::<T>::zeroed();
    // SAFETY: callers pair a libproc flavor with its documented struct. A short
    // or denied read is not treated as initialized telemetry.
    let len = unsafe {
        libc::proc_pidinfo(
            pid,
            flavor,
            0,
            value.as_mut_ptr().cast(),
            size_of::<T>() as i32,
        )
    };
    (len == size_of::<T>() as i32).then(|| unsafe { value.assume_init() })
}

fn pids() -> Option<Vec<u32>> {
    // SAFETY: a null buffer asks libproc for the current number of processes.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return None;
    }
    // Allow process creation during enumeration, with a strict allocation bound.
    let mut buffer = vec![0i32; (count as usize + 1024).min(1_000_000)];
    let len = unsafe {
        libc::proc_listallpids(
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(buffer.as_slice()) as i32,
        )
    };
    if len < 0 || len as usize > buffer.len() {
        return None;
    }
    buffer.truncate(len as usize);
    Some(
        buffer
            .into_iter()
            .filter_map(|pid| u32::try_from(pid).ok().filter(|pid| *pid > 0))
            .collect(),
    )
}

fn cchars(bytes: &[libc::c_char]) -> String {
    let bytes: Vec<_> = bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn performance_levels() -> Vec<PerformanceLevel> {
    // Aida128 demonstrated these runtime sysctls. Keep aggregate groups separate
    // from LogicalCpu: their order does not establish Mach processor ID affinity.
    (0..16)
        .filter_map(|index| {
            let read_name =
                |field| std::ffi::CString::new(format!("hw.perflevel{index}.{field}")).unwrap();
            let physical = sysctl::<u32>(&read_name("physicalcpu")).filter(|v| *v > 0)?;
            Some(PerformanceLevel {
                index,
                name: sysctl_string(&read_name("name")).filter(|v| !v.is_empty()),
                physical_core_count: physical,
                logical_cpu_count: sysctl::<u32>(&read_name("logicalcpu")).filter(|v| *v > 0),
                l1_instruction_bytes: sysctl::<u64>(&read_name("l1icachesize")).filter(|v| *v > 0),
                l1_data_bytes: sysctl::<u64>(&read_name("l1dcachesize")).filter(|v| *v > 0),
                l2_bytes: sysctl::<u64>(&read_name("l2cachesize")).filter(|v| *v > 0),
            })
        })
        .collect()
}

pub struct CpuCollector {
    data: Cpu,
    previous: Vec<[u32; 4]>,
}
impl CpuCache for CpuCollector {
    fn new() -> Self {
        Self {
            data: Cpu {
                name: sysctl_string(c"machdep.cpu.brand_string"),
                base_freq_khz: sysctl::<u64>(c"hw.cpufrequency").map(|v| v / 1000),
                l1_combined_cache_bytes: sysctl::<u64>(c"hw.l1icachesize")
                    .zip(sysctl::<u64>(c"hw.l1dcachesize"))
                    .map(|(a, b)| a + b),
                l2_cache_bytes: sysctl(c"hw.l2cachesize"),
                l3_cache_bytes: sysctl(c"hw.l3cachesize"),
                frequency_valid: Some(false),
                usage_valid: Some(false),
                process_count_valid: Some(false),
                thread_count_valid: Some(false),
                handle_count_valid: Some(false),
                physical_core_count: sysctl::<u32>(c"hw.physicalcpu").filter(|v| *v > 0),
                performance_levels: performance_levels(),
                ..Default::default()
            },
            previous: vec![],
        }
    }
    fn refresh(&mut self) {
        self.data.usage_valid = Some(false);
        let mut processors = 0;
        let mut info = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: Mach returns a VM-allocated integer array, released below.
        unsafe {
            let host = mach_host_self();
            let result = libc::host_processor_info(
                host,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut processors,
                &mut info,
                &mut count,
            );
            mach_port_deallocate(mach_task_self(), host);
            if result == libc::KERN_SUCCESS && !info.is_null() {
                let values = std::slice::from_raw_parts(info, count as usize);
                if count as usize == processors as usize * 4 {
                    let current: Vec<[u32; 4]> = values
                        .chunks_exact(4)
                        .map(|v| [v[0] as u32, v[1] as u32, v[2] as u32, v[3] as u32])
                        .collect();
                    let samples: Option<Vec<_>> = (current.len() == self.previous.len())
                        .then(|| {
                            self.previous
                                .iter()
                                .zip(&current)
                                .map(|(a, b)| cpu_delta(*a, *b))
                                .collect()
                        })
                        .flatten();
                    self.data.core_usage_percent = vec![0.; current.len()];
                    self.data.core_kernel_usage_percent = vec![0.; current.len()];
                    if let Some(samples) = samples.filter(|s| !s.is_empty()) {
                        self.data.core_usage_percent = samples.iter().map(|s| s.0).collect();
                        self.data.core_kernel_usage_percent = samples.iter().map(|s| s.1).collect();
                        self.data.total_usage_percent =
                            samples.iter().map(|s| s.0).sum::<f32>() / samples.len() as f32;
                        self.data.kernel_usage_percent =
                            samples.iter().map(|s| s.1).sum::<f32>() / samples.len() as f32;
                        self.data.usage_valid = Some(true);
                    }
                    self.previous = current;
                }
                libc::vm_deallocate(
                    mach_task_self(),
                    info as libc::vm_address_t,
                    count as libc::vm_size_t * size_of::<libc::integer_t>() as libc::vm_size_t,
                );
            }
        }
        self.data.uptime_seconds = sysinfo::System::uptime();
        if let Some(pids) = pids() {
            self.data.total_process_count = pids.len() as u64;
            self.data.process_count_valid = Some(true);
            let mut threads = 0u64;
            let mut complete_threads = true;
            for pid in pids {
                if let Some(info) = pid_info::<libc::proc_taskinfo>(pid, libc::PROC_PIDTASKINFO) {
                    threads += info.pti_threadnum.max(0) as u64;
                } else {
                    complete_threads = false;
                }
            }
            self.data.total_thread_count = threads;
            self.data.thread_count_valid = Some(complete_threads);
            // File descriptors are not Windows handles or Mach port rights.
            // Keep the UI's Handles field unknown, even with a complete fd scan.
            self.data.handle_count_valid = Some(false);
        } else {
            self.data.process_count_valid = Some(false);
            self.data.thread_count_valid = Some(false);
            self.data.handle_count_valid = Some(false);
        }
    }
    fn cached(&self) -> &Cpu {
        &self.data
    }
}

pub struct MemoryCollector {
    data: Memory,
    system: sysinfo::System,
}
impl MemoryCache for MemoryCollector {
    fn new() -> Self {
        Self {
            data: Memory {
                darwin_accounting: Some(true),
                sample_valid: Some(false),
                ..Default::default()
            },
            system: sysinfo::System::new(),
        }
    }
    fn refresh(&mut self) {
        self.system.refresh_memory();
        self.data.sample_valid = Some(false);
        let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        // SAFETY: sized, aligned VM statistics buffer, and writable page size.
        let success = unsafe {
            let host = mach_host_self();
            let result = libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &mut count,
            );
            mach_port_deallocate(mach_task_self(), host);
            result == libc::KERN_SUCCESS && count == libc::HOST_VM_INFO64_COUNT
        };
        if !success || page_size <= 0 {
            return;
        }
        let vm = unsafe { vm.assume_init() };
        let bytes = |pages: u32| pages as u64 * page_size as u64;
        self.data.mem_total = self.system.total_memory();
        self.data.mem_free = bytes(vm.free_count).min(self.data.mem_total);
        // Same public Darwin VM estimate used by sysinfo: free + inactive + purgeable.
        self.data.mem_available = self.system.available_memory().min(self.data.mem_total);
        self.data.active = bytes(vm.active_count);
        self.data.inactive = bytes(vm.inactive_count);
        self.data.cached = bytes(vm.external_page_count);
        self.data.wired_bytes = Some(bytes(vm.wire_count));
        self.data.compressed_bytes = Some(bytes(vm.compressor_page_count));
        self.data.swap_total = self.system.total_swap();
        self.data.swap_free = self.system.free_swap();
        self.data.sample_valid = Some(self.data.mem_total > 0);
    }
    fn memory(&self) -> &Memory {
        &self.data
    }
    fn devices(&self) -> &[MemoryDevice] {
        &[]
    }
}

struct PreviousProcess {
    start: u64,
    cpu_ns: u64,
    io: Option<u64>,
    at: Instant,
}
pub struct ProcessCollector {
    data: HashMap<u32, Process>,
    previous: HashMap<u32, PreviousProcess>,
}
impl ProcessCache for ProcessCollector {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            previous: HashMap::new(),
        }
    }
    fn refresh(&mut self) {
        let Some(pids) = pids() else {
            self.data.clear();
            self.previous.clear();
            return;
        };
        let now = Instant::now();
        let mut next = HashMap::with_capacity(pids.len());
        let mut previous = HashMap::with_capacity(pids.len());
        let mut names = HashMap::new();
        for pid in pids {
            let Some(bsd) = pid_info::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO) else {
                continue;
            };
            let start = bsd
                .pbi_start_tvsec
                .saturating_mul(1_000_000)
                .saturating_add(bsd.pbi_start_tvusec);
            let task = pid_info::<libc::proc_taskinfo>(pid, libc::PROC_PIDTASKINFO);
            let mut usage = ProcessUsageStats {
                cpu_valid: Some(false),
                memory_valid: Some(false),
                swap_valid: Some(false),
                disk_valid: Some(false),
                network_valid: Some(false),
                ..Default::default()
            };
            let old = self.previous.get(&pid).filter(|old| old.start == start);
            let mut rusage = MaybeUninit::<libc::rusage_info_v2>::zeroed();
            // SAFETY: RUSAGE_INFO_V2 pairs with rusage_info_v2; failure remains unavailable.
            let io = unsafe {
                (libc::proc_pid_rusage(
                    pid as i32,
                    libc::RUSAGE_INFO_V2,
                    rusage.as_mut_ptr().cast(),
                ) == 0)
                    .then(|| {
                        let info = rusage.assume_init();
                        info.ri_diskio_bytesread
                            .saturating_add(info.ri_diskio_byteswritten)
                    })
            };
            if let Some(task) = &task {
                usage.memory_usage = task.pti_resident_size;
                usage.memory_valid = Some(true);
                let cpu_ns = task.pti_total_user.saturating_add(task.pti_total_system);
                if let Some(old) = old {
                    let seconds = now.duration_since(old.at).as_secs_f64();
                    if let Some(cpu) = rate(old.cpu_ns, cpu_ns, seconds) {
                        usage.cpu_usage = (cpu * 100. / 1e9) as f32;
                        usage.cpu_valid = Some(true);
                    }
                    if let Some(io_rate) = old.io.zip(io).and_then(|(a, b)| rate(a, b, seconds)) {
                        usage.disk_usage = io_rate as f32;
                        usage.disk_valid = Some(true);
                    }
                }
                previous.insert(
                    pid,
                    PreviousProcess {
                        start,
                        cpu_ns,
                        io,
                        at: now,
                    },
                );
            }
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            let len = unsafe {
                libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32)
            };
            let exe = if len > 0 {
                String::from_utf8_lossy(&path[..len as usize])
                    .trim_end_matches('\0')
                    .to_owned()
            } else {
                String::new()
            };
            let name = cchars(&bsd.pbi_name);
            let user_name = names
                .entry(bsd.pbi_ruid)
                .or_insert_with(|| user_name(bsd.pbi_ruid))
                .clone();
            next.insert(
                pid,
                Process {
                    pid,
                    parent: bsd.pbi_ppid,
                    name: if name.is_empty() {
                        cchars(&bsd.pbi_comm)
                    } else {
                        name
                    },
                    exe,
                    cmd: vec![],
                    state: match bsd.pbi_status {
                        1 => ProcessState::Sleeping,
                        2 => ProcessState::Running,
                        3 => ProcessState::Sleeping,
                        4 => ProcessState::Stopped,
                        5 => ProcessState::Zombie,
                        _ => ProcessState::Unknown,
                    } as i32,
                    usage_stats: usage,
                    start_time_ticks: Some(start),
                    user_name,
                    task_count: task
                        .as_ref()
                        .map_or(0, |task| task.pti_threadnum.max(0) as u64),
                    task_count_valid: Some(task.is_some()),
                    children: vec![],
                },
            );
        }
        // Reject self-parent and backwards start-time edges: malformed/raced data
        // must never create recursion cycles in process-tree aggregation.
        let links: Vec<_> = next
            .values()
            .filter(|p| {
                p.parent != p.pid
                    && next
                        .get(&p.parent)
                        .is_some_and(|parent| parent.start_time_ticks < p.start_time_ticks)
            })
            .map(|p| (p.parent, p.pid))
            .collect();
        for (parent, child) in links {
            next.get_mut(&parent).unwrap().children.push(child);
        }
        self.data = next;
        self.previous = previous;
    }
    fn cached_entries(&self) -> &HashMap<u32, Process> {
        &self.data
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut entry = MaybeUninit::<libc::passwd>::zeroed();
    let mut buffer = vec![0u8; 16384];
    let mut result = std::ptr::null_mut();
    // SAFETY: reentrant lookup into owned buffers, copied before those buffers drop.
    unsafe {
        if libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        ) == 0
            && !result.is_null()
        {
            Some(
                CStr::from_ptr((*result).pw_name)
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            Some(uid.to_string())
        }
    }
}

pub struct ProcessActions;
impl ProcessActions {
    fn signal(&self, pids: Vec<u32>, signal: i32) {
        for pid in pids {
            // Never permit POSIX process-group or init signalling. No privilege escalation.
            let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 1) else {
                continue;
            };
            if unsafe { libc::kill(pid, signal) } != 0 {
                log::warn!("Process {pid}: {}", std::io::Error::last_os_error());
            }
        }
    }
}
impl ProcessManager for ProcessActions {
    fn new() -> Self {
        Self
    }
    fn terminate_processes(&self, pids: Vec<u32>) {
        self.signal(pids, libc::SIGTERM);
    }
    fn kill_processes(&self, pids: Vec<u32>) {
        self.signal(pids, libc::SIGKILL);
    }
}

pub struct AboutCollector(About);
impl AboutCache for AboutCollector {
    fn new() -> Self {
        Self(About::default())
    }
    fn refresh(&mut self) {
        self.0.os_info.name = Some("macOS".into());
        self.0.os_info.pretty_name = Some(format!(
            "macOS {}",
            sysinfo::System::os_version().unwrap_or_default()
        ));
        self.0.os_info.id = Some("macos".into());
        self.0.os_info.version_id = sysinfo::System::os_version();
        self.0.os_info.kernel_release = sysinfo::System::kernel_version();
        self.0.os_info.os_architecture = Some(std::env::consts::ARCH.into());
        self.0.device_info.hostname = sysinfo::System::host_name();
        self.0.device_info.vendor = Some("Apple".into());
        self.0.device_info.model = sysctl_string(c"hw.model");
        self.0.de_info.windowing_system = Some("Quartz".into());
        self.0.ready = Some(true);
    }
    fn about(&self) -> &About {
        &self.0
    }
}

pub struct AppsCollector(Vec<App>);
impl AppCache for AppsCollector {
    fn new() -> Self {
        Self(vec![])
    }
    fn refresh(&mut self, processes: &Mutex<HashMap<u32, Process>>) {
        let mut apps = HashMap::<String, App>::new();
        for process in processes.lock().values() {
            let Some((bundle, _)) = process.exe.split_once(".app/Contents/MacOS/") else {
                continue;
            };
            let id = format!("{bundle}.app");
            apps.entry(id.clone())
                .or_insert_with(|| App {
                    id,
                    name: std::path::Path::new(bundle)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    command: process.exe.clone(),
                    pids: vec![],
                })
                .pids
                .push(process.pid);
        }
        self.0 = apps.into_values().collect();
    }
    fn refresh_icons(&mut self) {}
    fn cached_entries(&self) -> &[App] {
        &self.0
    }
    fn cached_icons(&self) -> HashMap<String, Icon> {
        HashMap::new()
    }
}

// Empty inventories explicitly mean these collectors have not been implemented,
// never synthetic devices or successful operations. See the port capability table.
#[derive(Default)]
pub struct Unavailable {}

pub struct GpuCollector {
    data: HashMap<String, Gpu>,
    sequence: u64,
}
impl GpuCache for GpuCollector {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            sequence: 0,
        }
    }
    fn refresh(&mut self) {
        use magpie_platform::gpus::{
            DriverInfo, EngineKind, EngineTelemetry, GpuTelemetry, MemoryTelemetry, TelemetryState,
        };
        self.sequence = self.sequence.wrapping_add(1);
        self.data = crate::iokit::entries(c"IOAccelerator")
            .into_iter()
            .filter_map(|entry| {
                let id = format!("iokit:{}", entry.id()?);
                let stats = entry.dictionary("PerformanceStatistics");
                let percent = |key: &str| {
                    stats
                        .as_ref()
                        .and_then(|stats| crate::iokit::number(stats, key))
                        .filter(|v| *v <= 100)
                        .map(|v| v as f32)
                };
                let overall = percent("Device Utilization %");
                let engines = [
                    ("renderer", "Renderer", percent("Renderer Utilization %")),
                    ("tiler", "Tiler", percent("Tiler Utilization %")),
                ]
                .into_iter()
                .map(|(id, name, value)| EngineTelemetry {
                    id: id.into(),
                    name: name.into(),
                    kind: EngineKind::Render as i32,
                    usage_percent: value,
                    state: if value.is_some() {
                        TelemetryState::Available
                    } else {
                        TelemetryState::Unsupported
                    } as i32,
                    client_derived: Some(false),
                })
                .collect();
                Some((
                    id.clone(),
                    Gpu {
                        id,
                        device_name: entry.string("model"),
                        vendor_id: 0x106b,
                        // Apple unified memory has no dedicated VRAM. Allocated system
                        // memory is not residency and is deliberately not presented as used.
                        telemetry: Some(GpuTelemetry {
                            epoch: 1,
                            sequence: self.sequence,
                            sample_age_ms: 0,
                            state: if overall.is_some() {
                                TelemetryState::Partial
                            } else {
                                TelemetryState::Unsupported
                            } as i32,
                            engines,
                            overall_usage_percent: overall,
                            overall_is_partial: Some(true),
                            memory: Some(MemoryTelemetry {
                                dedicated_state: TelemetryState::Unsupported as i32,
                                shared_state: TelemetryState::Unsupported as i32,
                                ..Default::default()
                            }),
                            driver: Some(DriverInfo {
                                kernel_driver: Some("IOAccelerator".into()),
                                integrated: Some(true),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ))
            })
            .collect();
    }
    fn cached_entries(&self) -> &HashMap<String, Gpu> {
        &self.data
    }
}
impl FanCache for Unavailable {
    fn new() -> Self {
        Self::default()
    }
    fn refresh(&mut self) {}
    fn cached_entries(&self) -> &[Fan] {
        &[]
    }
    fn cached_temperatures(&self) -> &[TemperatureSensor] {
        &[]
    }
}
impl ServiceManager for Unavailable {
    type ServiceCache = crate::services::Services;
    fn new() -> Self {
        Self::default()
    }
    fn logs(
        &self,
        _: &Self::ServiceCache,
        _: u64,
        _: Option<std::num::NonZeroU32>,
    ) -> Option<String> {
        None
    }
    fn start(&self, _: &Self::ServiceCache, _: u64) {
        log::warn!("launchd actions are unavailable");
    }
    fn stop(&self, _: &Self::ServiceCache, _: u64) {
        log::warn!("launchd actions are unavailable");
    }
    fn restart(&self, _: &Self::ServiceCache, _: u64) {
        log::warn!("launchd actions are unavailable");
    }
    fn enable(&self, _: &Self::ServiceCache, _: u64) {
        log::warn!("launchd actions are unavailable");
    }
    fn disable(&self, _: &Self::ServiceCache, _: u64) {
        log::warn!("launchd actions are unavailable");
    }
}
impl DisksManager for Unavailable {
    fn new() -> Self {
        Self::default()
    }
    fn eject(
        &self,
        _: &str,
        _: &Mutex<HashMap<u32, Process>>,
    ) -> Result<(), DisksResponseErrorEjectFailed> {
        Err(DisksResponseErrorEjectFailed::default())
    }
    fn smart_data(&self, _: &str) -> Option<DiskSmartData> {
        None
    }
    fn automatic_smart_data(&self, _: &str) -> Option<DiskSmartData> {
        None
    }
}

pub struct DisksCollector {
    disks: sysinfo::Disks,
    data: Vec<Disk>,
    previous: HashMap<String, (u64, u64, Instant)>,
}
impl DisksCache for DisksCollector {
    fn new() -> Self {
        Self {
            disks: sysinfo::Disks::new(),
            data: vec![],
            previous: HashMap::new(),
        }
    }
    fn refresh(&mut self) {
        self.disks.refresh(true);
        self.data = self
            .disks
            .iter()
            .filter(|disk| disk.total_space() > 0)
            .map(|disk| {
                let mount = disk.mount_point().to_string_lossy().into_owned();
                let device = disk.name().to_string_lossy().into_owned();
                Disk {
                    id: mount.clone(),
                    model: Some(device.clone()),
                    kind: match disk.kind() {
                        sysinfo::DiskKind::SSD => Some(DiskKind::Ssd as i32),
                        sysinfo::DiskKind::HDD => Some(DiskKind::Hdd as i32),
                        _ => None,
                    },
                    capacity_bytes: disk.total_space(),
                    formatted_bytes: Some(disk.total_space()),
                    is_system: mount == "/",
                    is_volume: Some(true),
                    io_valid: Some(false),
                    busy_valid: Some(false),
                    response_time_valid: Some(false),
                    ejectable: false,
                    partitions: HashMap::from([(
                        device.clone(),
                        PartitionInfo {
                            devname: device,
                            size: Some(disk.total_space()),
                            used: Some(disk.total_space().saturating_sub(disk.available_space())),
                            filesystem: Some(disk.file_system().to_string_lossy().into_owned()),
                            mountpoints: vec![mount],
                        },
                    )]),
                    ..Default::default()
                }
            })
            .collect();
        let now = Instant::now();
        for entry in crate::iokit::entries(c"IOBlockStorageDriver") {
            let Some(id) = entry.id().map(|id| format!("iokit:{id}")) else {
                continue;
            };
            let counters = entry.dictionary("Statistics").and_then(|stats| {
                Some((
                    crate::iokit::number(&stats, "Bytes (Read)")?,
                    crate::iokit::number(&stats, "Bytes (Write)")?,
                ))
            });
            let rates = counters.and_then(|(rx, tx)| {
                self.previous.get(&id).and_then(|(a, b, at)| {
                    let seconds = now.duration_since(*at).as_secs_f64();
                    rate(*a, rx, seconds).zip(rate(*b, tx, seconds))
                })
            });
            if let Some((rx, tx)) = counters {
                self.previous.insert(id.clone(), (rx, tx, now));
            }
            self.data.push(Disk {
                id,
                model: entry.child_string("BSD Name"),
                capacity_bytes: entry.child_number("Size").unwrap_or(0),
                is_volume: Some(false),
                io_valid: Some(rates.is_some()),
                busy_valid: Some(false),
                response_time_valid: Some(false),
                rx_bytes_total: counters.map_or(0, |v| v.0),
                tx_bytes_total: counters.map_or(0, |v| v.1),
                rx_speed_bytes_ps: rates.map_or(0, |v| v.0 as u64),
                tx_speed_bytes_ps: rates.map_or(0, |v| v.1 as u64),
                ..Default::default()
            });
        }
        self.previous
            .retain(|id, _| self.data.iter().any(|disk| &disk.id == id));
    }
    fn cached_entries(&self) -> &[Disk] {
        &self.data
    }
}

pub struct NetworkCollector {
    networks: sysinfo::Networks,
    data: HashMap<String, Connection>,
    previous: HashMap<String, (u64, u64, Instant)>,
}
impl NetworkCache for NetworkCollector {
    fn new() -> Self {
        Self {
            networks: sysinfo::Networks::new(),
            data: HashMap::new(),
            previous: HashMap::new(),
        }
    }
    fn refresh(&mut self) {
        self.networks.refresh(true);
        let now = Instant::now();
        let kinds: HashMap<_, _> = crate::iokit::entries(c"IONetworkInterface")
            .into_iter()
            .filter_map(|entry| {
                let name = entry.string("BSD Name")?;
                let kind = if entry.conforms(c"IO80211Interface") {
                    ConnectionKind::Wireless
                } else if entry.conforms(c"IOEthernetInterface") {
                    ConnectionKind::Wired
                } else {
                    ConnectionKind::Other
                };
                Some((name, kind))
            })
            .collect();
        let mut flags = HashMap::new();
        let mut addresses = std::ptr::null_mut();
        // The list is owned by libc until freeifaddrs, all names are copied.
        if unsafe { libc::getifaddrs(&mut addresses) } == 0 {
            let mut address = addresses;
            while !address.is_null() {
                let item = unsafe { &*address };
                if !item.ifa_name.is_null() {
                    flags.insert(
                        unsafe { CStr::from_ptr(item.ifa_name) }
                            .to_string_lossy()
                            .into_owned(),
                        item.ifa_flags,
                    );
                }
                address = item.ifa_next;
            }
            unsafe {
                libc::freeifaddrs(addresses);
            }
        }
        self.data = self
            .networks
            .iter()
            .filter(|(name, _)| name.as_str() != "lo0")
            .map(|(name, network)| {
                let rx = network.total_received();
                let tx = network.total_transmitted();
                let rates = self.previous.get(name).and_then(|(a, b, at)| {
                    let seconds = now.duration_since(*at).as_secs_f64();
                    rate(*a, rx, seconds).zip(rate(*b, tx, seconds))
                });
                self.previous.insert(name.clone(), (rx, tx, now));
                (
                    name.clone(),
                    Connection {
                        id: name.clone(),
                        device_name: Some(name.clone()),
                        kind: kinds.get(name).copied().unwrap_or(ConnectionKind::Other) as i32,
                        state: flags.get(name).map_or(ConnectionState::Unknown, |flags| {
                            if flags & libc::IFF_UP as u32 != 0
                                && flags & libc::IFF_RUNNING as u32 != 0
                            {
                                ConnectionState::Connected
                            } else {
                                ConnectionState::Disconnected
                            }
                        }) as i32,
                        hw_address: network.mac_address().to_string(),
                        rx_total_bytes: rx,
                        tx_total_bytes: tx,
                        rx_rate_bytes_ps: rates.map_or(0., |r| r.0 as f32),
                        tx_rate_bytes_ps: rates.map_or(0., |r| r.1 as f32),
                        rates_valid: Some(rates.is_some()),
                        ipv4_address: network
                            .ip_networks()
                            .iter()
                            .find_map(|ip| ip.addr.is_ipv4().then(|| ip.addr.to_string())),
                        ipv6_address: network
                            .ip_networks()
                            .iter()
                            .find_map(|ip| ip.addr.is_ipv6().then(|| ip.addr.to_string())),
                        ..Default::default()
                    },
                )
            })
            .collect();
        self.previous.retain(|name, _| self.data.contains_key(name));
    }
    fn cached_entries(&self) -> &HashMap<String, Connection> {
        &self.data
    }
}

pub struct BatteryCollector {
    data: Vec<Battery>,
}
impl BatteryCache for BatteryCollector {
    fn new() -> Self {
        Self { data: vec![] }
    }
    fn refresh(&mut self) {
        self.data = crate::iokit::entries(c"AppleSmartBattery")
            .into_iter()
            .enumerate()
            .filter_map(|(index, battery)| {
                let current = battery.number("CurrentCapacity")? as f32;
                let maximum = battery.number("MaxCapacity").filter(|v| *v > 0)? as f32;
                let voltage = battery
                    .number("Voltage")
                    .filter(|v| *v > 0)
                    .map(|v| v as f32 / 1000.);
                let charging = battery.boolean("IsCharging");
                let external = battery.boolean("ExternalConnected");
                let state = if battery.boolean("FullyCharged") == Some(true) {
                    Some(BatteryState::FullyCharged)
                } else if charging == Some(true) {
                    Some(BatteryState::Charging)
                } else if external == Some(false) {
                    Some(BatteryState::Discharging)
                } else if external == Some(true) {
                    Some(BatteryState::PendingCharge)
                } else {
                    None
                };
                Some(Battery {
                    name: format!("Battery {}", index + 1),
                    model: Some("AppleSmartBattery".into()),
                    kind: Some(BatteryType::Bat as i32),
                    power_supply: Some(true),
                    percentage: (current * 100. / maximum).clamp(0., 100.),
                    voltage,
                    power: voltage
                        .zip(battery.number("Amperage"))
                        .map(|(v, a)| v * (a as f32 / 1000.).abs()),
                    charge_cycles: battery
                        .number("CycleCount")
                        .and_then(|v| u32::try_from(v).ok()),
                    state: state.map(|v| v as i32),
                    // Apple Silicon may expose only normalized 0..100 capacity;
                    // never relabel it as mWh or infer battery health from it.
                    ..Default::default()
                })
            })
            .collect();
    }
    fn cached_entries(&self) -> &[Battery] {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn native_memory_cpu_and_own_process_are_measured_after_warmup() {
        let mut cpu = CpuCollector::new();
        cpu.refresh();
        assert_eq!(cpu.cached().usage_valid, Some(false));
        std::thread::sleep(Duration::from_millis(150));
        cpu.refresh();
        assert_eq!(cpu.cached().usage_valid, Some(true));
        assert!((0. ..=100.).contains(&cpu.cached().total_usage_percent));
        assert_eq!(cpu.cached().frequency_valid, Some(false));
        assert!(cpu.cached().physical_core_count.is_some_and(|v| v > 0));
        for group in &cpu.cached().performance_levels {
            assert!(group.physical_core_count > 0);
            assert!(group.index < 16);
        }
        let mut memory = MemoryCollector::new();
        memory.refresh();
        assert_eq!(memory.memory().sample_valid, Some(true));
        assert!(memory.memory().mem_total > 0);
        let mut processes = ProcessCollector::new();
        processes.refresh();
        let pid = std::process::id();
        assert_eq!(
            processes.cached_entries()[&pid].usage_stats.cpu_valid,
            Some(false)
        );
        std::thread::sleep(Duration::from_millis(50));
        processes.refresh();
        let own = &processes.cached_entries()[&pid];
        assert_eq!(own.usage_stats.cpu_valid, Some(true));
        assert_eq!(own.usage_stats.memory_valid, Some(true));
        assert!(own.usage_stats.memory_usage > 0);
        assert_eq!(own.usage_stats.network_valid, Some(false));
    }

    #[test]
    fn process_actions_signal_only_owned_test_children() {
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
            ProcessActions.signal(vec![child.id()], signal);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    use std::os::unix::process::ExitStatusExt;
                    assert_eq!(status.signal(), Some(signal));
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("owned test child did not stop");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
