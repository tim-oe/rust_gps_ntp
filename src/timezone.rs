//! Timezone resolution and cache helpers for GPS coordinates.
//!
//! The implementation uses a lightweight HTTP lookup and stores the resulting
//! IANA timezone name in NVS for reuse across boots.

use anyhow::{Context, anyhow};
use embedded_svc::http::{Method, client::Client as HttpClient};
use embedded_svc::utils::io;
use esp_idf_svc::http::client::EspHttpConnection;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvs};

const NVS_NAMESPACE: &str = "rust_gps_ntp";
const NVS_KEY_LOCAL_TZ: &str = "local_tz";

/// NVS-backed storage for resolved timezone values.
pub struct TimezoneStore {
    nvs: EspDefaultNvs,
}

impl TimezoneStore {
    /// Open timezone cache namespace in default NVS partition.
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)
            .map_err(|e| anyhow!("failed to open NVS namespace {NVS_NAMESPACE}: {e}"))?;
        Ok(Self { nvs })
    }

    /// Load cached IANA timezone string from NVS.
    pub fn load_cached(&self) -> anyhow::Result<Option<String>> {
        let mut buf = [0_u8; 64];
        self.nvs
            .get_str(NVS_KEY_LOCAL_TZ, &mut buf)
            .map(|opt| opt.map(str::to_owned))
            .map_err(|e| anyhow!("failed to read NVS key {NVS_KEY_LOCAL_TZ}: {e}"))
    }

    /// Save IANA timezone string to NVS.
    pub fn save(&mut self, tz_name: &str) -> anyhow::Result<()> {
        self.nvs
            .set_str(NVS_KEY_LOCAL_TZ, tz_name)
            .map_err(|e| anyhow!("failed to write NVS key {NVS_KEY_LOCAL_TZ}: {e}"))
    }
}

/// Resolve timezone from latitude/longitude using an online API.
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

fn extract_json_string_field<'a>(json: &'a str, key: &str) -> Option<String> {
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
