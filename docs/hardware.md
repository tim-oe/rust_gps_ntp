# Hardware Notes

Target hardware:

- Adafruit ESP32-S3 TFT Feather (project default; S2 Feather docs remain useful reference)
- Adafruit Ultimate GPS FeatherWing

Note: the connected board currently identifies as ESP32-S3, and project
build/flash defaults are set to ESP32-S3 accordingly.

References:

- [Adafruit ESP32-S2 TFT Feather](https://learn.adafruit.com/adafruit-esp32-s2-tft-feather)
- [Adafruit Ultimate GPS FeatherWing](https://learn.adafruit.com/adafruit-ultimate-gps-featherwing)
- [Adafruit Adalogger FeatherWing pinouts](https://learn.adafruit.com/adafruit-adalogger-featherwing/pinouts)

## Assembly

1. Solder Feather headers to both boards.
2. Stack the Ultimate GPS FeatherWing onto the ESP32 Feather.
3. Stack the [Adalogger FeatherWing](https://learn.adafruit.com/adafruit-adalogger-featherwing/overview) above the GPS wing (or below the MCU board if stacking order differs).
4. Insert a CR1220 coin cell in the Adalogger for RTC battery backup.
5. Insert a FAT-formatted microSD card in the Adalogger socket (optional; firmware mounts at `/sdcard` when present).
6. Connect USB-C and confirm the board enumerates as `/dev/ttyACM0`.

## Signal expectations

When stacked as a FeatherWing, the GPS board uses shared Feather rails:

- Power and ground from Feather header
- Serial lines for NMEA traffic (GPS -> MCU UART receive)
- Optional PPS line can be jumper-wired to any interrupt-capable GPIO

## Firmware pin map (matches `src/app.rs` constants)

- GPS UART: TX=`GPIO1`, RX=`GPIO2`
- PPS input: `GPIO12` (rising-edge interrupt)
- TFT SPI: SCK=`GPIO36`, MOSI=`GPIO35`, CS=`GPIO7`, DC=`GPIO39`, RST=`GPIO40`, BL=`GPIO45`
- Button (page toggle / wake): `GPIO0` (active low with pull-up)
- Battery monitor I2C: SDA=`GPIO42`, SCL=`GPIO41` (MAX17048 or LC709203)
- Adalogger RTC (PCF8523): Feather I2C SDA/SCL → `GPIO42`/`GPIO41` @ `0x68`
- Adalogger microSD SPI ([pinouts](https://learn.adafruit.com/adafruit-adalogger-featherwing/pinouts)):
  - SCK=`GPIO36`, MOSI=`GPIO35`, MISO=`GPIO37` (shared with TFT SPI bus; TFT CS=`GPIO7`)
  - SD CS=`GPIO10` on ESP32-S3 TFT Feather + Adalogger (GPIO33 fallback; classic ESP32 uses GPIO33)

## Bring-up checklist

- GPS status LED blinks while searching, then slows after fix.
- Serial monitor shows firmware heartbeat.
- Monitor should display valid NMEA sentences after UART parser is active.
- After PPS is wired to `GPIO12`, firmware should observe a 1 Hz edge interrupt.

## Practical recommendations

- Keep GPS antenna with a clear sky view for first fix.
- For indoor testing, expect longer fix acquisition time.
- Add a CR1220 battery to the GPS Wing if you want faster warm starts.
- Add a CR1220 to the Adalogger wing so the PCF8523 keeps time across power loss.
- Format the microSD as FAT32 before first use; firmware writes a `.rust_gps_ntp_ready` marker at mount.
