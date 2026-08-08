use std::{
    fs,
    io::{self, ErrorKind},
    thread,
    time::Duration,
};

use log::{info, warn};

const MEMORY_USAGE_ENVIRONMENT_VARIABLE: &str = "BAH_MEMUSG";
const SMAPS_ROLLUP_PATH: &str = "/proc/self/smaps_rollup";
const STATUS_PATH: &str = "/proc/self/status";

#[derive(Debug, Eq, PartialEq)]
enum MemoryUsage {
    Rollup(MemoryRollup),
    ResidentOnly { rss_kib: u64 },
}

#[derive(Debug, Eq, PartialEq)]
struct MemoryRollup {
    rss_kib: u64,
    pss_kib: u64,
    private_kib: u64,
    shared_kib: u64,
    anonymous_kib: u64,
    swap_kib: u64,
}

/// Starts periodic process-memory logging when explicitly enabled at startup.
pub fn start_if_enabled() {
    if std::env::var(MEMORY_USAGE_ENVIRONMENT_VARIABLE)
        .ok()
        .as_deref()
        != Some("1")
    {
        return;
    }

    match thread::Builder::new()
        .name("bah-memory-usage".to_string())
        .spawn(|| {
            loop {
                thread::sleep(Duration::from_secs(1));
                match memory_usage() {
                    Ok(MemoryUsage::Rollup(usage)) => info!(
                        "bah memory usage: source=smaps_rollup, {}, {}, {}, {}, {}, {}",
                        format_kib("rss", usage.rss_kib),
                        format_kib("pss", usage.pss_kib),
                        format_kib("private", usage.private_kib),
                        format_kib("shared", usage.shared_kib),
                        format_kib("anonymous", usage.anonymous_kib),
                        format_kib("swap", usage.swap_kib),
                    ),
                    Ok(MemoryUsage::ResidentOnly { rss_kib }) => info!(
                        "bah memory usage: source=status-fallback, {}; \
                         pss/private/shared/anonymous/swap=unavailable",
                        format_kib("rss", rss_kib),
                    ),
                    Err(error) => warn!("could not read bah memory usage: {error}"),
                }
            }
        }) {
        Ok(_) => info!("memory usage logging enabled"),
        Err(error) => warn!("could not start memory usage logger: {error}"),
    }
}

fn format_kib(name: &str, kib: u64) -> String {
    format!("{name}={kib} KiB ({:.2} MiB)", kib as f64 / 1024.0)
}

fn memory_usage() -> io::Result<MemoryUsage> {
    read_memory_usage_with(
        || fs::read_to_string(SMAPS_ROLLUP_PATH),
        || fs::read_to_string(STATUS_PATH),
    )
}

fn read_memory_usage_with<R, S>(read_rollup: R, read_status: S) -> io::Result<MemoryUsage>
where
    R: FnOnce() -> io::Result<String>,
    S: FnOnce() -> io::Result<String>,
{
    let rollup_error = match read_rollup().and_then(|contents| parse_memory_rollup(&contents)) {
        Ok(rollup) => return Ok(MemoryUsage::Rollup(rollup)),
        Err(error) => error,
    };

    read_status()
        .and_then(|contents| parse_resident_memory_kib(&contents))
        .map(|rss_kib| MemoryUsage::ResidentOnly { rss_kib })
        .map_err(|status_error| {
            io::Error::new(
                status_error.kind(),
                format!(
                    "smaps rollup unavailable ({rollup_error}); \
                     status fallback unavailable ({status_error})"
                ),
            )
        })
}

fn parse_memory_rollup(contents: &str) -> io::Result<MemoryRollup> {
    let private_clean_kib = parse_kib_field(contents, "Private_Clean")?;
    let private_dirty_kib = parse_kib_field(contents, "Private_Dirty")?;
    let shared_clean_kib = parse_kib_field(contents, "Shared_Clean")?;
    let shared_dirty_kib = parse_kib_field(contents, "Shared_Dirty")?;

    Ok(MemoryRollup {
        rss_kib: parse_kib_field(contents, "Rss")?,
        pss_kib: parse_kib_field(contents, "Pss")?,
        private_kib: checked_sum("private memory", private_clean_kib, private_dirty_kib)?,
        shared_kib: checked_sum("shared memory", shared_clean_kib, shared_dirty_kib)?,
        anonymous_kib: parse_kib_field(contents, "Anonymous")?,
        swap_kib: parse_kib_field(contents, "Swap")?,
    })
}

