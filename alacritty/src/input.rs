//! Handle input from glutin.
//!
//! Certain key combinations should send some escape sequence back to the PTY.
//! In order to figure that out, state about which modifier keys are pressed
//! needs to be tracked. Additionally, we need a bit of a state machine to
//! determine what to do when a non-modifier key is pressed.

use std::borrow::Cow;
use std::cmp::{max, min, Ordering};
use std::marker::PhantomData;
use std::time::{Duration, Instant};

use glutin::dpi::PhysicalPosition;
use glutin::event::{
    ElementState, KeyboardInput, ModifiersState, MouseButton, MouseScrollDelta, TouchPhase,
};
use glutin::event_loop::EventLoopWindowTarget;
#[cfg(target_os = "macos")]
use glutin::platform::macos::EventLoopWindowTargetExtMacOS;
use glutin::window::CursorIcon;

use alacritty_terminal::ansi::{ClearMode, Handler};
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{ClipboardType, SizeInfo, Term, TermMode};
use alacritty_terminal::vi_mode::ViMotion;

use crate::clipboard::Clipboard;
use crate::config::{Action, BindingMode, Config, Key, MouseAction, SearchAction, ViAction};
use crate::daemon::start_daemon;
use crate::display::hint::HintMatch;
use crate::display::window::Window;
use crate::display::Display;
use crate::event::{ClickState, Event, Mouse, TYPING_SEARCH_DELAY};
use crate::message_bar::{self, Message};
use crate::scheduler::{Scheduler, TimerId};

/// Font size change interval.
pub const FONT_SIZE_STEP: f32 = 0.5;

/// Interval for mouse scrolling during selection outside of the boundaries.
const SELECTION_SCROLLING_INTERVAL: Duration = Duration::from_millis(15);

/// Minimum number of pixels at the bottom/top where selection scrolling is performed.
const MIN_SELECTION_SCROLLING_HEIGHT: f64 = 5.;

/// Number of pixels for increasing the selection scrolling speed factor by one.
const SELECTION_SCROLLING_STEP: f64 = 20.;

/// Processes input from glutin.
///
/// An escape sequence may be emitted in case specific keys or key combinations
/// are activated.
pub struct Processor<T: EventListener, A: ActionContext<T>> {
    pub ctx: A,
    _phantom: PhantomData<T>,
}

pub trait ActionContext<T: EventListener> {
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, _data: B) {}
    fn mark_dirty(&mut self) {}
    fn size_info(&self) -> SizeInfo;
    fn copy_selection(&mut self, _ty: ClipboardType) {}
    fn start_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn toggle_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn update_selection(&mut self, _point: Point, _side: Side) {}
    fn clear_selection(&mut self) {}
    fn selection_is_empty(&self) -> bool;
    fn mouse_mut(&mut self) -> &mut Mouse;
    fn mouse(&self) -> &Mouse;
    fn received_count(&mut self) -> &mut usize;
    fn suppress_chars(&mut self) -> &mut bool;
    fn modifiers(&mut self) -> &mut ModifiersState;
    fn scroll(&mut self, _scroll: Scroll) {}
    fn window(&mut self) -> &mut Window;
    fn display(&mut self) -> &mut Display;
    fn terminal(&self) -> &Term<T>;
    fn terminal_mut(&mut self) -> &mut Term<T>;
    fn spawn_new_instance(&mut self) {}
    fn change_font_size(&mut self, _delta: f32) {}
    fn reset_font_size(&mut self) {}
    fn pop_message(&mut self) {}
    fn message(&self) -> Option<&Message>;
    fn config(&self) -> &Config;
    fn event_loop(&self) -> &EventLoopWindowTarget<Event>;
    fn mouse_mode(&self) -> bool;
    fn clipboard_mut(&mut self) -> &mut Clipboard;
    fn scheduler_mut(&mut self) -> &mut Scheduler;
    fn start_search(&mut self, _direction: Direction) {}
    fn confirm_search(&mut self) {}
    fn cancel_search(&mut self) {}
    fn search_input(&mut self, _c: char) {}
    fn search_pop_word(&mut self) {}
    fn search_history_previous(&mut self) {}
    fn search_history_next(&mut self) {}
    fn search_next(&mut self, origin: Point, direction: Direction, side: Side) -> Option<Match>;
    fn advance_search_origin(&mut self, _direction: Direction) {}
    fn search_direction(&self) -> Direction;
    fn search_active(&self) -> bool;
    fn on_typing_start(&mut self) {}
    fn toggle_vi_mode(&mut self) {}
    fn hint_input(&mut self, _character: char) {}
    fn trigger_hint(&mut self, _hint: &HintMatch) {}
    fn paste(&mut self, _text: &str) {}
}

