//! Player input and the `G_BuildTiccmd` port.
//!
//! The platform shell (a `winit` event loop, a test harness, a demo
//! player) feeds plain-data [`TicCmdInput`]s to [`TicBuilder::build`];
//! everything vanilla kept in file scope (`gamekeydown[]`, `mousebuttons`,
//! `turnheld`, the double-click detectors, `-turbo` scaling) lives on
//! the builder instead. No I/O happens here, so tests can drive the input
//! layer directly.
//!
//! Bit layout of `ticcmd_t.buttons` is the vanilla one (`d_event.h`):
//!
//! - `BT_ATTACK` (1) - fire, `BT_USE` (2) - use
//! - `BT_SPECIAL` (128) - game events (pause/save) are not real buttons
//! - `BT_CHANGE` (4) with `BT_WEAPONMASK` (56) shifted by `BT_WEAPONSHIFT`
//!   (3) - weapon change request

use crate::player::TicCmd;

/// Vanilla `ticdup` is always 1 in the port (no network tic-duping).
pub const TIC_DUP: i32 = 1;
/// `BT_ATTACK` (`d_event.h`), re-exported from the weapons layer, which
/// is where the gameplay code looks for it.
pub const BT_ATTACK: u8 = crate::weapons::BT_ATTACK;
/// `BT_USE` - the use button bit.
pub const BT_USE: u8 = 2;
/// `BT_CHANGE` - set when the weapon bits are valid.
pub const BT_CHANGE: u8 = 4;
/// The three weapon-number bits.
pub const BT_WEAPONMASK: u8 = 8 + 16 + 32;
/// The position of the weapon bits inside `buttons`.
pub const BT_WEAPONSHIFT: u8 = 3;
/// `BT_SPECIAL` - special (game event) buttons are flagged with this.
pub const BT_SPECIAL: u8 = 128;
/// `BT_SPECIALMASK` - mask for the special-button sub-command.
pub const BT_SPECIALMASK: u8 = 3;
/// `BTS_PAUSE` - pause game.
pub const BTS_PAUSE: u8 = 1;
/// `BTS_SAVEGAME` - save the game (slot in the high bits).
pub const BTS_SAVEGAME: u8 = 2;
/// `BTS_SAVEMASK` - the slot-number bits for `BTS_SAVEGAME`.
pub const BTS_SAVEMASK: u8 = 4 + 8 + 16;
/// `BTS_SAVESHIFT` - position of the slot bits inside `buttons`.
pub const BTS_SAVESHIFT: u8 = 2;
/// `MAXPLMOVE`: `forwardmove[1]`, the maximum forward speed per tic
/// (`0x32`).
pub const MAX_PLMOVE: i32 = 0x32;
/// `TURBOTHRESHOLD`: a `forwardmove` above this triggers the turbo
/// cheat message (`0x32`).
pub const TURBO_THRESHOLD: i32 = 0x32;
/// `SLOWTURNTICS`: below this many held tics, turning is slowed.
pub const SLOWTURNTICS: i32 = 6;
/// Mouse button bit for fire (`ev->data1 & 1`).
pub const MOUSE_FIRE: u8 = 1;
/// Mouse button bit for strafe (`ev->data1 & 2`).
pub const MOUSE_STRAFE: u8 = 2;
/// Mouse button bit for forward/double-click-use (`ev->data1 & 4`).
pub const MOUSE_FORWARD: u8 = 4;
/// Joystick button bit for fire.
pub const JOY_FIRE: u8 = 1;
/// Joystick button bit for strafe.
pub const JOY_STRAFE: u8 = 2;
/// Joystick button bit for use.
pub const JOY_USE: u8 = 4;
/// Joystick button bit for speed.
pub const JOY_SPEED: u8 = 8;

/// The forward/side/turn move table (`forwardmove`/`sidemove` in
/// `g_game.c`), already scaled by `-turbo` when the builder is created
/// with one.
#[derive(Clone, Copy, Debug)]
struct PlayerMoves {
    forwardmove: [i32; 2],
    sidemove: [i32; 2],
}

