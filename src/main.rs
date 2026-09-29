use std::{
    env,
    io::{self, IsTerminal, Write},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
#[cfg(not(windows))]
use std::{ffi::CString, fs};

#[derive(Clone, Copy)]
struct CpuSample {
    idle: u64,
    total: u64,
}

#[derive(Default)]
struct Options {
    interval_ms: u64,
    no_bars: bool,
    ascii: bool,
    cpu: bool,
    ram: bool,
    gpu: bool,
    no_color: bool,
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";

#[cfg(windows)]
use windows_sys::Win32::{
    NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2},
    Storage::FileSystem::GetDiskFreeSpaceExW,
    System::{
        Console::{GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE},
        Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
        ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION},
        SystemInformation::{GetTickCount64, MEMORYSTATUSEX},
        Threading::GetSystemTimes,
    },
};

fn parse_args() -> Options {
    let mut o = Options {
        interval_ms: 1000,
        ..Default::default()
    };

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--interval" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    o.interval_ms = v.parse::<u64>().unwrap_or(1000).max(100);
                }
            }
            "--minimal" | "--no-bars" => o.no_bars = true,
            "--ascii" => o.ascii = true,
            "--cpu" => o.cpu = true,
            "--ram" => o.ram = true,
            "--gpu" => o.gpu = true,
            "--no-color" => o.no_color = true,
            "--version" | "-V" => {
                println!("winimon {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "winimon - tiny system monitor

USAGE:
  winimon [OPTIONS]

OPTIONS:
  --interval MS   Refresh interval (default: 1000)
  --cpu           Show CPU
  --ram           Show RAM
  --gpu           Show GPU + VRAM
  --minimal       Hide bars
  --no-bars       Hide bars
  --ascii         ASCII bars instead of Unicode
  --no-color      Disable ANSI styling
  --version       Show version
  -h, --help      Show help

Branding & adaptive layout:
  winimon shows a cute winimon mascot beside the metrics when the
  terminal is wide enough (>=68 cols) and shows extended system info
  (uptime, load, tasks, swap, disk, host/kernel/cores, os/freq/temp,
  net/battery) when even wider/taller. All UI respects --ascii
  and --no-color."
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    o
}

#[cfg(not(windows))]
fn cpu_sample() -> io::Result<CpuSample> {
    let text = fs::read_to_string("/proc/stat")?;
    let nums: Vec<u64> = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();

    if nums.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid /proc/stat",
        ));
    }

    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);

    Ok(CpuSample {
        idle,
        total: nums.iter().sum(),
    })
}

fn cpu_usage(previous: CpuSample, current: CpuSample) -> f64 {
    let total = current.total.saturating_sub(previous.total);
    let idle = current.idle.saturating_sub(previous.idle);

    if total == 0 {
        0.0
    } else {
        (total.saturating_sub(idle)) as f64 / total as f64 * 100.0
    }
}

#[cfg(not(windows))]
fn ram_metrics() -> io::Result<(f64, u64, u64)> {
    let text = fs::read_to_string("/proc/meminfo")?;
    let mut total = 0;
    let mut available = 0;

    for line in text.lines() {
        if line.starts_with("MemTotal:") {
            total = line
                .split_whitespace()
                .nth(1)
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024;
        } else if line.starts_with("MemAvailable:") {
            available = line
                .split_whitespace()
                .nth(1)
                .and_then(|x| x.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024;
        }
    }

    let used = total.saturating_sub(available);
    let pct = if total > 0 {
        used as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    Ok((pct, used, total))
}

fn gpu_metrics() -> (Option<f64>, Option<u32>, Option<u64>, Option<u64>) {
    let Ok(output) = Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
    else {
        return (None, None, None, None);
    };

    if !output.status.success() {
        return (None, None, None, None);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<_> = text
        .lines()
        .next()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .collect();

    (
        parts.first().and_then(|x| x.parse().ok()),
        parts.get(1).and_then(|x| x.parse().ok()),
        parts
            .get(2)
            .and_then(|x| x.parse::<u64>().ok())
            .map(|x| x * 1024 * 1024),
        parts
            .get(3)
            .and_then(|x| x.parse::<u64>().ok())
            .map(|x| x * 1024 * 1024),
    )
}

fn test_width_var() -> Option<String> {
    env::var("WINIMON_TEST_WIDTH").ok().or_else(|| env::var("MINIMON_TEST_WIDTH").ok())
}

fn test_height_var() -> Option<String> {
    env::var("WINIMON_TEST_HEIGHT").ok().or_else(|| env::var("MINIMON_TEST_HEIGHT").ok())
}

fn env_flag_set(win_key: &str, legacy_key: &str) -> bool {
    env::var_os(win_key).is_some() || env::var_os(legacy_key).is_some()
}

#[cfg(not(windows))]
fn terminal_size() -> (usize, usize) {
    if let (Some(w), Some(h)) = (test_width_var(), test_height_var()) {
        if let (Ok(ww), Ok(hh)) = (w.parse::<usize>(), h.parse::<usize>()) {
            if ww > 0 && hh > 0 {
                return (ww, hh);
            }
        }
    }
    if let Some(w) = test_width_var() {
        if let Ok(ww) = w.parse::<usize>() {
            if ww > 0 {
                let h = test_height_var()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(24);
                return (ww, h);
            }
        }
    }
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            let w = ws.ws_col as usize;
            let h = if ws.ws_row > 0 { ws.ws_row as usize } else { 24 };
            return (w, h);
        }
    }
    (80, 24)
}

#[allow(dead_code)]
fn terminal_width() -> usize {
    terminal_size().0
}

fn bar(pct: f64, width: usize, ascii: bool, color: bool) -> String {
    if width == 0 {
        return String::new();
    }

    let pct = pct.clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);

    if ascii {
        if color {
            format!("{}{}{}{}", "#".repeat(filled), DIM, "-".repeat(empty), RESET)
        } else {
            format!("{}{}", "#".repeat(filled), "-".repeat(empty))
        }
    } else if color {
        format!("{}{}{}{}", "█".repeat(filled), DIM, "░".repeat(empty), RESET)
    } else {
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    }
}
fn human(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;

    if bytes as f64 >= GIB {
        format!("{:.1}G", bytes as f64 / GIB)
    } else {
        format!("{:.0}M", bytes as f64 / MIB)
    }
}

