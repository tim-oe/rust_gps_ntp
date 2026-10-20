//! Mirror `info`-and-above log records to a file on the microSD card.
//!
//! The console logger ([`esp_idf_svc::log::EspLogger`]) is wrapped rather than
//! replaced: every record still goes to the USB console exactly as before, and
//! records at [`SINK_LEVEL`] or more severe are additionally queued for the
//! card.  The queue is drained by a dedicated low-priority thread, so the
//! main loop and UI task never block on SPI/FAT I/O while logging — a stall
//! there is precisely the kind of event this log exists to catch.
//!
//! Lines are prefixed with the same millisecond uptime stamp the console
//! shows, so a serial capture and the card log can be correlated directly.
//! The file is opened, appended, and closed on every flush so data survives a
//! power pull; it rolls over to [`ROTATED_PATH`] once it exceeds
//! [`ROTATE_AT_BYTES`].
//!
//! [`start_http_server`] exposes the current and rotated files read-only at
//! `http://<device>/log` and `/log/1` so a run can be collected over the LAN
//! without pulling the card.  A single [`CARD_LOCK`] serialises the writer's
//! appends with HTTP reads; the writer uses `try_lock` and simply defers a
//! flush while a download is in progress.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use anyhow::Context;
use embedded_svc::http::Method;
use embedded_svc::io::Write as _;
use esp_idf_svc::http::server::{Configuration as HttpConfiguration, EspHttpServer};
use esp_idf_svc::log::EspLogger;
use log::{Level, Log, Metadata, Record};

use crate::storage;

/// Records at this level or more severe are mirrored to the card.
const SINK_LEVEL: Level = Level::Info;
/// Current log file on the mounted card.
pub const LOG_PATH: &str = "/sdcard/gps-ntp.log";
/// Previous generation after rotation.
pub const ROTATED_PATH: &str = "/sdcard/gps-ntp.1.log";
/// Rotate when the current file grows past this size.
const ROTATE_AT_BYTES: u64 = 8 * 1024 * 1024;
/// Upper bound on queued-but-unwritten text; further lines are dropped and
/// counted so a burst cannot exhaust heap while the card is slow or absent.
const QUEUE_MAX_BYTES: usize = 16 * 1024;
/// How often the writer thread drains the queue to the card.
const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// Writer thread stack; FATFS write path plus a few line buffers.
const WRITER_STACK_BYTES: usize = 6144;
/// Writer thread priority: below the UI task (5) and far below the main task
/// (10), so card I/O only ever runs in otherwise idle time.
const WRITER_PRIORITY: u8 = 2;
/// HTTP server task stack: file streaming plus the httpd request machinery.
const HTTP_STACK_BYTES: usize = 8192;
/// Chunk size for streaming a log file over HTTP.
const HTTP_CHUNK_BYTES: usize = 2048;

/// Serialises card access between the writer thread and HTTP downloads.
static CARD_LOCK: Mutex<()> = Mutex::new(());

struct Queue {
    lines: VecDeque<String>,
    bytes: usize,
    /// Lines discarded since the last successful flush because the queue was full.
    dropped: u32,
}

/// Console logger plus a bounded queue of lines destined for the card.
pub struct SdLogger {
    console: EspLogger,
    queue: Mutex<Queue>,
}

static LOGGER: SdLogger = SdLogger::new();

impl SdLogger {
    const fn new() -> Self {
        Self {
            console: EspLogger::new(),
            queue: Mutex::new(Queue {
                lines: VecDeque::new(),
                bytes: 0,
                dropped: 0,
            }),
        }
    }

    fn enqueue(&self, line: String) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.bytes + line.len() > QUEUE_MAX_BYTES {
            q.dropped = q.dropped.saturating_add(1);
            return;
        }
        q.bytes += line.len();
        q.lines.push_back(line);
    }

    /// Take everything queued so far, leaving the queue empty.
    fn drain(&self) -> (Vec<String>, u32) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let lines = q.lines.drain(..).collect();
        q.bytes = 0;
        (lines, core::mem::take(&mut q.dropped))
    }
}

impl Log for SdLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.console.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        self.console.log(record);
        if record.level() > SINK_LEVEL || !self.enabled(record.metadata()) {
            return;
        }
        // SAFETY: argument-free read of the boot-relative millisecond counter
        // used by the ESP-IDF console logger; safe from any task context.
        let uptime_ms = unsafe { esp_idf_svc::sys::esp_log_timestamp() };
        let mut line = String::new();
        // Formatting into a String cannot fail; ignore the fmt::Result.
        let _ = writeln!(
            line,
            "{uptime_ms} {} {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        self.enqueue(line);
    }

    fn flush(&self) {}
}

/// Install the card-mirroring logger as the global `log` backend.
///
/// Call once at boot in place of `EspLogger::initialize_default()`.  Until
/// [`start_writer`] runs, qualifying records simply accumulate in the bounded
/// queue, so early boot messages are captured once the card comes up.
pub fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        LOGGER.console.initialize();
    }
}

