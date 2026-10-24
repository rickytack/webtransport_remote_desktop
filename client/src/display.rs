use common::control::{InputEvent, KeyCode, MouseButton as ControlMouseButton};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};
use std::collections::HashSet;
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

/// Which input classes the client forwards to the remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOptions {
    pub keyboard: bool,
    pub mouse: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self { keyboard: true, mouse: true }
    }
}

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

#[derive(Default)]
struct InputState {
    capture: CaptureOptions,
    previous_keys: HashSet<Key>,
    buttons: [bool; 5],
    mouse: Option<(u16, u16)>,
    active: bool,
}

const BUTTONS: [(MouseButton, ControlMouseButton); 5] = [
    (MouseButton::Left, ControlMouseButton::Left),
    (MouseButton::Middle, ControlMouseButton::Middle),
    (MouseButton::Right, ControlMouseButton::Right),
    (MouseButton::Back, ControlMouseButton::Back),
    (MouseButton::Forward, ControlMouseButton::Forward),
];

impl InputState {
    fn new(capture: CaptureOptions) -> Self {
        Self { capture, ..Self::default() }
    }

    fn poll(
        &mut self,
        window: &Window,
        width: usize,
        height: usize,
        input: &UnboundedSender<InputEvent>,
    ) {
        // Never forward input while the user is working in another local
        // window. Release anything held so the remote does not get stuck.
        if !window.is_active() {
            if self.active {
                self.release_all(input);
                self.previous_keys.clear();
                self.buttons = [false; 5];
            }
            self.active = false;
            self.mouse = None;
            return;
        }
        if !self.active {
            self.active = true;
            self.mouse = None;
        }

        if self.capture.keyboard {
            let current_keys: HashSet<Key> = window.get_keys().into_iter().collect();
            for key in current_keys.difference(&self.previous_keys) {
                if let Some(key) = map_key(*key) {
                    send(input, InputEvent::Key { key, pressed: true });
                }
            }
            for key in self.previous_keys.difference(&current_keys) {
                if let Some(key) = map_key(*key) {
                    send(input, InputEvent::Key { key, pressed: false });
                }
            }
            self.previous_keys = current_keys;
        } else {
            self.previous_keys.clear();
        }

        if self.capture.mouse {
            for (index, (local, remote)) in BUTTONS.iter().enumerate() {
                let pressed = window.get_mouse_down(*local);
                if pressed != self.buttons[index] {
                    self.buttons[index] = pressed;
                    send(input, InputEvent::Button { button: *remote, pressed });
                }
            }

            if let Some((x, y)) = window.get_mouse_pos(MouseMode::Clamp) {
                let max_x = width.saturating_sub(1) as f32;
                let max_y = height.saturating_sub(1) as f32;
                let position =
                    (x.round().clamp(0.0, max_x) as u16, y.round().clamp(0.0, max_y) as u16);
                if self.mouse != Some(position) {
                    self.mouse = Some(position);
                    send(input, InputEvent::PointerMove { x: position.0, y: position.1 });
                }
            }

            if let Some((dx, dy)) = window.get_scroll_wheel() {
                let dx = dx.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
                let dy = dy.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
                if dx != 0 || dy != 0 {
                    send(input, InputEvent::Scroll { dx, dy });
                }
            }
        } else {
            self.buttons = [false; 5];
            self.mouse = None;
        }
    }

    fn release_all(&self, input: &UnboundedSender<InputEvent>) {
        for key in &self.previous_keys {
            if let Some(key) = map_key(*key) {
                send(input, InputEvent::Key { key, pressed: false });
            }
        }
        for (index, (_, remote)) in BUTTONS.iter().enumerate() {
            if self.buttons[index] {
                send(input, InputEvent::Button { button: *remote, pressed: false });
            }
        }
    }
}

fn send(input: &UnboundedSender<InputEvent>, event: InputEvent) {
    // A closed channel means the connection/input task is gone; the video
    // task will report the actual connection error.
    let _ = input.send(event);
}

