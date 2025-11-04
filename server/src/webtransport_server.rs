use crate::datagram_fragmentor::Fragmentor;
use crate::remote_protocol::{to_bytes, BaseHeader};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{error, info, info_span, Instrument};
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

        for id in 0.. {
            let incoming_session = server.accept().await;
            let rx_clone = Arc::clone(&self.rx);

            tokio::spawn(
                async move {
                    if let Err(e) = Self::handle_connection(incoming_session, rx_clone).await {
                        error!("Connection error: {:?}", e);
                    }
                }
                .instrument(info_span!("Connection", id = id)),
            );
        }

        Ok(())
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
        let mut frame_number_counter: u32 = 0;
        loop {
            // Lock receiver each time to get next frame
            let mut rx_guard = rx.lock().await;
            if let Some(frame) = rx_guard.recv().await {
                println!("Captured frame {} bytes", frame.len());

                // Fragment
                let base = BaseHeader { channel_id: 1, direction: 1 };
                let fragmentor = Fragmentor::new(1200);
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as u64;
                let packets =
                    fragmentor.fragment_frame(frame_number_counter, timestamp, base, 0, frame);
                frame_number_counter = frame_number_counter.wrapping_add(1);

                // Serialize and send
                for packet in packets {
                    let serialized = to_bytes(&packet)?;
                    connection.send_datagram(serialized)?;
                }
                println!("Frame fragments sent. Frame number: {}", frame_number_counter)
            } else {
                println!("Channel closed, stopping stream loop.");
                break;
            }
            drop(rx_guard);
        }

        Ok(())
    }
}

// TODO: implement logical object: Client session. Client session is started in a new connection handler
