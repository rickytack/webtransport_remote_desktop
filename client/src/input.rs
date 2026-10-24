use common::control::{InputEvent, KeyCode, MouseButton as ControlMouseButton};
use minifb::{Key, MouseButton, MouseMode, Window};
use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;

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

#[derive(Default)]
pub(crate) struct InputState {
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
    pub(crate) fn new(capture: CaptureOptions) -> Self {
        Self { capture, ..Self::default() }
    }

    pub(crate) fn poll(
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

    pub(crate) fn release_all(&self, input: &UnboundedSender<InputEvent>) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