fn row(label: &str, pct: f64, detail: &str, width: usize, opts: &Options) -> String {
    if width < 31 {
        return if detail.is_empty() {
            format!("{} {:.0}%", label, pct)
        } else {
            format!("{} {:.0}% {}", label, pct, detail)
        };
    }

    if width < 43 && !opts.no_bars {
        let value_text = if detail.is_empty() {
            format!("{:.0}%", pct)
        } else {
            format!("{:.0}% {}", pct, detail)
        };

        let fixed = label.len() + 1 + 1 + value_text.len();
        let bar_width = width.saturating_sub(fixed).clamp(3, 8);

        return format!(
            "{} {} {}",
            label,
            bar(pct, bar_width, opts.ascii, !opts.no_color),
            value_text
        );
    }

    if opts.no_bars {
        return if detail.is_empty() {
            format!("{} {:.0}%", label, pct)
        } else {
            format!("{} {:.0}% {}", label, pct, detail)
        };
    }

    let value_text = if detail.is_empty() {
        format!("{:.0}%", pct)
    } else {
        format!("{:.0}% {}", pct, detail)
    };

    let fixed = 5 + 1 + 1 + value_text.len();
    // Wider bars when more horizontal space: fill whitespace
    let max_bar = if width >= 120 {
        36
    } else if width >= 100 {
        32
    } else if width >= 80 {
        26
    } else if width >= 60 {
        22
    } else {
        18
    };
    let bar_width = width.saturating_sub(fixed).clamp(4, max_bar);

    let base = format!(
        "{label:<5} {} {}",
        bar(pct, bar_width, opts.ascii, !opts.no_color),
        value_text
    );
    // Fill remaining horizontal whitespace with inline extra when wide
    if width >= 60 && visible_len(&base) + 4 < width {
        let extra = inline_extra_for_label(label, width, opts);
        if !extra.is_empty() {
            let avail = width.saturating_sub(visible_len(&base) + 2);
            if visible_len(&extra) <= avail {
                // Pad to align extra to the right, filling whitespace
                let pad = avail - visible_len(&extra);
                return format!("{}{}{}{}", base, " ".repeat(pad + 2), extra, "");
            } else if avail >= 10 {
                // Truncate extra to fit
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in extra.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= avail {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                let pad = avail.saturating_sub(vis);
                return format!("{}{}{}{}", base, " ".repeat(pad + 2), out, "");
            }
        }
    }
    base
}

// ---------- helpers for branding & extra info ----------

fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
                continue;
            }
        }
        len += 1;
    }
    len
}

