use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::task;
use tokio::time::sleep;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use openh264::encoder::Encoder;
use openh264::formats::{RGBSource, YUVBuffer};

pub struct RGBVecSource {
    data: Vec<u8>,
    width: usize,
    height: usize,
}

impl RGBVecSource {
    pub fn new(data: Vec<u8>, width: usize, height: usize) -> Self {
        // Validate data length for RGBA (4 bytes per pixel)
        assert_eq!(data.len(), width * height * 4, "RGBA data length must be width * height * 4");

        Self { data, width, height }
    }
}

impl RGBSource for RGBVecSource {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn pixel_f32(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let idx = (y * self.width + x) * 4; // Using *4 for RGBA
        (
            self.data[idx] as f32,     // R
            self.data[idx + 1] as f32, // G
            self.data[idx + 2] as f32, // B
        )
    }
}

// ===================== FRAME CAPTURE =====================

// TODO: Create trait (abstract capture device). Implement different capture methods
/// Captures frames from the active X11 display and sends raw RGB data to a channel.
pub struct FrameCaptureDevice {
    display_name: Option<String>,
}

impl FrameCaptureDevice {
    /// Create a new frame capture device.
    pub fn new(display_name: Option<String>) -> Self {
        println!("Creating FrameCaptureDevice for display {:?}", display_name);
        Self { display_name }
    }

    /// Starts capturing frames asynchronously and sends raw frame data to the provided channel.
    pub async fn start_capturing_frames(self, tx: Sender<Vec<u8>>) {
        task::spawn_local(async move {
            if let Err(e) = Self::capture_loop(self.display_name, tx).await {
                eprintln!("Frame capture failed: {}", e);
            }
        });
    }

    /// Internal capture loop.
    async fn capture_loop(display_name: Option<String>, tx: Sender<Vec<u8>>) -> Result<()> {
        // Connect to X server
        let (conn, screen_num) = RustConnection::connect(display_name.as_deref())?;
        let screen = &conn.setup().roots[screen_num];
        println!(
            "Connected to X11 display, screen size: {}x{}",
            screen.width_in_pixels, screen.height_in_pixels
        );

        // Setup encoder
        let mut encoder = Encoder::new()?;

        // Capture loop (~30 FPS)
        loop {
            let image_reply = conn
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    screen.root,
                    0,
                    0,
                    screen.width_in_pixels,
                    screen.height_in_pixels,
                    !0,
                )?
                .reply()?;

            let rgb_data = image_reply.data;

            // Convert to YUV
            let rgb_source = RGBVecSource::new(
                rgb_data,
                screen.width_in_pixels as _,
                screen.height_in_pixels as _,
            );
            let yuv_buffer = YUVBuffer::from_rgb_source(rgb_source);
            let bitstream = encoder.encode(&yuv_buffer)?;
            let encoded_data = bitstream.to_vec();

            // Send frame to channel
            if tx.send(encoded_data).await.is_err() {
                println!("Receiver dropped; stopping capture loop.");
                break;
            }

            sleep(Duration::from_millis(33)).await; // ~30 FPS
        }

        Ok(())
    }
}
