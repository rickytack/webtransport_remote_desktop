use anyhow::Result;

use common::logging;
use common::protocol::{PushOutcome, Reassembler};
use tracing::error;
use tracing::info;
use tracing::info_span;
use tracing::trace;
use tracing::Instrument;
use wtransport::ClientConfig;
use wtransport::Connection;
use wtransport::Endpoint;

use image::{ImageBuffer, Rgb};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use openh264::nal_units;

use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

#[tokio::main]
async fn main() {
    logging::init_logging();
    info!("Start");

    fs::create_dir_all("images").unwrap();

    let config = ClientConfig::builder().with_bind_default().with_no_cert_validation().build();

    let connection =
        Endpoint::client(config).unwrap().connect("https://localhost:4433").await.unwrap();

    // TODO: send connection start info
    info!("Opening bidirectional stream");
    let mut stream = connection.open_bi().await.unwrap().await.unwrap();
    stream.0.write_all(b"HELLO").await.unwrap();
    stream.0.finish().await.unwrap();

    info!("Start receiving data from server");
    let handle = tokio::spawn(
        async {
            handle_connection(connection).await;
        }
        .instrument(info_span!("Connection")),
    );

    handle.await.unwrap();
}

async fn handle_connection(connection: Connection) {
    let result = handle_connection_impl(connection).await;
    error!("{:?}", result);
}

async fn handle_connection_impl(connection: Connection) -> Result<()> {
    let mut decoder = Decoder::new()?;
    let mut reassembler = Reassembler::new(Default::default())?;

    // evict_expired() must be driven even when no datagrams arrive.
    let mut eviction_timer = tokio::time::interval(Duration::from_millis(50));
    eviction_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_stats = Instant::now();

    loop {
        tokio::select! {
            dgram = connection.receive_datagram() => {
                let datagram = dgram?;
                let now = Instant::now();

                match reassembler.push(datagram.payload(), now) {
                    PushOutcome::Complete(frame) => {
                        info!(
                            frame_id = frame.id,
                            bytes = frame.len(),
                            "Frame reassembled"
                        );

                        if let Ok(Some(rgb_buffer)) = decode_h264_data(&mut decoder, &frame.data) {
                            trace!(rgb_bytes = rgb_buffer.len(), "Decoded frame to RGB");
                        }
                    }
                    PushOutcome::Incomplete => {}
                    PushOutcome::Rejected(reason) => {
                        trace!(?reason, "Dropped video datagram");
                    }
                }
            }
            _ = eviction_timer.tick() => {
                let now = Instant::now();
                for evicted in reassembler.evict_expired(now) {
                    trace!(?evicted, "Dropped incomplete frame");
                }

                if last_stats.elapsed() >= Duration::from_secs(1) {
                    info!(
                        stats = ?reassembler.stats(),
                        inflight_frames = reassembler.inflight_frames(),
                        partial_bytes = reassembler.partial_bytes(),
                        "Reassembly stats"
                    );
                    last_stats = now;
                }
            }
        }
    }
}

fn get_next_filename() -> String {
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("images/output_{}.png", count)
}

fn save_as_png(
    rgb_data: &Vec<u8>,
    width: u32,
    height: u32,
    filename: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create RGB image buffer from raw data
    let img = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, rgb_data.as_slice())
        .ok_or("Invalid image dimensions or data length")?;

    // Save as PNG - just pass the filename directly
    img.save(filename)?;

    Ok(())
}

fn decode_h264_data(decoder: &mut Decoder, h264_data: &[u8]) -> Result<Option<Vec<u8>>> {
    for nal_packet in nal_units(h264_data) {
        match decoder.decode(nal_packet) {
            Ok(Some(yuv)) => {
                // Successfully decoded a YUV frame
                println!("Decoded a frame");

                let dim = yuv.dimensions();
                let rgb_buffer_size = dim.0 * dim.1 * 3;

                let mut rgb_buffer = vec![0u8; rgb_buffer_size];
                yuv.write_rgb8(&mut rgb_buffer);

                // Return the RGB buffer when we get a frame
                let _ = save_as_png(&rgb_buffer, dim.0 as _, dim.1 as _, &get_next_filename());
                return Ok(Some(rgb_buffer));
            }
            Ok(None) => {
                // No frame was available at this time
                continue;
            }
            Err(e) => {
                // TODO: Handle decoding errors
                eprintln!("A decoding error occurred: {:?}", e);
            }
        }
    }

    // Return None if no frame was decoded
    Ok(None)
}
