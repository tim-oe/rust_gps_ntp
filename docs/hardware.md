# Hardware Notes

Target hardware:

- Adafruit ESP32-S2 TFT Feather
- Adafruit Ultimate GPS FeatherWing

Note: the connected board currently identifies as ESP32-S3, and project
build/flash defaults are set to ESP32-S3 accordingly.

References:

- [Adafruit ESP32-S2 TFT Feather](https://learn.adafruit.com/adafruit-esp32-s2-tft-feather)
- [Adafruit Ultimate GPS FeatherWing](https://learn.adafruit.com/adafruit-ultimate-gps-featherwing)

## Assembly

1. Solder Feather headers to both boards.
2. Stack the Ultimate GPS FeatherWing onto the ESP32-S2 Feather.
3. Connect USB-C and confirm the board enumerates as `/dev/ttyACM0`.

## Signal expectations

When stacked as a FeatherWing, the GPS board uses shared Feather rails:

- Power and ground from Feather header
- Serial lines for NMEA traffic (GPS -> MCU UART receive)
- Optional PPS line can be jumper-wired to any interrupt-capable GPIO

## Firmware pin map currently in code

- GPS UART: TX=`GPIO1`, RX=`GPIO2`
- PPS input: `GPIO13` (rising-edge interrupt)
- TFT SPI: SCK=`GPIO36`, MOSI=`GPIO35`, CS=`GPIO7`, DC=`GPIO39`, RST=`GPIO40`, BL=`GPIO45`
- Button (page toggle / wake): `GPIO0` (active low with pull-up)
- Battery monitor (MAX17048) I2C: SDA=`GPIO3`, SCL=`GPIO4`

## Bring-up checklist

- GPS status LED blinks while searching, then slows after fix.
- Serial monitor shows firmware heartbeat.
- After UART parser is added, monitor should display valid NMEA sentences.
- After PPS is wired, firmware should observe a 1 Hz edge interrupt.

## Practical recommendations

- Keep GPS antenna with a clear sky view for first fix.
- For indoor testing, expect longer fix acquisition time.
- Add a CR1220 battery to the GPS Wing if you want faster warm starts.
