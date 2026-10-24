use anyhow::{anyhow, Result};
use common::protocol::{Frame, FrameFlags};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::nal_units;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

/// Byte offsets of R, G and B inside one 32-bit X11 pixel.
#[derive(Debug, Clone, Copy)]
struct PixelFormat {
    red: usize,
    green: usize,
    blue: usize,
}

impl PixelFormat {
    fn from_visual(visual: &Visualtype) -> Self {
        let red = channel_offset(visual.red_mask);
        let green = channel_offset(visual.green_mask);
        let blue = channel_offset(visual.blue_mask);

        match (red, green, blue) {
            (Some(red), Some(green), Some(blue)) => Self { red, green, blue },
            // Fallback for unusual visuals. Root visuals on the supported X11
            // setups are TrueColor, so this should not normally be used.
            _ => Self { red: 0, green: 1, blue: 2 },
        }
    }
}

fn channel_offset(mask: u32) -> Option<usize> {
    if mask == 0 {
        return None;
    }

    let shift = mask.trailing_zeros();
    if !shift.is_multiple_of(8) {
        return None;
    }

    let offset = (shift / 8) as usize;
    (offset < 4).then_some(offset)
}

/// Downscale a 4-byte-per-pixel X11 image and convert it directly to packed
/// RGB8. Combining these steps avoids a second full-frame pass and lets
/// OpenH264 use its faster `RGB8Source` conversion path.
fn downscale_to_rgb8(
    src: &[u8],
    src_width: usize,
    src_height: usize,
    width: usize,
    height: usize,
    format: PixelFormat,
) -> Vec<u8> {
    assert_eq!(src.len(), src_width * src_height * 4, "source dimensions do not match data");

    let mut dst = vec![0u8; width * height * 3];

    // The common case: exactly half resolution, averaging each 2x2 block.
    if src_width == width * 2 && src_height == height * 2 {
        let src_stride = src_width * 4;
        for y in 0..height {
            let row0 = y * 2 * src_stride;
            let row1 = row0 + src_stride;
            for x in 0..width {
                let i0 = row0 + x * 8;
                let i1 = row1 + x * 8;
                let o = (y * width + x) * 3;

                let average = |channel: usize| {
                    (u32::from(src[i0 + channel])
                        + u32::from(src[i0 + 4 + channel])
                        + u32::from(src[i1 + channel])
                        + u32::from(src[i1 + 4 + channel]))
                        / 4
                };

                dst[o] = average(format.red) as u8;
                dst[o + 1] = average(format.green) as u8;
                dst[o + 2] = average(format.blue) as u8;
            }
        }
        return dst;
    }

    for y in 0..height {
        let y0 = y * src_height / height;
        let y1 = (((y + 1) * src_height) / height).max(y0 + 1).min(src_height);

        for x in 0..width {
            let x0 = x * src_width / width;
            let x1 = (((x + 1) * src_width) / width).max(x0 + 1).min(src_width);

            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let idx = (sy * src_width + sx) * 4;
                    for channel in 0..4 {
                        sums[channel] += u32::from(src[idx + channel]);
                    }
                    count += 1;
                }
            }

            let o = (y * width + x) * 3;
            dst[o] = (sums[format.red] / count) as u8;
            dst[o + 1] = (sums[format.green] / count) as u8;
            dst[o + 2] = (sums[format.blue] / count) as u8;
        }
    }

    dst
}

fn root_pixel_format(screen: &Screen) -> Result<PixelFormat> {
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.visual_id == screen.root_visual)
        .ok_or_else(|| anyhow!("root visual {} not found", screen.root_visual))?;

    Ok(PixelFormat::from_visual(visual))
}

/// `openh264::nal_units` yields each NAL unit *including* its Annex B start
/// code, so the NAL header is not necessarily the first byte.
fn is_idr_nal(nal: &[u8]) -> bool {
    let header = if nal.starts_with(&[0, 0, 0, 1]) {
        nal.get(4)
    } else if nal.starts_with(&[0, 0, 1]) {
        nal.get(3)
    } else {
        None
    };

    header.is_some_and(|header| header & 0x1f == 5)
}

struct RawFrame {
    data: Vec<u8>,
    width: usize,
    height: usize,
    target_width: usize,
    target_height: usize,
    format: PixelFormat,
    timestamp_us: u64,
}

