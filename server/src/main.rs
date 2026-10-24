use anyhow::Result;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::LocalSet;

use common::logging;
use common::protocol::Frame;
mod webtransport_server;
use webtransport_server::RemoteDesktopServer;
mod frame_capture_device;
use frame_capture_device::FrameCaptureDevice;

const DEFAULT_STREAM_WIDTH: u16 = 1280;
const DEFAULT_STREAM_HEIGHT: u16 = 720;

fn stream_size() -> (u16, u16) {
    let read = |name: &str, default: u16| {
        std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
    };

    (read("WTR_WIDTH", DEFAULT_STREAM_WIDTH), read("WTR_HEIGHT", DEFAULT_STREAM_HEIGHT))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    logging::init_logging();

    // TODO: capture start/stop should be controlled by commands from the command channel.
    // Commands come from RemoteDesktopServer.

    let local = LocalSet::new();
    local
        .run_until(async {
            let (tx, rx) = watch::channel::<Option<Arc<Frame>>>(None);
            let force_keyframe = Arc::new(AtomicBool::new(false));
            let (width, height) = stream_size();

            // Start capture (capture and encode run on dedicated threads)
            let device = FrameCaptureDevice::new(None, width, height, Arc::clone(&force_keyframe));
            if let Err(error) = device.start_capturing_frames(tx) {
                eprintln!("Failed to start capture threads: {error}");
                return;
            }

            // Start the server (can be async)
            let server = RemoteDesktopServer::new(rx, force_keyframe);
            if let Err(e) = server.listen_for_connections().await {
                eprintln!("Server error: {e}");
            }
        })
        .await;

    Ok(())
}
