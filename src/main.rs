use axum::{
    body::Body,
    extract::{
        ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{
    fs,
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};

const OUTBOUND_BUFFER: usize = 1024;

mod session;
mod recording;
mod video_pipeline;
mod audio_mixer;
mod audio_capture;

#[derive(Clone)]
struct AppState {
    recorder: Arc<recording::Recorder>,
    mixer: Arc<audio_mixer::AudioMixer>,
    audio_broadcast: Option<audio_capture::AudioBroadcast>,
}

#[tokio::main]
async fn main() {
    let recorder = recording::Recorder::new();
    let mixer = audio_mixer::AudioMixer::new();
    
    // Start system audio capture (requires BlackHole for system audio)
    // We must keep _audio_capture alive - dropping it stops the capture
    let (_audio_capture, audio_broadcast) = match audio_capture::start_audio_capture() {
        Ok((capture, broadcast)) => {
            println!("System audio capture enabled");
            (Some(capture), Some(broadcast))
        }
        Err(err) => {
            eprintln!("Audio capture not available: {}", err);
            eprintln!("For system audio, install BlackHole: brew install blackhole-2ch");
            (None, None)
        }
    };
    
    let state = AppState {
        recorder: Arc::new(recorder),
        mixer: Arc::new(mixer),
        audio_broadcast,
    };

    let serve_files = [
        "root.js",
        "video_worker.js",
        "audio_worklet.js",
        "audio.js",
        "stats.js",
        "video.js",
        "gui.js",
        "screen.js",
        "screen.html",
    ];

    let mut app = Router::new()
        .route("/", get(move || serve_static("root.html")))
        .route("/ws", get(get_ws))
        .route("/dist/spark.module.js", get(move || serve_static("../../../dist/spark.module.js")))
        .with_state(state);

    for file in serve_files {
        let route = format!("/{}", file);
        let file_to_serve = file;
        app = app.route(route.as_str(), get(move || serve_static(file_to_serve)));
    }

    let listener = tokio::net::TcpListener::bind("0.0.0.0:23646")
        .await
        .unwrap();
    println!("Open http://localhost:23646/");
    axum::serve(listener, app).await.unwrap();
}

async fn serve_static(file: &'static str) -> Response {
    let path = format!("{}/src/{}", env!("CARGO_MANIFEST_DIR"), file);
    let content_type = if file.ends_with(".html") {
        "text/html"
    } else if file.ends_with(".js") {
        "text/javascript"
    } else {
        "application/octet-stream"
    };

    match fs::read(&path).await {
        Ok(bytes) => Response::builder()
            .header("Content-Type", content_type)
            .body(Body::from(bytes))
            .unwrap(),
        Err(err) => {
            eprintln!("failed to read static file {}: {}", file, err);
            Response::builder()
                .status(404)
                .body(Body::from("not found"))
                .unwrap()
        }
    }
}

async fn get_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(stream: WebSocket, state: AppState) {
    let (mut sender, receiver) = stream.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_BUFFER);

    // Task: push outbound messages (application + heartbeats) to the client.
    let outbound = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(10));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    if sender.send(msg).await.is_err() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if sender.send(Message::Text(Utf8Bytes::from("heartbeat"))).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Task: read inbound messages and decide what to do with them.
    let inbound = tokio::spawn(async move {
        session::start(receiver, tx, state).await;
    });

    // Wait for either task to finish; ignore the specific error to keep the
    // boilerplate simple.
    let _ = tokio::try_join!(outbound, inbound);
}
