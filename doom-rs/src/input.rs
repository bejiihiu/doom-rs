//! Input mapping: OS-level keys/buttons down to Doom key codes.
//!
//! The engine keeps one `gamekeydown[256]` array indexed by Doom key
//! codes (`doomdef.h`) and merges it with the per-tic [`TicCmdInput`].
//! The platform's job is to translate winit's physical-key events into
//! those codes without reinterpreting the user's bindings.
//!
//! Each physical key emits its configured binding plus the vanilla fixed
//! code when they differ, so menu hotkeys, cheats and the save-game
//! editor keep working whether or not the user rebound movement keys.
//! Printable keys also emit their ASCII byte, which is what the cheat
//! table and the menu text entry read.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

use doom_engine::game::{KeyBindings, keys};

/// Doom button bits (`MOUSE_*` / `JOY_*` in `d_event.h`). The engine keeps
/// these private, and the values are fixed by the vanilla format, so they
/// are mirrored here.
pub mod buttons {
    /// Left mouse button: fire.
    pub const MOUSE_FIRE: u8 = 1;
    /// Middle mouse button: strafe modifier.
    pub const MOUSE_STRAFE: u8 = 2;
    /// Left joystick trigger / fire.
    pub const JOY_FIRE: u8 = 1;
    /// Joystick strafe modifier.
    pub const JOY_STRAFE: u8 = 2;
    /// Joystick use.
    pub const JOY_USE: u8 = 4;
    /// Joystick speed modifier.
    pub const JOY_SPEED: u8 = 8;
}

