# Stratum 1 NTP Server Implementation Outline (Rust on ESP32)

## 1. High-Level Implementation Architecture

### A. Precision Timing (PPS Discipline)
*   **Interrupt Service Routine (ISR):** Configure a hardware interrupt on the GPIO connected to the GPS PPS pin[cite: 1, 2].
*   **Timestamping:** Capture the high-resolution monotonic timer value (microseconds) at the exact rising edge of the PPS pulse[cite: 1, 2].
*   **Global State:** Update an Atomic or Mutex-protected structure with the latest "Timer Anchor" and the corresponding UTC second[cite: 1, 2].

### B. Time Context (NMEA Parsing)
*   **UART Stream:** Continuously read serial data from the GPS FeatherWing[cite: 1, 2].
*   **Message Extraction:** Parse GPRMC or GPZDA sentences to extract the current UTC date and time[cite: 2].
*   **Verification:** Only update the system's "Current Second" if the GPS indicates a valid 3D fix[cite: 2].

### C. Data Persistence & Holdover (Adalogger)
*   **RTC Backup:** Sync the PCF8523 RTC to the GPS time periodically. Use this as the reference if the GPS fix is lost to maintain "Stratum 2" status instead of failing entirely[cite: 1, 2].
*   **SD Logging:** Record crystal drift metrics (difference between ESP32 internal clock and PPS intervals) and NTP request volume for long-term stability analysis[cite: 1, 2].

### D. NTP Server Engine
*   **UDP Listener:** Bind to port 123 using an asynchronous task[cite: 1, 2].
*   **Packet Construction:**
    *   Set **Stratum** to 1[cite: 1, 2].
    *   Set **Reference ID** to "GPS"[cite: 1, 2].
    *   Calculate **Transmit Timestamp** by adding the offset (Current Time - Last PPS Anchor) to the base UTC second[cite: 2].
    *   Apply the **NTP Epoch Offset** (2,208,988,800 seconds)[cite: 1, 2].

---

## 2. Primary Crate Dependencies

### Hardware Abstraction & Runtime
*   **esp-hal**: Low-level access to ESP32 peripherals (GPIO, UART, I2C, SPI)[cite: 1, 2].
*   **embassy-executor**: Async runtime for managing concurrent hardware tasks (NTP Server, GPS Parser, Logger)[cite: 2].
*   **embassy-time**: Handling high-resolution durations and instants for NTP packet math[cite: 2].
*   **esp-wifi**: Network stack and Wi-Fi driver for `no_std` environments[cite: 1].

### Peripheral Drivers
*   **nmea**: Zero-allocation NMEA sentence parser for GPS data[cite: 1, 2].
*   **pcf8523**: I2C driver for the Adalogger's Real-Time Clock[cite: 1, 2].
*   **embedded-sdmmc**: SPI-based driver for logging data to the SD card[cite: 1, 2].

### Utilities
*   **chrono**: (Optional, if using `alloc`) for easier calendar date manipulations, though manual math is often preferred in `no_std`[cite: 1].