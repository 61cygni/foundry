use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use xcap::{Frame, Monitor, Window};

pub type Listener = tokio::sync::mpsc::Receiver<Arc<Frame>>;
type ListenerSender = tokio::sync::mpsc::Sender<Arc<Frame>>;

/// Target frame rate for window capture polling
const WINDOW_CAPTURE_FPS: u32 = 60;

/// Specifies what to capture
#[derive(Debug, Clone)]
pub enum CaptureSource {
    /// Capture the primary monitor
    PrimaryMonitor,
    /// Capture a specific window by ID
    Window(u32),
}

pub struct Recorder {
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
}

impl Recorder {
    pub fn new(source: CaptureSource) -> Self {
        let listeners: Vec<ListenerSender> = Vec::new();
        let listeners = Arc::new(Mutex::new(listeners));

        let (video_startstop, receive_startstop) = std::sync::mpsc::channel();

        let listeners_clone = listeners.clone();
        let video_startstop_clone = video_startstop.clone();

        thread::spawn(move || match source {
            CaptureSource::PrimaryMonitor => {
                create_monitor_recorder_thread(
                    listeners_clone,
                    video_startstop_clone,
                    receive_startstop,
                )
            }
            CaptureSource::Window(window_id) => {
                create_window_recorder_thread(
                    window_id,
                    listeners_clone,
                    video_startstop_clone,
                    receive_startstop,
                )
            }
        });

        Self {
            listeners,
            video_startstop,
        }
    }

    pub fn new_listener(&self) -> Listener {
        let (tx, rx) = tokio::sync::mpsc::channel(1);

        let mut listeners = self.listeners.lock().unwrap();
        listeners.push(tx);
        if listeners.len() == 1 {
            self.video_startstop.send(true).unwrap();
        }

        rx
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        _ = self.video_startstop.send(false);
        println!("Video recorder dropped");
    }
}

/// Monitor capture using xcap's built-in VideoRecorder
fn create_monitor_recorder_thread(
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
    startstop_receiver: std::sync::mpsc::Receiver<bool>,
) {
    let monitors = Monitor::all().unwrap();
    let monitor = monitors
        .iter()
        .filter(|&monitor| monitor.is_primary().unwrap())
        .next()
        .unwrap();

    println!(
        "Creating video recorder for monitor: {} [id {}]",
        monitor.name().unwrap(),
        monitor.id().unwrap()
    );
    let (video_recorder, frame_receiver) = monitor.video_recorder().unwrap();
    let video_recorder = Arc::new(video_recorder);

    thread::spawn(move || create_frame_receiver_thread(frame_receiver, listeners, video_startstop));

    let mut started = false;

    loop {
        match startstop_receiver.recv() {
            Ok(start) => {
                if start && !started {
                    video_recorder.start().unwrap();
                    println!("Video recorder started");
                    started = true;
                }
                if !start && started {
                    video_recorder.stop().unwrap();
                    println!("Video recorder stopped");
                    started = false;
                }
            }
            Err(_) => break,
        }
    }
}

/// Window capture using polling with capture_image()
fn create_window_recorder_thread(
    window_id: u32,
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
    startstop_receiver: std::sync::mpsc::Receiver<bool>,
) {
    let windows = Window::all().unwrap();
    let window = windows
        .into_iter()
        .find(|w| w.id().unwrap_or(0) == window_id)
        .expect(&format!("Window with ID {} not found", window_id));

    println!(
        "Creating video recorder for window: {} [id {}] (app: {})",
        window.title().unwrap_or_default(),
        window_id,
        window.app_name().unwrap_or_default()
    );

    let running = Arc::new(AtomicBool::new(false));
    let running_clone = running.clone();
    let listeners_clone = listeners.clone();
    let video_startstop_clone = video_startstop.clone();

    // Capture thread - polls window at target FPS
    thread::spawn(move || {
        let frame_duration = Duration::from_secs_f64(1.0 / WINDOW_CAPTURE_FPS as f64);

        loop {
            if !running_clone.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
                continue;
            }

            let start = Instant::now();

            // Capture the window
            match window.capture_image() {
                Ok(image) => {
                    // Use image dimensions (includes Retina 2x scaling)
                    let frame = Frame {
                        width: image.width(),
                        height: image.height(),
                        raw: image.into_raw(),
                    };
                    let frame = Arc::new(frame);

                    let mut listeners = listeners_clone.lock().unwrap();
                    if !listeners.is_empty() {
                        static DROPPED_COUNT: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);

                        listeners.retain(|listener| match listener.try_send(frame.clone()) {
                            Ok(_) => {
                                DROPPED_COUNT.store(0, Ordering::Relaxed);
                                true
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                let count = DROPPED_COUNT.fetch_add(1, Ordering::Relaxed);
                                if count % 60 == 0 {
                                    eprintln!("encoder can't keep up, dropped {} frames", count + 1);
                                }
                                true
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        });

                        if listeners.is_empty() {
                            println!("no listeners left, stopping window capture");
                            video_startstop_clone.send(false).unwrap();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Window capture failed: {}", e);
                    // Window might have closed - stop capturing
                    break;
                }
            }

            // Sleep for remaining frame time
            let elapsed = start.elapsed();
            if elapsed < frame_duration {
                thread::sleep(frame_duration - elapsed);
            }
        }
        println!("window capture thread stopped");
    });

    // Control thread - handles start/stop commands
    loop {
        match startstop_receiver.recv() {
            Ok(start) => {
                let was_running = running.load(Ordering::Relaxed);
                if start && !was_running {
                    running.store(true, Ordering::Relaxed);
                    println!("Window capture started");
                }
                if !start && was_running {
                    running.store(false, Ordering::Relaxed);
                    println!("Window capture stopped");
                }
            }
            Err(_) => break,
        }
    }
}

fn create_frame_receiver_thread(
    frame_receiver: std::sync::mpsc::Receiver<Frame>,
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
) {
    loop {
        match frame_receiver.recv() {
            Ok(frame) => {
                // println!(
                //     "frame: {} x {} ({} bytes)",
                //     frame.width,
                //     frame.height,
                //     frame.raw.len()
                // );
                let frame = Arc::new(frame);

                let mut listeners = listeners.lock().unwrap();
                if !listeners.is_empty() {
                    // println!("sending frame to {} listeners", listeners.len());
                    static DROPPED_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    
                    listeners.retain(|listener| match listener.try_send(frame.clone()) {
                        Ok(_) => {
                            // Reset drop counter on successful send
                            DROPPED_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
                            true
                        },
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_frame)) => {
                            // Only log occasionally to avoid spam
                            let count = DROPPED_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if count % 60 == 0 {
                                eprintln!("encoder can't keep up, dropped {} frames", count + 1);
                            }
                            true
                        },
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(frame)) => {
                            println!("listener closed: frame: {} x {} ({} bytes)",
                                frame.width,
                                frame.height,
                                frame.raw.len()
                            );
                            false
                        },
                    });

                    if listeners.is_empty() {
                        println!("no listeners left, stopping video recorder");
                        video_startstop.send(false).unwrap();
                    }
                }
            }
            Err(err) => {
                eprintln!("frame receiver error: {}", err);
                break;
            }
            // _ => break,
        }
    }
    println!("recorder stopped");
}
