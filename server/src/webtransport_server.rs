use anyhow::Result;
use common::protocol::{Fragmenter, Frame, FrameFlags};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info, info_span, Instrument};
use wtransport::endpoint::IncomingSession;
use wtransport::{Connection, Endpoint, Identity, ServerConfig};

pub struct RemoteDesktopServer {
    rx: watch::Receiver<Option<Arc<Frame>>>,
    force_keyframe: Arc<AtomicBool>,
}

impl RemoteDesktopServer {
    pub fn new(rx: watch::Receiver<Option<Arc<Frame>>>, force_keyframe: Arc<AtomicBool>) -> Self {
        println!("Creating RemoteDesktopServer");
        Self { rx, force_keyframe }
    }

    pub async fn listen_for_connections(self) -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerConfig::builder()
            .with_bind_default(4433)
            .with_identity(Identity::self_signed(["localhost"]).unwrap())
            .keep_alive_interval(Some(Duration::from_secs(3)))
            .build();

        let server = Endpoint::server(config)?;
        info!("Server ready!");

        let mut connection_id = 0u64;
        loop {
            let incoming_session = server.accept().await;
            let rx_clone = self.rx.clone();
            let force_keyframe = Arc::clone(&self.force_keyframe);
            let id = connection_id;

            tokio::spawn(
                async move {
                    if let Err(e) =
                        Self::handle_connection(incoming_session, rx_clone, force_keyframe).await
                    {
                        error!("Connection error: {:?}", e);
                    }
                }
                .instrument(info_span!("Connection", id)),
            );

            connection_id = connection_id.wrapping_add(1);
        }
    }

    async fn handle_connection(
        incoming_session: IncomingSession,
        rx: watch::Receiver<Option<Arc<Frame>>>,
        force_keyframe: Arc<AtomicBool>,
    ) -> Result<()> {
        info!("Waiting for session request...");
        let session_request = incoming_session.await?;

        info!(
            "New session: Authority: '{}', Path: '{}'",
            session_request.authority(),
            session_request.path()
        );

        let connection = session_request.accept().await?;
        info!("Waiting for data from client...");

        Self::recv_start_conn_data_impl(&connection).await?;

        // Make sure the first frame this client sees is an IDR, not a P-frame
        // from the middle of the stream.
        force_keyframe.store(true, Ordering::SeqCst);

        // Send datagram to client
        Self::send_videostream_impl(rx, connection).await?;

        Ok(())
    }

    async fn recv_start_conn_data_impl(connection: &Connection) -> Result<()> {
        let mut buffer = vec![0; 65536].into_boxed_slice();
        let mut stream = connection.accept_bi().await?;
        let bytes_read = stream
            .1
            .read(&mut buffer)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Unexpected end of stream"))?;

        let str_data = std::str::from_utf8(&buffer[..bytes_read])?;
        info!("Received (bi) '{str_data}' from client");

        Ok(())
    }

    async fn send_videostream_impl(
        mut rx: watch::Receiver<Option<Arc<Frame>>>,
        connection: Connection,
    ) -> Result<()> {
        // The negotiated limit is the maximum application datagram payload,
        // including our fragment header. Never assume a fixed 1200 bytes.
        let max_datagram_size = connection
            .max_datagram_size()
            .ok_or_else(|| anyhow::anyhow!("peer does not support WebTransport datagrams"))?;
        let fragmenter = Fragmenter::new(max_datagram_size)?;
        info!(
            max_datagram_size,
            payload_per_fragment = fragmenter.payload_capacity(),
            "Video datagram sender ready"
        );

        let mut frame_number: u64 = 0;
        let mut awaiting_keyframe = true;
        loop {
            // Each client owns a watch receiver: a slow client skips frames
            // instead of stealing them from other clients or blocking capture.
            if rx.changed().await.is_err() {
                debug!("Frame publisher closed; stopping video sender");
                break;
            }

            let Some(captured) = rx.borrow_and_update().clone() else {
                continue;
            };

            if captured.is_empty() {
                // The encoder can legally produce no output for a frame.
                debug!(frame_number, "Encoder produced an empty frame");
                continue;
            }

            if awaiting_keyframe {
                if !captured.flags.contains(FrameFlags::KEYFRAME) {
                    debug!("Skipping P-frame while waiting for the first keyframe");
                    continue;
                }
                awaiting_keyframe = false;
            }

            let frame = Frame::new(
                frame_number,
                captured.timestamp_us,
                captured.flags,
                captured.data.clone(),
            );
            let datagrams = fragmenter.fragment(&frame)?;
            let fragment_count = datagrams.len();

            // TODO(M2): pace datagrams and drop stale frames instead of
            // dumping the whole frame into the QUIC send buffer at once.
            for datagram in datagrams {
                connection.send_datagram(datagram)?;
            }

            debug!(
                frame_number,
                bytes = frame.len(),
                fragment_count,
                keyframe = captured.flags.contains(FrameFlags::KEYFRAME),
                "Sent video frame"
            );
            frame_number = frame_number.wrapping_add(1);
        }

        Ok(())
    }
}

// TODO: implement logical object: Client session. Client session is started in a new connection handler
