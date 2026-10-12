//! What the numbers were measured on. A latency figure with no machine attached is not a result,
//! so every run records the operating system, CPU, core count, memory, toolchain and the load on
//! the machine when it started and when it ended.
//!
//! A probe that fails is written into `probe_errors` and its field is `null`. It is never
//! replaced by a guess: a reviewer reading `null` knows the field is unknown, a reviewer reading
//! a made-up value does not.

use crate::util::{Ctx, Res, run_text};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    /// `std::env::consts::OS`: macos, linux or windows.
    pub os: String,
    pub arch: String,
    pub os_version: Option<String>,
    pub kernel: Option<String>,
    pub cpu_model: Option<String>,
    /// What the OS offers this process (`available_parallelism`): a container limit shows here.
    pub cpu_logical_cores: Option<usize>,
    /// Only where the OS reports it cheaply (macOS); `null` elsewhere.
    pub cpu_physical_cores: Option<usize>,
    /// macOS: performance and efficiency cores, when the machine has both.
    pub cpu_core_layout: Option<String>,
    pub ram_bytes: Option<u64>,
    /// macOS: the power source, because a laptop on battery runs slower and more variably.
    pub power: Option<String>,
    pub rustc: Option<String>,
    pub cargo: Option<String>,
    pub host_triple: Option<String>,
    /// `RUSTFLAGS` when set: it applies to both builds alike, and is part of what was measured.
    pub rustflags: Option<String>,
    pub started_utc: String,
    pub load_at_start: Option<String>,
    pub load_at_end: Option<String>,
    pub probe_errors: Vec<String>,
}

fn sysctl(key: &str) -> Res<String> {
    run_text(Command::new("sysctl").args(["-n", key])).map(|s| s.trim().to_string())
}

