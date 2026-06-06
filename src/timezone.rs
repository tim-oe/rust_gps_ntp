//! IANA timezone resolution, NVS cache, and background HTTP lookup.
//!
//! HTTP lookups run on a background worker thread; JSON field extraction is
//! host-testable without ESP-IDF dependencies.

#[cfg(target_os = "espidf")]
use anyhow::{Context, anyhow};
#[cfg(target_os = "espidf")]
use embedded_svc::http::{Method, client::Client as HttpClient};
#[cfg(target_os = "espidf")]
use embedded_svc::utils::io;
#[cfg(target_os = "espidf")]
use esp_idf_svc::http::client::EspHttpConnection;
#[cfg(target_os = "espidf")]
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};

#[cfg(target_os = "espidf")]
const NVS_NAMESPACE: &str = "rust_gps_ntp";
#[cfg(target_os = "espidf")]
const NVS_KEY_LOCAL_TZ: &str = "local_tz";

/// NVS-backed storage for resolved IANA timezone names.
#[cfg(target_os = "espidf")]
pub struct TimezoneStore {
    nvs: EspDefaultNvs,
}

#[cfg(target_os = "espidf")]
impl TimezoneStore {
    /// Open the timezone cache namespace in the default NVS partition.
    ///
    /// # Parameters
    /// - `partition`: Default NVS partition handle taken at boot.
    ///
    /// # Returns
    /// - `Ok(TimezoneStore)` when the namespace opens successfully.
    /// - `Err` when the NVS namespace cannot be created or opened.
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)
            .map_err(|e| anyhow!("failed to open NVS namespace {NVS_NAMESPACE}: {e}"))?;
        Ok(Self { nvs })
    }

    /// Load a cached IANA timezone string from NVS.
    ///
    /// # Parameters
    /// - `self`: Open timezone cache store.
    ///
    /// # Returns
    /// - `Ok(Some(String))` when a cached timezone name is present.
    /// - `Ok(None)` when no value has been stored yet.
    /// - `Err` when the NVS read fails.
    pub fn load_cached(&self) -> anyhow::Result<Option<String>> {
        let mut buf = [0_u8; 64];
        self.nvs
            .get_str(NVS_KEY_LOCAL_TZ, &mut buf)
            .map(|opt| opt.map(str::to_owned))
            .map_err(|e| anyhow!("failed to read NVS key {NVS_KEY_LOCAL_TZ}: {e}"))
    }

    /// Persist an IANA timezone string to NVS.
    ///
    /// # Parameters
    /// - `self`: Open timezone cache store.
    /// - `tz_name`: IANA timezone identifier to store.
    ///
    /// # Returns
    /// - `Ok(())` when the value is written successfully.
    /// - `Err` when the NVS write fails.
    pub fn save(&mut self, tz_name: &str) -> anyhow::Result<()> {
        self.nvs
            .set_str(NVS_KEY_LOCAL_TZ, tz_name)
            .map_err(|e| anyhow!("failed to write NVS key {NVS_KEY_LOCAL_TZ}: {e}"))
    }
}

/// Resolve a timezone name from latitude and longitude using online APIs.
///
/// # Parameters
/// - `lat`: Latitude in decimal degrees.
/// - `lon`: Longitude in decimal degrees.
///
/// # Returns
/// - `Ok(Some(String))` when an IANA timezone name is resolved.
/// - `Ok(None)` when providers respond but contain no usable timezone field.
/// - `Err` when HTTP transport or parsing fails.
#[cfg(target_os = "espidf")]
pub fn fetch_timezone_for_coords(lat: f32, lon: f32) -> anyhow::Result<Option<String>> {
    // Primary provider: Open-Meteo (no key required).
    let open_meteo_url = format!(
        "http://api.open-meteo.com/v1/forecast?latitude={lat:.6}&longitude={lon:.6}&current=temperature_2m&timezone=auto"
    );
    if let Some(tz) = fetch_timezone_from_url(&open_meteo_url)
        .context("timezone lookup request failed (open-meteo)")?
    {
        return Ok(Some(tz));
    }

    // Fallback provider: GeoNames demo account (best-effort only; can be rate-limited).
    let geonames_url =
        format!("http://api.geonames.org/timezoneJSON?lat={lat:.6}&lng={lon:.6}&username=demo");
    fetch_timezone_from_url(&geonames_url).context("timezone lookup request failed (geonames)")
}

/// Background worker that performs blocking HTTP timezone lookups off the main loop.
#[cfg(target_os = "espidf")]
pub struct TimezoneWorker {
    request_tx: std::sync::mpsc::Sender<(f32, f32)>,
    result_rx: std::sync::mpsc::Receiver<anyhow::Result<Option<String>>>,
    pending: bool,
    _handle: std::thread::JoinHandle<()>,
}

