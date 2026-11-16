//! The winit application: a 320x200 DOOM view in a real window.
//!
//! Structure mirrors the softbuffer README: the window is created on
//! `resumed`, the surface renders in `RedrawRequested`, and frames are
//! paced with `ControlFlow` from `about_to_wait` so the loop never spins
//! at 100% CPU between the 60 Hz presentation ticks.
//!
//! Presentation path per frame: engine `advance` (zero to four tics plus
//! the `d_main.c`-style frame render) -> palette remap with the gamma LUT
//! folded in -> nearest-neighbor upscale into the window, centered with
//! black letterbox bars -> `softbuffer` present.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, Gilrs};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use doom_engine::game::{Game, InputEvent, KeyBindings, TicCmdInput, keys};
use doom_engine::gfx::palette::Palette;
use doom_engine::render::{IndexedFramebuffer, SCREEN_HEIGHT, SCREEN_PIXELS, SCREEN_WIDTH};

use softbuffer::{Context, Surface};

use crate::audio::{AudioOutput, pump_music};
use crate::input;

/// Display pacing for presentation. The engine's 35 Hz tics are decoupled
/// from this; vanilla decoupled them too.
const FRAME_DELTA: Duration = Duration::from_millis(1000 / 60);
/// One wheel notch, in winit line deltas. Touchpad pixel deltas are
/// normalized against this.
const PIXELS_PER_WHEEL_LINE: f32 = 40.0;

/// Setup handed to [`App::new`]: everything the run is built from,
/// grouped so the constructor does not drown in arguments.
pub struct AppInit {
    /// The running game.
    pub game: Game,
    /// The gamma-naive palette; the gamma LUT is applied at present time.
    pub palette: Palette,
    /// Gamma remap table (by `usegamma` from the config).
    pub gamma_lut: [u8; 256],
    /// Borderless fullscreen instead of a window.
    pub fullscreen: bool,
    /// Initial window width (windowed mode).
    pub window_width: u32,
    /// Initial window height (windowed mode).
    pub window_height: u32,
    /// Key bindings from the config.
    pub bindings: KeyBindings,
    /// Output stream, when the device allows it.
    pub audio: Option<AudioOutput>,
    /// Gamepad backend, when it initializes.
    pub gilrs: Option<Gilrs>,
    /// Softbuffer drawing context.
    pub context: Context<winit::event_loop::OwnedDisplayHandle>,
}

/// Everything the app owns for one run.
pub struct App {
    // -- setup, fixed for the run ---------------------------------------
    game: Game,
    fb: IndexedFramebuffer,
    palette: Palette,
    gamma_lut: [u8; 256],
    rgb: Box<[u32; SCREEN_PIXELS]>,
    bindings: KeyBindings,
    fullscreen: bool,
    window_width: u32,
    window_height: u32,
    audio: Option<AudioOutput>,
    gilrs: Option<Gilrs>,
    // -- windowing --------------------------------------------------------
    context: Context<winit::event_loop::OwnedDisplayHandle>,
    window: Option<Rc<Window>>,
    surface: Option<Surface<winit::event_loop::OwnedDisplayHandle, Rc<Window>>>,
    surface_size: Option<(u32, u32)>,
    scale_lut: Option<ScaleLut>,
    // -- frame pacing -----------------------------------------------------
    last_frame: Instant,
    next_frame_at: Instant,
    frames: u64,
    // -- input state ------------------------------------------------------
    mouse_buttons: u8,
    rmb_held: bool,
    focused: bool,
    pending_weapon: Option<u8>,
    wheel_acc: f32,
    wheel_weapon: u8,
    pad_start_prev: bool,
    pad_select_prev: bool,
    pad_north_prev: bool,
    /// Doom codes currently held down; released all at once on focus loss
    /// so alt-tabbing with a key held does not leave the engine stuck
    /// running forward forever.
    held_codes: Vec<u8>,
    // -- audio feeding ----------------------------------------------------
    music_credit: f64,
    music_buf: Vec<f32>,
    // -- failure handling ---------------------------------------------------
    consecutive_errors: u32,
    /// Set when the game loop fails deterministically; the run ends
    /// with a non-zero exit code instead of an infinite error loop.
    pub fatal_error: bool,
}