fn powershell(script: &str) -> Res<String> {
    run_text(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", script]))
        .map(|s| s.trim().to_string())
}

fn first_matching(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

fn os_version(os: &str) -> Res<String> {
    match os {
        "macos" => {
            let v = run_text(Command::new("sw_vers").arg("-productVersion"))?;
            let b = run_text(Command::new("sw_vers").arg("-buildVersion"))?;
            Ok(format!("macOS {} ({})", v.trim(), b.trim()))
        }
        "linux" => {
            let text = std::fs::read_to_string("/etc/os-release").ctx("read /etc/os-release")?;
            text.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
                .ok_or_else(|| "/etc/os-release has no PRETTY_NAME".to_string())
        }
        "windows" => powershell(
            "$o = Get-CimInstance Win32_OperatingSystem; \"$($o.Caption) $($o.Version)\"",
        ),
        other => Err(format!("no OS version probe for {other}")),
    }
}

fn kernel(os: &str) -> Res<String> {
    match os {
        "macos" | "linux" => {
            run_text(Command::new("uname").arg("-r")).map(|s| s.trim().to_string())
        }
        _ => Err("no kernel probe on this OS".to_string()),
    }
}

fn cpu_model(os: &str) -> Res<String> {
    match os {
        "macos" => sysctl("machdep.cpu.brand_string"),
        "linux" => {
            let text = std::fs::read_to_string("/proc/cpuinfo").ctx("read /proc/cpuinfo")?;
            if let Some(m) = first_matching(&text, "model name") {
                return Ok(m);
            }
            // Arm kernels name no model in /proc/cpuinfo; lscpu does.
            let l = run_text(&mut Command::new("lscpu"))
                .ctx("lscpu, after /proc/cpuinfo named no model")?;
            first_matching(&l, "Model name").ok_or_else(|| "lscpu names no model".to_string())
        }
        "windows" => powershell("(Get-CimInstance Win32_Processor | Select-Object -First 1).Name"),
        other => Err(format!("no CPU probe for {other}")),
    }
}

fn ram_bytes(os: &str) -> Res<u64> {
    match os {
        "macos" => sysctl("hw.memsize")?.parse::<u64>().ctx("parse hw.memsize"),
        "linux" => {
            let text = std::fs::read_to_string("/proc/meminfo").ctx("read /proc/meminfo")?;
            let kb = text
                .lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .ok_or("/proc/meminfo has no MemTotal")?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<u64>()
                .ctx("parse MemTotal")?;
            Ok(kb * 1024)
        }
        "windows" => powershell("(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory")?
            .parse::<u64>()
            .ctx("parse TotalPhysicalMemory"),
        other => Err(format!("no memory probe for {other}")),
    }
}

/// The machine's load right now, as text, so the reviewer sees what the number is.
pub fn load_now(os: &str) -> Res<String> {
    match os {
        "macos" => {
            let raw = sysctl("vm.loadavg")?;
            let nums: Vec<&str> = raw
                .trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .split_whitespace()
                .collect();
            if nums.len() != 3 {
                return Err(format!("unexpected vm.loadavg {raw:?}"));
            }
            Ok(format!(
                "load average {} {} {} (1, 5, 15 min)",
                nums[0], nums[1], nums[2]
            ))
        }
        "linux" => {
            let raw = std::fs::read_to_string("/proc/loadavg").ctx("read /proc/loadavg")?;
            let p: Vec<&str> = raw.split_whitespace().take(3).collect();
            if p.len() != 3 {
                return Err(format!("unexpected /proc/loadavg {raw:?}"));
            }
            Ok(format!(
                "load average {} {} {} (1, 5, 15 min)",
                p[0], p[1], p[2]
            ))
        }
        "windows" => {
            let v = powershell(
                "(Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average",
            )?;
            Ok(format!("CPU {v}% busy (instantaneous)"))
        }
        other => Err(format!("no load probe for {other}")),
    }
}

fn power(os: &str) -> Res<String> {
    if os != "macos" {
        return Err("no power-source probe on this OS".to_string());
    }
    let raw = run_text(Command::new("pmset").args(["-g", "batt"]))?;
    raw.lines()
        .next()
        .map(|l| l.trim().to_string())
        .ok_or_else(|| "pmset printed nothing".to_string())
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn rfc3339_utc(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn now_utc() -> Res<String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ctx("the clock is before 1970")?
        .as_secs();
    Ok(rfc3339_utc(secs))
}

/// Collects the probes that failed, so none of them fails silently.
struct Probes {
    errors: Vec<String>,
}

impl Probes {
    fn take<T>(&mut self, what: &str, r: Res<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.errors.push(format!("{what}: {e}"));
                None
            }
        }
    }
}

fn parse<T: std::str::FromStr>(text: String, what: &str) -> Res<T>
where
    T::Err: std::fmt::Display,
{
    text.trim()
        .parse::<T>()
        .ctx(&format!("parse {what} {text:?}"))
}

/// Probe this machine. Never fails: what cannot be probed is `null` and named in `probe_errors`.
pub fn probe() -> Environment {
    let os = std::env::consts::OS;
    let mut p = Probes { errors: Vec::new() };
    let os_version = p.take("os_version", os_version(os));
    let kernel = p.take("kernel", kernel(os));
    let cpu_model = p.take("cpu_model", cpu_model(os));
    let ram_bytes = p.take("ram_bytes", ram_bytes(os));
    let (physical, layout) = if os == "macos" {
        let phys = p.take(
            "cpu_physical_cores",
            sysctl("hw.physicalcpu").and_then(|s| parse::<usize>(s, "hw.physicalcpu")),
        );
        let perf = p.take("cpu_perf_cores", sysctl("hw.perflevel0.physicalcpu"));
        let eff = p.take("cpu_efficiency_cores", sysctl("hw.perflevel1.physicalcpu"));
        let layout = match (perf, eff) {
            (Some(a), Some(b)) => Some(format!("{a} performance + {b} efficiency")),
            _ => None,
        };
        (phys, layout)
    } else {
        (None, None)
    };
    let power_source = if os == "macos" {
        p.take("power", power(os))
    } else {
        None
    };
    let rustc_v = p.take("rustc", run_text(Command::new("rustc").arg("-vV")));
    let cargo = p.take(
        "cargo",
        run_text(Command::new("cargo").arg("--version")).map(|s| s.trim().to_string()),
    );
    let (rustc, host_triple) = match rustc_v {
        Some(v) => (
            v.lines().next().map(str::to_string),
            v.lines()
                .find_map(|l| l.strip_prefix("host: "))
                .map(str::to_string),
        ),
        None => (None, None),
    };
    let load_at_start = p.take("load_at_start", load_now(os));
    let started_utc = p.take("started_utc", now_utc());
    let cpu_logical_cores = p.take(
        "cpu_logical_cores",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .ctx("available_parallelism"),
    );
    Environment {
        os: os.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os_version,
        kernel,
        cpu_model,
        cpu_logical_cores,
        cpu_physical_cores: physical,
        cpu_core_layout: layout,
        ram_bytes,
        power: power_source,
        rustc,
        cargo,
        host_triple,
        rustflags: std::env::var("RUSTFLAGS").ok().filter(|s| !s.is_empty()),
        started_utc: started_utc.unwrap_or_else(|| "unknown".to_string()),
        load_at_start,
        load_at_end: None,
        probe_errors: p.errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339_utc(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn a_probe_that_cannot_run_is_named_not_invented() {
        assert!(os_version("plan9").is_err());
        assert!(cpu_model("plan9").is_err());
        assert!(load_now("plan9").is_err());
    }
}