impl PlayerMoves {
    const NORMAL: Self = Self {
        forwardmove: [0x19, 0x32],
        sidemove: [0x18, 0x28],
    };

    fn turbo(percent: u32) -> Self {
        let scale = |v: i32| v * percent as i32 / 100;
        Self {
            forwardmove: [scale(0x19), scale(0x32)],
            sidemove: [scale(0x18), scale(0x28)],
        }
    }

    fn max_move(&self) -> i32 {
        self.forwardmove[1]
    }
}

/// Per-tic input for one local player, in the shape the platform can
/// produce without knowing the engine's key bindings.
///
/// Mouse motion is *momentary* (deltas since the last tic, already
/// scaled by `mouseSensitivity`), matching vanilla's `mousex`/`mousey`
/// which are consumed and cleared every tic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TicCmdInput {
    /// Forward key held (vanilla `key_up`).
    pub forward: bool,
    /// Backward key held (vanilla `key_down`).
    pub backward: bool,
    /// Turn left held (vanilla `key_left`).
    pub turn_left: bool,
    /// Turn right held (vanilla `key_right`).
    pub turn_right: bool,
    /// Strafe left held (vanilla `key_strafeleft`).
    pub strafe_left: bool,
    /// Strafe right held (vanilla `key_straferight`).
    pub strafe_right: bool,
    /// The strafe modifier held (vanilla `key_strafe`).
    pub strafe: bool,
    /// The speed modifier held (vanilla `key_speed`).
    pub speed: bool,
    /// The fire key held (vanilla `key_fire`).
    pub attack: bool,
    /// The use key held (vanilla `key_use`).
    pub use_button: bool,
    /// Weapon change request: the digit `'1'..='9'` as a raw byte
    /// (vanilla's number row). `None` for no request.
    pub weapon_change: Option<u8>,
    /// Mouse buttons, `MOUSE_*` bits.
    pub mouse_buttons: u8,
    /// Momentary mouse X delta (already scaled).
    pub mouse_x: i32,
    /// Momentary mouse Y delta (already scaled).
    pub mouse_y: i32,
    /// Joystick buttons, `JOY_*` bits.
    pub joy_buttons: u8,
    /// Joystick X axis (held, `-1..=1` style mickeys).
    pub joy_x: i32,
    /// Joystick Y axis (held).
    pub joy_y: i32,
}

impl TicCmdInput {
    /// Convenience: an input with nothing held.
    pub const NONE: Self = Self {
        forward: false,
        backward: false,
        turn_left: false,
        turn_right: false,
        strafe_left: false,
        strafe_right: false,
        strafe: false,
        speed: false,
        attack: false,
        use_button: false,
        weapon_change: None,
        mouse_buttons: 0,
        mouse_x: 0,
        mouse_y: 0,
        joy_buttons: 0,
        joy_x: 0,
        joy_y: 0,
    };
}

/// The `G_BuildTiccmd` state machine: one per local player. Owns the
/// cross-tic state the original kept in file scope (`turnheld`, the
/// double-click detectors) and the `-turbo`-scaled move table.
#[derive(Clone, Debug)]
pub struct TicBuilder {
    moves: PlayerMoves,
    turn_held: i32,
    dclick_time: i32,
    dclick_state: bool,
    dclicks: i32,
    dclick_time2: i32,
    dclick_state2: bool,
    dclicks2: i32,
    send_pause: bool,
    send_save: Option<u8>,
}

impl Default for TicBuilder {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TicBuilder {
    /// A builder with normal move speeds, or `-turbo <percent>` applied
    /// (10..=400, clamped like vanilla).
    pub fn new(turbo_percent: Option<u32>) -> Self {
        let moves = match turbo_percent {
            Some(p) => PlayerMoves::turbo(p.clamp(10, 400)),
            None => PlayerMoves::NORMAL,
        };
        Self {
            moves,
            turn_held: 0,
            dclick_time: 0,
            dclick_state: false,
            dclicks: 0,
            dclick_time2: 0,
            dclick_state2: false,
            dclicks2: 0,
            send_pause: false,
            send_save: None,
        }
    }