/// The Doom codes a physical key stands for, in emission order.
///
/// `Escape`, `Tab`, `Enter`, `Backspace`, `Pause` and the F-keys have no
/// bindings; everything else goes through the configured bindings so
/// `doom.cfg` rebinds win over the compiled-in vanilla defaults.
pub fn doom_codes_for_key(code: KeyCode, bindings: &KeyBindings) -> Vec<u8> {
    let mut out = Vec::with_capacity(3);
    let mut push = |byte: u8| {
        if !out.contains(&byte) {
            out.push(byte);
        }
    };
    let b = bindings;
    match code {
        // -- movement aliases: WASD mirrors the arrows -------------------
        KeyCode::KeyW => {
            push(b'w');
            push(b.key_up);
            push(keys::UPARROW);
        }
        KeyCode::KeyS => {
            push(b's');
            push(b.key_down);
            push(keys::DOWNARROW);
        }
        KeyCode::KeyA => {
            push(b'a');
            push(b.key_strafeleft);
            push(b',');
        }
        KeyCode::KeyD => {
            push(b'd');
            push(b.key_straferight);
            push(b'.');
        }
        KeyCode::ArrowUp => {
            push(keys::UPARROW);
            push(b.key_up);
        }
        KeyCode::ArrowDown => {
            push(keys::DOWNARROW);
            push(b.key_down);
        }
        KeyCode::ArrowLeft => {
            push(keys::LEFTARROW);
            push(b.key_left);
        }
        KeyCode::ArrowRight => {
            push(keys::RIGHTARROW);
            push(b.key_right);
        }
        // -- modifiers ----------------------------------------------------
        KeyCode::ControlLeft | KeyCode::ControlRight => {
            push(keys::RCTRL);
            push(b.key_fire);
        }
        KeyCode::Space => {
            push(b' ');
            push(b.key_use);
        }
        KeyCode::ShiftLeft | KeyCode::ShiftRight => {
            push(keys::RSHIFT);
            push(b.key_speed);
        }
        KeyCode::AltLeft | KeyCode::AltRight => {
            push(keys::RALT);
            push(b.key_strafe);
        }
        KeyCode::KeyQ => {
            push(b'q');
            push(b.key_strafe);
        }
        // -- raw printable bytes: cheats and the save/load editor ---------
        KeyCode::KeyE => push(b'e'),
        KeyCode::KeyF => push(b'f'),
        KeyCode::KeyG => push(b'g'),
        KeyCode::KeyH => push(b'h'),
        KeyCode::KeyI => push(b'i'),
        KeyCode::KeyJ => push(b'j'),
        KeyCode::KeyK => push(b'k'),
        KeyCode::KeyL => push(b'l'),
        KeyCode::KeyM => push(b'm'),
        KeyCode::KeyN => push(b'n'),
        KeyCode::KeyO => push(b'o'),
        KeyCode::KeyP => push(b'p'),
        KeyCode::KeyR => push(b'r'),
        KeyCode::KeyT => push(b't'),
        KeyCode::KeyU => push(b'u'),
        KeyCode::KeyV => push(b'v'),
        KeyCode::KeyX => push(b'x'),
        KeyCode::KeyY => push(b'y'),
        KeyCode::KeyZ => push(b'z'),
        KeyCode::KeyB => push(b'b'),
        KeyCode::KeyC => push(b'c'),
        KeyCode::Digit0 | KeyCode::Numpad0 => push(b'0'),
        KeyCode::Comma => {
            push(b',');
            push(b.key_strafeleft);
        }
        KeyCode::Period => {
            push(b'.');
            push(b.key_straferight);
        }
        KeyCode::Slash => push(b'/'),
        KeyCode::Backslash => push(b'\\'),
        KeyCode::Minus => push(b'-'),
        KeyCode::Equal => push(b'='),
        KeyCode::Semicolon => push(b';'),
        KeyCode::Quote => push(b'\''),
        KeyCode::BracketLeft => push(b'['),
        KeyCode::BracketRight => push(b']'),
        KeyCode::Backquote => push(b'`'),
        // -- fixed keys ---------------------------------------------------
        KeyCode::Escape => push(keys::ESCAPE),
        KeyCode::Tab => push(keys::TAB),
        KeyCode::Enter | KeyCode::NumpadEnter => push(keys::ENTER),
        KeyCode::Backspace => push(keys::BACKSPACE),
        KeyCode::Pause => push(keys::PAUSE),
        KeyCode::F1 => push(keys::F1),
        KeyCode::F2 => push(keys::F2),
        KeyCode::F3 => push(keys::F3),
        KeyCode::F4 => push(keys::F4),
        KeyCode::F5 => push(keys::F5),
        KeyCode::F6 => push(keys::F6),
        KeyCode::F7 => push(0x80 + 0x41),
        KeyCode::F8 => push(0x80 + 0x42),
        KeyCode::F9 => push(keys::F9),
        KeyCode::F10 => push(keys::F10),
        KeyCode::F11 => push(keys::F11),
        KeyCode::F12 => push(keys::F12),
        // digits double as weapon keys
        KeyCode::Digit1 | KeyCode::Numpad1 => push(b'1'),
        KeyCode::Digit2 | KeyCode::Numpad2 => push(b'2'),
        KeyCode::Digit3 | KeyCode::Numpad3 => push(b'3'),
        KeyCode::Digit4 | KeyCode::Numpad4 => push(b'4'),
        KeyCode::Digit5 | KeyCode::Numpad5 => push(b'5'),
        KeyCode::Digit6 | KeyCode::Numpad6 => push(b'6'),
        KeyCode::Digit7 | KeyCode::Numpad7 => push(b'7'),
        KeyCode::Digit8 | KeyCode::Numpad8 => push(b'8'),
        KeyCode::Digit9 | KeyCode::Numpad9 => push(b'9'),
        _ => {}
    }
    out
}

/// Weapon number a digit key selects, if it is one (`1..=9`).
pub fn weapon_digit(code: KeyCode) -> Option<u8> {
    let offset = match code {
        KeyCode::Digit1 | KeyCode::Numpad1 => 0,
        KeyCode::Digit2 | KeyCode::Numpad2 => 1,
        KeyCode::Digit3 | KeyCode::Numpad3 => 2,
        KeyCode::Digit4 | KeyCode::Numpad4 => 3,
        KeyCode::Digit5 | KeyCode::Numpad5 => 4,
        KeyCode::Digit6 | KeyCode::Numpad6 => 5,
        KeyCode::Digit7 | KeyCode::Numpad7 => 6,
        KeyCode::Digit8 | KeyCode::Numpad8 => 7,
        KeyCode::Digit9 | KeyCode::Numpad9 => 8,
        _ => return None,
    };
    Some(b'1' + offset)
}