impl Action {
    fn toggle_selection<T, A>(ctx: &mut A, ty: SelectionType)
    where
        A: ActionContext<T>,
        T: EventListener,
    {
        ctx.toggle_selection(ty, ctx.terminal().vi_mode_cursor.point, Side::Left);

        // Make sure initial selection is not empty.
        if let Some(selection) = &mut ctx.terminal_mut().selection {
            selection.include_all();
        }
    }
}

trait Execute<T: EventListener> {
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A);
}

impl<T: EventListener> Execute<T> for Action {
    #[inline]
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A) {
        match self {
            Action::Esc(s) => {
                ctx.on_typing_start();

                ctx.clear_selection();
                ctx.scroll(Scroll::Bottom);
                ctx.write_to_pty(s.clone().into_bytes())
            },
            Action::Command(program) => start_daemon(program.program(), program.args()),
            Action::Hint(hint) => {
                ctx.display().hint_state.start(hint.clone());
                ctx.mark_dirty();
            },
            Action::ToggleViMode => ctx.toggle_vi_mode(),
            Action::ViMotion(motion) => {
                ctx.on_typing_start();
                ctx.terminal_mut().vi_motion(*motion);
                ctx.mark_dirty();
            },
            Action::Vi(ViAction::ToggleNormalSelection) => {
                Self::toggle_selection(ctx, SelectionType::Simple);
            },
            Action::Vi(ViAction::ToggleLineSelection) => {
                Self::toggle_selection(ctx, SelectionType::Lines);
            },
            Action::Vi(ViAction::ToggleBlockSelection) => {
                Self::toggle_selection(ctx, SelectionType::Block);
            },
            Action::Vi(ViAction::ToggleSemanticSelection) => {
                Self::toggle_selection(ctx, SelectionType::Semantic);
            },
            Action::Vi(ViAction::Open) => {
                let hint = ctx.display().vi_highlighted_hint.take();
                if let Some(hint) = &hint {
                    ctx.mouse_mut().block_hint_launcher = false;
                    ctx.trigger_hint(hint);
                }
                ctx.display().vi_highlighted_hint = hint;
            },
            Action::Vi(ViAction::SearchNext) => {
                let terminal = ctx.terminal();
                let direction = ctx.search_direction();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchPrevious) => {
                let terminal = ctx.terminal();
                let direction = ctx.search_direction().opposite();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchStart) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.sub(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Left, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchEnd) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.add(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Right, Side::Right) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.end());
                    ctx.mark_dirty();
                }
            },
            Action::Search(SearchAction::SearchFocusNext) => {
                ctx.advance_search_origin(ctx.search_direction());
            },
            Action::Search(SearchAction::SearchFocusPrevious) => {
                let direction = ctx.search_direction().opposite();
                ctx.advance_search_origin(direction);
            },
            Action::Search(SearchAction::SearchConfirm) => ctx.confirm_search(),
            Action::Search(SearchAction::SearchCancel) => ctx.cancel_search(),
            Action::Search(SearchAction::SearchClear) => {
                let direction = ctx.search_direction();
                ctx.cancel_search();
                ctx.start_search(direction);
            },
            Action::Search(SearchAction::SearchDeleteWord) => ctx.search_pop_word(),
            Action::Search(SearchAction::SearchHistoryPrevious) => ctx.search_history_previous(),
            Action::Search(SearchAction::SearchHistoryNext) => ctx.search_history_next(),
            Action::Mouse(MouseAction::ExpandSelection) => {
                let selection_type = match ctx.mouse().click_state {
                    ClickState::Click => {
                        if ctx.modifiers().ctrl() {
                            Some(SelectionType::Block)
                        } else {
                            Some(SelectionType::Simple)
                        }
                    },
                    ClickState::DoubleClick => Some(SelectionType::Semantic),
                    ClickState::TripleClick => Some(SelectionType::Lines),
                    ClickState::None => None,
                };

                if let Some(selection_type) = selection_type {
                    // Load mouse point, treating message bar and padding as the closest cell.
                    let display_offset = ctx.terminal().grid().display_offset();
                    let point = ctx.mouse().point(&ctx.size_info(), display_offset);

                    let cell_side = ctx.mouse().cell_side;

                    let selection = match &mut ctx.terminal_mut().selection {
                        Some(selection) => selection,
                        None => return,
                    };

                    selection.ty = selection_type;
                    ctx.update_selection(point, cell_side);

                    // Move vi mode cursor to mouse click position.
                    if ctx.terminal().mode().contains(TermMode::VI) && !ctx.search_active() {
                        ctx.terminal_mut().vi_mode_cursor.point = point;
                    }
                }
            },
            Action::SearchForward => ctx.start_search(Direction::Right),
            Action::SearchBackward => ctx.start_search(Direction::Left),
            Action::Copy => ctx.copy_selection(ClipboardType::Clipboard),
            #[cfg(not(any(target_os = "macos", windows)))]
            Action::CopySelection => ctx.copy_selection(ClipboardType::Selection),
            Action::ClearSelection => ctx.clear_selection(),
            Action::Paste => {
                let text = ctx.clipboard_mut().load(ClipboardType::Clipboard);
                ctx.paste(&text);
            },
            Action::PasteSelection => {
                let text = ctx.clipboard_mut().load(ClipboardType::Selection);
                ctx.paste(&text);
            },
            Action::ToggleFullscreen => ctx.window().toggle_fullscreen(),
            #[cfg(target_os = "macos")]
            Action::ToggleSimpleFullscreen => ctx.window().toggle_simple_fullscreen(),
            #[cfg(target_os = "macos")]
            Action::Hide => ctx.event_loop().hide_application(),
            #[cfg(target_os = "macos")]
            Action::HideOtherApplications => ctx.event_loop().hide_other_applications(),
            #[cfg(not(target_os = "macos"))]
            Action::Hide => ctx.window().set_visible(false),
            Action::Minimize => ctx.window().set_minimized(true),
            Action::Quit => ctx.terminal_mut().exit(),
            Action::IncreaseFontSize => ctx.change_font_size(FONT_SIZE_STEP),
            Action::DecreaseFontSize => ctx.change_font_size(FONT_SIZE_STEP * -1.),
            Action::ResetFontSize => ctx.reset_font_size(),
            Action::ScrollPageUp => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let scroll_lines = term.screen_lines() as i32;
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, scroll_lines);

                ctx.scroll(Scroll::PageUp);
            },
            Action::ScrollPageDown => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let scroll_lines = -(term.screen_lines() as i32);
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, scroll_lines);

                ctx.scroll(Scroll::PageDown);
            },
            Action::ScrollHalfPageUp => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let scroll_lines = term.screen_lines() as i32 / 2;
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, scroll_lines);

                ctx.scroll(Scroll::Delta(scroll_lines));
            },
            Action::ScrollHalfPageDown => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let scroll_lines = -(term.screen_lines() as i32 / 2);
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, scroll_lines);

                ctx.scroll(Scroll::Delta(scroll_lines));
            },
            Action::ScrollLineUp => ctx.scroll(Scroll::Delta(1)),
            Action::ScrollLineDown => ctx.scroll(Scroll::Delta(-1)),
            Action::ScrollToTop => {
                ctx.scroll(Scroll::Top);

                // Move vi mode cursor.
                let topmost_line = ctx.terminal().topmost_line();
                ctx.terminal_mut().vi_mode_cursor.point.line = topmost_line;
                ctx.terminal_mut().vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ScrollToBottom => {
                ctx.scroll(Scroll::Bottom);

                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                term.vi_mode_cursor.point.line = term.bottommost_line();

                // Move to beginning twice, to always jump across linewraps.
                term.vi_motion(ViMotion::FirstOccupied);
                term.vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ClearHistory => ctx.terminal_mut().clear_screen(ClearMode::Saved),
            Action::ClearLogNotice => ctx.pop_message(),
            Action::SpawnNewInstance => ctx.spawn_new_instance(),
            Action::ReceiveChar | Action::None => (),
        }
    }
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    pub fn new(ctx: A) -> Self {
        Self { ctx, _phantom: Default::default() }
    }

    #[inline]
    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let size_info = self.ctx.size_info();

        let (x, y) = position.into();

        let lmb_pressed = self.ctx.mouse().left_button_state == ElementState::Pressed;
        let rmb_pressed = self.ctx.mouse().right_button_state == ElementState::Pressed;
        if !self.ctx.selection_is_empty() && (lmb_pressed || rmb_pressed) {
            self.update_selection_scrolling(y);
        }

        let display_offset = self.ctx.terminal().grid().display_offset();
        let old_point = self.ctx.mouse().point(&size_info, display_offset);

        let x = min(max(x, 0), size_info.width() as i32 - 1) as usize;
        let y = min(max(y, 0), size_info.height() as i32 - 1) as usize;
        self.ctx.mouse_mut().x = x;
        self.ctx.mouse_mut().y = y;

        let inside_text_area = size_info.contains_point(x, y);
        let cell_side = self.cell_side(x);

        let point = self.ctx.mouse().point(&size_info, display_offset);
        let cell_changed = old_point != point;

        // If the mouse hasn't changed cells, do nothing.
        if !cell_changed
            && self.ctx.mouse().cell_side == cell_side
            && self.ctx.mouse().inside_text_area == inside_text_area
        {
            return;
        }

        self.ctx.mouse_mut().inside_text_area = inside_text_area;
        self.ctx.mouse_mut().cell_side = cell_side;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Don't launch URLs if mouse has moved.
        self.ctx.mouse_mut().block_hint_launcher = true;

        if (lmb_pressed || rmb_pressed) && (self.ctx.modifiers().shift() || !self.ctx.mouse_mode())
        {
            self.ctx.update_selection(point, cell_side);
        } else if cell_changed
            && self.ctx.terminal().mode().intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
        {
            if lmb_pressed {
                self.mouse_report(32, ElementState::Pressed);
            } else if self.ctx.mouse().middle_button_state == ElementState::Pressed {
                self.mouse_report(33, ElementState::Pressed);
            } else if self.ctx.mouse().right_button_state == ElementState::Pressed {
                self.mouse_report(34, ElementState::Pressed);
            } else if self.ctx.terminal().mode().contains(TermMode::MOUSE_MOTION) {
                self.mouse_report(35, ElementState::Pressed);
            }
        }
    }

    /// Check which side of a cell an X coordinate lies on.
    fn cell_side(&self, x: usize) -> Side {
        let size_info = self.ctx.size_info();

        let cell_x =
            x.saturating_sub(size_info.padding_x() as usize) % size_info.cell_width() as usize;
        let half_cell_width = (size_info.cell_width() / 2.0) as usize;

        let additional_padding =
            (size_info.width() - size_info.padding_x() * 2.) % size_info.cell_width();
        let end_of_grid = size_info.width() - size_info.padding_x() - additional_padding;

        if cell_x > half_cell_width
            // Edge case when mouse leaves the window.
            || x as f32 >= end_of_grid
        {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn mouse_report(&mut self, button: u8, state: ElementState) {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        // Assure the mouse point is not in the scrollback.
        if point.line < 0 {
            return;
        }

        // Calculate modifiers value.
        let mut mods = 0;
        let modifiers = self.ctx.modifiers();
        if modifiers.shift() {
            mods += 4;
        }
        if modifiers.alt() {
            mods += 8;
        }
        if modifiers.ctrl() {
            mods += 16;
        }

        // Report mouse events.
        if self.ctx.terminal().mode().contains(TermMode::SGR_MOUSE) {
            self.sgr_mouse_report(point, button + mods, state);
        } else if let ElementState::Released = state {
            self.normal_mouse_report(point, 3 + mods);
        } else {
            self.normal_mouse_report(point, button + mods);
        }
    }

    fn normal_mouse_report(&mut self, point: Point, button: u8) {
        let Point { line, column } = point;
        let utf8 = self.ctx.terminal().mode().contains(TermMode::UTF8_MOUSE);

        let max_point = if utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.ctx.write_to_pty(msg);
    }

    fn sgr_mouse_report(&mut self, point: Point, button: u8, state: ElementState) {
        let c = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        let msg = format!("\x1b[<{};{};{}{}", button, point.column + 1, point.line + 1, c);
        self.ctx.write_to_pty(msg.into_bytes());
    }

    fn on_mouse_press(&mut self, button: MouseButton) {
        // Handle mouse mode.
        if !self.ctx.modifiers().shift() && self.ctx.mouse_mode() {
            self.ctx.mouse_mut().click_state = ClickState::None;

            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons..
                MouseButton::Other(_) => return,
            };

            self.mouse_report(code, ElementState::Pressed);
        } else {
            // Calculate time since the last click to handle double/triple clicks.
            let now = Instant::now();
            let elapsed = now - self.ctx.mouse().last_click_timestamp;
            self.ctx.mouse_mut().last_click_timestamp = now;

            // Update multi-click state.
            let mouse_config = &self.ctx.config().ui_config.mouse;
            self.ctx.mouse_mut().click_state = match self.ctx.mouse().click_state {
                // Reset click state if button has changed.
                _ if button != self.ctx.mouse().last_click_button => {
                    self.ctx.mouse_mut().last_click_button = button;
                    ClickState::Click
                },
                ClickState::Click if elapsed < mouse_config.double_click.threshold() => {
                    ClickState::DoubleClick
                },
                ClickState::DoubleClick if elapsed < mouse_config.triple_click.threshold() => {
                    ClickState::TripleClick
                },
                _ => ClickState::Click,
            };

            // Load mouse point, treating message bar and padding as the closest cell.
            let display_offset = self.ctx.terminal().grid().display_offset();
            let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

            if let MouseButton::Left = button {
                self.on_left_click(point)
            }
        }
    }

    /// Handle left click selection and vi mode cursor movement.
    fn on_left_click(&mut self, point: Point) {
        let side = self.ctx.mouse().cell_side;

        match self.ctx.mouse().click_state {
            ClickState::Click => {
                // Don't launch URLs if this click cleared the selection.
                self.ctx.mouse_mut().block_hint_launcher = !self.ctx.selection_is_empty();

                self.ctx.clear_selection();

                // Start new empty selection.
                if self.ctx.modifiers().ctrl() {
                    self.ctx.start_selection(SelectionType::Block, point, side);
                } else {
                    self.ctx.start_selection(SelectionType::Simple, point, side);
                }
            },
            ClickState::DoubleClick => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Semantic, point, side);
            },
            ClickState::TripleClick => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Lines, point, side);
            },
            ClickState::None => (),
        };

        // Move vi mode cursor to mouse click position.
        if self.ctx.terminal().mode().contains(TermMode::VI) && !self.ctx.search_active() {
            self.ctx.terminal_mut().vi_mode_cursor.point = point;
        }
    }

    fn on_mouse_release(&mut self, button: MouseButton) {
        if !self.ctx.modifiers().shift() && self.ctx.mouse_mode() {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons.
                MouseButton::Other(_) => return,
            };
            self.mouse_report(code, ElementState::Released);
            return;
        }

        // Trigger hints highlighted by the mouse.
        let hint = self.ctx.display().highlighted_hint.take();
        if let Some(hint) = hint.as_ref().filter(|_| button == MouseButton::Left) {
            self.ctx.trigger_hint(hint);
        }
        self.ctx.display().highlighted_hint = hint;

        self.ctx.scheduler_mut().unschedule(TimerId::SelectionScrolling);

        // Copy selection on release, to prevent flooding the display server.
        self.ctx.copy_selection(ClipboardType::Selection);
    }

    pub fn mouse_wheel_input(&mut self, delta: MouseScrollDelta, phase: TouchPhase) {
        match delta {
            MouseScrollDelta::LineDelta(_columns, lines) => {
                let new_scroll_px = lines * self.ctx.size_info().cell_height();
                self.scroll_terminal(f64::from(new_scroll_px));
            },
            MouseScrollDelta::PixelDelta(lpos) => {
                match phase {
                    TouchPhase::Started => {
                        // Reset offset to zero.
                        self.ctx.mouse_mut().scroll_px = 0.;
                    },
                    TouchPhase::Moved => {
                        self.scroll_terminal(lpos.y);
                    },
                    _ => (),
                }
            },
        }
    }

    fn scroll_terminal(&mut self, new_scroll_px: f64) {
        let height = f64::from(self.ctx.size_info().cell_height());

        if self.ctx.mouse_mode() {
            self.ctx.mouse_mut().scroll_px += new_scroll_px;

            let code = if new_scroll_px > 0. { 64 } else { 65 };
            let lines = (self.ctx.mouse().scroll_px / height).abs() as i32;

            for _ in 0..lines {
                self.mouse_report(code, ElementState::Pressed);
            }
        } else if self
            .ctx
            .terminal()
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !self.ctx.modifiers().shift()
        {
            let multiplier = f64::from(self.ctx.config().scrolling.multiplier);
            self.ctx.mouse_mut().scroll_px += new_scroll_px * multiplier;

            let cmd = if new_scroll_px > 0. { b'A' } else { b'B' };
            let lines = (self.ctx.mouse().scroll_px / height).abs() as i32;

            let mut content = Vec::with_capacity(lines as usize * 3);
            for _ in 0..lines {
                content.push(0x1b);
                content.push(b'O');
                content.push(cmd);
            }
            self.ctx.write_to_pty(content);
        } else {
            let multiplier = f64::from(self.ctx.config().scrolling.multiplier);
            self.ctx.mouse_mut().scroll_px += new_scroll_px * multiplier;

            let lines = self.ctx.mouse().scroll_px / height;

            self.ctx.scroll(Scroll::Delta(lines as i32));
        }

        self.ctx.mouse_mut().scroll_px %= height;
    }

    pub fn on_focus_change(&mut self, is_focused: bool) {
        if self.ctx.terminal().mode().contains(TermMode::FOCUS_IN_OUT) {
            let chr = if is_focused { "I" } else { "O" };

            let msg = format!("\x1b[{}", chr);
            self.ctx.write_to_pty(msg.into_bytes());
        }
    }

    pub fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        match button {
            MouseButton::Left => self.ctx.mouse_mut().left_button_state = state,
            MouseButton::Middle => self.ctx.mouse_mut().middle_button_state = state,
            MouseButton::Right => self.ctx.mouse_mut().right_button_state = state,
            _ => (),
        }

        // Skip normal mouse events if the message bar has been clicked.
        if self.message_bar_cursor_state() == Some(CursorIcon::Hand)
            && state == ElementState::Pressed
        {
            let size = self.ctx.size_info();

            let current_lines = self.ctx.message().map(|m| m.text(&size).len()).unwrap_or(0);

            self.ctx.clear_selection();
            self.ctx.pop_message();

            // Reset cursor when message bar height changed or all messages are gone.
            let new_lines = self.ctx.message().map(|m| m.text(&size).len()).unwrap_or(0);

            let new_icon = match current_lines.cmp(&new_lines) {
                Ordering::Less => CursorIcon::Default,
                Ordering::Equal => CursorIcon::Hand,
                Ordering::Greater => {
                    if self.ctx.mouse_mode() {
                        CursorIcon::Default
                    } else {
                        CursorIcon::Text
                    }
                },
            };

            self.ctx.window().set_mouse_cursor(new_icon);
        } else {
            match state {
                ElementState::Pressed => {
                    // process mouse press before bindings to
                    // correctly set click_state
                    self.on_mouse_press(button);
                    self.process_mouse_bindings(button);
                },
                ElementState::Released => self.on_mouse_release(button),
            }
        }
    }

    /// Process key input.
    pub fn key_input(&mut self, input: KeyboardInput) {
        // All key bindings are disabled while a hint is being selected.
        if self.ctx.display().hint_state.active() {
            *self.ctx.suppress_chars() = false;
            return;
        }

        // Reset search delay when the user is still typing.
        if self.ctx.search_active() {
            if let Some(timer) = self.ctx.scheduler_mut().get_mut(TimerId::DelayedSearch) {
                timer.deadline = Instant::now() + TYPING_SEARCH_DELAY;
            }
        }

        match input.state {
            ElementState::Pressed => {
                *self.ctx.received_count() = 0;
                self.process_key_bindings(input);
            },
            ElementState::Released => *self.ctx.suppress_chars() = false,
        }
    }

    /// Modifier state change.
    pub fn modifiers_input(&mut self, modifiers: ModifiersState) {
        *self.ctx.modifiers() = modifiers;

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Reset mouse cursor based on modifier and terminal state.
    #[inline]
    pub fn reset_mouse_cursor(&mut self) {
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Process a received character.
    pub fn received_char(&mut self, c: char) {
        let suppress_chars = *self.ctx.suppress_chars();

        // Handle hint selection over anything else.
        if self.ctx.display().hint_state.active() && !suppress_chars {
            self.ctx.hint_input(c);
            return;
        }

        // Pass keys to search and ignore them during `suppress_chars`.
        let search_active = self.ctx.search_active();
        if suppress_chars || search_active || self.ctx.terminal().mode().contains(TermMode::VI) {
            if search_active && !suppress_chars {
                self.ctx.search_input(c);
            }

            return;
        }

        self.ctx.on_typing_start();

        self.ctx.scroll(Scroll::Bottom);
        self.ctx.clear_selection();

        let utf8_len = c.len_utf8();
        let mut bytes = Vec::with_capacity(utf8_len);
        unsafe {
            bytes.set_len(utf8_len);
            c.encode_utf8(&mut bytes[..]);
        }

        if self.ctx.config().ui_config.alt_send_esc
            && *self.ctx.received_count() == 0
            && self.ctx.modifiers().alt()
            && utf8_len == 1
        {
            bytes.insert(0, b'\x1b');
        }

        self.ctx.write_to_pty(bytes);

        *self.ctx.received_count() += 1;
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_key_bindings(&mut self, input: KeyboardInput) {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mods = *self.ctx.modifiers();
        let mut suppress_chars = None;

        for i in 0..self.ctx.config().ui_config.key_bindings().len() {
            let binding = &self.ctx.config().ui_config.key_bindings()[i];

            let key = match (binding.trigger, input.virtual_keycode) {
                (Key::Scancode(_), _) => Key::Scancode(input.scancode),
                (_, Some(key)) => Key::Keycode(key),
                _ => continue,
            };

            if binding.is_triggered_by(mode, mods, &key) {
                // Pass through the key if any of the bindings has the `ReceiveChar` action.
                *suppress_chars.get_or_insert(true) &= binding.action != Action::ReceiveChar;

                // Binding was triggered; run the action.
                binding.action.clone().execute(&mut self.ctx);
            }
        }

        // Don't suppress char if no bindings were triggered.
        *self.ctx.suppress_chars() = suppress_chars.unwrap_or(false);
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_mouse_bindings(&mut self, button: MouseButton) {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mouse_mode = self.ctx.mouse_mode();
        let mods = *self.ctx.modifiers();

        for i in 0..self.ctx.config().ui_config.mouse_bindings().len() {
            let mut binding = self.ctx.config().ui_config.mouse_bindings()[i].clone();

            // Require shift for all modifiers when mouse mode is active.
            if mouse_mode {
                binding.mods |= ModifiersState::SHIFT;
            }

            if binding.is_triggered_by(mode, mods, &button) {
                binding.action.execute(&mut self.ctx);
            }
        }
    }

    /// Check mouse icon state in relation to the message bar.
    fn message_bar_cursor_state(&self) -> Option<CursorIcon> {
        // Since search is above the message bar, the button is offset by search's height.
        let search_height = if self.ctx.search_active() { 1 } else { 0 };

        // Calculate Y position of the end of the last terminal line.
        let size = self.ctx.size_info();
        let terminal_end = size.padding_y() as usize
            + size.cell_height() as usize * (size.screen_lines() + search_height);

        let mouse = self.ctx.mouse();
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        if self.ctx.message().is_none() || (mouse.y <= terminal_end) {
            None
        } else if mouse.y <= terminal_end + size.cell_height() as usize
            && point.column + message_bar::CLOSE_BUTTON_TEXT.len() >= size.columns()
        {
            Some(CursorIcon::Hand)
        } else {
            Some(CursorIcon::Default)
        }
    }

    /// Icon state of the cursor.
    fn cursor_state(&mut self) -> CursorIcon {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        // Function to check if mouse is on top of a hint.
        let hint_highlighted = |hint: &HintMatch| hint.bounds.contains(&point);

        if let Some(mouse_state) = self.message_bar_cursor_state() {
            mouse_state
        } else if self.ctx.display().highlighted_hint.as_ref().map_or(false, hint_highlighted) {
            CursorIcon::Hand
        } else if !self.ctx.modifiers().shift() && self.ctx.mouse_mode() {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        }
    }

    /// Handle automatic scrolling when selecting above/below the window.
    fn update_selection_scrolling(&mut self, mouse_y: i32) {
        let dpr = self.ctx.window().dpr;
        let size = self.ctx.size_info();
        let scheduler = self.ctx.scheduler_mut();

        // Scale constants by DPI.
        let min_height = (MIN_SELECTION_SCROLLING_HEIGHT * dpr) as i32;
        let step = (SELECTION_SCROLLING_STEP * dpr) as i32;

        // Compute the height of the scrolling areas.
        let end_top = max(min_height, size.padding_y() as i32);
        let text_area_bottom = size.padding_y() + size.screen_lines() as f32 * size.cell_height();
        let start_bottom = min(size.height() as i32 - min_height, text_area_bottom as i32);

        // Get distance from closest window boundary.
        let delta = if mouse_y < end_top {
            end_top - mouse_y + step
        } else if mouse_y >= start_bottom {
            start_bottom - mouse_y - step
        } else {
            scheduler.unschedule(TimerId::SelectionScrolling);
            return;
        };

        // Scale number of lines scrolled based on distance to boundary.
        let delta = delta as i32 / step as i32;
        let event = Event::Scroll(Scroll::Delta(delta));

        // Schedule event.
        match scheduler.get_mut(TimerId::SelectionScrolling) {
            Some(timer) => timer.event = event.into(),
            None => {
                scheduler.schedule(
                    event.into(),
                    SELECTION_SCROLLING_INTERVAL,
                    true,
                    TimerId::SelectionScrolling,
                );
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use glutin::event::{Event as GlutinEvent, VirtualKeyCode, WindowEvent};

    use alacritty_terminal::event::Event as TerminalEvent;

    use crate::config::Binding;
    use crate::message_bar::MessageBuffer;

    const KEY: VirtualKeyCode = VirtualKeyCode::Key0;

    struct MockEventProxy;
    impl EventListener for MockEventProxy {}

    struct ActionContext<'a, T> {
        pub terminal: &'a mut Term<T>,
        pub size_info: &'a SizeInfo,
        pub mouse: &'a mut Mouse,
        pub clipboard: &'a mut Clipboard,
        pub message_buffer: &'a mut MessageBuffer,
        pub received_count: usize,
        pub suppress_chars: bool,
        pub modifiers: ModifiersState,
        config: &'a Config,
    }

    impl<'a, T: EventListener> super::ActionContext<T> for ActionContext<'a, T> {
        fn search_next(
            &mut self,
            _origin: Point,
            _direction: Direction,
            _side: Side,
        ) -> Option<Match> {
            None
        }

        fn search_direction(&self) -> Direction {
            Direction::Right
        }

        fn search_active(&self) -> bool {
            false
        }

        fn terminal(&self) -> &Term<T> {
            self.terminal
        }

        fn terminal_mut(&mut self) -> &mut Term<T> {
            &mut self.terminal
        }

        fn size_info(&self) -> SizeInfo {
            *self.size_info
        }

        fn selection_is_empty(&self) -> bool {
            true
        }

        fn scroll(&mut self, scroll: Scroll) {
            self.terminal.scroll_display(scroll);
        }

        fn mouse_mode(&self) -> bool {
            false
        }

        #[inline]
        fn mouse_mut(&mut self) -> &mut Mouse {
            self.mouse
        }

        #[inline]
        fn mouse(&self) -> &Mouse {
            self.mouse
        }

        fn received_count(&mut self) -> &mut usize {
            &mut self.received_count
        }

        fn suppress_chars(&mut self) -> &mut bool {
            &mut self.suppress_chars
        }

        fn modifiers(&mut self) -> &mut ModifiersState {
            &mut self.modifiers
        }

        fn window(&mut self) -> &mut Window {
            unimplemented!();
        }

        fn display(&mut self) -> &mut Display {
            unimplemented!();
        }

        fn pop_message(&mut self) {
            self.message_buffer.pop();
        }

        fn message(&self) -> Option<&Message> {
            self.message_buffer.message()
        }

        fn config(&self) -> &Config {
            self.config
        }

        fn clipboard_mut(&mut self) -> &mut Clipboard {
            self.clipboard
        }

        fn event_loop(&self) -> &EventLoopWindowTarget<Event> {
            unimplemented!();
        }

        fn scheduler_mut(&mut self) -> &mut Scheduler {
            unimplemented!();
        }
    }

    macro_rules! test_clickstate {
        {
            name: $name:ident,
            initial_state: $initial_state:expr,
            initial_button: $initial_button:expr,
            input: $input:expr,
            end_state: $end_state:expr,
            end_button: $end_button:expr,
        } => {
            #[test]
            fn $name() {
                let mut clipboard = Clipboard::new_nop();
                let cfg = Config::default();
                let size = SizeInfo::new(
                    21.0,
                    51.0,
                    3.0,
                    3.0,
                    0.,
                    0.,
                    false,
                );

                let mut terminal = Term::new(&cfg, size, MockEventProxy);

                let mut mouse = Mouse {
                    click_state: $initial_state,
                    last_click_button: $initial_button,
                    ..Mouse::default()
                };

                let mut message_buffer = MessageBuffer::new();

                let context = ActionContext {
                    terminal: &mut terminal,
                    mouse: &mut mouse,
                    size_info: &size,
                    clipboard: &mut clipboard,
                    received_count: 0,
                    suppress_chars: false,
                    modifiers: Default::default(),
                    message_buffer: &mut message_buffer,
                    config: &cfg,
                };

                let mut processor = Processor::new(context);

                let event: GlutinEvent::<'_, TerminalEvent> = $input;
                if let GlutinEvent::WindowEvent {
                    event: WindowEvent::MouseInput {
                        state,
                        button,
                        ..
                    },
                    ..
                } = event
                {
                    processor.mouse_input(state, button);
                };

                assert_eq!(processor.ctx.mouse.click_state, $end_state);
                assert_eq!(processor.ctx.mouse.last_click_button, $end_button);
            }
        }
    }

    macro_rules! test_process_binding {
        {
            name: $name:ident,
            binding: $binding:expr,
            triggers: $triggers:expr,
            mode: $mode:expr,
            mods: $mods:expr,
        } => {
            #[test]
            fn $name() {
                if $triggers {
                    assert!($binding.is_triggered_by($mode, $mods, &KEY));
                } else {
                    assert!(!$binding.is_triggered_by($mode, $mods, &KEY));
                }
            }
        }
    }

    test_clickstate! {
        name: single_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::Click,
        end_button: MouseButton::Left,
    }

    test_clickstate! {
        name: single_right_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::Click,
        end_button: MouseButton::Right,
    }

    test_clickstate! {
        name: single_middle_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::Click,
        end_button: MouseButton::Middle,
    }

    test_clickstate! {
        name: double_click,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Left,
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::DoubleClick,
        end_button: MouseButton::Left,
    }

    test_clickstate! {
        name: double_click_right,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Right,
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::DoubleClick,
        end_button: MouseButton::Right,
    }

    test_clickstate! {
        name: triple_click,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::TripleClick,
        end_button: MouseButton::Left,
    }

    test_clickstate! {
        name: triple_click_middle,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Middle,
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::TripleClick,
        end_button: MouseButton::Middle,
    }

    test_clickstate! {
        name: multi_click_separate_buttons,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: GlutinEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: unsafe { std::mem::transmute_copy(&0) },
                modifiers: ModifiersState::default(),
            },
            window_id: unsafe { std::mem::transmute_copy(&0) },
        },
        end_state: ClickState::Click,
        end_button: MouseButton::Right,
    }

    test_process_binding! {
        name: process_binding_nomode_shiftmod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::SHIFT,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_controlmod,
        binding: Binding { trigger: KEY, mods: ModifiersState::CTRL, action: Action::from("\x1b[1;5D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::CTRL,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_not_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1b[D"), mode: BindingMode::empty(), notmode: BindingMode::APP_CURSOR },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_appkeypadmode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR | BindingMode::APP_KEYPAD,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_fail_with_extra_mods,
        binding: Binding { trigger: KEY, mods: ModifiersState::LOGO, action: Action::from("arst"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::ALT | ModifiersState::LOGO,
    }
}