/// Nearest-neighbor source indices for a given destination size. Built
/// once per resize instead of paying an f64 divide per destination pixel.
struct ScaleLut {
    bar_x: usize,
    bar_y: usize,
    x: Vec<usize>,
    y: Vec<usize>,
}

impl ScaleLut {
    fn build(width: usize, height: usize) -> Self {
        let scale = (width as f64 / SCREEN_WIDTH as f64).min(height as f64 / SCREEN_HEIGHT as f64);
        let dest_w = ((SCREEN_WIDTH as f64 * scale).round() as usize).max(1);
        let dest_h = ((SCREEN_HEIGHT as f64 * scale).round() as usize).max(1);
        let bar_x = (width - dest_w) / 2;
        let bar_y = (height - dest_h) / 2;
        let x = (0..dest_w).map(|dx| dx * SCREEN_WIDTH / dest_w).collect();
        let y = (0..dest_h).map(|dy| dy * SCREEN_HEIGHT / dest_h).collect();
        Self { bar_x, bar_y, x, y }
    }
}

impl App {
    /// Tell the surviving net-game nodes we are leaving (vanilla's
    /// `D_QuitNetGame`) before the process exits. A no-op outside a
    /// network game or before it started.
    pub fn net_shutdown(&mut self) {
        self.game.net_quit();
    }

    pub fn new(init: AppInit) -> Self {
        let AppInit {
            game,
            palette,
            gamma_lut,
            fullscreen,
            window_width,
            window_height,
            bindings,
            audio,
            gilrs,
            context,
        } = init;
        let now = Instant::now();
        Self {
            game,
            fb: IndexedFramebuffer::new(),
            palette,
            gamma_lut,
            rgb: Box::new([0u32; SCREEN_PIXELS]),
            bindings,
            fullscreen,
            window_width,
            window_height,
            audio,
            gilrs,
            context,
            window: None,
            surface: None,
            surface_size: None,
            scale_lut: None,
            last_frame: now,
            next_frame_at: now,
            frames: 0,
            mouse_buttons: 0,
            rmb_held: false,
            focused: true,
            pending_weapon: None,
            wheel_acc: 0.0,
            wheel_weapon: 1,
            pad_start_prev: false,
            pad_select_prev: false,
            pad_north_prev: false,
            held_codes: Vec::with_capacity(16),
            music_credit: 0.0,
            music_buf: Vec::with_capacity(4096),
            consecutive_errors: 0,
            fatal_error: false,
        }
    }

