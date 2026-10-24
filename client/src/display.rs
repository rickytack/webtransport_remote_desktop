use crate::input::{CaptureOptions, InputState};
use common::control::InputEvent;
use minifb::{Key, Window, WindowOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// One decoded RGB frame ready for the window.
pub struct DisplayFrame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

type FrameSlot = Arc<(Mutex<Option<DisplayFrame>>, Condvar)>;

/// Handle to the display thread.
///
/// The display thread reads from a single-slot mailbox. If the network delivers
/// faster than the window can present, the newest frame replaces the previous
/// one instead of queueing and adding latency.
pub struct Display {
    slot: FrameSlot,
    running: Arc<AtomicBool>,
}

impl Display {
    /// `input` receives enabled keyboard and mouse events from the window
    /// thread. Local quit shortcuts stay active regardless of `capture`.
    pub fn spawn(
        input: UnboundedSender<InputEvent>,
        capture: CaptureOptions,
    ) -> std::io::Result<Self> {
        let slot: FrameSlot = Arc::new((Mutex::new(None), Condvar::new()));
        let running = Arc::new(AtomicBool::new(true));

        let thread_slot = Arc::clone(&slot);
        let thread_running = Arc::clone(&running);
        std::thread::Builder::new()
            .name("display".to_string())
            .spawn(move || run_window(thread_slot, thread_running, input, capture))?;

        Ok(Self { slot, running })
    }

    /// Returns false when the window has been closed.
    pub fn show(&self, frame: DisplayFrame) -> bool {
        if !self.is_running() {
            return false;
        }

        let (lock, condvar) = &*self.slot;
        let mut slot = lock.lock().expect("display mailbox poisoned");
        *slot = Some(frame);
        condvar.notify_one();
        true
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

fn run_window(
    slot: FrameSlot,
    running: Arc<AtomicBool>,
    input: UnboundedSender<InputEvent>,
    capture: CaptureOptions,
) {
    let first = match wait_for_frame(&slot, &running) {
        Some(frame) => frame,
        None => return,
    };

    let width = first.width;
    let height = first.height;
    if width == 0 || height == 0 {
        eprintln!("display: refusing empty frame");
        running.store(false, Ordering::Relaxed);
        return;
    }

    let mut window = match Window::new(
        "WebTransport Remote",
        width,
        height,
        WindowOptions { resize: false, ..Default::default() },
    ) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("display: cannot create window: {error}");
            running.store(false, Ordering::Relaxed);
            return;
        }
    };
    window.set_target_fps(60);

    let mut buffer = vec![0u32; width * height];
    let mut current = first;
    let mut input_state = InputState::new(capture);

    loop {
        if !window.is_open() || local_quit(&window) {
            break;
        }

        let (lock, _) = &*slot;
        let next = lock.lock().expect("display mailbox poisoned").take();
        if let Some(frame) = next {
            if frame.width == width && frame.height == height {
                current = frame;
            } else {
                eprintln!(
                    "display: ignoring frame size change {}x{} (window is {}x{})",
                    frame.width, frame.height, width, height
                );
            }
        }

        if !blit(&current, &mut buffer) {
            eprintln!(
                "display: unexpected frame size {}x{} (expected {}x{})",
                current.width, current.height, width, height
            );
            break;
        }

        if let Err(error) = window.update_with_buffer(&buffer, width, height) {
            eprintln!("display: update failed: {error}");
            break;
        }

        input_state.poll(&window, width, height, &input);

        // `update_with_buffer` already paces to the target fps, but a tiny
        // sleep keeps the thread from spinning if minifb changes its pacing.
        std::thread::sleep(Duration::from_millis(1));
    }

    // Do not leave keys or buttons stuck on the remote host.
    input_state.release_all(&input);
    running.store(false, Ordering::Relaxed);
}

fn local_quit(window: &Window) -> bool {
    let ctrl = window.is_key_down(Key::LeftCtrl) || window.is_key_down(Key::RightCtrl);
    let alt = window.is_key_down(Key::LeftAlt) || window.is_key_down(Key::RightAlt);
    let shift = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
    // Some desktops intercept Alt chords before the window sees them, so
    // Ctrl+Shift+Q is an equivalent fallback.
    ctrl && (alt || shift) && window.is_key_down(Key::Q)
}

fn wait_for_frame(slot: &FrameSlot, running: &AtomicBool) -> Option<DisplayFrame> {
    let (lock, condvar) = &**slot;
    let mut guard = lock.lock().expect("display mailbox poisoned");

    loop {
        if let Some(frame) = guard.take() {
            return Some(frame);
        }
        if !running.load(Ordering::Relaxed) {
            return None;
        }
        guard = condvar.wait(guard).expect("display mailbox poisoned");
    }
}

fn blit(frame: &DisplayFrame, buffer: &mut [u32]) -> bool {
    let expected = frame.width * frame.height;
    if buffer.len() != expected || frame.rgb.len() != expected * 3 {
        return false;
    }

    for (dst, rgb) in buffer.iter_mut().zip(frame.rgb.as_chunks::<3>().0) {
        *dst = pack_rgb(rgb[0], rgb[1], rgb[2]);
    }
    true
}

fn pack_rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_rgb_in_minifb_order() {
        assert_eq!(pack_rgb(0x12, 0x34, 0x56), 0x0012_3456);
    }

    #[test]
    fn blits_and_rejects_wrong_sizes() {
        let frame = DisplayFrame { width: 2, height: 1, rgb: vec![255, 0, 0, 0, 255, 0] };
        let mut buffer = vec![0u32; 2];
        assert!(blit(&frame, &mut buffer));
        assert_eq!(buffer, vec![0x00ff_0000, 0x0000_ff00]);

        let bad = DisplayFrame { width: 2, height: 1, rgb: vec![0; 5] };
        assert!(!blit(&bad, &mut buffer));
    }

    #[test]
    fn newest_frame_replaces_queued_frame() {
        let slot: FrameSlot = Arc::new((Mutex::new(None), Condvar::new()));
        let running = AtomicBool::new(true);
        let display = Display { slot: Arc::clone(&slot), running: Arc::new(AtomicBool::new(true)) };

        assert!(display.show(DisplayFrame { width: 1, height: 1, rgb: vec![1, 0, 0] }));
        assert!(display.show(DisplayFrame { width: 1, height: 1, rgb: vec![0, 1, 0] }));

        let frame = wait_for_frame(&slot, &running).expect("frame");
        assert_eq!(frame.rgb, vec![0, 1, 0]);
    }
}
