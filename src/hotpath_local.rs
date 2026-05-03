//! Local Hotpath integration helpers for running swaps with profiling enabled.

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static HOTPATH_ACTIVE: AtomicBool = AtomicBool::new(false);

static PROCESS_HOTPATH_RUN: LazyLock<Mutex<Option<HotpathRun>>> =
    LazyLock::new(|| Mutex::new(None));

const IO_RETRIES: usize = 50;
const IO_RETRY_SLEEP: Duration = Duration::from_millis(25);
const ROW_LIMIT: usize = 50;

/// Builds a default report path inside `{data_dir}/hotpath/`.
pub fn default_report_path(data_dir: &Path, prefix: &str, swap_id: &str) -> PathBuf {
    let report_dir = data_dir.join("hotpath");
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    report_dir.join(format!("{prefix}_{swap_id}_{ts}.json"))
}

/// Stores a process-wide [`HotpathRun`] so it can be finalized later.
///
/// Returns `true` if the run was stored, or `false` if one was already stored.
pub fn try_set_process_hotpath_run(run: HotpathRun) -> bool {
    let mut slot = PROCESS_HOTPATH_RUN
        .lock()
        .expect("process hotpath mutex should not be poisoned");
    if slot.is_some() {
        return false;
    }
    *slot = Some(run);
    true
}

/// Takes the stored process-wide [`HotpathRun`], if any.
pub fn take_process_hotpath_run() -> Option<HotpathRun> {
    PROCESS_HOTPATH_RUN
        .lock()
        .expect("process hotpath mutex should not be poisoned")
        .take()
}

/// A single Hotpath profiling session.
///
/// Only one `HotpathRun` is allowed per process at a time.
pub struct HotpathRun {
    _guard: hotpath::HotpathGuard,
    report_path: PathBuf,
}

impl Drop for HotpathRun {
    fn drop(&mut self) {
        HOTPATH_ACTIVE.store(false, Ordering::Release);
    }
}

fn read_json_with_retries(path: &Path) -> std::io::Result<Value> {
    let mut last_err = std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("report not readable at {}", path.display()),
    );
    for _ in 0..IO_RETRIES {
        match fs::read_to_string(path)
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(std::io::Error::other))
        {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                thread::sleep(IO_RETRY_SLEEP);
            }
        }
    }
    Err(last_err)
}

fn write_json_pretty(path: &Path, v: &Value) -> std::io::Result<()> {
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let writer = std::io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, v).map_err(std::io::Error::other)
}

fn pretty_format_json_file_in_place(path: &Path) -> std::io::Result<()> {
    // Hotpath writes the report when the guard is dropped; give it a moment.
    let v = read_json_with_retries(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
    let tmp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("hotpath")
    ));
    write_json_pretty(&tmp_path, &v)?;
    fs::rename(&tmp_path, path)
}

impl HotpathRun {
    fn build(caller_name: &'static str, report_path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = report_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut sections = vec![hotpath::Section::FunctionsTiming, hotpath::Section::Threads];
        #[cfg(feature = "hotpath-alloc")]
        {
            sections.insert(1, hotpath::Section::FunctionsAlloc);
        }

        let guard = hotpath::HotpathGuardBuilder::new(caller_name)
            .format(hotpath::Format::Json)
            .functions_limit(0)
            .threads_limit(0)
            .output_path(&report_path)
            .sections(sections)
            .build();

        Ok(Self {
            _guard: guard,
            report_path,
        })
    }

    /// Starts profiling for the current process until `finish_and_print` is called.
    pub fn start(caller_name: &'static str, report_path: PathBuf) -> std::io::Result<Self> {
        if HOTPATH_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "hotpath run already active in this process",
            ));
        }

        match Self::build(caller_name, report_path) {
            Ok(run) => Ok(run),
            Err(e) => {
                HOTPATH_ACTIVE.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    /// Ends profiling, then prints timing/alloc tables parsed from the JSON.
    pub fn finish_and_print(self) {
        let path = self.report_path.clone();
        drop(self);

        // Make the on-disk report readable for humans.
        if let Err(e) = pretty_format_json_file_in_place(&path) {
            eprintln!(
                "[hotpath] failed to pretty-format report {}: {e}",
                path.display()
            );
            return;
        }

        println!("\n[hotpath] JSON report: {}", path.display());
        print_hotpath_tables_from_path(&path);
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn row_str<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn row_u64(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_u64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_owned())
}

fn print_section(v: &Value, key: &str) {
    let Some(section) = v.get(key) else {
        return;
    };
    let elapsed = row_str(section, "time_elapsed");
    let Some(rows) = section.get("data").and_then(|x| x.as_array()) else {
        return;
    };

    println!("\n[hotpath] {key} (elapsed {elapsed})");

    let is_alloc = key == "functions_alloc";
    if is_alloc {
        println!(
            "{:<80} {:>7} {:>14} {:>14} {:>9}",
            "name", "calls", "total", "avg", "%total"
        );
        println!("{}", "-".repeat(80 + 7 + 14 + 14 + 9 + 5));
    } else {
        println!(
            "{:<80} {:>7} {:>12} {:>12} {:>12} {:>9}",
            "name", "calls", "total", "avg", "p95", "%total"
        );
        println!("{}", "-".repeat(80 + 7 + 12 + 12 + 12 + 9 + 5));
    }

    for row in rows.iter().take(ROW_LIMIT) {
        let name = row_str(row, "name");
        let calls = row_u64(row, "calls");
        let total = row_str(row, "total");
        let avg = row_str(row, "avg");
        let pct = row_str(row, "percent_total");
        if is_alloc {
            println!(
                "{:<80} {:>7} {:>14} {:>14} {:>9}",
                truncate(name, 80),
                calls,
                total,
                avg,
                pct
            );
        } else {
            let p95 = row_str(row, "p95");
            println!(
                "{:<80} {:>7} {:>12} {:>12} {:>12} {:>9}",
                truncate(name, 80),
                calls,
                total,
                avg,
                p95,
                pct
            );
        }
    }
}

/// Prints Hotpath function timing/allocation sections from a JSON report.
fn print_hotpath_tables_from_path(report_path: &Path) {
    let v = match read_json_with_retries(report_path) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("[hotpath] report not readable at {}", report_path.display());
            return;
        }
    };
    print_section(&v, "functions_timing");
    print_section(&v, "functions_alloc");
}
