//! The `NetGameHooks` seam for the net layer: everything `d_net.c` reached
//! through `doomstat` globals is routed here instead, so the net engine can
//! run the tic governor without knowing the world.
//!
//! Notes on the port:
//!
//! - `pump_events` is a no-op: OS events arrive synchronously through
//!   [`super::Game::handle_event`] before the frame loop reaches the
//!   governor, there is no queue for the engine to drain.
//! - `build_ticcmd` samples the platform input itself: keyboard/mouse held
//!   state is stable for the whole frame, mouse deltas are consumed by the
//!   first built tic exactly like vanilla's `mousex = mousey = 0` reset.
//!   Multiple tics built in one frame see the same held state but no stray
//!   mouse motion, which is the vanilla behavior.
//! - `cmd->consistancy` is read from the ported consistency buffer
//!   (`consistancy[consoleplayer][maketic % BACKUPTICS]` in vanilla); the
//!   write happens in the tic body through
//!   `ConsistencyCheck::check_and_store`, exactly like vanilla's `G_Ticker`.
//! - `run_game_tic` cannot return an error, so a failed tic latches
//!   `net_fatal` and aborts the frame instead of unwinding through the
//!   governor; the platform sees it after `try_run_tics` returns.
//! - Demo screens never advance here: the attract mode is a static page in
//!   this port, so `advance_demo` is always false and
//!   [`NetGameHooks::try_advance_demo`] does nothing.

use crate::net::BACKUPTICS;
use crate::player::{MAXPLAYERS, TicCmd};

use super::game::Game;

impl crate::net::NetGameHooks for Game {
    fn pump_events(&mut self) {}

    fn build_ticcmd(&mut self) -> crate::player::TicCmd {
        let input = self.net_platform_input;
        let merged = self.sample_local_input(&input);
        let mut cmd = self.builder.build(&merged);
        // vanilla: cmd->consistancy = consistancy[consoleplayer][maketic%BACKUPTICS]
        cmd.consistancy = self
            .consistency
            .predicted(self.consoleplayer, (self.net_maketic % BACKUPTICS) as usize);
        self.net_maketic += 1;
        cmd
    }

    fn run_game_tic(&mut self, cmds: [TicCmd; MAXPLAYERS]) {
        if self.net_fatal.is_some() {
            return;
        }
        if let Err(err) = self.run_tic_net(cmds) {
            self.net_fatal = Some(err);
        }
    }

    fn menu_ticker(&mut self) {
        self.menu.tick(self.gametic as u64);
    }

    fn try_advance_demo(&mut self) {}

    fn demo_playback(&self) -> bool {
        self.demoplayback
    }

    fn demo_recording(&self) -> bool {
        self.demorecording
    }

    fn advance_demo(&self) -> bool {
        false
    }

    fn singletics(&self) -> bool {
        false
    }

    fn user_game(&self) -> bool {
        self.usergame
    }

    fn playeringame(&self, player: usize) -> bool {
        self.playeringame.get(player).copied().unwrap_or(false)
    }

    fn set_playeringame(&mut self, player: usize, in_game: bool) {
        if let Some(slot) = self.playeringame.get_mut(player) {
            *slot = in_game;
        }
    }

    fn on_player_left(&mut self, player: usize) {
        if let Some(slot) = self.playeringame.get_mut(player) {
            *slot = false;
        }
        tracing::info!("net: player {} left the game", player + 1);
        self.players[self.consoleplayer].message =
            Some(format!("Player {} left the game", player + 1));
    }

    fn on_demo_status_needed(&mut self) {
        if self.demorecording {
            self.end_demo_recording();
        }
    }

    fn wait_vbl(&mut self) {
        // no VBL to wait on outside a frame loop; a yield keeps the quit
        // burst from melting the CPU
        std::thread::yield_now();
    }

    fn check_abort_requested(&mut self) -> bool {
        self.net_abort_requested
    }

    fn gametic(&self) -> i32 {
        self.gametic
    }

    fn set_gametic(&mut self, value: i32) {
        self.gametic = value;
    }
}