#[cfg(not(windows))]
fn available_ram_bytes() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    text.lines()
        .find(|line| line.starts_with("MemAvailable:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value * 1024)
}

#[cfg(windows)]
fn available_ram_bytes() -> Option<u64> {
    memory_status().map(|status| status.ullAvailPhys)
}

#[cfg(not(windows))]
fn cached_ram_bytes() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    text.lines()
        .find(|line| line.starts_with("Cached:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|value| value * 1024)
}

#[cfg(windows)]
fn cached_ram_bytes() -> Option<u64> {
    None
}

fn inline_extra_for_label(label: &str, width: usize, opts: &Options) -> String {
    match label {
        "CPU" => {
            // Inline shows CORES/FREQ distinct from extra's TEMP/PSI
            let mut parts = Vec::new();
            if width >= 65 {
                let cores = cpu_cores();
                if opts.no_color {
                    parts.push(format!("{}C", cores));
                } else {
                    parts.push(format!("{}{}C{RESET}", DIM, cores));
                }
            }
            if width >= 70 {
                if let Some(freq) = cpu_freq_mhz() {
                    let s = if freq >= 1000.0 {
                        format!("{:.1}GHz", freq / 1000.0)
                    } else {
                        format!("{:.0}MHz", freq)
                    };
                    if opts.no_color {
                        parts.push(format!("FREQ {}", s));
                    } else {
                        parts.push(format!("{}FREQ{} {}", DIM, RESET, s));
                    }
                }
            }
            // TEMP shown in extra panel, not inline to avoid duplicate
            parts.join("  ")
        }
        "RAM" => {
            // Inline shows AVAIL/CACHE, distinct from extra's SWAP
            let mut parts = Vec::new();
            if width >= 65 {
                if let Some(avail) = available_ram_bytes() {
                    if opts.no_color {
                        parts.push(format!("AVAIL {}", human(avail)));
                    } else {
                        parts.push(format!("{}AVAIL{} {}", DIM, RESET, human(avail)));
                    }
                }
            }
            if width >= 75 {
                if let Some(cached) = cached_ram_bytes() {
                    if opts.no_color {
                        parts.push(format!("CACHE {}", human(cached)));
                    } else {
                        parts.push(format!("{}CACHE{} {}", DIM, RESET, human(cached)));
                    }
                }
            }
            parts.join("  ")
        }
        "GPU" => String::new(),
        "VRAM" => {
            // Distinct: only show alternative mount when very wide, to avoid duplicating extra's DISK /
            if width >= 85 {
                if let Some((used, total, pct)) = disk_usage("/home") {
                    if total > 0 && human(used) != human(total) {
                        if opts.no_color {
                            return format!("DISK/home {}/{} ({:.0}%)", human(used), human(total), pct);
                        } else {
                            return format!("{}DISK/home{} {}/{} ({:.0}%)", DIM, RESET, human(used), human(total), pct);
                        }
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

#[cfg(not(windows))]
fn uptime_secs() -> Option<u64> {
    let t = fs::read_to_string("/proc/uptime").ok()?;
    let v: f64 = t.split_whitespace().next()?.parse().ok()?;
    Some(v as u64)
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hrs = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;
    if days > 0 {
        format!("{}d {:02}:{:02}:{:02}", days, hrs, mins, secs)
    } else {
        format!("{:02}:{:02}:{:02}", hrs, mins, secs)
    }
}

#[cfg(not(windows))]
fn load_avg() -> Option<(f64, f64, f64)> {
    let t = fs::read_to_string("/proc/loadavg").ok()?;
    let mut it = t.split_whitespace();
    let a: f64 = it.next()?.parse().ok()?;
    let b: f64 = it.next()?.parse().ok()?;
    let c: f64 = it.next()?.parse().ok()?;
    Some((a, b, c))
}

#[cfg(not(windows))]
fn swap_metrics() -> Option<(f64, u64, u64)> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut free = None;
    for line in text.lines() {
        if line.starts_with("SwapTotal:") {
            total = line
                .split_whitespace()
                .nth(1)
                .and_then(|x| x.parse::<u64>().ok())
                .map(|x| x * 1024);
        } else if line.starts_with("SwapFree:") {
            free = line
                .split_whitespace()
                .nth(1)
                .and_then(|x| x.parse::<u64>().ok())
                .map(|x| x * 1024);
        }
    }
    let total = total?;
    let free = free.unwrap_or(0);
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(free);
    let pct = used as f64 / total as f64 * 100.0;
    Some((pct, used, total))
}

#[cfg(not(windows))]
fn disk_usage(path: &str) -> Option<(u64, u64, f64)> {
    let c = CString::new(path).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let total = stat.f_blocks as u64 * stat.f_frsize as u64;
        let free = stat.f_bfree as u64 * stat.f_frsize as u64;
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(free);
        let pct = used as f64 / total as f64 * 100.0;
        Some((used, total, pct))
    }
}

#[cfg(not(windows))]
fn hostname() -> Option<String> {
    if let Ok(s) = fs::read_to_string("/proc/sys/kernel/hostname") {
        let h = s.trim().to_string();
        if !h.is_empty() {
            return Some(h);
        }
    }
    env::var("HOSTNAME").ok().filter(|s| !s.is_empty())
}

#[cfg(not(windows))]
fn kernel_version() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(not(windows))]
fn cpu_cores() -> usize {
    unsafe {
        let n = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
        if n > 0 {
            n as usize
        } else {
            1
        }
    }
}

#[cfg(not(windows))]
fn cpu_model() -> Option<String> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if line.starts_with("model name") {
            if let Some(pos) = line.find(':') {
                let m = line[pos + 1..].trim().to_string();
                if !m.is_empty() {
                    return Some(m);
                }
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn process_count() -> Option<usize> {
    let entries = fs::read_dir("/proc").ok()?;
    let mut n = 0;
    for e in entries.flatten() {
        if let Some(name) = e.file_name().to_str() {
            if name.chars().all(|c| c.is_ascii_digit()) {
                n += 1;
            }
        }
    }
    Some(n)
}

#[cfg(not(windows))]
fn cpu_freq_mhz() -> Option<f64> {
    let text = fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut sum = 0.0;
    let mut cnt = 0;
    for line in text.lines() {
        if line.starts_with("cpu MHz") {
            if let Some(p) = line.find(':') {
                if let Ok(v) = line[p + 1..].trim().parse::<f64>() {
                    sum += v;
                    cnt += 1;
                }
            }
        }
    }
    if cnt > 0 { Some(sum / cnt as f64) } else { None }
}

#[cfg(not(windows))]
fn cpu_temp_c() -> Option<f32> {
    if let Ok(s) = fs::read_to_string("/sys/class/thermal/thermal_zone0/temp") {
        if let Ok(v) = s.trim().parse::<i32>() {
            return Some(v as f32 / 1000.0);
        }
    }
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for e in entries.flatten() {
            let p = e.path().join("temp1_input");
            if let Ok(s) = fs::read_to_string(&p) {
                if let Ok(v) = s.trim().parse::<i32>() {
                    return Some(v as f32 / 1000.0);
                }
            }
            // also try temp2, temp3
            for idx in 2..5 {
                let pp = e.path().join(format!("temp{}_input", idx));
                if let Ok(s) = fs::read_to_string(&pp) {
                    if let Ok(v) = s.trim().parse::<i32>() {
                        if v > 0 {
                            return Some(v as f32 / 1000.0);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn os_pretty() -> Option<String> {
    let text = fs::read_to_string("/etc/os-release").ok()?;
    for line in text.lines() {
        if line.starts_with("PRETTY_NAME=") {
            let v = line.trim_start_matches("PRETTY_NAME=").trim_matches('"').trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn battery_info() -> Option<(u8, String)> {
    let dir = fs::read_dir("/sys/class/power_supply").ok()?;
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("BAT") {
            let cap = fs::read_to_string(e.path().join("capacity"))
                .ok()?
                .trim()
                .parse::<u8>()
                .ok()?;
            let status = fs::read_to_string(e.path().join("status"))
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            return Some((cap, status));
        }
    }
    None
}

#[cfg(not(windows))]
fn network_info() -> Option<(String, u64, u64)> {
    let text = fs::read_to_string("/proc/net/dev").ok()?;
    let mut best: Option<(String, u64, u64)> = None;
    let mut max_rx = 0;
    for line in text.lines().skip(2) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.find(':') {
            let iface = line[..colon].trim();
            if iface == "lo" {
                continue;
            }
            let rest = line[colon + 1..].trim();
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() >= 9 {
                let rx: u64 = parts[0].parse().unwrap_or(0);
                let tx: u64 = parts[8].parse().unwrap_or(0);
                if rx > max_rx {
                    max_rx = rx;
                    best = Some((iface.to_string(), rx, tx));
                }
            }
        }
    }
    best
}

#[cfg(not(windows))]
fn all_disk_usages() -> Vec<(String, u64, u64, f64)> {
    let mut out = Vec::new();
    for m in ["/", "/home", "/boot"] {
        if let Some((used, total, pct)) = disk_usage(m) {
            // avoid duplicates and very small mounts
            if total > 1024 * 1024 * 100 {
                out.push((m.to_string(), used, total, pct));
            }
        }
    }
    // also try to read /proc/mounts for additional mounts if we still have space
    if out.len() < 3 {
        if let Ok(text) = fs::read_to_string("/proc/mounts") {
            for line in text.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mp = parts[1];
                    if mp.starts_with("/snap") || mp.starts_with("/dev") || mp == "/" || mp == "/home" || mp == "/boot" {
                        continue;
                    }
                    if let Some((used, total, pct)) = disk_usage(mp) {
                        if total > 1024 * 1024 * 500 {
                            out.push((mp.to_string(), used, total, pct));
                            if out.len() >= 3 {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn top_processes_by_mem(limit: usize) -> Vec<(String, u64)> {
    let mut procs: Vec<(String, u64)> = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else { return procs; };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid = name;
        let status_path = format!("/proc/{}/status", pid);
        let Ok(text) = fs::read_to_string(&status_path) else { continue; };
        let mut mem = 0u64;
        for line in text.lines() {
            if line.starts_with("VmRSS:") {
                if let Some(v) = line.split_whitespace().nth(1).and_then(|x| x.parse::<u64>().ok()) {
                    mem = v * 1024;
                }
                break;
            }
        }
        if mem == 0 {
            continue;
        }
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        let cmd = fs::read_to_string(&cmdline_path)
            .ok()
            .map(|s| {
                let c = s.replace('\0', " ").trim().to_string();
                if c.is_empty() {
                    fs::read_to_string(format!("/proc/{}/comm", pid))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .unwrap_or(pid.clone())
                } else {
                    c.split_whitespace()
                        .next()
                        .unwrap_or(&pid)
                        .split('/')
                        .last()
                        .unwrap_or(&pid)
                        .to_string()
                }
            })
            .unwrap_or(pid.clone());
        let short = if cmd.len() > 18 {
            cmd.chars().take(18).collect()
        } else {
            cmd
        };
        procs.push((short, mem));
    }
    procs.sort_by(|a, b| b.1.cmp(&a.1));
    procs.truncate(limit);
    procs
}

#[cfg(not(windows))]
fn cpu_per_core_samples() -> Vec<CpuSample> {
    let Ok(text) = fs::read_to_string("/proc/stat") else { return Vec::new(); };
    let mut out = Vec::new();
    for line in text.lines().filter(|l| l.starts_with("cpu") && !l.starts_with("cpu ")) {
        let nums: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|x| x.parse().ok())
            .collect();
        if nums.len() < 4 { continue; }
        let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
        out.push(CpuSample { idle, total: nums.iter().sum() });
    }
    out
}

#[cfg(not(windows))]
fn psi_info() -> Option<String> {
    // Pressure Stall Info - shows system pressure, newer info
    if let Ok(t) = fs::read_to_string("/proc/pressure/cpu") {
        // format: some avg10=0.00 avg60=0.00 avg300=0.00 total=...
        let first = t.lines().next()?;
        let mut vals = Vec::new();
        for part in first.split_whitespace() {
            if let Some(v) = part.split('=').nth(1) {
                vals.push(v.to_string());
                if vals.len() >= 2 { break; }
            }
        }
        if !vals.is_empty() {
            return Some(format!("PSI {}", vals.join("/")));
        }
    }
    None
}

#[cfg(not(windows))]
fn sensors_temps() -> Vec<(String, f32)> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for e in entries.flatten() {
            let base = e.path();
            let name = fs::read_to_string(base.join("name")).ok().map(|s| s.trim().to_string()).unwrap_or("hwmon".to_string());
            for idx in 1..4 {
                let p = base.join(format!("temp{}_input", idx));
                if let Ok(s) = fs::read_to_string(&p) {
                    if let Ok(v) = s.trim().parse::<i32>() {
                        if v > 1000 && v < 150000 {
                            out.push((format!("{}:{}", name, idx), v as f32 / 1000.0));
                            if out.len() >= 3 { break; }
                        }
                    }
                }
            }
            if out.len() >= 3 { break; }
        }
    }
    out
}

#[cfg(windows)]
fn filetime_ticks(time: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64
}

#[cfg(windows)]
fn cpu_sample() -> io::Result<CpuSample> {
    let mut idle = unsafe { std::mem::zeroed() };
    let mut kernel = unsafe { std::mem::zeroed() };
    let mut user = unsafe { std::mem::zeroed() };

    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    if !ok {
        return Err(io::Error::last_os_error());
    }

    let idle = filetime_ticks(idle);
    let kernel = filetime_ticks(kernel);
    let user = filetime_ticks(user);
    Ok(CpuSample {
        idle,
        total: kernel.saturating_add(user),
    })
}

#[cfg(windows)]
fn memory_status() -> Option<MEMORYSTATUSEX> {
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { windows_sys::Win32::System::SystemInformation::GlobalMemoryStatusEx(&mut status) } != 0;
    ok.then_some(status)
}

#[cfg(windows)]
fn ram_metrics() -> io::Result<(f64, u64, u64)> {
    let status = memory_status().ok_or_else(io::Error::last_os_error)?;
    let used = status.ullTotalPhys.saturating_sub(status.ullAvailPhys);
    let pct = if status.ullTotalPhys == 0 {
        0.0
    } else {
        used as f64 / status.ullTotalPhys as f64 * 100.0
    };
    Ok((pct, used, status.ullTotalPhys))
}

#[cfg(windows)]
fn terminal_size() -> (usize, usize) {
    if let (Some(w), Some(h)) = (test_width_var(), test_height_var()) {
        if let (Ok(w), Ok(h)) = (w.parse::<usize>(), h.parse::<usize>()) {
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
    }

    let width = env::var("COLUMNS").ok().and_then(|v| v.parse::<usize>().ok());
    let height = env::var("LINES").ok().and_then(|v| v.parse::<usize>().ok());
    if let (Some(w), Some(h)) = (width, height) {
        if w > 0 && h > 0 {
            return (w, h);
        }
    }

    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) != 0 {
            let w = (info.srWindow.Right as i32 - info.srWindow.Left as i32 + 1).max(1) as usize;
            let h = (info.srWindow.Bottom as i32 - info.srWindow.Top as i32 + 1).max(1) as usize;
            return (w, h);
        }
    }

    (80, 24)
}

#[cfg(windows)]
fn uptime_secs() -> Option<u64> {
    Some(unsafe { GetTickCount64() / 1000 })
}

#[cfg(windows)]
fn load_avg() -> Option<(f64, f64, f64)> {
    None
}

#[cfg(windows)]
fn swap_metrics() -> Option<(f64, u64, u64)> {
    let status = memory_status()?;
    let total = status.ullTotalPageFile;
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(status.ullAvailPageFile);
    Some((used as f64 / total as f64 * 100.0, used, total))
}

#[cfg(windows)]
fn windows_path(path: &str) -> String {
    if path == "/" || path.starts_with('/') {
        let drive = env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        format!("{}\\", drive.trim_end_matches(['\\', '/']))
    } else {
        path.to_string()
    }
}

#[cfg(windows)]
fn disk_usage(path: &str) -> Option<(u64, u64, f64)> {
    let path = windows_path(path);
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free)
    } != 0;
    if !ok || total == 0 {
        return None;
    }
    let used = total.saturating_sub(free);
    Some((used, total, used as f64 / total as f64 * 100.0))
}

#[cfg(windows)]
fn hostname() -> Option<String> {
    env::var("COMPUTERNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(windows)]
fn kernel_version() -> Option<String> {
    Some("Windows".to_string())
}

#[cfg(windows)]
fn cpu_cores() -> usize {
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
}

#[cfg(windows)]
fn cpu_model() -> Option<String> {
    env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(windows)]
fn process_count() -> Option<usize> {
    let mut info: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetPerformanceInfo(
            &mut info,
            std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32,
        )
    } != 0;
    ok.then_some(info.ProcessCount as usize)
}

#[cfg(windows)]
fn cpu_freq_mhz() -> Option<f64> {
    None
}

#[cfg(windows)]
fn cpu_temp_c() -> Option<f32> {
    None
}

#[cfg(windows)]
fn os_pretty() -> Option<String> {
    Some("Windows".to_string())
}

#[cfg(windows)]
fn battery_info() -> Option<(u8, String)> {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    if !ok || status.BatteryLifePercent == 255 || status.BatteryFlag == 128 {
        return None;
    }
    let state = if status.BatteryFlag & 8 != 0 || status.ACLineStatus == 1 {
        "Charging"
    } else {
        "Discharging"
    };
    Some((status.BatteryLifePercent, state.to_string()))
}

#[cfg(windows)]
fn network_info() -> Option<(String, u64, u64)> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    let result = unsafe { GetIfTable2(&mut table) };
    if result != 0 || table.is_null() {
        return None;
    }

    let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };
    let mut best = None;
    let mut max_rx = 0u64;
    for row in rows {
        let alias_end = row.Alias.iter().position(|value| *value == 0).unwrap_or(row.Alias.len());
        let desc_end = row.Description.iter().position(|value| *value == 0).unwrap_or(row.Description.len());
        let alias = String::from_utf16_lossy(&row.Alias[..alias_end]);
        let description = String::from_utf16_lossy(&row.Description[..desc_end]);
        let name = if alias.is_empty() { description } else { alias };
        if name.to_ascii_lowercase().contains("loopback") {
            continue;
        }
        if row.InOctets > max_rx {
            max_rx = row.InOctets;
            best = Some((name, row.InOctets, row.OutOctets));
        }
    }

    unsafe { FreeMibTable(table.cast()) };
    best
}

#[cfg(windows)]
fn all_disk_usages() -> Vec<(String, u64, u64, f64)> {
    let mut out = Vec::new();
    for drive in b'A'..=b'Z' {
        let root = format!("{}:\\", drive as char);
        if let Some((used, total, pct)) = disk_usage(&root) {
            if total > 100 * 1024 * 1024 {
                out.push((format!("{}:", drive as char), used, total, pct));
            }
        }
    }
    out
}

#[cfg(windows)]
fn parse_tasklist_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(field.trim().to_string());
                field.clear();
            }
            _ => field.push(ch),
        }
    }
    fields.push(field.trim().to_string());
    fields
}

#[cfg(windows)]
fn process_memory_bytes(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let number = parts.next()?.replace(',', "").parse::<u64>().ok()?;
    let unit = parts.next().unwrap_or("K");
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "b" => 1,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => 1024,
    };
    Some(number.saturating_mul(multiplier))
}

#[cfg(windows)]
fn top_processes_by_mem(limit: usize) -> Vec<(String, u64)> {
    let Ok(output) = Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut processes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let fields = parse_tasklist_csv(line);
            if fields.len() < 5 {
                return None;
            }
            let memory = process_memory_bytes(&fields[4])?;
            let name = fields[0].trim_end_matches(".exe").to_string();
            Some((name, memory))
        })
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| right.1.cmp(&left.1));
    processes.truncate(limit);
    processes
}

#[cfg(windows)]
fn cpu_per_core_samples() -> Vec<CpuSample> {
    Vec::new()
}

#[cfg(windows)]
fn psi_info() -> Option<String> {
    None
}

#[cfg(windows)]
fn sensors_temps() -> Vec<(String, f32)> {
    Vec::new()
}

fn side_logo_lines(opts: &Options) -> Vec<String> {
    let v = env!("CARGO_PKG_VERSION");
    if opts.ascii {
        if opts.no_color {
            vec![
                "  /\\_/\\  winimon".to_string(),
                format!(" ( o.o ) v{} ", v),
                "  > ^ <  tiny mon".to_string(),
                " /  -  \\  monitor".to_string(),
            ]
        } else {
            vec![
                format!("  {CYAN}/\\_/\\{RESET}  {BOLD}winimon{RESET}"),
                format!(" {CYAN}( o.o ){RESET} {DIM}v{:<5}{RESET}", v),
                format!("  {CYAN}> ^ <{RESET}  {DIM}tiny mon{RESET}"),
                format!(" {CYAN}/  -  \\{RESET}  {DIM} monitor{RESET}"),
            ]
        }
    } else if opts.no_color {
        vec![
            "  ∧__∧   winimon".to_string(),
            format!(" (｡•ω•｡) v{} ", v),
            " / つ♥   tiny mon".to_string(),
            " しーＪ  monitor ".to_string(),
        ]
    } else {
        vec![
            format!("  {CYAN}∧__∧{RESET}   {BOLD}winimon{RESET}"),
            format!(" {CYAN}(｡•ω•｡){RESET} {DIM}v{:<5}{RESET}", v),
            format!(" {CYAN}/ つ♥{RESET}   {DIM}tiny mon{RESET}"),
            format!(" {CYAN}しーＪ{RESET}  {DIM} monitor {RESET}"),
        ]
    }
}

fn header_logo_lines(opts: &Options, width: usize) -> Vec<String> {
    let v = env!("CARGO_PKG_VERSION");
    let raw: Vec<String> = if opts.ascii {
        vec![
            format!(" /\\_/\\   winimon v{}", v),
            " ( o.o )  tiny system monitor".to_string(),
            "  > ^ <   for terminals too small".to_string(),
        ]
    } else {
        vec![
            format!("  ∧__∧    winimon v{}", v),
            " (｡•ω•｡)  tiny system monitor".to_string(),
            " / つ♥    for terminals too small".to_string(),
        ]
    };

    // center each line within width, add color if needed
    let mut out = Vec::new();
    for line in raw {
        let vis = visible_len(&line);
        if vis >= width {
            // truncate? just push as is with color handling
            let colored = if opts.no_color {
                line
            } else if opts.ascii {
                // color monster part cyan/bold, but raw already plain; add color
                // simpler: just wrap monster cyan if possible
                // For header we do simple cyan highlight on first part
                line
            } else {
                line
            };
            out.push(colored);
            continue;
        }
        let pad = (width.saturating_sub(vis)) / 2;
        let padded = format!("{}{}", " ".repeat(pad), line);
        if opts.no_color {
            out.push(padded);
        } else {
            // add subtle coloring: make winimon BOLD CYAN
            // We already have raw without codes, so wrap "winimon" part
            let colored = padded
                .replacen("winimon", &format!("{BOLD}{CYAN}winimon{RESET}"), 1);
            out.push(colored);
        }
    }
    out
}

fn big_word_lines(width: usize, opts: &Options) -> Vec<String> {
    // Figlet small font for "winimon" (height 4 visible) + tagline
    let raw = vec![
        "          _          _",
        "  _ __   (_)  _ _   (_)  _ __    ___   _ _",
        " | '  \\  | | | ' \\  | | | '  \\  / _ \\ | ' \\",
        " |_|_|_| |_| |_||_| |_| |_|_|_| \\___/ |_||_|",
    ];
    let max_vis = raw.iter().map(|l| l.len()).max().unwrap_or(0);
    let block_pad = if max_vis >= width {
        0
    } else {
        (width - max_vis) / 2
    };
    let mut out = Vec::new();
    for line in raw {
        if max_vis >= width {
            // not enough width to center block, fallback per-line truncate/center
            let vis = line.len();
            if vis >= width {
                if opts.no_color {
                    out.push(line.to_string());
                } else {
                    out.push(format!("{BOLD}{CYAN}{}{RESET}", line));
                }
                continue;
            }
            let pad = (width - vis) / 2;
            let padded = format!("{}{}", " ".repeat(pad), line);
            if opts.no_color {
                out.push(padded);
            } else {
                out.push(format!("{BOLD}{CYAN}{}{RESET}", padded));
            }
            continue;
        }
        let padded = format!("{}{}", " ".repeat(block_pad), line);
        if opts.no_color {
            out.push(padded);
        } else {
            out.push(format!("{BOLD}{CYAN}{}{RESET}", padded));
        }
    }
    // tagline version + tiny monitor
    let v = env!("CARGO_PKG_VERSION");
    let tag = format!("v{}  tiny system monitor — for terminals too small", v);
    let tvis = tag.len();
    if tvis < width {
        let tpad = (width - tvis) / 2;
        let tline = format!("{}{}", " ".repeat(tpad), tag);
        if opts.no_color {
            out.push(tline);
        } else {
            out.push(format!("{DIM}{}{RESET}", tline));
        }
    } else if width >= tag.len() / 2 {
        // fallback shorter tag for narrower big-word widths
        let short = format!("v{}  tiny monitor", v);
        let spad = (width.saturating_sub(short.len())) / 2;
        let sline = format!("{}{}", " ".repeat(spad), short);
        if opts.no_color {
            out.push(sline);
        } else {
            out.push(format!("{DIM}{}{RESET}", sline));
        }
    }
    out
}

fn separator_line(width: usize, opts: &Options) -> String {
    let ch = if opts.ascii { "-" } else { "─" };
    let line = ch.repeat(width.min(80));
    if opts.no_color {
        line
    } else {
        format!("{DIM}{}{RESET}", line)
    }
}

fn extra_panel_lines(width: usize, opts: &Options) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    let up = uptime_secs().map(format_uptime);
    let load = load_avg();
    let tasks = process_count();
    let swap = swap_metrics();
    let disk = disk_usage("/");
    let host = hostname();
    let kern = kernel_version();
    let cores = cpu_cores();

    // Line 1: Uptime / Load / Tasks - width aware
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(u) = up.as_deref() {
            if opts.no_color {
                parts.push(format!("UP {}", u));
            } else {
                parts.push(format!("{DIM}UP{RESET} {}", u));
            }
        }
        if let Some((a, b, c)) = load {
            let load_str = if width >= 60 {
                if opts.no_color {
                    format!("LOAD {:.2} {:.2} {:.2}", a, b, c)
                } else {
                    format!("{DIM}LOAD{RESET} {YELLOW}{:.2}{RESET} {:.2} {:.2}", a, b, c)
                }
            } else if width >= 50 {
                if opts.no_color {
                    format!("LOAD {:.2} {:.2}", a, b)
                } else {
                    format!("{DIM}LOAD{RESET} {YELLOW}{:.2}{RESET} {:.2}", a, b)
                }
            } else if width >= 40 {
                if opts.no_color {
                    format!("LOAD {:.2}", a)
                } else {
                    format!("{DIM}LOAD{RESET} {YELLOW}{:.2}{RESET}", a)
                }
            } else {
                // very narrow, just first value without label?
                if opts.no_color {
                    format!("LD {:.2}", a)
                } else {
                    format!("{DIM}LD{RESET} {YELLOW}{:.2}{RESET}", a)
                }
            };
            parts.push(load_str);
        }
        if let Some(n) = tasks {
            if width >= 50 {
                if opts.no_color {
                    parts.push(format!("TASKS {}", n));
                } else {
                    parts.push(format!("{DIM}TASKS{RESET} {}", n));
                }
            } else if width >= 40 {
                // shorter label for narrow
                if opts.no_color {
                    parts.push(format!("TSK {}", n));
                } else {
                    parts.push(format!("{DIM}TSK{RESET} {}", n));
                }
            }
        }
        if !parts.is_empty() {
            let line = parts.join("  ");
            if visible_len(&line) > width {
                // fallback: truncate gracefully by taking width chars (should rarely happen now)
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in line.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= width {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                lines.push(out);
            } else {
                lines.push(line);
            }
        }
    }

    // Line 2: SWAP + DISK - width aware
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some((_, used, total)) = swap {
            let s = if opts.no_color {
                format!("SWAP {}/{}", human(used), human(total))
            } else {
                format!("{DIM}SWAP{RESET} {}/{}", human(used), human(total))
            };
            parts.push(s);
        } else if width >= 60 {
            if opts.no_color {
                parts.push("SWAP 0M/0M".to_string());
            } else {
                parts.push(format!("{DIM}SWAP{RESET} 0M/0M"));
            }
        }
        if let Some((used, total, pct)) = disk {
            let s = if width >= 50 {
                if opts.no_color {
                    format!("DISK {}/{} ({:.0}%)", human(used), human(total), pct)
                } else {
                    format!("{DIM}DISK{RESET} {}/{} ({:.0}%)", human(used), human(total), pct)
                }
            } else {
                // narrow: omit pct
                if opts.no_color {
                    format!("DISK {}/{}", human(used), human(total))
                } else {
                    format!("{DIM}DISK{RESET} {}/{}", human(used), human(total))
                }
            };
            parts.push(s);
        }
        if !parts.is_empty() {
            let line = parts.join("  ");
            if visible_len(&line) <= width {
                lines.push(line);
            } else {
                // for still too wide, try dropping disk pct or swap etc. Already handled.
                // fallback truncate with esc awareness
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in line.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= width {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                lines.push(out);
            }
        }
    }

    // Line 3: HOST / KERNEL / CORES / MODEL - width aware
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(h) = host {
            // truncate hostname aggressively for narrow
            let max_h = if width >= 60 { 15 } else { 10 };
            let trunc = if h.len() > max_h { &h[..max_h] } else { &h };
            if opts.no_color {
                parts.push(format!("HOST {}", trunc));
            } else {
                parts.push(format!("{DIM}HOST{RESET} {}", trunc));
            }
        }
        if let Some(k) = kern {
            if width >= 45 {
                let max_k = if width >= 60 { 20 } else { 12 };
                let trunc = if k.len() > max_k { &k[..max_k] } else { &k };
                if opts.no_color {
                    parts.push(format!("KERNEL {}", trunc));
                } else {
                    parts.push(format!("{DIM}KERNEL{RESET} {}", trunc));
                }
            }
        }
        if width >= 40 {
            if opts.no_color {
                parts.push(format!("CORES {}", cores));
            } else {
                parts.push(format!("{DIM}CORES{RESET} {}", cores));
            }
        } else if width >= 35 {
            // narrow only show cores number without label
            if opts.no_color {
                parts.push(format!("C{}", cores));
            } else {
                parts.push(format!("{DIM}C{RESET}{}", cores));
            }
        }
        if width >= 110 {
            if let Some(m) = cpu_model() {
                let short = m.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                let trunc = if short.len() > 22 { &short[..22] } else { &short };
                if opts.no_color {
                    parts.push(trunc.to_string());
                } else {
                    parts.push(format!("{DIM}{}{RESET}", trunc));
                }
            }
        }
        if !parts.is_empty() {
            let line = parts.join("  ");
            if visible_len(&line) <= width {
                lines.push(line);
            } else {
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in line.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= width {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                lines.push(out);
            }
        }
    }

    // Line 4: OS / CPU freq / CPU temp  (more info when wide)
    {
        let mut parts: Vec<String> = Vec::new();
        if let Some(os) = os_pretty() {
            if width >= 60 {
                let trunc = if os.len() > 22 { &os[..22] } else { &os };
                if opts.no_color {
                    parts.push(format!("OS {}", trunc));
                } else {
                    parts.push(format!("{DIM}OS{RESET} {}", trunc));
                }
            } else if width >= 50 {
                // shorter OS for medium
                let short = os.split_whitespace().next().unwrap_or(&os).to_string();
                let trunc = if short.len() > 12 { &short[..12] } else { &short };
                if opts.no_color {
                    parts.push(format!("OS {}", trunc));
                } else {
                    parts.push(format!("{DIM}OS{RESET} {}", trunc));
                }
            }
        }
        if let Some(temp) = cpu_temp_c() {
            if width >= 40 {
                if opts.no_color {
                    parts.push(format!("TEMP {:.0}°C", temp));
                } else {
                    parts.push(format!("{DIM}TEMP{RESET} {YELLOW}{:.0}°C{RESET}", temp));
                }
            }
        }
        if !parts.is_empty() {
            let line = parts.join("  ");
            if visible_len(&line) <= width {
                lines.push(line);
            } else if width >= 45 {
                // try fallback truncate
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in line.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= width {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                lines.push(out);
            }
        }
    }

    // Line 5: Network + Battery
    {
        let mut final_parts: Vec<String> = Vec::new();
        if let Some((iface, rx, tx)) = network_info() {
            if width >= 35 {
                if opts.no_color {
                    if width >= 60 {
                        final_parts.push(format!("NET {} RX {} TX {}", iface, human(rx), human(tx)));
                    } else {
                        final_parts.push(format!("NET {} {}", iface, human(rx)));
                    }
                } else if width >= 60 {
                    final_parts.push(format!("{DIM}NET{RESET} {} RX {} TX {}", iface, human(rx), human(tx)));
                } else {
                    final_parts.push(format!("{DIM}NET{RESET} {} {}", iface, human(rx)));
                }
            }
        }
        if let Some((cap, status)) = battery_info() {
            if width >= 35 {
                let s = if opts.no_color {
                    if status.to_lowercase().contains("charging") {
                        format!("BAT {}%↑", cap)
                    } else if status.to_lowercase().contains("discharging") {
                        format!("BAT {}%↓", cap)
                    } else {
                        format!("BAT {}%", cap)
                    }
                } else {
                    let col = if cap < 20 { "\x1b[31m" } else if cap < 50 { YELLOW } else { "" };
                    if status.to_lowercase().contains("charging") {
                        if col.is_empty() {
                            format!("{DIM}BAT{RESET} {}%↑", cap)
                        } else {
                            format!("{DIM}BAT{RESET} {}{}%↑{RESET}", col, cap)
                        }
                    } else if status.to_lowercase().contains("discharging") {
                        if col.is_empty() {
                            format!("{DIM}BAT{RESET} {}%↓", cap)
                        } else {
                            format!("{DIM}BAT{RESET} {}{}%↓{RESET}", col, cap)
                        }
                    } else if col.is_empty() {
                        format!("{DIM}BAT{RESET} {}%", cap)
                    } else {
                        format!("{DIM}BAT{RESET} {}{}%{RESET}", col, cap)
                    }
                };
                final_parts.push(s);
            }
        }
        if !final_parts.is_empty() {
            let line = final_parts.join("  ");
            if visible_len(&line) <= width {
                lines.push(line);
            } else if width >= 40 {
                let mut out = String::new();
                let mut vis = 0;
                let mut in_esc = false;
                for ch in line.chars() {
                    if ch == '\x1b' {
                        in_esc = true;
                        out.push(ch);
                        continue;
                    }
                    if in_esc {
                        out.push(ch);
                        if ch == 'm' {
                            in_esc = false;
                        }
                        continue;
                    }
                    if vis >= width {
                        break;
                    }
                    out.push(ch);
                    vis += 1;
                }
                lines.push(out);
            }
        }
    }

    // Line 6: PSI - newer pressure info, only when wide (distinct from earlier)
    if width >= 80 {
        if let Some(psi) = psi_info() {
            let line = if opts.no_color {
                psi.clone()
            } else {
                format!("{DIM}{}{RESET}", psi)
            };
            if visible_len(&line) <= width {
                lines.push(line);
            }
        }
    }

    // Line 7: Sensors - temps from hwmon, only when wide
    if width >= 85 {
        let temps = sensors_temps();
        if !temps.is_empty() {
            let mut parts = Vec::new();
            for (name, temp) in temps.iter().take(2) {
                if opts.no_color {
                    parts.push(format!("{} {:.0}°C", name, temp));
                } else {
                    parts.push(format!("{}{:.0}°C{RESET}", format!("{DIM}{}", name), temp));
                }
            }
            let line = parts.join("  ");
            if visible_len(&line) <= width {
                lines.push(line);
            }
        }
    }

    // Line 8: Per-core count when very wide, distinct new info
    if width >= 100 {
        let per_core = cpu_per_core_samples();
        if !per_core.is_empty() {
            let line = if opts.no_color {
                format!("PER-CORE {} cores", per_core.len())
            } else {
                format!("{DIM}PER-CORE{RESET} {} cores", per_core.len())
            };
            if visible_len(&line) <= width {
                lines.push(line);
            }
        }
    }

    lines
}

fn render_dashboard(
    width: usize,
    height: usize,
    cpu_pct: f64,
    ram_pct: f64,
    ram_used: u64,
    ram_total: u64,
    gpu_pct: Option<f64>,
    gpu_temp: Option<u32>,
    vram_used: Option<u64>,
    vram_total: Option<u64>,
    opts: &Options,
    interactive: bool,
) -> Vec<String> {
    let selected = opts.cpu || opts.ram || opts.gpu;

    // Decide branding mode - big word pre-check to avoid duplicate side logo
    let show_big_word_prelim = !selected && interactive && width >= 70 && height >= 10 && {
        let cand_len = 3;
        height >= cand_len + 1 + 4
    };
    let mut show_side = false;
    let mut logo_lines: Vec<String> = Vec::new();
    let mut logo_width = 0;
    let gap = 4;

    if interactive && width >= 68 && !show_big_word_prelim {
        let candidate = side_logo_lines(opts);
        let w = candidate.iter().map(|s| visible_len(s)).max().unwrap_or(0);
        let left_needed = 31usize;
        let left_w = width.saturating_sub(w + gap);
        if left_w >= left_needed && height >= 4 {
            show_side = true;
            logo_lines = candidate;
            logo_width = w;
        }
    }

    let left_width = if show_side {
        width.saturating_sub(logo_width + gap)
    } else {
        width
    };

    // Build main metric lines with adaptive detail
    let mut main_lines: Vec<String> = Vec::new();

    // Adaptive details based on left_width
    let cpu_detail = if left_width >= 90 {
        let cores = cpu_cores();
        if width >= 100 {
            if let Some((a, _, _)) = load_avg() {
                format!("{} cores • load {:.2}", cores, a)
            } else {
                format!("{} cores", cores)
            }
        } else {
            format!("{} cores", cores)
        }
    } else {
        String::new()
    };

    let ram_detail = if left_width >= 95 {
        let free = ram_total.saturating_sub(ram_used);
        format!("{}/{} {} free", human(ram_used), human(ram_total), human(free))
    } else {
        format!("{}/{}", human(ram_used), human(ram_total))
    };

    let gpu_detail_base = gpu_temp.map(|x| format!("{x}°C")).unwrap_or_default();
    let vram_detail = if let (Some(u), Some(t)) = (vram_used, vram_total) {
        if left_width >= 95 {
            let free = t.saturating_sub(u);
            format!("{}/{} {} free", human(u), human(t), human(free))
        } else {
            format!("{}/{}", human(u), human(t))
        }
    } else {
        String::new()
    };

    if !selected || opts.cpu {
        main_lines.push(row("CPU", cpu_pct, &cpu_detail, left_width, opts));
    }
    if !selected || opts.ram {
        main_lines.push(row("RAM", ram_pct, &ram_detail, left_width, opts));
    }
    if !selected || opts.gpu {
        if let Some(gpu_pct_val) = gpu_pct {
            let g_detail = if left_width >= 90 && !gpu_detail_base.is_empty() {
                // enrich with maybe short label
                gpu_detail_base.clone()
            } else {
                gpu_detail_base.clone()
            };
            main_lines.push(row("GPU", gpu_pct_val, &g_detail, left_width, opts));
            if let (Some(used), Some(total)) = (vram_used, vram_total) {
                let pct = if total > 0 {
                    used as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                main_lines.push(row("VRAM", pct, &vram_detail, left_width, opts));
            }
        } else {
            // GPU unavailable line: pad to left_width
            let msg = "GPU   unavailable";
            let line = if visible_len(msg) >= left_width {
                msg.to_string()
            } else if opts.no_color {
                format!("{:<width$}", msg, width = left_width)
            } else {
                format!("{}{}{}", DIM, format!("{:<width$}", msg, width = left_width), RESET)
            };
            // For side mode we want left_width consistent, but msg may be shorter; pad
            // Ensure main_lines length matches for side combining
            main_lines.push(line);
        }
    }

    // Big ASCII word "winimon" for very wide & tall terminals (char art)
    let mut big_word_block: Vec<String> = Vec::new();
    let mut show_big_word = false;
    if !selected && interactive && width >= 70 && height >= 10 {
        let candidate = big_word_lines(width, opts);
        let metrics_block_height = main_lines.len().max(if show_side { logo_lines.len() } else { 0 }).max(1);
        let needed = candidate.len() + 1 + metrics_block_height;
        if height >= needed {
            show_big_word = true;
            big_word_block = candidate;
        }
    }

    // If side logo, combine side-by-side
    let mut out: Vec<String> = Vec::new();
    if show_big_word {
        out.extend(big_word_block);
        out.push(String::new());
    }

    if show_side {
        let max_rows = main_lines.len().max(logo_lines.len());
        for i in 0..max_rows {
            let left = if i < main_lines.len() {
                main_lines[i].clone()
            } else {
                " ".repeat(left_width)
            };
            let right = if i < logo_lines.len() {
                logo_lines[i].clone()
            } else {
                String::new()
            };
            if right.is_empty() {
                out.push(left);
            } else {
                out.push(format!("{}{}{}", left, " ".repeat(gap), right));
            }
        }
    } else {
        // No side logo: maybe show header logo on top if height allows and width moderate
        // Only when showing all metrics (not filtered) to keep filtered views minimal
        let can_show_header = interactive
            && height >= main_lines.len() + 4
            && width >= 48
            && width < 68
            && !selected;

        let header_allowed =
            interactive && height >= main_lines.len() + 5 && width >= 48 && width < 68 && !selected;

        if header_allowed {
            let header = header_logo_lines(opts, width);
            out.extend(header);
            out.push(String::new());
        } else if can_show_header {
            let header = header_logo_lines(opts, width);
            out.extend(header);
            out.push(String::new());
        }

        out.extend(main_lines.clone());

        // If not side and no header but still interactive and height large, we could show tiny footer branding when no extra panel
        // We'll handle footer via extra panel logic below
    }

    // Extra info panel when wide/tall enough
    if interactive && width >= 40 && height >= out.len() + 2 {
        let remaining = height.saturating_sub(out.len());
        if remaining >= 2 {
            let sep = separator_line(width, opts);
            out.push(sep);
            let extra = extra_panel_lines(width, opts);
            // only push as many as fit in remaining-1
            let capacity = remaining - 1;
            for (idx, line) in extra.into_iter().enumerate() {
                if idx >= capacity {
                    break;
                }
                // Also ensure total out len doesn't exceed height
                if out.len() >= height {
                    break;
                }
                out.push(line);
            }
        }
    } else if interactive && width >= 35 && width < 40 && height >= out.len() + 2 {
        // Very compact footer for narrow-tall with extra vertical space
        // For medium width with extra vertical space, show single compact info line
        // like uptime + load as footer
        let remaining = height.saturating_sub(out.len());
        if remaining >= 2 {
            if let Some(secs) = uptime_secs() {
                let up = format_uptime(secs);
                let load = load_avg().map(|(a, _, _)| format!("{:.2}", a)).unwrap_or_else(|| "-".to_string());
                let mut footer = if opts.no_color {
                    format!("up {}  load {}", up, load)
                } else {
                    format!("{DIM}up{RESET} {}  {DIM}load{RESET} {}", up, load)
                };
                // truncate
                while visible_len(&footer) > width && footer.len() > 10 {
                    footer.pop();
                }
                let sep = separator_line(width, opts);
                out.push(sep);
                out.push(footer);
            }
        }
    }

    // Fill remaining vertical whitespace with dense info (top processes, extra disks)
    if interactive && height > out.len() + 1 && width >= 35 {
        let remaining = height.saturating_sub(out.len());
        if remaining >= 2 {
            // Show top processes - fill as much vertical whitespace as possible, expect some left
            let procs = top_processes_by_mem((remaining - 1).min(12));
            if !procs.is_empty() && out.len() + 1 + procs.len() <= height {
                let hdr = if opts.no_color {
                    "TOP  MEM     CMD".to_string()
                } else {
                    format!("{DIM}TOP  MEM     CMD{RESET}")
                };
                out.push(hdr);
                for (cmd, mem) in procs {
                    if out.len() >= height {
                        break;
                    }
                    let line = if opts.no_color {
                        format!("     {:>6}  {}", human(mem), cmd)
                    } else {
                        format!("     {DIM}{:>6}{RESET}  {}", human(mem), cmd)
                    };
                    if visible_len(&line) <= width {
                        out.push(line);
                    } else {
                        let mut l = line;
                        while visible_len(&l) > width && l.len() > 10 {
                            l.pop();
                        }
                        if visible_len(&l) <= width {
                            out.push(l);
                        }
                    }
                }
            }
        }
        // Additional disk mounts if still space
        if out.len() + 1 < height && width >= 50 {
            let disks = all_disk_usages();
            for (mp, used, total, pct) in disks.iter().skip(1) {
                if out.len() >= height {
                    break;
                }
                if *mp == "/" {
                    continue;
                }
                let line = if opts.no_color {
                    format!("DISK {} {}/{} ({:.0}%)", mp, human(*used), human(*total), pct)
                } else {
                    format!("{DIM}DISK{RESET} {} {}/{} ({:.0}%)", mp, human(*used), human(*total), pct)
                };
                if visible_len(&line) <= width {
                    out.push(line);
                }
            }
        }
    }

    // Ensure we don't exceed height: truncate extra first (already), else truncate from end?
    // If still exceeds, trim to height (prefer keeping metrics at top)
    if out.len() > height && height > 0 {
        out.truncate(height);
    }

    // For non-interactive, just return main_lines (already handled) but we used out which includes branding.
    // For non-interactive we want plain metrics only, so if !interactive, return main_lines directly.
    if !interactive {
        return main_lines;
    }

    out
}

fn main() -> io::Result<()> {
    let mut opts = parse_args();

    if env::var_os("NO_COLOR").is_some() {
        opts.no_color = true;
    }

    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();

    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })
    .expect("failed to install Ctrl+C handler");

    let interactive = io::stdout().is_terminal() || env_flag_set("WINIMON_FORCE_TTY", "MINIMON_FORCE_TTY");

    let mut previous_cpu = cpu_sample()?;
    thread::sleep(Duration::from_millis(200));

    if interactive {
        print!("\x1b[?25l");
    }

    while running.load(Ordering::SeqCst) {
        let current_cpu = cpu_sample()?;
        let cpu_pct = cpu_usage(previous_cpu, current_cpu);
        previous_cpu = current_cpu;

        let (ram_pct, ram_used, ram_total) = ram_metrics()?;
        let (gpu_pct, gpu_temp, vram_used, vram_total) = gpu_metrics();

        let (width, height) = terminal_size();

        let lines = render_dashboard(
            width,
            height,
            cpu_pct,
            ram_pct,
            ram_used,
            ram_total,
            gpu_pct,
            gpu_temp,
            vram_used,
            vram_total,
            &opts,
            interactive,
        );

        if interactive {
            print!("\x1b[H\x1b[J{}", lines.join("\n"));
            io::stdout().flush()?;
            if env_flag_set("WINIMON_ONE_SHOT", "MINIMON_ONE_SHOT") {
                break;
            }
        } else {
            println!("{}", lines.join("\n"));
            break;
        }

        let mut waited = 0;
        while waited < opts.interval_ms && running.load(Ordering::SeqCst) {
            let step = (opts.interval_ms - waited).min(100);
            thread::sleep(Duration::from_millis(step));
            waited += step;
        }
    }

    if interactive {
        print!("\x1b[?25h\n");
        io::stdout().flush()?;
    }

    Ok(())
}