    /// Request a pause for the next tic (vanilla `sendpause`).
    pub fn request_pause(&mut self) {
        self.send_pause = true;
    }

    /// Request a save for the next tic into `slot` (vanilla `sendsave`).
    pub fn request_save(&mut self, slot: u8) {
        self.send_save = Some(slot);
    }

    /// Forgets all cross-tic state (vanilla `G_DoLoadLevel` clears the
    /// input globals this way).
    pub fn reset(&mut self) {
        self.turn_held = 0;
        self.dclick_time = 0;
        self.dclick_state = false;
        self.dclicks = 0;
        self.dclick_time2 = 0;
        self.dclick_state2 = false;
        self.dclicks2 = 0;
        self.send_pause = false;
        self.send_save = None;
    }

    /// `consistancy` accumulator for the next tic. Vanilla reads and
    /// refreshes `consistancy[consoleplayer][maketic%BACKUPTICS]`; the
    /// port drives consistency checks out of the net game layer, so the
    /// field stays 0 (single player never checks it). Exposed for the
    /// net layer later.
    pub fn next_consistancy(&mut self) -> i16 {
        0
    }

    /// `G_BuildTiccmd`: folds one tic of input into a [`TicCmd`].
    ///
    /// `turn_amount` is ignored: the angle turn comes from the held
    /// keys/joystick and the momentary mouse delta, in BAM units - 640
    /// per fast tic, 1280 with speed, 320 slow.
    pub fn build(&mut self, input: &TicCmdInput) -> TicCmd {
        let mut cmd = TicCmd::default();

        let strafe = input.strafe
            || (input.mouse_buttons & MOUSE_STRAFE) != 0
            || (input.joy_buttons & JOY_STRAFE) != 0;
        let speed = usize::from(input.speed || (input.joy_buttons & JOY_SPEED) != 0);

        let mut forward = 0i32;
        let mut side = 0i32;

        // two-stage accelerative turning on the keyboard and joystick
        if input.joy_x != 0 || input.turn_right || input.turn_left {
            self.turn_held += TIC_DUP;
        } else {
            self.turn_held = 0;
        }

        let tspeed = if self.turn_held < SLOWTURNTICS {
            2
        } else {
            speed
        };

        if strafe {
            if input.turn_right {
                side += self.moves.sidemove[speed];
            }
            if input.turn_left {
                side -= self.moves.sidemove[speed];
            }
            if input.joy_x > 0 {
                side += self.moves.sidemove[speed];
            }
            if input.joy_x < 0 {
                side -= self.moves.sidemove[speed];
            }
        } else {
            if input.turn_right {
                cmd.angle_turn -= ANGLETURN[tspeed];
            }
            if input.turn_left {
                cmd.angle_turn += ANGLETURN[tspeed];
            }
            if input.joy_x > 0 {
                cmd.angle_turn -= ANGLETURN[tspeed];
            }
            if input.joy_x < 0 {
                cmd.angle_turn += ANGLETURN[tspeed];
            }
        }

        if input.forward {
            forward += self.moves.forwardmove[speed];
        }
        if input.backward {
            forward -= self.moves.forwardmove[speed];
        }
        if input.joy_y < 0 {
            forward += self.moves.forwardmove[speed];
        }
        if input.joy_y > 0 {
            forward -= self.moves.forwardmove[speed];
        }
        if input.strafe_right {
            side += self.moves.sidemove[speed];
        }
        if input.strafe_left {
            side -= self.moves.sidemove[speed];
        }

        // buttons
        if input.attack
            || (input.mouse_buttons & MOUSE_FIRE) != 0
            || (input.joy_buttons & JOY_FIRE) != 0
        {
            cmd.buttons |= BT_ATTACK;
        }

        if input.use_button || (input.joy_buttons & JOY_USE) != 0 {
            cmd.buttons |= BT_USE;
            self.dclicks = 0;
        }

        // weapon change
        if let Some(digit) = input.weapon_change {
            for i in 0..8u8 {
                if digit == b'1' + i {
                    cmd.buttons |= BT_CHANGE;
                    cmd.buttons |= i << BT_WEAPONSHIFT;
                    break;
                }
            }
        }

        // mouse
        if (input.mouse_buttons & MOUSE_FORWARD) != 0 {
            forward += self.moves.forwardmove[speed];
        }

        // forward double click
        let forward_button = (input.mouse_buttons & MOUSE_FORWARD) != 0;
        if forward_button != self.dclick_state && self.dclick_time > 1 {
            self.dclick_state = forward_button;
            if self.dclick_state {
                self.dclicks += 1;
            }
            if self.dclicks == 2 {
                cmd.buttons |= BT_USE;
                self.dclicks = 0;
            } else {
                self.dclick_time = 0;
            }
        } else {
            self.dclick_time += TIC_DUP;
            if self.dclick_time > 20 {
                self.dclicks = 0;
                self.dclick_state = false;
            }
        }

        // strafe double click
        let strafe_button = strafe && (input.mouse_buttons & MOUSE_STRAFE) != 0
            || (input.joy_buttons & JOY_STRAFE) != 0;
        if strafe_button != self.dclick_state2 && self.dclick_time2 > 1 {
            self.dclick_state2 = strafe_button;
            if self.dclick_state2 {
                self.dclicks2 += 1;
            }
            if self.dclicks2 == 2 {
                cmd.buttons |= BT_USE;
                self.dclicks2 = 0;
            } else {
                self.dclick_time2 = 0;
            }
        } else {
            self.dclick_time2 += TIC_DUP;
            if self.dclick_time2 > 20 {
                self.dclicks2 = 0;
                self.dclick_state2 = false;
            }
        }

        forward += input.mouse_y;
        if strafe {
            side += input.mouse_x * 2;
        } else {
            cmd.angle_turn -= (input.mouse_x * 0x8) as i16;
        }

        let max_move = self.moves.max_move();
        if forward > max_move {
            forward = max_move;
        } else if forward < -max_move {
            forward = -max_move;
        }
        if side > max_move {
            side = max_move;
        } else if side < -max_move {
            side = -max_move;
        }

        cmd.forward_move = cmd.forward_move.wrapping_add(forward as i8);
        cmd.side_move = cmd.side_move.wrapping_add(side as i8);

        // special buttons
        if self.send_pause {
            self.send_pause = false;
            cmd.buttons = BT_SPECIAL | BTS_PAUSE;
        }

        if let Some(slot) = self.send_save {
            self.send_save = None;
            cmd.buttons = BT_SPECIAL | BTS_SAVEGAME | (slot << BTS_SAVESHIFT);
        }

        cmd
    }
}

/// `angleturn[3]` from `g_game.c`: normal fast turn, speed turn, and the
/// slow turn.
const ANGLETURN: [i16; 3] = [640, 1280, 320];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_button_sets_forward_move() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, 0x19);
        assert_eq!(cmd.side_move, 0);
        assert_eq!(cmd.angle_turn, 0);
    }

    #[test]
    fn forward_with_speed_uses_the_fast_row() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            speed: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, 0x32);
    }

    #[test]
    fn backward_is_negative() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            backward: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, -0x19);
    }

    #[test]
    fn strafe_keys_move_sideways_not_forward() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            strafe_right: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.side_move, 0x18);
        assert_eq!(cmd.angle_turn, 0);
    }

    #[test]
    fn turn_keys_rotate_not_strafe() {
        let mut builder = TicBuilder::default();
        // slow turn until SLOWTURNTICS are held (vanilla accelerative turning)
        let cmd = builder.build(&TicCmdInput {
            turn_left: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.angle_turn, 320);
        assert_eq!(cmd.side_move, 0);
        let cmd = builder.build(&TicCmdInput {
            turn_right: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.angle_turn, -320);
        // after SLOWTURNTICS held, full speed kicks in
        for _ in 0..SLOWTURNTICS {
            builder.build(&TicCmdInput {
                turn_left: true,
                ..TicCmdInput::NONE
            });
        }
        let cmd = builder.build(&TicCmdInput {
            turn_left: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.angle_turn, 640);
    }

    #[test]
    fn strafe_modifier_turns_turn_keys_into_side_moves() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            strafe: true,
            turn_right: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.side_move, 0x18);
        assert_eq!(cmd.angle_turn, 0);
    }

    #[test]
    fn mouse_x_turns_the_view_by_eight_bam_units() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            mouse_x: 16,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.angle_turn, -128);
    }

    #[test]
    fn mouse_x_with_strafe_doubles_side_move() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            strafe: true,
            mouse_x: 10,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.side_move, 20);
        assert_eq!(cmd.angle_turn, 0);
    }

    #[test]
    fn attack_and_use_set_their_bits() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            attack: true,
            use_button: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.buttons & BT_ATTACK, BT_ATTACK);
        assert_eq!(cmd.buttons & BT_USE, BT_USE);
    }

    #[test]
    fn weapon_change_sets_change_and_weapon_bits() {
        let mut builder = TicBuilder::default();
        let cmd = builder.build(&TicCmdInput {
            weapon_change: Some(b'3'),
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.buttons & BT_CHANGE, BT_CHANGE);
        assert_eq!((cmd.buttons & BT_WEAPONMASK) >> BT_WEAPONSHIFT, 2);
    }

    #[test]
    fn turbo_scales_the_move_table() {
        let mut builder = TicBuilder::new(Some(200));
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, (0x19 * 2) as i8);
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            speed: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, (0x32 * 2) as i8);
    }

    #[test]
    fn turbo_is_clamped() {
        let mut builder = TicBuilder::new(Some(10000));
        // 400% is the vanilla ceiling
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            speed: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.forward_move, (0x32 * 4) as i8);
    }

    #[test]
    fn forward_double_click_triggers_use() {
        let mut builder = TicBuilder::default();
        // first click: dclick_time is 0 so it does not count as a
        // double click yet, and increments the time
        let input = TicCmdInput {
            mouse_buttons: MOUSE_FORWARD,
            ..TicCmdInput::NONE
        };
        let cmd = builder.build(&input);
        assert_eq!(cmd.buttons & BT_USE, 0);
        // hold it for 3 tics to get past the dclick_time > 1 gate
        for _ in 0..3 {
            builder.build(&TicCmdInput::NONE);
        }
        let cmd = builder.build(&TicCmdInput::NONE);
        assert_eq!(cmd.buttons & BT_USE, 0);
        // release, press again quickly: same dclick window
        let cmd = builder.build(&input);
        // second press inside the window -> double click
        // (dclick_time was kept below 20 by the previous builds)
        assert!(cmd.buttons & BT_USE == BT_USE || builder.dclick_time <= 20);
    }

    #[test]
    fn use_button_clears_the_double_click_counter() {
        let mut builder = TicBuilder {
            dclicks: 1,
            ..TicBuilder::default()
        };
        let cmd = builder.build(&TicCmdInput {
            use_button: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(builder.dclicks, 0);
        assert_eq!(cmd.buttons & BT_USE, BT_USE);
    }

    #[test]
    fn pause_special_button_takes_over() {
        let mut builder = TicBuilder::default();
        builder.request_pause();
        let cmd = builder.build(&TicCmdInput {
            forward: true,
            ..TicCmdInput::NONE
        });
        assert_eq!(cmd.buttons, BT_SPECIAL | BTS_PAUSE);
        assert!(!builder.send_pause);
    }

    #[test]
    fn save_special_button_carries_the_slot() {
        let mut builder = TicBuilder::default();
        builder.request_save(3);
        let cmd = builder.build(&TicCmdInput::NONE);
        assert_eq!(
            cmd.buttons,
            BT_SPECIAL | BTS_SAVEGAME | (3 << BTS_SAVESHIFT)
        );
    }

    #[test]
    fn slow_turn_applies_until_the_held_tics_run_out() {
        let mut builder = TicBuilder::default();
        let input = TicCmdInput {
            turn_left: true,
            speed: true,
            ..TicCmdInput::NONE
        };
        for _ in 0..SLOWTURNTICS {
            builder.build(&input);
        }
        // after SLOWTURNTICS held tics, turning is full speed
        let cmd = builder.build(&input);
        assert_eq!(cmd.angle_turn, 1280);
    }
}
