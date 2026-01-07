use std::{
    sync::{Arc, Mutex},
    thread,
};

use xcap::{Frame, Monitor};

pub type Listener = tokio::sync::mpsc::Receiver<Arc<Frame>>;
type ListenerSender = tokio::sync::mpsc::Sender<Arc<Frame>>;

pub struct Recorder {
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
}

impl Recorder {
    pub fn new() -> Self {
        let listeners: Vec<ListenerSender> = Vec::new();
        let listeners = Arc::new(Mutex::new(listeners));

        let (video_startstop, receive_startstop) = std::sync::mpsc::channel();

        let listeners_clone = listeners.clone();
        let video_startstop_clone = video_startstop.clone();

        thread::spawn(move || {
            create_video_recorder_thread(
                listeners_clone,
                video_startstop_clone,
                receive_startstop,
            )
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

fn create_video_recorder_thread(
    listeners: Arc<Mutex<Vec<ListenerSender>>>,
    video_startstop: std::sync::mpsc::Sender<bool>,
    startstop_receiver: std::sync::mpsc::Receiver<bool>,
) {
    let monitors = Monitor::all().unwrap();
    let monitor = monitors.iter()
        .filter(|&monitor| monitor.is_primary().unwrap())
        .next()
        .unwrap();

    println!("Creating video recorder for monitor: {} [id {}]", monitor.name().unwrap(), monitor.id().unwrap());
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
                    listeners.retain(|listener| match listener.try_send(frame.clone()) {
                        Ok(_) => {
                            // println!("sent frame to listener");
                            true
                        },
                        Err(tokio::sync::mpsc::error::TrySendError::Full(frame)) => {
                            eprintln!("listener full: frame: {} x {} ({} bytes)",
                                frame.width,
                                frame.height,
                                frame.raw.len()
                            );
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