#[cfg(target_os = "espidf")]
impl TimezoneWorker {
    /// Spawn a worker thread that executes HTTP lookups off the main loop.
    ///
    /// # Parameters
    /// - None.
    ///
    /// # Returns
    /// - `Ok(TimezoneWorker)` when the background thread starts successfully.
    /// - `Err` when thread spawn fails.
    pub fn spawn() -> anyhow::Result<Self> {
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("tz_lookup".into())
            .stack_size(12_000)
            .spawn(move || {
                while let Ok((lat, lon)) = request_rx.recv() {
                    let result = fetch_timezone_for_coords(lat, lon);
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            })
            .context("failed to spawn timezone lookup worker")?;

        Ok(Self {
            request_tx,
            result_rx,
            pending: false,
            _handle: handle,
        })
    }

    /// Return whether a lookup request is currently in flight.
    ///
    /// # Parameters
    /// - `self`: Background timezone worker.
    ///
    /// # Returns
    /// - `true` while a queued lookup has not yet completed.
    /// - `false` when the worker is idle and accepts new requests.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Queue a coordinate lookup when no request is pending.
    ///
    /// # Parameters
    /// - `self`: Background timezone worker.
    /// - `lat`: Latitude in decimal degrees.
    /// - `lon`: Longitude in decimal degrees.
    ///
    /// # Returns
    /// - `true` when the request is queued successfully.
    /// - `false` when a request is already pending or the worker channel is closed.
    pub fn try_request(&mut self, lat: f32, lon: f32) -> bool {
        if self.pending {
            return false;
        }
        match self.request_tx.send((lat, lon)) {
            Ok(()) => {
                self.pending = true;
                true
            }
            Err(_) => false,
        }
    }

    /// Poll for a completed lookup result without blocking the main loop.
    ///
    /// # Parameters
    /// - `self`: Background timezone worker.
    ///
    /// # Returns
    /// - `None` when no completed result is available yet.
    /// - `Some(Ok(Some(name)))` when a timezone name was resolved.
    /// - `Some(Ok(None))` when lookup succeeded but returned no timezone.
    /// - `Some(Err(..))` when lookup failed or the worker disconnected.
    pub fn poll(&mut self) -> Option<anyhow::Result<Option<String>>> {
        use std::sync::mpsc::TryRecvError;

        match self.result_rx.try_recv() {
            Ok(result) => {
                self.pending = false;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = false;
                Some(Err(anyhow!("timezone worker disconnected")))
            }
        }
    }
}

/// Perform one HTTP GET and extract a timezone field from the JSON body.
///
/// # Parameters
/// - `url`: Fully formed timezone lookup URL.
///
/// # Returns
/// - `Ok(Some(String))` when a supported timezone field is found.
/// - `Ok(None)` when the response parses but contains no timezone field.
/// - `Err` on HTTP, read, or UTF-8 failures.
#[cfg(target_os = "espidf")]
fn fetch_timezone_from_url(url: &str) -> anyhow::Result<Option<String>> {
    let mut client = HttpClient::wrap(
        EspHttpConnection::new(&Default::default())
            .context("failed to create HTTP connection for timezone lookup")?,
    );
    let request = client
        .request(Method::Get, url, &[])
        .context("failed to create timezone lookup request")?;
    let mut response = request
        .submit()
        .context("failed to execute timezone lookup request")?;
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(anyhow!("timezone lookup HTTP status {status}"));
    }

    let mut buf = [0_u8; 1536];
    let bytes_read = io::try_read_full(&mut response, &mut buf)
        .map_err(|e| anyhow!("failed reading timezone lookup body: {}", e.0))?;
    let body = std::str::from_utf8(&buf[..bytes_read])
        .context("timezone lookup body is not valid UTF-8")?;

    Ok(extract_json_string_field(body, "timezone")
        .or_else(|| extract_json_string_field(body, "timezoneId"))
        .or_else(|| extract_json_string_field(body, "ianaTimeZoneId")))
}

/// Extract a JSON string field value from a minimal API response body.
///
/// # Parameters
/// - `json`: Raw JSON response text.
/// - `key`: Object key to locate (for example `"timezone"` or `"timezoneId"`).
///
/// # Returns
/// - `Some(String)` when the key exists and its value is a JSON string.
/// - `None` when the key is missing or the value is not a quoted string.
pub fn extract_json_string_field(json: &str, key: &str) -> Option<String> {
    let key_needle = format!("\"{key}\"");
    let key_pos = json.find(&key_needle)?;
    let after_key = &json[key_pos + key_needle.len()..];
    let colon_pos = after_key.find(':')?;
    let mut tail = &after_key[colon_pos + 1..];
    tail = tail.trim_start();
    if !tail.starts_with('"') {
        return None;
    }
    tail = &tail[1..];
    let end = tail.find('"')?;
    Some(tail[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::extract_json_string_field;

    #[test]
    fn extract_json_string_field_rejects_non_string_value() {
        let body = r#"{"timezone":123}"#;
        assert_eq!(extract_json_string_field(body, "timezone"), None);
    }

    #[test]
    fn extract_json_string_field_reads_timezone_key() {
        let body = r#"{"latitude":38.9,"longitude":-90.2,"timezone":"America/Chicago"}"#;
        assert_eq!(
            extract_json_string_field(body, "timezone"),
            Some("America/Chicago".to_owned())
        );
    }

    #[test]
    fn extract_json_string_field_reads_geonames_alias() {
        let body = r#"{"timezoneId":"Europe/Berlin","status":"OK"}"#;
        assert_eq!(
            extract_json_string_field(body, "timezoneId"),
            Some("Europe/Berlin".to_owned())
        );
    }

    #[test]
    fn extract_json_string_field_missing_key_returns_none() {
        let body = r#"{"latitude":0.0,"longitude":0.0}"#;
        assert_eq!(extract_json_string_field(body, "timezone"), None);
    }
}
