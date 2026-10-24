use anyhow::{bail, Result};

use common::control::{self, ControlMessage, Hello, Welcome};
use common::logging;
use common::protocol::{FrameFlags, PushOutcome, Reassembler};
use tracing::error;
use tracing::info;
use tracing::info_span;
use tracing::trace;
use tracing::Instrument;
use wtransport::ClientConfig;
use wtransport::Connection;
use wtransport::Endpoint;

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use openh264::nal_units;

use std::time::{Duration, Instant};

mod display;
use display::{Display, DisplayFrame};

#[tokio::main]
async fn main() -> Result<()> {
    logging::init_logging();
    info!("Start");

    let display = Display::spawn()?;

    let config = ClientConfig::builder().with_bind_default().with_no_cert_validation().build();

    let connection = Endpoint::client(config)?.connect("https://localhost:4433").await?;

    info!("Opening control stream");
    let mut stream = connection.open_bi().await?.await?;
    let hello = ControlMessage::Hello(Hello { protocol_version: control::VERSION }).encode()?;
    stream.0.write_all(&hello).await?;

    match read_control_message(&mut stream.1).await? {
        ControlMessage::Welcome(Welcome { protocol_version, width, height }) => {
            if protocol_version != control::VERSION {
                bail!("server control version {protocol_version}, expected {}", control::VERSION);
            }
            info!(width, height, "Control handshake complete");
        }
        other => bail!("expected Welcome, got {other:?}"),
    }

    info!("Start receiving data from server");
    let handle = tokio::spawn(
        async {
            handle_connection(connection, display).await;
        }
        .instrument(info_span!("Connection")),
    );

    handle.await?;
    Ok(())
}

async fn read_control_message(
    stream: &mut wtransport::stream::RecvStream,
) -> Result<ControlMessage> {
    let mut header = [0u8; control::HEADER_LEN];
    stream.read_exact(&mut header).await?;
    let payload_len = control::payload_len(&header)?;

    let mut frame = vec![0u8; control::HEADER_LEN + payload_len];
    frame[..control::HEADER_LEN].copy_from_slice(&header);
    stream.read_exact(&mut frame[control::HEADER_LEN..]).await?;

    Ok(ControlMessage::decode(&frame)?)
}

async fn handle_connection(connection: Connection, display: Display) {
    match handle_connection_impl(connection, display).await {
        Ok(()) => info!("Connection closed"),
        Err(error) => error!("Connection error: {error:?}"),
    }
}

async fn handle_connection_impl(connection: Connection, display: Display) -> Result<()> {
    let mut decoder = Decoder::new()?;
    let mut reassembler = Reassembler::new(Default::default())?;

    // evict_expired() must be driven even when no datagrams arrive.
    let mut eviction_timer = tokio::time::interval(Duration::from_millis(50));
    eviction_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_stats = Instant::now();
    let mut frames_shown = 0u64;

    loop {
        tokio::select! {
            dgram = connection.receive_datagram() => {
                let datagram = dgram?;
                let now = Instant::now();

                match reassembler.push(datagram.payload(), now) {
                    PushOutcome::Complete(frame) => {
                        trace!(
                            frame_id = frame.id,
                            bytes = frame.len(),
                            keyframe = frame.flags.contains(FrameFlags::KEYFRAME),
                            "Frame reassembled"
                        );

                        if let Ok(Some(decoded)) = decode_h264_data(&mut decoder, &frame.data) {
                            trace!(
                                width = decoded.width,
                                height = decoded.height,
                                "Decoded frame"
                            );

                            frames_shown += 1;
                            let shown = display.show(DisplayFrame {
                                width: decoded.width,
                                height: decoded.height,
                                rgb: decoded.rgb,
                            });
                            if !shown {
                                info!("Display closed; stopping client");
                                return Ok(());
                            }
                        }
                    }
                    PushOutcome::Incomplete => {}
                    PushOutcome::Rejected(reason) => {
                        trace!(?reason, "Dropped video datagram");
                    }
                }
            }
            _ = eviction_timer.tick() => {
                if !display.is_running() {
                    info!("Display closed; stopping client");
                    return Ok(());
                }

                let now = Instant::now();
                for evicted in reassembler.evict_expired(now) {
                    trace!(?evicted, "Dropped incomplete frame");
                }

                if last_stats.elapsed() >= Duration::from_secs(1) {
                    let elapsed = last_stats.elapsed().as_secs_f64();
                    info!(
                        fps = frames_shown as f64 / elapsed,
                        stats = ?reassembler.stats(),
                        inflight_frames = reassembler.inflight_frames(),
                        partial_bytes = reassembler.partial_bytes(),
                        "Reassembly stats"
                    );
                    last_stats = now;
                    frames_shown = 0;
                }
            }
        }
    }
}

struct DecodedFrame {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn decode_h264_data(decoder: &mut Decoder, h264_data: &[u8]) -> Result<Option<DecodedFrame>> {
    for nal_packet in nal_units(h264_data) {
        match decoder.decode(nal_packet) {
            Ok(Some(yuv)) => {
                let (width, height) = yuv.dimensions();
                let mut rgb = vec![0u8; width * height * 3];
                yuv.write_rgb8(&mut rgb);

                return Ok(Some(DecodedFrame { width, height, rgb }));
            }
            Ok(None) => {
                // No frame was available from this NAL unit yet.
            }
            Err(error) => {
                // TODO(M2): request a keyframe after a decode error.
                error!("H.264 decode error: {error:?}");
            }
        }
    }

    Ok(None)
}