fn map_key(key: Key) -> Option<KeyCode> {
    Some(match key {
        Key::Key0 => KeyCode::Key0,
        Key::Key1 => KeyCode::Key1,
        Key::Key2 => KeyCode::Key2,
        Key::Key3 => KeyCode::Key3,
        Key::Key4 => KeyCode::Key4,
        Key::Key5 => KeyCode::Key5,
        Key::Key6 => KeyCode::Key6,
        Key::Key7 => KeyCode::Key7,
        Key::Key8 => KeyCode::Key8,
        Key::Key9 => KeyCode::Key9,
        Key::A => KeyCode::A,
        Key::B => KeyCode::B,
        Key::C => KeyCode::C,
        Key::D => KeyCode::D,
        Key::E => KeyCode::E,
        Key::F => KeyCode::F,
        Key::G => KeyCode::G,
        Key::H => KeyCode::H,
        Key::I => KeyCode::I,
        Key::J => KeyCode::J,
        Key::K => KeyCode::K,
        Key::L => KeyCode::L,
        Key::M => KeyCode::M,
        Key::N => KeyCode::N,
        Key::O => KeyCode::O,
        Key::P => KeyCode::P,
        Key::Q => KeyCode::Q,
        Key::R => KeyCode::R,
        Key::S => KeyCode::S,
        Key::T => KeyCode::T,
        Key::U => KeyCode::U,
        Key::V => KeyCode::V,
        Key::W => KeyCode::W,
        Key::X => KeyCode::X,
        Key::Y => KeyCode::Y,
        Key::Z => KeyCode::Z,
        Key::F1 => KeyCode::F1,
        Key::F2 => KeyCode::F2,
        Key::F3 => KeyCode::F3,
        Key::F4 => KeyCode::F4,
        Key::F5 => KeyCode::F5,
        Key::F6 => KeyCode::F6,
        Key::F7 => KeyCode::F7,
        Key::F8 => KeyCode::F8,
        Key::F9 => KeyCode::F9,
        Key::F10 => KeyCode::F10,
        Key::F11 => KeyCode::F11,
        Key::F12 => KeyCode::F12,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Up => KeyCode::Up,
        Key::Apostrophe => KeyCode::Apostrophe,
        Key::Backquote => KeyCode::Backquote,
        Key::Backslash => KeyCode::Backslash,
        Key::Comma => KeyCode::Comma,
        Key::Equal => KeyCode::Equal,
        Key::LeftBracket => KeyCode::LeftBracket,
        Key::Minus => KeyCode::Minus,
        Key::Period => KeyCode::Period,
        Key::RightBracket => KeyCode::RightBracket,
        Key::Semicolon => KeyCode::Semicolon,
        Key::Slash => KeyCode::Slash,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::End => KeyCode::End,
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Escape,
        Key::Home => KeyCode::Home,
        Key::Insert => KeyCode::Insert,
        Key::PageDown => KeyCode::PageDown,
        Key::PageUp => KeyCode::PageUp,
        Key::Space => KeyCode::Space,
        Key::Tab => KeyCode::Tab,
        Key::NumLock => KeyCode::NumLock,
        Key::CapsLock => KeyCode::CapsLock,
        Key::ScrollLock => KeyCode::ScrollLock,
        Key::LeftShift => KeyCode::ShiftLeft,
        Key::RightShift => KeyCode::ShiftRight,
        Key::LeftCtrl => KeyCode::ControlLeft,
        Key::RightCtrl => KeyCode::ControlRight,
        Key::LeftAlt => KeyCode::AltLeft,
        Key::RightAlt => KeyCode::AltRight,
        _ => return None,
    })
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

    #[test]
    fn release_all_emits_key_and_button_releases() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = InputState::default();
        state.previous_keys.insert(Key::A);
        state.buttons[0] = true;

        state.release_all(&tx);

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&InputEvent::Key { key: KeyCode::A, pressed: false }));
        assert!(events
            .contains(&InputEvent::Button { button: ControlMouseButton::Left, pressed: false }));
    }

    #[test]
    fn maps_common_keys_and_ignores_unsupported_keys() {
        assert_eq!(map_key(Key::A), Some(KeyCode::A));
        assert_eq!(map_key(Key::Key9), Some(KeyCode::Key9));
        assert_eq!(map_key(Key::Escape), Some(KeyCode::Escape));
        assert_eq!(map_key(Key::LeftShift), Some(KeyCode::ShiftLeft));
        assert_eq!(map_key(Key::F12), Some(KeyCode::F12));
        assert_eq!(map_key(Key::Unknown), None);
        assert_eq!(map_key(Key::NumPad0), None);
    }
}
