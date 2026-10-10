//! CAN bus plumbing independent of any UI: frames, the candump log format, a
//! columnar frame store, decoded signal series, bus statistics and (on Linux)
//! live SocketCAN capture and transmission.

pub mod candump;
pub mod frame;
pub mod series;
#[cfg(target_os = "linux")]
pub mod socket;
pub mod stats;
pub mod store;

pub use frame::{Frame, flags};
pub use series::{Series, SeriesCache};
pub use stats::{Bitrate, BusStats, IdStats};
pub use store::{FrameRef, FrameStore};
