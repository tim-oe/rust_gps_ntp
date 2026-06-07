use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rust_gps_ntp::gps::{self, GpsSnapshot};

const RMC: &str = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
const GGA: &str = "$GPGGA,123520,4807.038,N,01131.000,E,1,08,1.0,545.4,M,46.9,M,,*45";

fn bench_gps(c: &mut Criterion) {
    c.bench_function("gps/nmea_checksum_valid", |b| {
        b.iter(|| gps::nmea_checksum_valid(black_box(RMC)))
    });

    c.bench_function("gps/parse_rmc", |b| {
        b.iter(|| {
            let mut snap = GpsSnapshot::default();
            gps::parse_rmc(black_box(RMC), &mut snap)
        })
    });

    c.bench_function("gps/parse_gga", |b| {
        b.iter(|| {
            let mut snap = GpsSnapshot::default();
            gps::parse_gga(black_box(GGA), &mut snap)
        })
    });

    c.bench_function("gps/parse_rmc_and_gga", |b| {
        b.iter(|| {
            let mut snap = GpsSnapshot::default();
            gps::parse_rmc(black_box(RMC), &mut snap);
            gps::parse_gga(black_box(GGA), &mut snap);
        })
    });
}

criterion_group!(benches, bench_gps);
criterion_main!(benches);
