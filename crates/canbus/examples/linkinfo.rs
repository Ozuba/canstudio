//! Print an interface's bitrate, state and error counters: `cargo run --example linkinfo -- can0`.
fn main() {
    let iface = std::env::args().nth(1).unwrap_or_else(|| "can0".into());
    println!("{iface}: {:?}", canbus::socket::link_info(&iface));
}
