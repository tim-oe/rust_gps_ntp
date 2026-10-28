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
//! without pulling the card, and a `/files` page listing the card root with
//! per-file delete (POST only, top-level files only, never the ready marker)
//! for clearing out cruft.  Appends, reads, and deletes all run on the SD
//! worker ([`storage::call`]), so a stuck card transfer is the worker's
//! problem and the HTTP task never enters the SPI driver.

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

/// Upper bound on a `POST /files/delete` form body.
const DELETE_BODY_MAX: usize = 512;

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
/// Runs on the SD worker, which also runs HTTP reads, so a flush cannot
/// start in the middle of a chunk.
fn flush_to_card() -> anyhow::Result<()> {
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
                match storage::call("sd log", flush_to_card) {
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
/// Responses are `text/plain`, streamed in [`HTTP_CHUNK_BYTES`] pieces.  Each
/// chunk is read on the SD worker, so the writer cannot append mid-chunk.
/// There is
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
            resp.write_all(
                b"rust_gps_ntp log mirror\n  /log    current\n  /log/1  rotated\n  /files  card root listing (delete via form)\n",
            )?;
            Ok(())
        })
        .context("route /")?;
    server
        .fn_handler::<anyhow::Error, _>("/files", Method::Get, serve_listing)
        .context("route /files")?;
    server
        .fn_handler::<anyhow::Error, _>("/files/delete", Method::Post, handle_delete)
        .context("route /files/delete")?;
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
    let path_owned = path.to_owned();
    let len = storage::call("sd stat", {
        let path_owned = path_owned.clone();
        move || fs::metadata(path_owned).map(|meta| meta.len())
    });
    let Ok(len) = len else {
        let mut resp = req.into_response(404, None, &[("Content-Type", "text/plain")])?;
        resp.write_all(b"no such log\n")?;
        return Ok(());
    };
    let len_header = len.to_string();
    let mut resp = req.into_response(
        200,
        None,
        &[
            ("Content-Type", "text/plain"),
            ("Content-Length", &len_header),
        ],
    )?;
    let mut offset = 0_u64;
    while offset < len {
        let chunk = read_chunk(&path_owned, offset, HTTP_CHUNK_BYTES).context("read log")?;
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(chunk.len() as u64);
        resp.write_all(&chunk)?;
    }
    Ok(())
}

/// Read one slice of `path` on the SD worker.
fn read_chunk(path: &str, offset: u64, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let path = path.to_owned();
    storage::call("sd read", move || {
        let mut file = fs::File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0_u8; max];
        let n = file.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    })
}

/// Minimal HTML escaping for file names rendered into the listing page.
fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `GET /files`: HTML listing of the card root with a delete button per file.
///
/// Directories are listed but not deletable; the ready marker is listed as
/// protected.  Deleting goes through `POST /files/delete` so a plain page
/// load (or a link prefetch) can never remove anything.
fn serve_listing(
    req: embedded_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
) -> anyhow::Result<()> {
    let entries = storage::call("sd list", || {
        let mut entries: Vec<(String, Option<u64>)> = Vec::new();
        if let Ok(dir) = fs::read_dir(storage::MOUNT_POINT) {
            for entry in dir.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let size = entry
                    .metadata()
                    .ok()
                    .filter(|meta| meta.is_file())
                    .map(|meta| meta.len());
                entries.push((name, size));
            }
        }
        entries.sort();
        entries
    });

    let mut page = String::from(
        "<!doctype html><title>SD card</title><style>body{font:14px monospace}td{padding:2px 10px}</style><h2>/sdcard</h2><table>",
    );
    let marker_name = storage::READY_MARKER.rsplit('/').next().unwrap_or_default();
    for (name, size) in &entries {
        let escaped = html_escape(name);
        let _ = write!(page, "<tr><td>{escaped}</td>");
        match size {
            None => {
                let _ = write!(page, "<td>dir</td><td></td></tr>");
            }
            Some(_) if name == marker_name => {
                let _ = write!(
                    page,
                    "<td>{}</td><td>protected</td></tr>",
                    size.unwrap_or(0)
                );
            }
            Some(bytes) => {
                let _ = write!(
                    page,
                    "<td>{bytes}</td><td><form method=post action=/files/delete style=\"display:inline\"><input type=hidden name=name value=\"{escaped}\"><button onclick=\"return confirm('Delete {escaped}?')\">delete</button></form></td></tr>"
                );
            }
        }
    }
    let _ = write!(
        page,
        "</table><p>{} entries. Logs: <a href=/log>/log</a> <a href=/log/1>/log/1</a></p>",
        entries.len()
    );
    let mut resp = req.into_response(200, None, &[("Content-Type", "text/html; charset=utf-8")])?;
    resp.write_all(page.as_bytes())?;
    Ok(())
}

/// Decode an `application/x-www-form-urlencoded` value (`+` and `%XX`).
fn form_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = &raw[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `POST /files/delete` with body `name=<file>`: remove one top-level file.
///
/// Rejects anything containing a path separator or `..`, directories, and the
/// ready marker.  Responds with a redirect back to `/files` on success so the
/// browser shows the updated listing.
fn handle_delete(
    mut req: embedded_svc::http::server::Request<
        &mut esp_idf_svc::http::server::EspHttpConnection<'_>,
    >,
) -> anyhow::Result<()> {
    let mut body = Vec::new();
    let mut chunk = [0_u8; 128];
    loop {
        let n = req.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
        if body.len() > DELETE_BODY_MAX {
            return respond_text(req, 413, "body too large\n");
        }
    }
    let body = String::from_utf8_lossy(&body);
    let Some(name) = body
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "name")
        .map(|(_, v)| form_decode(v))
    else {
        return respond_text(req, 400, "missing name\n");
    };
    let marker_name = storage::READY_MARKER.rsplit('/').next().unwrap_or_default();
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") || name == marker_name {
        return respond_text(req, 400, "refused\n");
    }
    let path = format!("{}/{name}", storage::MOUNT_POINT);
    let result = {
        let path = path.clone();
        storage::call("sd delete", move || match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => fs::remove_file(&path).map(|()| true),
            Ok(_) => Ok(false),
            Err(err) => Err(err),
        })
    };
    match result {
        Ok(true) => {
            log::info!("SD log: deleted {path} via HTTP");
            let _ = req.into_response(303, None, &[("Location", "/files")])?;
            Ok(())
        }
        Ok(false) => respond_text(req, 400, "not a file\n"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            respond_text(req, 404, "no such file\n")
        }
        Err(e) => {
            log::warn!("SD log: delete {path} failed: {e}");
            respond_text(req, 500, "delete failed\n")
        }
    }
}

/// Send a short plain-text response with the given status.
fn respond_text(
    req: embedded_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
    status: u16,
    text: &str,
) -> anyhow::Result<()> {
    let mut resp = req.into_response(status, None, &[("Content-Type", "text/plain")])?;
    resp.write_all(text.as_bytes())?;
    Ok(())
}
