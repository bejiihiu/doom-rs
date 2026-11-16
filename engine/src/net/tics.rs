//! The tic governor: `TryRunTics`, the console-player pace adaptation and
//! the consistency check — the timing half of `d_net.c` plus the net-relevant
//! part of `G_Ticker`.
//!
//! `TryRunTics` decides how many game tics the current frame is allowed to
//! run, busy-waits for the slowest node to have the tics (bounded by a
//! 20-tic bailout to the menu), then runs them. `decide_counts` is the pure
//! decision kernel, extracted so it can be tested exhaustively;
//! [`try_run_tics`] is the full port parametrized on a [`TimeSource`].
//!
//! The consistency check (`consistancy[MAXPLAYERS][BACKUPTICS]`) lives in
//! vanilla `G_Ticker`; it is net-specific, so it lives here as a standalone
//! helper the game shell calls with `mo->x` (or `rndindex`).

use crate::player::{MAXPLAYERS, TicCmd};

use super::engine::{NetEngine, NetError, NetGameHooks};
use super::packet::BACKUPTICS;
use super::time::TimeSource;

/// `BT_SPECIAL` from `d_ticcmd.h` semantics (see `game::input`, where the
/// same value lives privately): high bit of `buttons` marks a game event
/// (pause/save), not a real button. Duplicated tics strip it.
pub const BT_SPECIAL: u8 = 128;

/// What happened when [`try_run_tics`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRunTicsOutcome {
    /// All requested tics ran.
    Completed {
        /// `counts * ticdup` tics.
        tics: i32,
    },
    /// The 20-tic wait bailout fired; `M_Ticker` ran instead so the menus
    /// stay usable while we are waiting for a remote node.
    BailoutToMenu,
}

/// The `counts` decision at the heart of `TryRunTics`.
///
/// ```text
/// if realtics < availabletics-1: counts = realtics+1   (we are behind, catch up)
/// else if realtics < availabletics: counts = realtics  (steady state)
/// else:                           counts = availabletics (starved on tics)
/// ```
/// with a floor of 1. Pure; unit-tested exhaustively.
pub fn decide_counts(realtics: i32, availabletics: i32) -> i32 {
    let counts = if realtics < availabletics.saturating_sub(1) {
        realtics + 1
    } else if realtics < availabletics {
        realtics
    } else {
        availabletics
    };
    counts.max(1)
}

/// The `frameskip`/`skiptics` pace adaptation (`if (!demoplayback)` block
/// of `TryRunTics`).
///
/// If the local console is ahead of the first in-game remote player, slow
/// time down by decrementing `gametime` (so the next `NetUpdate` builds
/// more tics). Four consecutive frames where the local node gained on the
/// remote set `skiptics = 1` ("thin" netcode: drop one tic of local input
/// to let the remote catch up).
fn adapt_pace(engine: &mut NetEngine, hooks: &dyn NetGameHooks) {
    for i in 0..MAXPLAYERS {
        if !hooks.playeringame(i) {
            continue;
        }
        // the key player does not adapt
        if engine.console_player() != i {
            let remote = engine.nodes()[engine.node_for_player()[i] as usize].net_tics;
            if engine.nodes()[0].net_tics <= remote {
                engine.game_time -= 1;
            }
            let slot = (engine.frame_on & 3) as usize;
            engine.frame_skip[slot] = engine.old_net_tics > remote;
            engine.old_net_tics = engine.nodes()[0].net_tics;
            if engine.frame_skip.iter().all(|&skip| skip) {
                engine.skip_tics = 1;
            }
        }
        return;
    }
}