/// Append queued lines to the card, rotating first if the file is too large.
///
/// Returns without draining when an HTTP download holds [`CARD_LOCK`]; the
/// lines stay queued for the next cycle.
fn flush_to_card() -> anyhow::Result<()> {
    let Ok(_card) = CARD_LOCK.try_lock() else {
        return Ok(());
    };
    let (lines, dropped) = LOGGER.drain();
    if lines.is_empty() && dropped == 0 {
        return Ok(());
    }
    if let Ok(meta) = fs::metadata(LOG_PATH)
        && meta.len() > ROTATE_AT_BYTES
    {
        // FATFS rename does not overwrite; clear the old generation first.
        let _ = fs::remove_file(ROTATED_PATH);
        fs::rename(LOG_PATH, ROTATED_PATH).context("rotate log")?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(LOG_PATH)
        .context("open log")?;
    for line in &lines {
        file.write_all(line.as_bytes()).context("write log")?;
    }
    if dropped > 0 {
        writeln!(
            file,
            "- WARN rust_gps_ntp::sdlog: {dropped} line(s) dropped (queue full)"
        )
        .context("write log")?;
    }
    file.flush().context("flush log")?;
    Ok(())
}

/// Spawn the writer thread.  Call after the card is mounted.
///
/// # Returns
/// - `Ok(JoinHandle)` that MUST be kept alive for the firmware lifetime
///   (dropping it detaches rather than stops the thread, but keeping it makes
///   ownership explicit alongside the other board keep-alives).
/// - `Err` when the thread configuration or spawn fails.
pub fn start_writer() -> anyhow::Result<thread::JoinHandle<()>> {
    use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;

    ThreadSpawnConfiguration {
        priority: WRITER_PRIORITY,
        ..Default::default()
    }
    .set()
    .context("failed to configure SD log writer priority")?;

    let handle = thread::Builder::new()
        .name("sd_log".into())
        .stack_size(WRITER_STACK_BYTES)
        .spawn(|| {
            let mut failed_once = false;
            loop {
                thread::sleep(FLUSH_INTERVAL);
                if !storage::is_ready() {
                    continue;
                }
                match flush_to_card() {
                    Ok(()) => failed_once = false,
                    Err(err) if !failed_once => {
                        failed_once = true;
                        log::warn!("SD log: {err:#}");
                    }
                    Err(_) => {}
                }
            }
        })
        .context("failed to spawn SD log writer")?;

    ThreadSpawnConfiguration::default()
        .set()
        .context("failed to restore thread spawn configuration")?;

    log::info!(
        "SD log: mirroring {SINK_LEVEL}+ to {LOG_PATH} (flush every {} s)",
        FLUSH_INTERVAL.as_secs()
    );
    Ok(handle)
}

/// Start a read-only HTTP server exposing the log files on port 80.
///
/// Routes: `/` (index), `/log` (current file), `/log/1` (rotated file).
/// Responses are `text/plain`, streamed in [`HTTP_CHUNK_BYTES`] pieces while
/// holding [`CARD_LOCK`], so the writer never appends mid-download.  There is
/// no authentication: the endpoint is intended for the same trusted LAN that
/// is allowed to use the NTP service, and the log contains the STA SSID and
/// client IP addresses.
///
/// # Returns
/// - `Ok(EspHttpServer)` that MUST be kept alive for the server to run.
/// - `Err` when the server or a route cannot be registered.
pub fn start_http_server() -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpConfiguration {
        stack_size: HTTP_STACK_BYTES,
        ..Default::default()
    })
    .context("failed to start log HTTP server")?;

    server
        .fn_handler::<anyhow::Error, _>("/", Method::Get, |req| {
            let mut resp = req.into_response(200, None, &[("Content-Type", "text/plain")])?;
            resp.write_all(b"rust_gps_ntp log mirror\n  /log    current\n  /log/1  rotated\n")?;
            Ok(())
        })
        .context("route /")?;
    server
        .fn_handler::<anyhow::Error, _>("/log", Method::Get, |req| serve_file(req, LOG_PATH))
        .context("route /log")?;
    server
        .fn_handler::<anyhow::Error, _>("/log/1", Method::Get, |req| serve_file(req, ROTATED_PATH))
        .context("route /log/1")?;

    log::info!(
        "SD log: HTTP download at http://{}/log",
        env!("DEVICE_HOSTNAME")
    );
    Ok(server)
}

/// Stream `path` to the client, or 404 when it does not exist.
fn serve_file(
    req: embedded_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
    path: &str,
) -> anyhow::Result<()> {
    use std::io::Read as _;

    let _card = CARD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(mut file) = fs::File::open(path) else {
        let mut resp = req.into_response(404, None, &[("Content-Type", "text/plain")])?;
        resp.write_all(b"no such log\n")?;
        return Ok(());
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0).to_string();
    let mut resp = req.into_response(
        200,
        None,
        &[("Content-Type", "text/plain"), ("Content-Length", &len)],
    )?;
    let mut buf = [0_u8; HTTP_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).context("read log")?;
        if n == 0 {
            break;
        }
        resp.write_all(&buf[..n])?;
    }
    Ok(())
}
