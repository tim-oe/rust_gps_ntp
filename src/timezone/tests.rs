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