    fn cycle_weapon(&mut self, direction: i8) {
        self.wheel_weapon = if direction >= 0 {
            (self.wheel_weapon % 9) + 1
        } else {
            match self.wheel_weapon {
                1 => 9,
                n => n - 1,
            }
        };
        self.pending_weapon = Some(b'0' + self.wheel_weapon);
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let mut attributes = Window::default_attributes()
            .with_title("doom-rs")
            .with_inner_size(LogicalSize::new(self.window_width, self.window_height))
            .with_min_inner_size(LogicalSize::new(SCREEN_WIDTH as f64, SCREEN_HEIGHT as f64))
            .with_resizable(true);
        if self.fullscreen {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => window,
            Err(err) => {
                tracing::error!("could not create the window: {err}");
                self.fatal_error = true;
                event_loop.exit();
                return;
            }
        };
        let window = Rc::new(window);
        let mut surface = match Surface::new(&self.context, Rc::clone(&window)) {
            Ok(surface) => surface,
            Err(err) => {
                tracing::error!("could not create the drawing surface: {err}");
                self.fatal_error = true;
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        if let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            if let Err(err) = surface.resize(width, height) {
                tracing::warn!("initial surface resize failed: {err}");
            }
            self.surface_size = Some((size.width, size.height));
        } else {
            self.surface_size = Some((self.window_width, self.window_height));
        }
        self.surface = Some(surface);
        self.apply_focus(true);
        self.window = Some(window);
    }

    fn apply_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            // stop everything we told the engine about: key releases that
            // happened in another window are lost to us
            let released = std::mem::take(&mut self.held_codes);
            for doom_code in released {
                self.game.handle_event(&InputEvent::KeyUp(doom_code));
            }
            self.mouse_buttons = 0;
            self.rmb_held = false;
            self.wheel_acc = 0.0;
            self.game.handle_event(&InputEvent::MouseButtons(0));
            self.game.handle_event(&InputEvent::JoystickButtons(0));
            self.game
                .handle_event(&InputEvent::JoystickAxis { x: 0, y: 0 });
        }
        if let Some(window) = &self.window {
            window.set_cursor_visible(!focused);
            let grab = if focused {
                CursorGrabMode::Confined
            } else {
                CursorGrabMode::None
            };
            if let Err(err) = window.set_cursor_grab(grab) {
                tracing::debug!("cursor grab ({grab:?}) failed: {err}");
            }
        }
    }

    fn on_resize(&mut self, width: u32, height: u32) {
        self.scale_lut = None;
        let nonzero = match (NonZeroU32::new(width), NonZeroU32::new(height)) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        };
        self.surface_size = nonzero.map(|(w, h)| (w.get(), h.get()));
        if let (Some((w, h)), Some(surface)) = (self.surface_size, self.surface.as_mut()) {
            if let Err(err) = surface.resize(
                NonZeroU32::new(w).expect("checked above"),
                NonZeroU32::new(h).expect("checked above"),
            ) {
                tracing::warn!("surface resize to {w}x{h} failed: {err}");
            }
        }
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        if event.repeat {
            return;
        }
        let pressed = event.state == ElementState::Pressed;
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        if pressed {
            if let Some(digit) = input::weapon_digit(code) {
                self.pending_weapon = Some(digit);
            }
        }
        for doom_code in input::doom_codes_for_key(code, &self.bindings) {
            if pressed {
                if !self.held_codes.contains(&doom_code) {
                    self.held_codes.push(doom_code);
                }
            } else {
                self.held_codes.retain(|&held| held != doom_code);
            }
            let event = if pressed {
                InputEvent::KeyDown(doom_code)
            } else {
                InputEvent::KeyUp(doom_code)
            };
            self.game.handle_event(&event);
        }
    }

    fn on_mouse_button(&mut self, state: ElementState, button: MouseButton) {
        let pressed = state == ElementState::Pressed;
        match button {
            MouseButton::Right => {
                self.rmb_held = pressed;
            }
            _ => {
                if let Some(bit) = input::mouse_button_bit(button) {
                    if pressed {
                        self.mouse_buttons |= bit;
                    } else {
                        self.mouse_buttons &= !bit;
                    }
                    self.game
                        .handle_event(&InputEvent::MouseButtons(self.mouse_buttons));
                }
            }
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(position) => position.y as f32 / PIXELS_PER_WHEEL_LINE,
        };
        self.wheel_acc += lines;
        while self.wheel_acc >= 1.0 {
            self.cycle_weapon(1);
            self.wheel_acc -= 1.0;
        }
        while self.wheel_acc <= -1.0 {
            self.cycle_weapon(-1);
            self.wheel_acc += 1.0;
        }
    }

    fn on_mouse_motion(&mut self, delta_x: f64, delta_y: f64) {
        self.game.handle_event(&InputEvent::MouseMove {
            x: delta_x.round() as i32,
            y: delta_y.round() as i32,
        });
    }

    /// Reads the first connected gamepad (if any) and mirrors it into
    /// engine input events. Called once per frame; deadzoned sticks feed
    /// the same joystick path vanilla used.
    fn poll_gamepad(&mut self) {
        let Some(gilrs) = &mut self.gilrs else {
            return;
        };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::Connected => {
                    tracing::info!("gamepad connected: {}", gilrs.gamepad(event.id).name());
                }
                gilrs::EventType::Disconnected => {
                    tracing::info!("gamepad disconnected");
                }
                _ => {}
            }
        }

        const DEADZONE: f32 = 0.25;
        let signum = |value: f32| if value >= 0.0 { 1 } else { -1 };

        let mut axis_x = 0i32;
        let mut axis_y = 0i32;
        let mut buttons = 0u8;
        let mut start = false;
        let mut select = false;
        let mut north = false;

        if let Some((_id, pad)) = gilrs.gamepads().next() {
            let left_x = pad.value(Axis::LeftStickX);
            let left_y = pad.value(Axis::LeftStickY);
            let right_x = pad.value(Axis::RightStickX);

            // the right stick always wins the shared turn/strafe axis
            if right_x.abs() >= DEADZONE {
                axis_x = signum(right_x);
            } else if left_x.abs() >= DEADZONE {
                axis_x = signum(left_x);
                buttons |= input::buttons::JOY_STRAFE;
            }
            // gilrs reports stick up as positive; the engine wants negative
            if left_y.abs() >= DEADZONE {
                axis_y = -signum(left_y);
            }
            if pad.is_pressed(Button::DPadUp) {
                axis_y = -1;
            }
            if pad.is_pressed(Button::DPadDown) {
                axis_y = 1;
            }
            if pad.is_pressed(Button::DPadLeft) {
                axis_x = -1;
                buttons &= !input::buttons::JOY_STRAFE;
            }
            if pad.is_pressed(Button::DPadRight) {
                axis_x = 1;
                buttons &= !input::buttons::JOY_STRAFE;
            }

            if pad.is_pressed(Button::RightTrigger)
                || pad.is_pressed(Button::RightTrigger2)
                || pad.value(Axis::RightZ) > 0.5
            {
                buttons |= input::buttons::JOY_FIRE;
            }
            if pad.is_pressed(Button::LeftTrigger)
                || pad.is_pressed(Button::LeftTrigger2)
                || pad.value(Axis::LeftZ) > 0.5
            {
                buttons |= input::buttons::JOY_USE;
            }
            if pad.is_pressed(Button::South) {
                buttons |= input::buttons::JOY_USE;
            }
            if pad.is_pressed(Button::East) {
                buttons |= input::buttons::JOY_FIRE;
            }
            if pad.is_pressed(Button::West) {
                buttons |= input::buttons::JOY_SPEED;
            }
            start = pad.is_pressed(Button::Start);
            select = pad.is_pressed(Button::Select);
            north = pad.is_pressed(Button::North);
        }

        self.game.handle_event(&InputEvent::JoystickAxis {
            x: axis_x,
            y: axis_y,
        });
        self.game
            .handle_event(&InputEvent::JoystickButtons(buttons));

        // Start opens/closes the menu, Select toggles the automap; both are
        // edge-triggered so holding the button does not strobe the menu.
        if start && !self.pad_start_prev {
            self.game.handle_event(&InputEvent::KeyDown(keys::ESCAPE));
            self.game.handle_event(&InputEvent::KeyUp(keys::ESCAPE));
        }
        if select && !self.pad_select_prev {
            self.game.handle_event(&InputEvent::KeyDown(keys::TAB));
            self.game.handle_event(&InputEvent::KeyUp(keys::TAB));
        }
        if north && !self.pad_north_prev {
            self.cycle_weapon(1);
        }
        self.pad_start_prev = start;
        self.pad_select_prev = select;
        self.pad_north_prev = north;
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        // 35 tics per second are the engine's business; this is how much
        // wall time we hand to `advance` for pacing, and to the music pump
        let elapsed_ms = now
            .saturating_duration_since(self.last_frame)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        self.last_frame = now;

        let tic_input = TicCmdInput {
            use_button: self.rmb_held,
            weapon_change: self.pending_weapon.take(),
            ..TicCmdInput::default()
        };
        match self.game.advance(&tic_input, elapsed_ms, &mut self.fb) {
            Ok(()) => self.consecutive_errors = 0,
            Err(err) => {
                self.consecutive_errors += 1;
                let failures = self.consecutive_errors;
                tracing::error!("game tick failed ({failures} in a row): {err}");
                // a net game is deterministic: a sync error will never cure
                // itself by retrying - bail out immediately
                if self.game.is_net_game() {
                    eprintln!("doom-rs: error: the network game failed; last error: {err}");
                    self.fatal_error = true;
                } else if self.consecutive_errors >= 10 {
                    eprintln!(
                        "doom-rs: error: the game loop failed 10 times in a row; last error: {err}"
                    );
                    self.fatal_error = true;
                }
            }
        }

        if let Some(audio) = &self.audio {
            pump_music(
                &mut self.game,
                audio,
                elapsed_ms,
                &mut self.music_credit,
                &mut self.music_buf,
            );
        }

        self.present();
        self.frames += 1;
        if self.frames % 600 == 0 {
            tracing::debug!("frame {}", self.frames);
        }
    }

    /// Palette remap, with the gamma LUT folded into the channels. The
    /// engine's `apply_palette` does the same job without the LUT.
    fn apply_palette_with_gamma(&mut self) {
        let lut = &self.gamma_lut;
        let fb = self.fb.as_slice();
        for (out, &index) in self.rgb.iter_mut().zip(fb.iter()) {
            let [r, g, b] = self.palette.color(index);
            *out = ((u32::from(lut[usize::from(r)])) << 16)
                | ((u32::from(lut[usize::from(g)])) << 8)
                | u32::from(lut[usize::from(b)]);
        }
    }

    fn present(&mut self) {
        let Some((width, height)) = self.surface_size else {
            return;
        };
        self.apply_palette_with_gamma();
        let lut = self
            .scale_lut
            .get_or_insert_with(|| ScaleLut::build(width as usize, height as usize));
        let Some(surface) = &mut self.surface else {
            return;
        };
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(err) => {
                tracing::warn!("could not lock the framebuffer: {err}");
                return;
            }
        };
        buffer.fill(0);
        let rgb = &self.rgb[..];
        for (dy, &src_y) in lut.y.iter().enumerate() {
            let dst_start = (lut.bar_y + dy) * width as usize + lut.bar_x;
            let src_row = src_y * SCREEN_WIDTH;
            for (dx, &src_x) in lut.x.iter().enumerate() {
                buffer[dst_start + dx] = rgb[src_row + src_x];
            }
        }
        if let Err(err) = buffer.present() {
            tracing::warn!("present failed: {err}");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.create_window(event_loop);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(window) = &self.window {
            if window.id() != id {
                return;
            }
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.on_resize(size.width, size.height);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::Focused(focused) => self.apply_focus(focused),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        // raw device deltas are what vanilla called mousex/mousey: cursor
        // position is meaningless while the cursor is confined to the window
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.focused {
                self.on_mouse_motion(delta.0, delta.1);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.fatal_error || self.game.should_quit() {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        self.poll_gamepad();
        if now >= self.next_frame_at {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            self.next_frame_at = now + FRAME_DELTA;
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_at));
    }
}

/// Runs the event loop to completion.
///
/// On success returns `fatal_error` from the app, so `main` can pick the
/// exit code: closing the window is a clean quit, failing the game loop
/// is not.
pub fn run(mut app: App, event_loop: EventLoop<()>) -> Result<bool, String> {
    // wake immediately: the first redraw is requested from `about_to_wait`
    event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now()));
    event_loop
        .run_app(&mut app)
        .map_err(|err| format!("the event loop failed: {err}"))?;
    app.net_shutdown();
    Ok(app.fatal_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_4x_fills_the_whole_window() {
        let lut = ScaleLut::build(1280, 800);
        assert_eq!((lut.bar_x, lut.bar_y), (0, 0));
        assert_eq!(lut.x.len(), 1280);
        assert_eq!(lut.y.len(), 800);
        assert_eq!(lut.x[1279], 319);
        assert_eq!(lut.y[799], 199);
    }

    #[test]
    fn odd_window_gets_letterboxed_and_stays_integer() {
        // 1000x600 fits 3"320 = 960 wide, so 20 px of bar on each side
        let lut = ScaleLut::build(1000, 600);
        assert_eq!((lut.bar_x, lut.bar_y), (20, 0));
        assert_eq!(lut.x.len(), 960);
        assert_eq!(lut.y.len(), 600);
        assert_eq!(*lut.x.iter().max().unwrap(), 319);
        assert_eq!(*lut.y.iter().max().unwrap(), 199);
    }

    #[test]
    fn source_indices_are_monotonic_and_in_range() {
        let lut = ScaleLut::build(641, 381);
        assert!(lut.x.windows(2).all(|w| w[0] <= w[1]));
        assert!(lut.y.windows(2).all(|w| w[0] <= w[1]));
        assert!(lut.x.iter().all(|&i| i < SCREEN_WIDTH));
        assert!(lut.y.iter().all(|&i| i < SCREEN_HEIGHT));
    }

    #[test]
    fn native_size_is_identity() {
        let lut = ScaleLut::build(SCREEN_WIDTH, SCREEN_HEIGHT);
        assert_eq!((lut.bar_x, lut.bar_y), (0, 0));
        assert_eq!(lut.x, (0..SCREEN_WIDTH).collect::<Vec<_>>());
        assert_eq!(lut.y, (0..SCREEN_HEIGHT).collect::<Vec<_>>());
    }
}
