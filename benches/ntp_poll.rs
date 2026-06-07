use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use rust_gps_ntp::ntp::NtpServer;
use std::net::UdpSocket;

const NTP_PACKET_LEN: usize = 48;

fn client_request() -> [u8; NTP_PACKET_LEN] {
    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = (4 << 3) | 3;
    req
}

fn synced_loopback_server() -> NtpServer {
    let mut server = NtpServer::new_loopback().expect("loopback server");
    server.update_gps_utc_seconds(1_700_000_000);
    server.observe_pps_pulse(None, 0);
    server
}

fn bench_ntp(c: &mut Criterion) {
    let mut group = c.benchmark_group("ntp");

    // Fresh server per batch avoids the 2 s per-client rate limiter polluting timings.
    group.bench_function("poll_single_client_request", |b| {
        b.iter_batched(
            || {
                let server = synced_loopback_server();
                let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
                let server_addr = server.loopback_addr();
                let req = client_request();
                let buf = [0_u8; 64];
                (server, client, server_addr, req, buf)
            },
            |(mut server, client, server_addr, req, mut buf)| {
                client.send_to(black_box(&req), server_addr).expect("send");
                server.poll(black_box(true)).expect("poll");
                client.recv_from(&mut buf).expect("recv");
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("ntp_snapshot_synced", |b| {
        let mut server = NtpServer::new_loopback().expect("loopback server");
        server.update_gps_utc_seconds(1_700_000_000);
        server.observe_pps_pulse(None, 0);

        b.iter(|| black_box(server.ntp_snapshot(true)));
    });

    group.finish();
}

criterion_group!(benches, bench_ntp);
criterion_main!(benches);
