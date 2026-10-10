//! Live SocketCAN round trip on `vcan0` (skipped when the interface is absent).
//!
//! Without root:
//! `unshare -rn sh -c 'ip link add vcan0 type vcan && ip link set vcan0 mtu 72 up && cargo test -p canbus --test vcan'`

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use canbus::Frame;
use canbus::frame::flags;
use canbus::socket::{LiveBus, list_interfaces};

fn collect(bus: &LiveBus, n: usize, timeout: Duration) -> Vec<Frame> {
    let start = Instant::now();
    let mut out = Vec::new();
    while out.len() < n && start.elapsed() < timeout {
        bus.drain(&mut out);
        std::thread::sleep(Duration::from_millis(5));
    }
    out
}

#[test]
fn vcan_round_trip() {
    let Ok(tx) = LiveBus::open("vcan0", || {}) else {
        eprintln!("vcan0 not present, skipping");
        return;
    };
    assert!(list_interfaces().iter().any(|i| i == "vcan0"), "interface listing");
    let rx = LiveBus::open("vcan0", || {}).unwrap();

    // Classic standard, extended and remote frames.
    let frames = [
        Frame::new(0x123, false, &[1, 2, 3]),
        Frame::new(0x1ABC_DEF0, true, &[0xAA; 8]),
        Frame { flags: flags::RTR, len: 4, ..Frame::new(0x7FF, false, &[]) },
    ];
    for f in &frames {
        tx.send(f).unwrap();
    }
    let got = collect(&rx, 3, Duration::from_secs(2));
    assert_eq!(got.len(), 3, "{got:?}");
    for (a, b) in frames.iter().zip(&got) {
        assert_eq!((a.id, a.flags, a.data()), (b.id, b.flags, b.data()));
        assert!(b.ts > 1.6e9, "kernel timestamp: {}", b.ts);
    }

    // CAN-FD (needs `mtu 72` on the interface).
    let fd = Frame { flags: flags::FD | flags::BRS, ..Frame::new(0x100, false, &[7; 20]) };
    if tx.send(&fd).is_ok() {
        let got = collect(&rx, 1, Duration::from_secs(2));
        assert_eq!(got[0].data().len(), 20);
        assert_eq!(got[0].flags & (flags::FD | flags::BRS), flags::FD | flags::BRS);
    }

    // Periodic scheduler: 10 ms period for 250 ms.
    tx.scheduler.set(1, Some((Frame::new(0x55, false, &[0x55]), Duration::from_millis(10))));
    let got = collect(&rx, 1000, Duration::from_millis(250));
    tx.scheduler.set(1, None);
    let times: Vec<f64> = got.iter().filter(|f| f.id == 0x55).map(|f| f.ts).collect();
    assert!((20..=30).contains(&times.len()), "periodic frames: {}", times.len());
    let mean = (times.last().unwrap() - times[0]) / (times.len() - 1) as f64;
    assert!((mean - 0.010).abs() < 0.001, "mean period {mean}");
}
