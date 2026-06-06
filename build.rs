fn main() {
    println!("cargo:rerun-if-env-changed=WIFI_SSID");
    println!("cargo:rerun-if-env-changed=WIFI_PASS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }
}