/// Mouse button bit the engine understands, or `None` when the button is
/// handled by the platform instead (the right button is "use").
pub fn mouse_button_bit(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Left => Some(buttons::MOUSE_FIRE),
        MouseButton::Middle => Some(buttons::MOUSE_STRAFE),
        // right button and the rest are platform-level (use, weapon cycling)
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_bindings() -> KeyBindings {
        KeyBindings::default()
    }

    #[test]
    fn wasd_emits_letters_and_bound_movement_codes() {
        let bindings = default_bindings();
        assert_eq!(
            doom_codes_for_key(KeyCode::KeyW, &bindings),
            vec![b'w', keys::UPARROW]
        );
        assert_eq!(
            doom_codes_for_key(KeyCode::KeyS, &bindings),
            vec![b's', keys::DOWNARROW]
        );
        assert!(doom_codes_for_key(KeyCode::KeyA, &bindings).contains(&b','));
        assert!(doom_codes_for_key(KeyCode::KeyD, &bindings).contains(&b'.'));
    }

    #[test]
    fn custom_binding_replaces_the_bound_code_but_not_the_letter() {
        let mut bindings = default_bindings();
        bindings.key_up = b'x';
        assert_eq!(
            doom_codes_for_key(KeyCode::KeyW, &bindings),
            vec![b'w', b'x', keys::UPARROW]
        );
    }

    #[test]
    fn duplicate_codes_are_emitted_once() {
        // default key_use is a space, and space emits b' ' itself too
        let bindings = default_bindings();
        let codes = doom_codes_for_key(KeyCode::Space, &bindings);
        assert_eq!(codes.iter().filter(|&&c| c == b' ').count(), 1);
    }

    #[test]
    fn arrows_always_emit_the_fixed_codes() {
        let mut bindings = default_bindings();
        bindings.key_up = b'i';
        assert_eq!(
            doom_codes_for_key(KeyCode::ArrowUp, &bindings),
            vec![keys::UPARROW, b'i']
        );
    }

    #[test]
    fn cheat_letters_are_lowercase() {
        let bindings = default_bindings();
        for (code, letter) in [
            (KeyCode::KeyI, b'i'),
            (KeyCode::KeyD, b'd'),
            (KeyCode::KeyG, b'g'),
            (KeyCode::KeyQ, b'q'),
        ] {
            assert!(doom_codes_for_key(code, &bindings).contains(&letter));
        }
    }

    #[test]
    fn escape_and_tab_are_fixed() {
        let bindings = default_bindings();
        assert_eq!(
            doom_codes_for_key(KeyCode::Escape, &bindings),
            vec![keys::ESCAPE]
        );
        assert_eq!(doom_codes_for_key(KeyCode::Tab, &bindings), vec![keys::TAB]);
    }

    #[test]
    fn unmapped_keys_emit_nothing() {
        let bindings = default_bindings();
        assert!(doom_codes_for_key(KeyCode::F13, &bindings).is_empty());
    }

    #[test]
    fn weapon_digits_1_through_9() {
        assert_eq!(weapon_digit(KeyCode::Digit1), Some(b'1'));
        assert_eq!(weapon_digit(KeyCode::Digit9), Some(b'9'));
        assert_eq!(weapon_digit(KeyCode::Numpad3), Some(b'3'));
        assert_eq!(weapon_digit(KeyCode::Digit0), None);
        assert_eq!(weapon_digit(KeyCode::Space), None);
    }

    #[test]
    fn digits_do_not_leak_into_letters_except_as_digits() {
        let bindings = default_bindings();
        assert_eq!(doom_codes_for_key(KeyCode::Digit5, &bindings), vec![b'5']);
    }

    #[test]
    fn mouse_bits_left_and_middle_only() {
        assert_eq!(
            mouse_button_bit(MouseButton::Left),
            Some(buttons::MOUSE_FIRE)
        );
        assert_eq!(
            mouse_button_bit(MouseButton::Middle),
            Some(buttons::MOUSE_STRAFE)
        );
        assert_eq!(mouse_button_bit(MouseButton::Right), None);
    }
}
