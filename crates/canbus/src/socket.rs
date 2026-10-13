//! Live SocketCAN access (Linux): a receive thread, a transmit socket and a
//! periodic transmit scheduler.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use socketcan::id::FdFlags;
use socketcan::{
    CanAnyFrame, CanFdFrame, CanFdSocket, CanFrame, EmbeddedFrame, ExtendedId, Frame as _, Id, Socket, SocketOptions, StandardId,
};

use crate::frame::{Frame, flags};

const EFF_FLAG: u32 = 0x8000_0000;
const RTR_FLAG: u32 = 0x4000_0000;
const ERR_FLAG: u32 = 0x2000_0000;

/// CAN network interfaces (`/sys/class/net/*/type == ARPHRD_CAN`).
pub fn list_interfaces() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| std::fs::read_to_string(e.path().join("type")).is_ok_and(|t| t.trim() == "280"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn from_socketcan(f: &CanAnyFrame, ts: f64) -> Frame {
    let w = f.id_word();
    let mut out = Frame { ts, ..Default::default() };
    if w & ERR_FLAG != 0 {
        out.flags |= flags::ERR;
        out.id = w & 0x1FFF_FFFF;
    } else if w & EFF_FLAG != 0 {
        out.flags |= flags::EXT;
        out.id = w & 0x1FFF_FFFF;
    } else {
        out.id = w & 0x7FF;
    }
    if w & RTR_FLAG != 0 {
        out.flags |= flags::RTR;
        out.len = f.dlc().min(8) as u8;
        return out;
    }
    if let CanAnyFrame::Fd(fd) = f {
        out.flags |= flags::FD;
        if fd.is_brs() {
            out.flags |= flags::BRS;
        }
        if fd.is_esi() {
            out.flags |= flags::ESI;
        }
    }
    out.set_data(f.data());
    out
}

fn to_id(f: &Frame) -> io::Result<Id> {
    let bad = || io::Error::new(io::ErrorKind::InvalidInput, format!("invalid CAN id {:X}", f.id));
    Ok(if f.flags & flags::EXT != 0 {
        Id::Extended(ExtendedId::new(f.id).ok_or_else(bad)?)
    } else {
        Id::Standard(StandardId::new(f.id as u16).ok_or_else(bad)?)
    })
}

/// Write one frame (classic, remote or FD) to a socket.
fn send_on(sock: &CanFdSocket, f: &Frame) -> io::Result<()> {
    let id = to_id(f)?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid frame");
    if f.flags & flags::FD != 0 || f.len > 8 {
        let mut fl = FdFlags::empty();
        if f.flags & flags::BRS != 0 {
            fl |= FdFlags::BRS;
        }
        let mut data = f.data().to_vec();
        data.resize(crate::frame::fd_len_ceil(data.len()), 0);
        sock.write_frame(&CanFdFrame::with_flags(id, &data, fl).ok_or_else(invalid)?)
    } else if f.flags & flags::RTR != 0 {
        sock.write_frame(&CanFrame::new_remote(id, f.len as usize).ok_or_else(invalid)?)
    } else {
        sock.write_frame(&CanFrame::new(id, f.data()).ok_or_else(invalid)?)
    }
}

/// An open SocketCAN interface with a background receive thread.
pub struct LiveBus {
    pub iface: String,
    rx: Receiver<Frame>,
    tx: Arc<CanFdSocket>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    error: Arc<Mutex<Option<String>>>,
    pub scheduler: Scheduler,
}

impl LiveBus {
    /// Open `iface` (e.g. `can0`, `vcan0`). Error frames are always captured.
    /// `wake` is called after each received batch (e.g. to request a repaint).
    pub fn open(iface: &str, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let sock = CanFdSocket::open(iface)?;
        sock.set_read_timeout(Duration::from_millis(50))?;
        sock.set_error_filter_accept_all()?;
        let has_ts = sock.set_recv_timestamp(true).is_ok();
        let tx = Arc::new(CanFdSocket::open(iface)?);

        let (sender, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let thread = {
            let (stop, error) = (stop.clone(), error.clone());
            std::thread::Builder::new().name(format!("rx-{iface}")).spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let res = if has_ts {
                        // Kernel arrival time when attached; a frame without one must not
                        // be treated as a fatal error, so fall back to host time.
                        sock.read_frame_with_timestamps().map(|(f, t)| {
                            let ts = t.socket.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or_else(now, |d| d.as_secs_f64());
                            (f, ts)
                        })
                    } else {
                        sock.read_frame().map(|f| (f, now()))
                    };
                    match res {
                        Ok((f, ts)) => {
                            if sender.send(from_socketcan(&f, ts)).is_err() {
                                break;
                            }
                            wake();
                        }
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
                        Err(e) => {
                            *error.lock().unwrap() = Some(e.to_string());
                            wake();
                            break;
                        }
                    }
                }
            })?
        };

        Ok(LiveBus { iface: iface.to_string(), rx, scheduler: Scheduler::new(tx.clone()), tx, stop, thread: Some(thread), error })
    }

    /// Move all received frames into `out`. Returns `false` once the receive thread died.
    pub fn drain(&self, out: &mut Vec<Frame>) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(f) => out.push(f),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// The error that stopped the receive thread, if any.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub fn send(&self, f: &Frame) -> io::Result<()> {
        send_on(&self.tx, f)
    }
}