/// `TryRunTics`: the tic governor, ported line by line from `d_net.c`.
///
/// `time` replaces `I_GetTime()`; everything else goes through the hooks.
/// `gametic` is owned by the hooks (the game shell) and incremented here.
///
/// # Errors
///
/// Returns [`NetError::LowticBelowGametic`] / [`NetError::GameticAboveLowtic`]
/// exactly where vanilla calls `I_Error`, and propagates transport errors.
pub fn try_run_tics(
    engine: &mut NetEngine,
    time: &dyn TimeSource,
    hooks: &mut dyn NetGameHooks,
) -> Result<TryRunTicsOutcome, NetError> {
    let ticdup = engine.ticdup();

    // get real tics
    let entertic = time.now_tics() / ticdup;
    let realtics = entertic - engine.old_enter_tics;
    engine.old_enter_tics = entertic;

    // get available tics
    engine.net_update(time.now_tics(), hooks)?;

    let availabletics = engine.lowtic() - hooks.gametic() / ticdup;

    // decide how many tics to run
    let counts = decide_counts(realtics, availabletics);

    engine.frame_on += 1;

    if !hooks.demo_playback() {
        adapt_pace(engine, hooks);
    }

    // wait for new tics if needed
    let mut lowtic = engine.lowtic();
    while lowtic < hooks.gametic() / ticdup + counts {
        engine.net_update(time.now_tics(), hooks)?;
        lowtic = engine.lowtic();

        if lowtic < hooks.gametic() / ticdup {
            return Err(NetError::LowticBelowGametic);
        }

        // don't stay in here forever -- give the menu a chance to work
        if time.now_tics() / ticdup - entertic >= 20 {
            hooks.menu_ticker();
            return Ok(TryRunTicsOutcome::BailoutToMenu);
        }
    }

    // run the count * ticdup tics
    for _ in 0..counts {
        for i in 0..ticdup {
            if hooks.gametic() / ticdup > lowtic {
                return Err(NetError::GameticAboveLowtic);
            }
            if hooks.advance_demo() {
                hooks.try_advance_demo();
            }
            hooks.menu_ticker();
            hooks.run_game_tic();
            hooks.set_gametic(hooks.gametic() + 1);

            // modify command for duplicated tics
            if i != ticdup - 1 {
                let buf = ((hooks.gametic() / ticdup) % BACKUPTICS) as usize;
                for j in 0..MAXPLAYERS {
                    let cmd = &mut engine.netcmds_mut(j, buf);
                    cmd.chat_char = 0;
                    if cmd.buttons & BT_SPECIAL != 0 {
                        cmd.buttons = 0;
                    }
                }
            }
        }
        // check for new console commands
        engine.net_update(time.now_tics(), hooks)?;
    }

    Ok(TryRunTicsOutcome::Completed {
        tics: counts * ticdup,
    })
}

/// Vanilla `consistancy[MAXPLAYERS][BACKUPTICS]` with its `G_Ticker` check.
///
/// The check runs only on non-duplicated tics of a net game with no demo
/// playback. `source_x` is `players[i].mo->x`, or `rndindex` when the
/// player's avatar is gone — passed by the caller since the net layer does
/// not own the world.
#[derive(Clone, Debug)]
pub struct ConsistencyCheck {
    consistancy: [[i16; BACKUPTICS as usize]; MAXPLAYERS],
}

/// The gate condition for a consistency check; mirrors the
/// `netgame && !netdemo && !(gametic%ticdup)` guard in `G_Ticker`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsistencyGate {
    /// `netgame`: a net game is running.
    pub netgame: bool,
    /// `netdemo`: a net demo is being recorded/played (no check then).
    pub netdemo: bool,
    /// `ticdup`: duplicate-tic factor; only every `ticdup`-th tic is checked.
    pub ticdup: i32,
}

impl ConsistencyGate {
    /// A plain net-game gate with tic duplication 1.
    pub fn netgame() -> Self {
        Self {
            netgame: true,
            netdemo: false,
            ticdup: 1,
        }
    }
}

impl ConsistencyCheck {
    /// A zeroed check, like vanilla's bss.
    pub fn new() -> Self {
        Self {
            consistancy: [[0; BACKUPTICS as usize]; MAXPLAYERS],
        }
    }

    /// Verifies `cmd.consistancy` against the prediction and stores the new
    /// one. Vanilla only checks when `gametic > BACKUPTICS`; earlier tics
    /// just fill the buffer.
    ///
    /// # Errors
    ///
    /// [`NetError::ConsistencyFailure`] on mismatch (vanilla `I_Error`).
    pub fn check_and_store(
        &mut self,
        cmd: &TicCmd,
        gate: ConsistencyGate,
        gametic: i32,
        player: usize,
        source_x: i32,
    ) -> Result<(), NetError> {
        if !gate.netgame || gate.netdemo {
            return Ok(());
        }
        if gametic % gate.ticdup != 0 {
            return Ok(());
        }
        let buf = ((gametic / gate.ticdup) % BACKUPTICS) as usize;
        if gametic > BACKUPTICS && self.consistancy[player][buf] != cmd.consistancy {
            return Err(NetError::ConsistencyFailure {
                player,
                expected: self.consistancy[player][buf],
                actual: cmd.consistancy,
            });
        }
        self.consistancy[player][buf] = source_x as i16;
        Ok(())
    }

    /// The predicted value for a slot (test/diagnostic use).
    pub fn predicted(&self, player: usize, buf: usize) -> i16 {
        self.consistancy[player][buf]
    }
}

impl Default for ConsistencyCheck {
    fn default() -> Self {
        Self::new()
    }
}