fn parse_resident_memory_kib(status: &str) -> io::Result<u64> {
    parse_kib_field(status, "VmRSS")
}

fn parse_kib_field(contents: &str, name: &str) -> io::Result<u64> {
    let prefix = format!("{name}:");
    let line = contents
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("{name} is missing from procfs"),
            )
        })?;
    let mut fields = line.split_whitespace();
    let value = fields.next().ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{name} has no value in procfs"),
        )
    })?;
    let unit = fields.next();
    if unit != Some("kB") {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{name} does not use the kB unit in procfs"),
        ));
    }

    value.parse().map_err(|error| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{name} is not a valid KiB value: {error}"),
        )
    })
}

fn checked_sum(name: &str, left: u64, right: u64) -> io::Result<u64> {
    left.checked_add(right).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("{name} overflows a 64-bit KiB value"),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{
        MemoryRollup, MemoryUsage, parse_memory_rollup, parse_resident_memory_kib,
        read_memory_usage_with,
    };

    const ROLLUP: &str = concat!(
        "00400000-7fffffff ---p 00000000 00:00 0 [rollup]\n",
        "Rss:                12000 kB\n",
        "Pss:                 9000 kB\n",
        "Shared_Clean:         900 kB\n",
        "Shared_Dirty:         100 kB\n",
        "Private_Clean:       3000 kB\n",
        "Private_Dirty:       8000 kB\n",
        "Anonymous:           7900 kB\n",
        "Swap:                  20 kB\n",
    );

    #[test]
    fn parses_memory_rollup_and_sums_private_and_shared_memory() {
        assert_eq!(
            parse_memory_rollup(ROLLUP).unwrap(),
            MemoryRollup {
                rss_kib: 12_000,
                pss_kib: 9_000,
                private_kib: 11_000,
                shared_kib: 1_000,
                anonymous_kib: 7_900,
                swap_kib: 20,
            }
        );
    }

    #[test]
    fn parses_rollup_fields_in_any_order() {
        let reversed = ROLLUP.lines().rev().collect::<Vec<_>>().join("\n");

        assert_eq!(parse_memory_rollup(&reversed).unwrap().rss_kib, 12_000);
    }

    #[test]
    fn rejects_missing_or_malformed_rollup_fields() {
        assert!(parse_memory_rollup("Rss: 12000 kB\n").is_err());
        assert!(
            parse_memory_rollup(
                &ROLLUP.replace("Pss:                 9000", "Pss:                 many")
            )
            .is_err()
        );
        assert!(
            parse_memory_rollup(&ROLLUP.replace(
                "Swap:                  20 kB",
                "Swap:                  20 bytes"
            ))
            .is_err()
        );
    }

    #[test]
    fn falls_back_to_status_when_rollup_is_unavailable() {
        let usage = read_memory_usage_with(
            || Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
            || Ok("Name:\tbah\nVmRSS:\t   7890 kB\n".to_string()),
        )
        .unwrap();

        assert_eq!(usage, MemoryUsage::ResidentOnly { rss_kib: 7_890 });
    }

    #[test]
    fn falls_back_to_status_when_rollup_is_invalid() {
        let usage = read_memory_usage_with(
            || Ok("Rss: 12000 kB\n".to_string()),
            || Ok("Name:\tbah\nVmRSS:\t   7890 kB\n".to_string()),
        )
        .unwrap();

        assert_eq!(usage, MemoryUsage::ResidentOnly { rss_kib: 7_890 });
    }

    #[test]
    fn reports_both_errors_when_rollup_and_status_are_unavailable() {
        let error = read_memory_usage_with(
            || Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
            || Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        )
        .unwrap_err();

        assert!(error.to_string().contains("smaps rollup unavailable"));
        assert!(error.to_string().contains("status fallback unavailable"));
    }

    #[test]
    fn parses_resident_memory_from_procfs_status() {
        let status = "Name:\tbah\nVmSize:\t  123456 kB\nVmRSS:\t   7890 kB\n";

        assert_eq!(parse_resident_memory_kib(status).unwrap(), 7890);
    }
}