impl Drop for LiveBus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Job {
    id: u64,
    frame: Frame,
    period: Duration,
    next: Instant,
}

#[derive(Default)]
struct SchedState {
    jobs: Vec<Job>,
    sent: u64,
    last_error: Option<String>,
}

/// Periodic transmitter running on its own thread for accurate timing.
pub struct Scheduler {
    state: Arc<Mutex<SchedState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Scheduler {
    fn new(sock: Arc<CanFdSocket>) -> Self {
        let state = Arc::new(Mutex::new(SchedState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::Builder::new()
                .name("tx-scheduler".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let wait = {
                            let mut st = state.lock().unwrap();
                            let now = Instant::now();
                            let mut errors = None;
                            let mut sent = 0;
                            for job in st.jobs.iter_mut().filter(|j| j.next <= now) {
                                match send_on(&sock, &job.frame) {
                                    Ok(()) => sent += 1,
                                    Err(e) => errors = Some(e.to_string()),
                                }
                                // Skip missed slots instead of bursting.
                                job.next += job.period;
                                if job.next < now {
                                    job.next = now + job.period;
                                }
                            }
                            st.sent += sent;
                            if errors.is_some() {
                                st.last_error = errors;
                            }
                            st.jobs
                                .iter()
                                .map(|j| j.next.saturating_duration_since(now))
                                .min()
                                .unwrap_or(Duration::from_millis(20))
                                .min(Duration::from_millis(20))
                        };
                        std::thread::sleep(wait.max(Duration::from_micros(100)));
                    }
                })
                .expect("spawn tx scheduler")
        };
        Scheduler { state, stop, thread: Some(thread) }
    }

    /// Install, update or (with `None`) remove periodic job `id`.
    pub fn set(&self, id: u64, job: Option<(Frame, Duration)>) {
        let mut st = self.state.lock().unwrap();
        match (st.jobs.iter().position(|j| j.id == id), job) {
            (Some(i), None) => {
                st.jobs.remove(i);
            }
            (Some(i), Some((frame, period))) => {
                let j = &mut st.jobs[i];
                if j.period != period {
                    j.next = Instant::now();
                }
                j.frame = frame;
                j.period = period;
            }
            (None, Some((frame, period))) => {
                st.jobs.push(Job { id, frame, period: period.max(Duration::from_micros(100)), next: Instant::now() })
            }
            (None, None) => {}
        }
    }

    /// (frames sent, last error).
    pub fn status(&self) -> (u64, Option<String>) {
        let st = self.state.lock().unwrap();
        (st.sent, st.last_error.clone())
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
