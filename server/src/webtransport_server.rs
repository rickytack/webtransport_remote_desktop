use anyhow::Result;
use common::protocol::{Fragmenter, Frame, FrameFlags};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{debug, error, info, info_span, Instrument};
use wtransport::endpoint::IncomingSession;
use wtransport::{Connection, Endpoint, Identity, ServerConfig};

pub struct RemoteDesktopServer {
    rx: Arc<Mutex<Receiver<Vec<u8>>>>,
}

impl RemoteDesktopServer {
    pub fn new(rx: Receiver<Vec<u8>>) -> Self {
        println!("Creating RemoteDesktopServer");
        Self { rx: Arc::new(Mutex::new(rx)) }
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
            let rx_clone = Arc::clone(&self.rx);
            let id = connection_id;

            tokio::spawn(
                async move {
                    if let Err(e) = Self::handle_connection(incoming_session, rx_clone).await {
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
        rx: Arc<Mutex<Receiver<Vec<u8>>>>,
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
        rx: Arc<Mutex<Receiver<Vec<u8>>>>,
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
        loop {
            // Lock receiver each time to get next frame.
            // TODO(M2): replace this shared receiver with a watch channel so
            // each client gets the latest frame independently.
            let mut rx_guard = rx.lock().await;
            let Some(frame_data) = rx_guard.recv().await else {
                debug!("Frame channel closed; stopping video sender");
                break;
            };

            if frame_data.is_empty() {
                // The encoder can legally produce no output for a frame (for
                // example when it skips a frame). There is nothing to send.
                debug!(frame_number, "Encoder produced an empty frame");
                frame_number = frame_number.wrapping_add(1);
                drop(rx_guard);
                continue;
            }

            let timestamp_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_micros() as u64;
            let frame = Frame::new(frame_number, timestamp_us, FrameFlags::empty(), frame_data);
            let datagrams = fragmenter.fragment(&frame)?;
            let fragment_count = datagrams.len();

            // TODO(M2): pace datagrams and drop stale frames instead of
            // dumping the whole frame into the QUIC send buffer at once.
            for datagram in datagrams {
                connection.send_datagram(datagram)?;
            }

            debug!(frame_number, bytes = frame.len(), fragment_count, "Sent video frame");
            frame_number = frame_number.wrapping_add(1);
            drop(rx_guard);
        }

        Ok(())
    }
}

// TODO: implement logical object: Client session. Client session is started in a new connection handler
