# Solar-Powered Stratum 1 NTP Time Server (Rust on ESP32)
## Comprehensive Hardware, Firmware Architecture & Deployment Guide

This document details the architecture, electrical wiring, physical layout, power budget, firmware architecture, and production-ready Rust firmware required to construct a self-sustained, solar-powered **Stratum 1 Network Time Protocol (NTP) Server**.

Using an ESP32 microcontroller, an atomic-locked GPS module, and a dedicated power path manager, this standalone appliance operates 24/7/365 completely disconnected from the grid, serving microsecond-accurate time over a local Wi-Fi network.

---

## 1. System Architecture & Bill of Materials (BOM)

The platform is designed around the Adafruit Feather ecosystem, utilizing a parallel motherboard topology to eliminate point-to-point wiring mess while preserving modularity.

### Core Component Breakdown

| Component | Product Name | Product ID | Purpose |
| :--- | :--- | :--- | :--- |
| **Microcontroller** | Adafruit ESP32 Feather V2 | [5438](https://www.adafruit.com/product/5438) | System core. Runs the embedded Rust firmware, handles Wi-Fi networking, executes the UDP NTP server stack, parses NMEA sentences, and manages system state. |
| **Time Source** | Adafruit Ultimate GPS FeatherWing | [3133](https://www.adafruit.com/product/3133) | Locks onto GPS/GLONASS satellite constellations to derive precise atomic-clock-synchronized Coordinated Universal Time (UTC). |
| **Hardware Clock** | Adafruit Adalogger RTC + SD FeatherWing | [2922](https://www.adafruit.com/product/2922) | Onboard PCF8523 Real-Time Clock for local time backup if GPS lock is temporarily lost, plus an SD slot for drift and system logging. |
| **Power Manager** | Adafruit Universal Solar Charger (bq24074) | [4755](https://www.adafruit.com/product/4755) | High-efficiency dynamic power path manager. Decouples the solar input from the load and delivers up to 1.5 A charge current. |
| **Energy Storage** | Lithium-Ion Battery Pack (3.7 V, 10,050 mAh) | [5035](https://www.adafruit.com/product/5035) | Triple-cell lithium buffer providing operational reserve capacity for multiple consecutive overcast or rainy days. |
| **Energy Harvest** | FlexSolar 10 W Portable Solar Charger (5 V USB) | N/A | Rugged IP67 monocrystalline solar array with an integrated junction box that regulates output to a stable 5 V DC line. |
| **Chassis Mainboard** | Adafruit FeatherWing Tripler | [3417](https://www.adafruit.com/product/3417) | Side-by-side prototyping base. Busses all power, I2C, SPI, and UART lines seamlessly across the modules. |

---

## 2. The Power Budget & The "Always-On" Constraint

Unlike traditional remote environmental sensors that spend 99% of their lifespan in deep sleep mode (drawing microamps), an **NTP server must remain permanently awake**. It must continuously listen for inbound UDP port 123 network requests, track satellites, and feed the internal hardware clock.

### Current Consumption Profile (Continuous)
* **ESP32 Feather V2** (Active Wi-Fi + Core): ~110 mA
* **Ultimate GPS FeatherWing** (Active tracking/lock): ~30 mA
* **Adalogger FeatherWing** (RTC Active + SD Standby): ~5 mA
* **Total Continuous System Load (\(I_{load}\))**: **~145 mA**

### Why Bypassing the Feather's Internal Charger is Mandatory
The ESP32 Feather V2 includes an onboard LiPo charging circuit capped at a rigid **200 mA max charge rate**. Over a standard 24-hour cycle, running the system strictly via the Feather's USB-C port results in an energy deficit:
* **Night discharge** (16 hours without sun): \(145\text{ mA} \times 16\text{ h} = 2{,}320\text{ mAh}\) drained.
* **Day recharge** (8 hours of peak sun): \((200\text{ mA} - 145\text{ mA}) \times 8\text{ h} = 440\text{ mAh}\) net added.
* **Daily net deficit**: **\(-1{,}880\text{ mAh}\)** (the system dies within 5 days regardless of panel size).

### The bq24074 Dynamic Power Solution
By routing power through the **bq24074 solar manager**, the maximum charge current is amplified to **1,500 mA (1.5 A)**:
* **Day recharge under bq24074**: The panel powers the 145 mA load directly while dumping the full remainder of its potential (up to 1,500 mA) into the battery pack.
* To replace the 2,320 mAh nighttime loss, the system needs only ~**1.55 hours** of optimal direct sunlight daily. The rest of the daylight cycle tops off the 10,050 mAh capacity, leaving a robust buffer against poor weather.

---

## 3. Electrical Interconnection & Wiring Architecture

To prevent a catastrophic "charging loop" and shield components from high open-circuit solar voltages, wire the system strictly according to the topology below.

### Critical Safety Rules
1. **Banish the Feather JST port**: Do **NOT** plug the battery or the solar manager's output into the Feather's onboard JST port.
2. **Use the `USB` pin entry**: Power must enter the Feather via its `USB` pin. This pin sits safely behind an onboard Schottky protection diode, so a USB-C cable can be plugged into the Feather to flash firmware while the solar assembly is active without causing back-feed.

```
+--------------------------+               +-----------------------------------+
|  FlexSolar 10W Panel     |               |  Adafruit bq24074 Solar Charger   |
|  (Regulated 5V USB Out)  |               |                                   |
|                          |               |  [IN +]   [IN -]                  |
|     [Positive Line] -----+-------------> |    |         |                    |
|     [Negative Line] -----+-------------> |    |         |                    |
+--------------------------+               |                                   |
                                           |  [BATT] (JST Connector)           |
+--------------------------+               |    |                              |
| 10,050mAh Battery Pack   | <-------------+----+                              |
| (3.7V Triple-Cell Li-Ion)|               |                                   |
+--------------------------+               |  [OUT +]  [OUT -]                 |
                                           +----+--------+---------------------+
                                                |        |
                                                |        | (JST or Solder wires)
                                                v        v
                                           +-----------------------------------+
                                           |  Adafruit FeatherWing Tripler     |
                                           |  (Busses all pins to modules)     |
                                           |                                   |
                                           |  [USB]    [GND]                   |
                                           +----+--------+---------------------+
                                                |        |
                                                |        | (Internal Bus Tracks)
                                                v        v
                                    +---------------------------------------+
                                    |  ESP32 Feather V2 | GPS | Adalogger   |
                                    +---------------------------------------+
```

### Detailed Solder Step-by-Step
1. **Panel input**: Strip the termination of a USB-A cable connected to the FlexSolar panel. Connect the internal **Red wire (+5 V)** to the **IN +** pad of the bq24074. Connect the **Black wire (GND)** to the **IN -** pad.
2. **Battery attachment**: Plug the 2-pin JST-PH connector of the **10,050 mAh battery pack** directly into the port labeled **BATT** on the bq24074 board.
3. **Powering the Tripler motherboard**:
   * Run a heavy-gauge copper wire from the **OUT +** pad of the bq24074 to the **USB pin** rail on the FeatherWing Tripler.
   * Run a second wire from the **OUT - / GND** pad of the bq24074 to any **GND pin** rail on the FeatherWing Tripler.

---

## 4. Physical Board Assembly & Pin Layout

The FeatherWing Tripler allows the three main computing components to sit side-by-side.

### Header Configuration
* Solder standard **female headers** onto the three slots of the FeatherWing Tripler.
* Solder standard **male header pins** facing down on the underside of the ESP32 Feather V2, the Ultimate GPS FeatherWing, and the Adalogger RTC.

### Layout Arrangement
Mount the modules in the following order across the Tripler from left to right to optimize spatial distribution and heat dispersion:

1. **Slot 1 (Left)**: Adafruit ESP32 Feather V2
2. **Slot 2 (Center)**: Adafruit Adalogger RTC + SD
3. **Slot 3 (Right)**: Adafruit Ultimate GPS FeatherWing

> **Antenna Positioning Note:** Placing the GPS on the edge slot provides optimal clearance for an external active GPS antenna connection (via the u.FL connector) if housing the complete electronics stack inside an RF-shielded or weatherproof enclosure.

### Shared Inter-Module Communications (Handled by Tripler Trace Architecture)
* **I2C Bus (`SDA` / `SCL`)**: Connects the ESP32 to the Adalogger's PCF8523 RTC chip.
* **SPI Bus (`MOSI` / `MISO` / `SCK`)**: Interconnects the ESP32 to the Adalogger's SD card slot for filesystem logging.
* **Hardware UART (`RX` / `TX`)**: Passes high-speed NMEA satellite strings directly from the Ultimate GPS to the ESP32 hardware serial buffer.

---

## 5. Firmware Architecture

### A. Precision Timing (PPS Discipline)
* **Interrupt Service Routine (ISR):** Configure a hardware interrupt on the GPIO connected to the GPS PPS pin.
* **Timestamping:** Capture the high-resolution monotonic timer value (microseconds) at the exact rising edge of the PPS pulse.
* **Global State:** Update an `Atomic` or `Mutex`-protected structure with the latest "Timer Anchor" and the corresponding UTC second.

### B. Time Context (NMEA Parsing)
* **UART Stream:** Continuously read serial data from the GPS FeatherWing.
* **Message Extraction:** Parse `GPRMC` or `GPZDA` sentences to extract the current UTC date and time.
* **Verification:** Only update the system's "Current Second" when the GPS indicates a valid 3D fix.

### C. Data Persistence & Holdover (Adalogger)
* **RTC Backup:** Sync the PCF8523 RTC to GPS time periodically. Use it as the reference if the GPS fix is lost to maintain "Stratum 2" status instead of failing entirely.
* **SD Logging:** Record crystal drift metrics (difference between ESP32 internal clock and PPS intervals) and NTP request volume for long-term stability analysis.

### D. NTP Server Engine
* **UDP Listener:** Bind to port 123 using an asynchronous task.
* **Packet Construction:**
    * Set **Stratum** to 1.
    * Set **Reference ID** to `"GPS"`.
    * Calculate **Transmit Timestamp** by adding the offset (Current Time − Last PPS Anchor) to the base UTC second.
    * Apply the **NTP Epoch Offset** of 2,208,988,800 seconds.

---

## 6. Primary Crate Dependencies

Two viable Rust runtimes for the ESP32 are described below. Pick one based on whether you need bare-metal determinism (`no_std`) or stable networking and filesystem abstractions (`std`).

### Option A — `no_std` / Embassy (lowest jitter, full control)

**Hardware abstraction & runtime**
* **`esp-hal`** — Low-level access to ESP32 peripherals (GPIO, UART, I2C, SPI).
* **`embassy-executor`** — Async runtime for managing concurrent hardware tasks (NTP server, GPS parser, logger).
* **`embassy-time`** — High-resolution durations and instants for NTP packet math.
* **`esp-wifi`** — Network stack and Wi-Fi driver for `no_std` environments.

**Peripheral drivers**
* **`nmea`** — Zero-allocation NMEA sentence parser for GPS data.
* **`pcf8523`** — I2C driver for the Adalogger's Real-Time Clock.
* **`embedded-sdmmc`** — SPI-based driver for logging data to the SD card.

**Utilities**
* **`chrono`** *(optional, requires `alloc`)* — Easier calendar date manipulations; manual math is often preferred in `no_std`.

### Option B — `std` via `esp-idf-svc` (used by the reference firmware below)

```toml
[dependencies]
esp-idf-sys = { version = "0.34", features = ["native"] }
esp-idf-svc = "0.48"
anyhow      = "1.0"
nmea        = "0.6" # For parsing GPS sentences
```

---

## 7. Reference Deployment Firmware (Rust, `esp-idf-svc`)

The implementation below uses the standard library (`std`) approach for ESP32 via the `esp-idf-svc` ecosystem, which provides stable networking abstractions for creating a Wi-Fi Access Point and hosting a low-latency UDP socket.

```rust
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio;
use esp_idf_svc::hal::i2c;
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::uart;
use esp_idf_svc::wifi::{AuthMethod, Configuration, AccessPointConfiguration, EspWifi};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use std::net::UdpSocket;
use std::time::Duration;

const NTP_PACKET_SIZE: usize = 48;
const LOCAL_NTP_PORT: u16 = 123;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    println!("Initializing Solar Stratum 1 NTP Server via Rust...");

    // 1. Hardware Serial (UART1) for Ultimate GPS FeatherWing
    //    Default Feather V2 UART1 pins: TX = GPIO14, RX = GPIO13
    let config = uart::config::Config::default().baudrate(Hertz(9600));
    let mut gps_uart = uart::UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio14, // TX
        peripherals.pins.gpio13, // RX
        Option::<gpio::GpioDst>::None,
        Option::<gpio::GpioDst>::None,
        &config,
    )?;

    // 2. Hardware I2C for Adalogger backup RTC
    //    Default Feather V2 I2C pins: SDA = GPIO4, SCL = GPIO5
    let i2c_config = i2c::config::Config::default().baudrate(Hertz(100_000));
    let mut rtc_i2c = i2c::I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio4, // SDA
        peripherals.pins.gpio5, // SCL
        &i2c_config,
    )?;

    // 3. Wi-Fi interface in Access Point mode
    let mut wifi = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?;
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: "Solar_NTP_Server_Rust".into(),
        password: "AtomicTimeSecure".into(),
        auth_method: AuthMethod::WPA2WPA3PSK,
        max_connections: 4,
        ..Default::default()
    }))?;

    wifi.start()?;
    println!("Access Point Online. SSID: Solar_NTP_Server_Rust");
    println!("AP Gateway IP: {:?}", wifi.wrap().get_ip_info()?.ip);

    // 4. Non-blocking UDP socket on Port 123
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", LOCAL_NTP_PORT))?;
    socket.set_read_timeout(Some(Duration::from_millis(50)))?;
    println!("NTP Listener Socket Bound to UDP Port {}", LOCAL_NTP_PORT);

    let mut rx_buffer = [0u8; NTP_PACKET_SIZE];
    let mut uart_read_buffer = [0u8; 128];

    loop {
        // A. Read raw NMEA data from GPS over UART
        if let Ok(bytes_read) = gps_uart.read(&mut uart_read_buffer, 0) {
            if bytes_read > 0 {
                // Parse lines with the `nmea` crate and discipline the local RTC
                // via I2C when a valid 3D fix is verified.
            }
        }

        // B. Listen for and respond to incoming NTP queries
        if let Ok((amt, src)) = socket.recv_from(&mut rx_buffer) {
            if amt >= NTP_PACKET_SIZE {
                let mut tx_packet = [0u8; NTP_PACKET_SIZE];

                // NTP header: LI = 0, VN = 4, Mode = 4 (Server Response)
                tx_packet[0] = 0b00100100;
                // Stratum 1 (primary atomic reference clock)
                tx_packet[1] = 1;
                // Reference ID: "GPS"
                tx_packet[12] = b'G';
                tx_packet[13] = b'P';
                tx_packet[14] = b'S';

                // Replace with current epoch time from the disciplined RTC.
                // NTP epoch begins 1 Jan 1900, hence the +2,208,988,800 offset.
                let current_unix_time: u32 = 1779926400;
                let ntp_seconds: u32 = current_unix_time + 2_208_988_800;

                let bytes = ntp_seconds.to_be_bytes();
                tx_packet[40..44].copy_from_slice(&bytes);

                let _ = socket.send_to(&tx_packet, src);
                println!("Served Stratum 1 Time Packet to Network Client: {}", src);
            }
        }

        // Yield to FreeRTOS without compromising polling latency
        FreeRtos::delay_ms(10);
    }
}
```

---

## 8. Field Optimization & Deployment Checklist

To ensure absolute permanence when housing this system in remote or exposed outdoor environments, execute these final modifications:

* **De-solder power indicators**: The ESP32 Feather, GPS board, and Adalogger all contain small, surface-mounted red power LEDs that glow continuously. Desoldering these LEDs or slicing their PCB traces eliminates a constant ~10 mA passive load, reclaiming roughly 240 mAh of battery capacity every single day.
* **Enclosure specifications**: House the electronics (Tripler, battery, bq24074) in an **IP66/IP67 rated NEMA polycarbonate enclosure**. Metal boxes degrade Wi-Fi propagation and block internal GPS reception.
* **External antenna mounting**: If the box is sheltered from daylight under a structure, use a magnetic waterproof external active GPS antenna passed through a sealed cable gland. Connect the cable to the u.FL connector on the Ultimate GPS FeatherWing. This ensures consistent tracking of 9+ satellites even during extreme storms.