/// Captures X11 frames on one thread and encodes them on another. Capture and
/// encoding overlap instead of adding their latencies together; if the encoder
/// falls behind, the one-frame queue drops old capture buffers.
pub struct FrameCaptureDevice {
    display_name: Option<String>,
    width: u16,
    height: u16,
    force_keyframe: Arc<AtomicBool>,
}

impl FrameCaptureDevice {
    /// Create a new frame capture device. `width`/`height` are the requested
    /// encoded size; they are clamped to the X11 screen and made even for H.264.
    pub fn new(
        display_name: Option<String>,
        width: u16,
        height: u16,
        force_keyframe: Arc<AtomicBool>,
    ) -> Self {
        println!(
            "Creating FrameCaptureDevice for display {:?} at {}x{}",
            display_name, width, height
        );
        Self { display_name, width, height, force_keyframe }
    }

    pub fn start_capturing_frames(
        self,
        tx: watch::Sender<Option<Arc<Frame>>>,
    ) -> std::io::Result<()> {
        let (raw_tx, raw_rx) = sync_channel(1);

        let display_name = self.display_name;
        let requested_width = self.width;
        let requested_height = self.height;
        thread::Builder::new().name("capture".to_string()).spawn(move || {
            if let Err(error) =
                capture_loop(display_name, requested_width, requested_height, raw_tx)
            {
                eprintln!("Frame capture failed: {error}");
            }
        })?;

        let force_keyframe = self.force_keyframe;
        thread::Builder::new().name("encoder".to_string()).spawn(move || {
            if let Err(error) = encode_loop(raw_rx, tx, force_keyframe) {
                eprintln!("Frame encoder failed: {error}");
            }
        })?;

        Ok(())
    }
}

fn capture_loop(
    display_name: Option<String>,
    requested_width: u16,
    requested_height: u16,
    raw_tx: SyncSender<RawFrame>,
) -> Result<()> {
    let (conn, screen_num) = RustConnection::connect(display_name.as_deref())?;
    let screen = &conn.setup().roots[screen_num];
    let screen_width = screen.width_in_pixels;
    let screen_height = screen.height_in_pixels;

    let width = requested_width.min(screen_width).max(2) & !1;
    let height = requested_height.min(screen_height).max(2) & !1;
    let format = root_pixel_format(screen)?;

    println!(
        "Connected to X11 display, screen size: {}x{}, encoding: {}x{}",
        screen_width, screen_height, width, height
    );

    let mut capture_time = Duration::ZERO;
    let mut frames = 0u32;
    let frame_interval = Duration::from_millis(33);

    loop {
        let started = Instant::now();
        let image_reply = conn
            .get_image(ImageFormat::Z_PIXMAP, screen.root, 0, 0, screen_width, screen_height, !0)?
            .reply()?;
        capture_time += started.elapsed();

        let timestamp_us = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
        let raw = RawFrame {
            data: image_reply.data,
            width: screen_width as usize,
            height: screen_height as usize,
            target_width: width as usize,
            target_height: height as usize,
            format,
            timestamp_us,
        };

        frames += 1;
        if frames >= 30 {
            tracing::debug!(
                capture_ms = capture_time.as_secs_f64() * 1000.0 / f64::from(frames),
                "Capture timing"
            );
            frames = 0;
            capture_time = Duration::ZERO;
        }

        match raw_tx.try_send(raw) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // The encoder is busy; keep the newest frame and drop this one.
            }
            Err(TrySendError::Disconnected(_)) => {
                println!("Encoder dropped; stopping capture loop.");
                break;
            }
        }

        // Cap capture at ~30 fps. Encoding can run faster on static content,
        // but sending 80 near-identical frames per second wastes CPU and
        // bandwidth without improving the target experience.
        let elapsed = started.elapsed();
        if elapsed < frame_interval {
            thread::sleep(frame_interval - elapsed);
        }
    }

    Ok(())
}

