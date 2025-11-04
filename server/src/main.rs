use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::LocalSet;

use common::logging;
use common::remote_protocol;
mod datagram_fragmentor;
mod webtransport_server;
use webtransport_server::RemoteDesktopServer;
mod frame_capture_device;
use frame_capture_device::FrameCaptureDevice;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    logging::init_logging();

    // TODO: capture start/stop should be controlled by commands from the command channel.
    // Commands come from RemoteDesktopServer.

    let local = LocalSet::new();
    local
        .run_until(async {
            let (tx, rx) = mpsc::channel::<Vec<u8>>(8);

            // Start capture (uses spawn_local inside)
            let device = FrameCaptureDevice::new(None);
            device.start_capturing_frames(tx).await;

            // Start the server (can be async)
            let server = RemoteDesktopServer::new(rx);
            if let Err(e) = server.listen_for_connections().await {
                eprintln!("Server error: {e}");
            }
        })
        .await;

    Ok(())
}