fn encode_loop(
    raw_rx: Receiver<RawFrame>,
    tx: watch::Sender<Option<Arc<Frame>>>,
    force_keyframe: Arc<AtomicBool>,
) -> Result<()> {
    // Screen content at 720p30: favor speed over compression quality.
    let encoder_config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(30.0))
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .complexity(Complexity::Low)
        .skip_frames(false);
    let mut encoder =
        Encoder::with_api_config(openh264::OpenH264API::from_source(), encoder_config)?;

    let mut frames_since_keyframe = 0u32;
    let mut scale_time = Duration::ZERO;
    let mut convert_time = Duration::ZERO;
    let mut encode_time = Duration::ZERO;
    let mut timing_frames = 0u32;

    while let Ok(raw) = raw_rx.recv() {
        let forced = force_keyframe.swap(false, Ordering::SeqCst);
        if forced || frames_since_keyframe >= 60 {
            tracing::debug!("Requesting encoder keyframe (forced={forced})");
            encoder.force_intra_frame();
            frames_since_keyframe = 0;
        }

        let scale_started = Instant::now();
        let rgb8 = downscale_to_rgb8(
            &raw.data,
            raw.width,
            raw.height,
            raw.target_width,
            raw.target_height,
            raw.format,
        );
        scale_time += scale_started.elapsed();

        let convert_started = Instant::now();
        let rgb_source = RgbSliceU8::new(&rgb8, (raw.target_width, raw.target_height));
        let yuv_buffer = YUVBuffer::from_rgb8_source(rgb_source);
        convert_time += convert_started.elapsed();

        let encode_started = Instant::now();
        let bitstream = encoder.encode(&yuv_buffer)?;
        let encoded_data = bitstream.to_vec();
        encode_time += encode_started.elapsed();
        frames_since_keyframe = frames_since_keyframe.saturating_add(1);
        timing_frames += 1;

        if timing_frames >= 30 {
            tracing::debug!(
                scale_ms = scale_time.as_secs_f64() * 1000.0 / f64::from(timing_frames),
                convert_ms = convert_time.as_secs_f64() * 1000.0 / f64::from(timing_frames),
                encode_ms = encode_time.as_secs_f64() * 1000.0 / f64::from(timing_frames),
                "Encoder timing"
            );
            timing_frames = 0;
            scale_time = Duration::ZERO;
            convert_time = Duration::ZERO;
            encode_time = Duration::ZERO;
        }

        if encoded_data.is_empty() {
            continue;
        }

        let is_idr = nal_units(&encoded_data).any(is_idr_nal);
        if is_idr {
            tracing::debug!(bytes = encoded_data.len(), "Encoder produced an IDR frame");
        }
        let flags =
            if is_idr { FrameFlags::KEYFRAME | FrameFlags::IDR } else { FrameFlags::empty() };
        let frame = Frame::new(0, raw.timestamp_us, flags, encoded_data);

        tx.send_replace(Some(Arc::new(frame)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_visual_masks_to_byte_offsets() {
        assert_eq!(channel_offset(0x00ff_0000), Some(2));
        assert_eq!(channel_offset(0x0000_ff00), Some(1));
        assert_eq!(channel_offset(0x0000_00ff), Some(0));
        assert_eq!(channel_offset(0), None);
        assert_eq!(channel_offset(0b1111_0000), None);
    }

    #[test]
    fn reads_bgrx_through_visual_masks() {
        let format = PixelFormat { red: 2, green: 1, blue: 0 };
        let src = vec![0x12, 0x34, 0x56, 0x00];
        let rgb = downscale_to_rgb8(&src, 1, 1, 1, 1, format);
        assert_eq!(rgb, vec![0x56, 0x34, 0x12]);
    }

    #[test]
    fn detects_idr_after_annex_b_start_code() {
        assert!(is_idr_nal(&[0, 0, 1, 0x65, 0x88]));
        assert!(is_idr_nal(&[0, 0, 0, 1, 0x65, 0x88]));
        assert!(!is_idr_nal(&[0, 0, 1, 0x41, 0x9a]));
        assert!(!is_idr_nal(&[0x65, 0x88]));
    }

    #[test]
    fn downscale_averages_2x2_block_and_reorders_rgb() {
        let src = vec![
            0, 0, 255, 0, // red (BGRX)
            0, 255, 0, 0, // green
            255, 0, 0, 0, // blue
            255, 255, 255, 0, // white
        ];
        let dst = downscale_to_rgb8(&src, 2, 2, 1, 1, PixelFormat { red: 2, green: 1, blue: 0 });
        assert_eq!(dst, vec![127, 127, 127]);
    }

    #[test]
    fn downscale_preserves_size_and_channels() {
        let src = vec![0u8; 4 * 4 * 4];
        let dst = downscale_to_rgb8(&src, 4, 4, 2, 2, PixelFormat { red: 2, green: 1, blue: 0 });
        assert_eq!(dst.len(), 2 * 2 * 3);
    }
}
