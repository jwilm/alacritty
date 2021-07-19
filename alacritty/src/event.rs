//! Process window events.

use std::borrow::Cow;
use std::cmp::{max, min};
use std::collections::VecDeque;
use std::env;
use std::f32;
use std::fmt::Debug;
#[cfg(not(any(target_os = "macos", windows)))]
use std::fs;
use std::fs::File;
use std::io::Write;
use std::mem;
use std::path::{Path, PathBuf};
#[cfg(not(any(target_os = "macos", windows)))]
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glutin::dpi::PhysicalSize;
use glutin::event::{ElementState, Event as GlutinEvent, ModifiersState, MouseButton, WindowEvent};
use glutin::event_loop::{ControlFlow, EventLoop, EventLoopProxy, EventLoopWindowTarget};
use glutin::platform::run_return::EventLoopExtRunReturn;
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use glutin::platform::unix::EventLoopWindowTargetExtUnix;
use log::info;
use serde_json as json;

use crossfont::{self, Size};

use alacritty_terminal::config::LOG_TARGET_CONFIG;
use alacritty_terminal::event::{Event as TerminalEvent, EventListener, Notify, OnResize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{ClipboardType, SizeInfo, Term, TermMode};
#[cfg(not(windows))]
use alacritty_terminal::tty;

use crate::cli::Options as CLIOptions;
use crate::clipboard::Clipboard;
use crate::config::ui_config::{HintAction, HintInternalAction};
use crate::config::{self, Config};
use crate::daemon::start_daemon;
use crate::display::hint::HintMatch;
use crate::display::window::Window;
use crate::display::{self, Display, DisplayUpdate};
use crate::input::{self, ActionContext as _, FONT_SIZE_STEP};
#[cfg(target_os = "macos")]
use crate::macos;
use crate::message_bar::{Message, MessageBuffer};
use crate::scheduler::{Scheduler, TimerId};

/// Duration after the last user input until an unlimited search is performed.
pub const TYPING_SEARCH_DELAY: Duration = Duration::from_millis(500);

/// Maximum number of lines for the blocking search while still typing the search regex.
const MAX_SEARCH_WHILE_TYPING: Option<usize> = Some(1000);

/// Maximum number of search terms stored in the history.
const MAX_SEARCH_HISTORY_SIZE: usize = 255;

/// Events dispatched through the UI event loop.
#[derive(Debug, Clone)]
pub enum Event {
    Terminal(TerminalEvent),
    DprChanged(f64, (u32, u32)),
    Scroll(Scroll),
    ConfigReload(PathBuf),
    Message(Message),
    BlinkCursor,
    SearchNext,
}

impl From<Event> for GlutinEvent<'_, Event> {
    fn from(event: Event) -> Self {
        GlutinEvent::UserEvent(event)
    }
}

impl From<TerminalEvent> for Event {
    fn from(event: TerminalEvent) -> Self {
        Event::Terminal(event)
    }
}

/// Regex search state.
pub struct SearchState {
    /// Search direction.
    direction: Direction,

    /// Change in display offset since the beginning of the search.
    display_offset_delta: i32,

    /// Search origin in viewport coordinates relative to original display offset.
    origin: Point,

    /// Focused match during active search.
    focused_match: Option<Match>,

    /// Search regex and history.
    ///
    /// During an active search, the first element is the user's current input.
    ///
    /// While going through history, the [`SearchState::history_index`] will point to the element
    /// in history which is currently being previewed.
    history: VecDeque<String>,

    /// Current position in the search history.
    history_index: Option<usize>,

    /// Compiled search automatons.
    dfas: Option<RegexSearch>,
}

impl SearchState {
    /// Search regex text if a search is active.
    pub fn regex(&self) -> Option<&String> {
        self.history_index.and_then(|index| self.history.get(index))
    }

    /// Direction of the search from the search origin.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Focused match during vi-less search.
    pub fn focused_match(&self) -> Option<&Match> {
        self.focused_match.as_ref()
    }

    /// Active search dfas.
    pub fn dfas(&self) -> Option<&RegexSearch> {
        self.dfas.as_ref()
    }

    /// Search regex text if a search is active.
    fn regex_mut(&mut self) -> Option<&mut String> {
        self.history_index.and_then(move |index| self.history.get_mut(index))
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            display_offset_delta: Default::default(),
            focused_match: Default::default(),
            history_index: Default::default(),
            history: Default::default(),
            origin: Default::default(),
            dfas: Default::default(),
        }
    }
}

pub struct ActionContext<'a, N, T> {
    pub notifier: &'a mut N,
    pub terminal: &'a mut Term<T>,
    pub clipboard: &'a mut Clipboard,
    pub mouse: &'a mut Mouse,
    pub received_count: &'a mut usize,
    pub suppress_chars: &'a mut bool,
    pub modifiers: &'a mut ModifiersState,
    pub display: &'a mut Display,
    pub message_buffer: &'a mut MessageBuffer,
    pub display_update_pending: &'a mut DisplayUpdate,
    pub config: &'a mut Config,
    pub event_loop: &'a EventLoopWindowTarget<Event>,
    pub scheduler: &'a mut Scheduler,
    pub search_state: &'a mut SearchState,
    cli_options: &'a CLIOptions,
    font_size: &'a mut Size,
    dirty: &'a mut bool,
}

impl<'a, N: Notify + 'a, T: EventListener> input::ActionContext<T> for ActionContext<'a, N, T> {
    #[inline]
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, val: B) {
        self.notifier.notify(val);
    }

    /// Request a redraw.
    #[inline]
    fn mark_dirty(&mut self) {
        *self.dirty = true;
    }

    #[inline]
    fn size_info(&self) -> SizeInfo {
        self.display.size_info
    }

    fn scroll(&mut self, scroll: Scroll) {
        let old_offset = self.terminal.grid().display_offset() as i32;

        self.terminal.scroll_display(scroll);

        // Keep track of manual display offset changes during search.
        if self.search_active() {
            let display_offset = self.terminal.grid().display_offset();
            self.search_state.display_offset_delta += old_offset - display_offset as i32;
        }

        // Update selection.
        if self.terminal.mode().contains(TermMode::VI)
            && self.terminal.selection.as_ref().map_or(true, |s| !s.is_empty())
        {
            self.update_selection(self.terminal.vi_mode_cursor.point, Side::Right);
        } else if self.mouse.left_button_state == ElementState::Pressed
            || self.mouse.right_button_state == ElementState::Pressed
        {
            let display_offset = self.terminal.grid().display_offset();
            let point = self.mouse.point(&self.size_info(), display_offset);
            self.update_selection(point, self.mouse.cell_side);
        }
        self.copy_selection(ClipboardType::Selection);

        *self.dirty = true;
    }

    // Copy text selection.
    fn copy_selection(&mut self, ty: ClipboardType) {
        let text = match self.terminal.selection_to_string().filter(|s| !s.is_empty()) {
            Some(text) => text,
            None => return,
        };

        if ty == ClipboardType::Selection && self.config.selection.save_to_clipboard {
            self.clipboard.store(ClipboardType::Clipboard, text.clone());
        }
        self.clipboard.store(ty, text);
    }

    fn selection_is_empty(&self) -> bool {
        self.terminal.selection.as_ref().map(Selection::is_empty).unwrap_or(true)
    }

    fn clear_selection(&mut self) {
        self.terminal.selection = None;
        *self.dirty = true;
    }

    fn update_selection(&mut self, mut point: Point, side: Side) {
        let mut selection = match self.terminal.selection.take() {
            Some(selection) => selection,
            None => return,
        };

        // Treat motion over message bar like motion over the last line.
        point.line = min(point.line, self.terminal.bottommost_line());

        // Update selection.
        selection.update(point, side);

        // Move vi cursor and expand selection.
        if self.terminal.mode().contains(TermMode::VI) && !self.search_active() {
            self.terminal.vi_mode_cursor.point = point;
            selection.include_all();
        }

        self.terminal.selection = Some(selection);
        *self.dirty = true;
    }

    fn start_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        self.terminal.selection = Some(Selection::new(ty, point, side));
        *self.dirty = true;

        self.copy_selection(ClipboardType::Selection);
    }

    fn toggle_selection(&mut self, ty: SelectionType, point: Point, side: Side) {
        match &mut self.terminal.selection {
            Some(selection) if selection.ty == ty && !selection.is_empty() => {
                self.clear_selection();
            },
            Some(selection) if !selection.is_empty() => {
                selection.ty = ty;
                *self.dirty = true;

                self.copy_selection(ClipboardType::Selection);
            },
            _ => self.start_selection(ty, point, side),
        }
    }

    #[inline]
    fn mouse_mode(&self) -> bool {
        self.terminal.mode().intersects(TermMode::MOUSE_MODE)
            && !self.terminal.mode().contains(TermMode::VI)
    }

    #[inline]
    fn mouse_mut(&mut self) -> &mut Mouse {
        self.mouse
    }

    #[inline]
    fn mouse(&self) -> &Mouse {
        self.mouse
    }

    #[inline]
    fn received_count(&mut self) -> &mut usize {
        &mut self.received_count
    }

    #[inline]
    fn suppress_chars(&mut self) -> &mut bool {
        &mut self.suppress_chars
    }

    #[inline]
    fn modifiers(&mut self) -> &mut ModifiersState {
        &mut self.modifiers
    }

    #[inline]
    fn window(&mut self) -> &mut Window {
        &mut self.display.window
    }

    #[inline]
    fn display(&mut self) -> &mut Display {
        &mut self.display
    }

    #[inline]
    fn terminal(&self) -> &Term<T> {
        self.terminal
    }

    #[inline]
    fn terminal_mut(&mut self) -> &mut Term<T> {
        self.terminal
    }

    fn spawn_new_instance(&mut self) {
        let mut env_args = env::args();
        let alacritty = env_args.next().unwrap();

        #[cfg(unix)]
        let mut args = {
            // Use working directory of controlling process, or fallback to initial shell.
            let mut pid = unsafe { libc::tcgetpgrp(tty::master_fd()) };
            if pid < 0 {
                pid = tty::child_pid();
            }

            #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
            let link_path = format!("/proc/{}/cwd", pid);
            #[cfg(target_os = "freebsd")]
            let link_path = format!("/compat/linux/proc/{}/cwd", pid);
            #[cfg(not(target_os = "macos"))]
            let cwd = fs::read_link(link_path);
            #[cfg(target_os = "macos")]
            let cwd = macos::proc::cwd(pid);

            // Add the current working directory as parameter.
            cwd.map(|path| vec!["--working-directory".into(), path]).unwrap_or_default()
        };

        #[cfg(not(unix))]
        let mut args: Vec<PathBuf> = Vec::new();

        let working_directory_set = !args.is_empty();

        // Reuse the arguments passed to Alacritty for the new instance.
        while let Some(arg) = env_args.next() {
            // Drop working directory from existing parameters.
            if working_directory_set && arg == "--working-directory" {
                let _ = env_args.next();
                continue;
            }

            args.push(arg.into());
        }

        start_daemon(&alacritty, &args);
    }

    fn change_font_size(&mut self, delta: f32) {
        *self.font_size = max(*self.font_size + delta, Size::new(FONT_SIZE_STEP));
        let font = self.config.ui_config.font.clone().with_size(*self.font_size);
        self.display_update_pending.set_font(font);
        *self.dirty = true;
    }

    fn reset_font_size(&mut self) {
        *self.font_size = self.config.ui_config.font.size();
        self.display_update_pending.set_font(self.config.ui_config.font.clone());
        *self.dirty = true;
    }

    #[inline]
    fn pop_message(&mut self) {
        if !self.message_buffer.is_empty() {
            self.display_update_pending.dirty = true;
            self.message_buffer.pop();
            *self.dirty = true;
        }
    }

    #[inline]
    fn start_search(&mut self, direction: Direction) {
        // Only create new history entry if the previous regex wasn't empty.
        if self.search_state.history.get(0).map_or(true, |regex| !regex.is_empty()) {
            self.search_state.history.push_front(String::new());
            self.search_state.history.truncate(MAX_SEARCH_HISTORY_SIZE);
        }

        self.search_state.history_index = Some(0);
        self.search_state.direction = direction;
        self.search_state.focused_match = None;

        // Store original search position as origin and reset location.
        if self.terminal.mode().contains(TermMode::VI) {
            self.search_state.origin = self.terminal.vi_mode_cursor.point;
            self.search_state.display_offset_delta = 0;
        } else {
            let viewport_top = Line(-(self.terminal.grid().display_offset() as i32)) - 1;
            let viewport_bottom = viewport_top + self.terminal.bottommost_line();
            let last_column = self.terminal.last_column();
            self.search_state.origin = match direction {
                Direction::Right => Point::new(viewport_top, Column(0)),
                Direction::Left => Point::new(viewport_bottom, last_column),
            };
        }

        self.display_update_pending.dirty = true;
        *self.dirty = true;
    }

    #[inline]
    fn confirm_search(&mut self) {
        // Just cancel search when not in vi mode.
        if !self.terminal.mode().contains(TermMode::VI) {
            self.cancel_search();
            return;
        }

        // Force unlimited search if the previous one was interrupted.
        if self.scheduler.scheduled(TimerId::DelayedSearch) {
            self.goto_match(None);
        }

        self.exit_search();
    }

    #[inline]
    fn cancel_search(&mut self) {
        if self.terminal.mode().contains(TermMode::VI) {
            // Recover pre-search state in vi mode.
            self.search_reset_state();
        } else if let Some(focused_match) = &self.search_state.focused_match {
            // Create a selection for the focused match.
            let start = *focused_match.start();
            let end = *focused_match.end();
            self.start_selection(SelectionType::Simple, start, Side::Left);
            self.update_selection(end, Side::Right);
            self.copy_selection(ClipboardType::Selection);
        }

        self.search_state.dfas = None;

        self.exit_search();
    }

    #[inline]
    fn search_input(&mut self, c: char) {
        match self.search_state.history_index {
            Some(0) => (),
            // When currently in history, replace active regex with history on change.
            Some(index) => {
                self.search_state.history[0] = self.search_state.history[index].clone();
                self.search_state.history_index = Some(0);
            },
            None => return,
        }
        let regex = &mut self.search_state.history[0];

        match c {
            // Handle backspace/ctrl+h.
            '\x08' | '\x7f' => {
                let _ = regex.pop();
            },
            // Add ascii and unicode text.
            ' '..='~' | '\u{a0}'..='\u{10ffff}' => regex.push(c),
            // Ignore non-printable characters.
            _ => return,
        }

        if !self.terminal.mode().contains(TermMode::VI) {
            // Clear selection so we do not obstruct any matches.
            self.terminal.selection = None;
        }

        self.update_search();
    }

    #[inline]
    fn search_pop_word(&mut self) {
        if let Some(regex) = self.search_state.regex_mut() {
            *regex = regex.trim_end().to_owned();
            regex.truncate(regex.rfind(' ').map(|i| i + 1).unwrap_or(0));
            self.update_search();
        }
    }

    /// Go to the previous regex in the search history.
    #[inline]
    fn search_history_previous(&mut self) {
        let index = match &mut self.search_state.history_index {
            None => return,
            Some(index) if *index + 1 >= self.search_state.history.len() => return,
            Some(index) => index,
        };

        *index += 1;
        self.update_search();
    }

    /// Go to the previous regex in the search history.
    #[inline]
    fn search_history_next(&mut self) {
        let index = match &mut self.search_state.history_index {
            Some(0) | None => return,
            Some(index) => index,
        };

        *index -= 1;
        self.update_search();
    }

    #[inline]
    fn advance_search_origin(&mut self, direction: Direction) {
        // Use focused match as new search origin if available.
        if let Some(focused_match) = &self.search_state.focused_match {
            let new_origin = match direction {
                Direction::Right => focused_match.end().add(self.terminal, Boundary::None, 1),
                Direction::Left => focused_match.start().sub(self.terminal, Boundary::None, 1),
            };

            self.terminal.scroll_to_point(new_origin);

            self.search_state.display_offset_delta = 0;
            self.search_state.origin = new_origin;
        }

        // Search for the next match using the supplied direction.
        let search_direction = mem::replace(&mut self.search_state.direction, direction);
        self.goto_match(None);
        self.search_state.direction = search_direction;

        // If we found a match, we set the search origin right in front of it to make sure that
        // after modifications to the regex the search is started without moving the focused match
        // around.
        let focused_match = match &self.search_state.focused_match {
            Some(focused_match) => focused_match,
            None => return,
        };

        // Set new origin to the left/right of the match, depending on search direction.
        let new_origin = match self.search_state.direction {
            Direction::Right => *focused_match.start(),
            Direction::Left => *focused_match.end(),
        };

        // Store the search origin with display offset by checking how far we need to scroll to it.
        let old_display_offset = self.terminal.grid().display_offset() as i32;
        self.terminal.scroll_to_point(new_origin);
        let new_display_offset = self.terminal.grid().display_offset() as i32;
        self.search_state.display_offset_delta = new_display_offset - old_display_offset;

        // Store origin and scroll back to the match.
        self.terminal.scroll_display(Scroll::Delta(-self.search_state.display_offset_delta));
        self.search_state.origin = new_origin;
    }

    /// Find the next search match.
    fn search_next(&mut self, origin: Point, direction: Direction, side: Side) -> Option<Match> {
        self.search_state
            .dfas
            .as_ref()
            .and_then(|dfas| self.terminal.search_next(dfas, origin, direction, side, None))
    }

    #[inline]
    fn search_direction(&self) -> Direction {
        self.search_state.direction
    }

    #[inline]
    fn search_active(&self) -> bool {
        self.search_state.history_index.is_some()
    }

    /// Handle keyboard typing start.
    ///
    /// This will temporarily disable some features like terminal cursor blinking or the mouse
    /// cursor.
    ///
    /// All features are re-enabled again automatically.
    #[inline]
    fn on_typing_start(&mut self) {
        // Disable cursor blinking.
        let blink_interval = self.config.cursor.blink_interval();
        if let Some(timer) = self.scheduler.get_mut(TimerId::BlinkCursor) {
            timer.deadline = Instant::now() + Duration::from_millis(blink_interval);
            self.display.cursor_hidden = false;
            *self.dirty = true;
        }

        // Hide mouse cursor.
        if self.config.ui_config.mouse.hide_when_typing {
            self.display.window.set_mouse_visible(false);
        }
    }

    /// Process a new character for keyboard hints.
    fn hint_input(&mut self, c: char) {
        if let Some(hint) = self.display.hint_state.keyboard_input(self.terminal, c) {
            self.mouse.block_hint_launcher = false;
            self.trigger_hint(&hint);
        }
        *self.dirty = true;
    }

    /// Trigger a hint action.
    fn trigger_hint(&mut self, hint: &HintMatch) {
        if self.mouse.block_hint_launcher {
            return;
        }

        match &hint.action {
            // Launch an external program.
            HintAction::Command(command) => {
                let text = self.terminal.bounds_to_string(*hint.bounds.start(), *hint.bounds.end());
                let mut args = command.args().to_vec();
                args.push(text);
                start_daemon(command.program(), &args);
            },
            // Copy the text to the clipboard.
            HintAction::Action(HintInternalAction::Copy) => {
                let text = self.terminal.bounds_to_string(*hint.bounds.start(), *hint.bounds.end());
                self.clipboard.store(ClipboardType::Clipboard, text);
            },
            // Write the text to the PTY/search.
            HintAction::Action(HintInternalAction::Paste) => {
                let text = self.terminal.bounds_to_string(*hint.bounds.start(), *hint.bounds.end());
                self.paste(&text);
            },
            // Select the text.
            HintAction::Action(HintInternalAction::Select) => {
                self.start_selection(SelectionType::Simple, *hint.bounds.start(), Side::Left);
                self.update_selection(*hint.bounds.end(), Side::Right);
                self.copy_selection(ClipboardType::Selection);
            },
            // Move the vi mode cursor.
            HintAction::Action(HintInternalAction::MoveViModeCursor) => {
                // Enter vi mode if we're not in it already.
                if !self.terminal.mode().contains(TermMode::VI) {
                    self.terminal.toggle_vi_mode();
                }

                self.terminal.vi_goto_point(*hint.bounds.start());
            },
        }
    }

    /// Expand the selection to the current mouse cursor position.
    #[inline]
    fn expand_selection(&mut self) {
        let selection_type = match self.mouse().click_state {
            ClickState::Click => {
                if self.modifiers().ctrl() {
                    SelectionType::Block
                } else {
                    SelectionType::Simple
                }
            },
            ClickState::DoubleClick => SelectionType::Semantic,
            ClickState::TripleClick => SelectionType::Lines,
            ClickState::None => return,
        };

        // Load mouse point, treating message bar and padding as the closest cell.
        let display_offset = self.terminal().grid().display_offset();
        let point = self.mouse().point(&self.size_info(), display_offset);

        let cell_side = self.mouse().cell_side;

        let selection = match &mut self.terminal_mut().selection {
            Some(selection) => selection,
            None => return,
        };

        selection.ty = selection_type;
        self.update_selection(point, cell_side);

        // Move vi mode cursor to mouse click position.
        if self.terminal().mode().contains(TermMode::VI) && !self.search_active() {
            self.terminal_mut().vi_mode_cursor.point = point;
        }
    }

    /// Paste a text into the terminal.
    fn paste(&mut self, text: &str) {
        if self.search_active() {
            for c in text.chars() {
                self.search_input(c);
            }
        } else if self.terminal().mode().contains(TermMode::BRACKETED_PASTE) {
            self.write_to_pty(&b"\x1b[200~"[..]);
            self.write_to_pty(text.replace("\x1b", "").into_bytes());
            self.write_to_pty(&b"\x1b[201~"[..]);
        } else {
            // In non-bracketed (ie: normal) mode, terminal applications cannot distinguish
            // pasted data from keystrokes.
            // In theory, we should construct the keystrokes needed to produce the data we are
            // pasting... since that's neither practical nor sensible (and probably an impossible
            // task to solve in a general way), we'll just replace line breaks (windows and unix
            // style) with a single carriage return (\r, which is what the Enter key produces).
            self.write_to_pty(text.replace("\r\n", "\r").replace("\n", "\r").into_bytes());
        }
    }

    /// Toggle the vi mode status.
    #[inline]
    fn toggle_vi_mode(&mut self) {
        if !self.terminal.mode().contains(TermMode::VI) {
            self.clear_selection();
        }

        self.cancel_search();
        self.terminal.toggle_vi_mode();

        *self.dirty = true;
    }

    fn message(&self) -> Option<&Message> {
        self.message_buffer.message()
    }

    fn config(&self) -> &Config {
        self.config
    }

    fn event_loop(&self) -> &EventLoopWindowTarget<Event> {
        self.event_loop
    }

    fn clipboard_mut(&mut self) -> &mut Clipboard {
        self.clipboard
    }

    fn scheduler_mut(&mut self) -> &mut Scheduler {
        self.scheduler
    }
}

impl<'a, N: Notify + 'a, T: EventListener> ActionContext<'a, N, T> {
    fn update_search(&mut self) {
        let regex = match self.search_state.regex() {
            Some(regex) => regex,
            None => return,
        };

        // Hide cursor while typing into the search bar.
        if self.config.ui_config.mouse.hide_when_typing {
            self.display.window.set_mouse_visible(false);
        }

        if regex.is_empty() {
            // Stop search if there's nothing to search for.
            self.search_reset_state();
            self.search_state.dfas = None;
        } else {
            // Create search dfas for the new regex string.
            self.search_state.dfas = RegexSearch::new(regex).ok();

            // Update search highlighting.
            self.goto_match(MAX_SEARCH_WHILE_TYPING);
        }

        *self.dirty = true;
    }

    /// Reset terminal to the state before search was started.
    fn search_reset_state(&mut self) {
        // Unschedule pending timers.
        self.scheduler.unschedule(TimerId::DelayedSearch);

        // Clear focused match.
        self.search_state.focused_match = None;

        // The viewport reset logic is only needed for vi mode, since without it our origin is
        // always at the current display offset instead of at the vi cursor position which we need
        // to recover to.
        if !self.terminal.mode().contains(TermMode::VI) {
            return;
        }

        // Reset display offset and cursor position.
        self.terminal.scroll_display(Scroll::Delta(self.search_state.display_offset_delta));
        self.search_state.display_offset_delta = 0;
        self.terminal.vi_mode_cursor.point =
            self.search_state.origin.grid_clamp(self.terminal, Boundary::Grid);

        *self.dirty = true;
    }

    /// Jump to the first regex match from the search origin.
    fn goto_match(&mut self, mut limit: Option<usize>) {
        let dfas = match &self.search_state.dfas {
            Some(dfas) => dfas,
            None => return,
        };

        // Limit search only when enough lines are available to run into the limit.
        limit = limit.filter(|&limit| limit <= self.terminal.total_lines());

        // Jump to the next match.
        let direction = self.search_state.direction;
        let clamped_origin = self.search_state.origin.grid_clamp(self.terminal, Boundary::Grid);
        match self.terminal.search_next(dfas, clamped_origin, direction, Side::Left, limit) {
            Some(regex_match) => {
                let old_offset = self.terminal.grid().display_offset() as i32;

                if self.terminal.mode().contains(TermMode::VI) {
                    // Move vi cursor to the start of the match.
                    self.terminal.vi_goto_point(*regex_match.start());
                } else {
                    // Select the match when vi mode is not active.
                    self.terminal.scroll_to_point(*regex_match.start());
                }

                // Update the focused match.
                self.search_state.focused_match = Some(regex_match);

                // Store number of lines the viewport had to be moved.
                let display_offset = self.terminal.grid().display_offset();
                self.search_state.display_offset_delta += old_offset - display_offset as i32;

                // Since we found a result, we require no delayed re-search.
                self.scheduler.unschedule(TimerId::DelayedSearch);
            },
            // Reset viewport only when we know there is no match, to prevent unnecessary jumping.
            None if limit.is_none() => self.search_reset_state(),
            None => {
                // Schedule delayed search if we ran into our search limit.
                if !self.scheduler.scheduled(TimerId::DelayedSearch) {
                    self.scheduler.schedule(
                        Event::SearchNext.into(),
                        TYPING_SEARCH_DELAY,
                        false,
                        TimerId::DelayedSearch,
                    );
                }

                // Clear focused match.
                self.search_state.focused_match = None;
            },
        }

        *self.dirty = true;
    }

    /// Cleanup the search state.
    fn exit_search(&mut self) {
        self.display_update_pending.dirty = true;
        self.search_state.history_index = None;
        *self.dirty = true;

        // Clear focused match.
        self.search_state.focused_match = None;
    }

    /// Update the cursor blinking state.
    fn update_cursor_blinking(&mut self) {
        // Get config cursor style.
        let mut cursor_style = self.config.cursor.style;
        if self.terminal.mode().contains(TermMode::VI) {
            cursor_style = self.config.cursor.vi_mode_style.unwrap_or(cursor_style);
        };

        // Check terminal cursor style.
        let terminal_blinking = self.terminal.cursor_style().blinking;
        let blinking = cursor_style.blinking_override().unwrap_or(terminal_blinking);

        // Update cursor blinking state.
        self.scheduler.unschedule(TimerId::BlinkCursor);
        if blinking && self.terminal.is_focused {
            self.scheduler.schedule(
                GlutinEvent::UserEvent(Event::BlinkCursor),
                Duration::from_millis(self.config.cursor.blink_interval()),
                true,
                TimerId::BlinkCursor,
            )
        } else {
            self.display.cursor_hidden = false;
            *self.dirty = true;
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ClickState {
    None,
    Click,
    DoubleClick,
    TripleClick,
}

/// State of the mouse.
#[derive(Debug)]
pub struct Mouse {
    pub left_button_state: ElementState,
    pub middle_button_state: ElementState,
    pub right_button_state: ElementState,
    pub last_click_timestamp: Instant,
    pub last_click_button: MouseButton,
    pub click_state: ClickState,
    pub scroll_px: f64,
    pub cell_side: Side,
    pub lines_scrolled: f32,
    pub block_hint_launcher: bool,
    pub hint_highlight_dirty: bool,
    pub inside_text_area: bool,
    pub x: usize,
    pub y: usize,
}

impl Default for Mouse {
    fn default() -> Mouse {
        Mouse {
            last_click_timestamp: Instant::now(),
            last_click_button: MouseButton::Left,
            left_button_state: ElementState::Released,
            middle_button_state: ElementState::Released,
            right_button_state: ElementState::Released,
            click_state: ClickState::None,
            cell_side: Side::Left,
            hint_highlight_dirty: Default::default(),
            block_hint_launcher: Default::default(),
            inside_text_area: Default::default(),
            lines_scrolled: Default::default(),
            scroll_px: Default::default(),
            x: Default::default(),
            y: Default::default(),
        }
    }
}

impl Mouse {
    /// Convert mouse pixel coordinates to viewport point.
    ///
    /// If the coordinates are outside of the terminal grid, like positions inside the padding, the
    /// coordinates will be clamped to the closest grid coordinates.
    #[inline]
    pub fn point(&self, size: &SizeInfo, display_offset: usize) -> Point {
        let col = self.x.saturating_sub(size.padding_x() as usize) / (size.cell_width() as usize);
        let col = min(Column(col), size.last_column());

        let line = self.y.saturating_sub(size.padding_y() as usize) / (size.cell_height() as usize);
        let line = min(line, size.bottommost_line().0 as usize);

        display::viewport_to_point(display_offset, Point::new(line, col))
    }
}

/// The event processor.
///
/// Stores some state from received events and dispatches actions when they are
/// triggered.
pub struct Processor<N> {
    notifier: N,
    mouse: Mouse,
    received_count: usize,
    suppress_chars: bool,
    modifiers: ModifiersState,
    config: Config,
    message_buffer: MessageBuffer,
    display: Display,
    font_size: Size,
    event_queue: Vec<GlutinEvent<'static, Event>>,
    search_state: SearchState,
    cli_options: CLIOptions,
    dirty: bool,
}

impl<N: Notify + OnResize> Processor<N> {
    /// Create a new event processor.
    ///
    /// Takes a writer which is expected to be hooked up to the write end of a PTY.
    pub fn new(
        notifier: N,
        message_buffer: MessageBuffer,
        config: Config,
        display: Display,
        cli_options: CLIOptions,
    ) -> Processor<N> {
        Processor {
            font_size: config.ui_config.font.size(),
            message_buffer,
            cli_options,
            notifier,
            display,
            config,
            received_count: Default::default(),
            suppress_chars: Default::default(),
            search_state: Default::default(),
            event_queue: Default::default(),
            modifiers: Default::default(),
            mouse: Default::default(),
            dirty: Default::default(),
        }
    }

    /// Return `true` if `event_queue` is empty, `false` otherwise.
    #[inline]
    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    fn event_queue_empty(&mut self) -> bool {
        let wayland_event_queue = match self.display.wayland_event_queue.as_mut() {
            Some(wayland_event_queue) => wayland_event_queue,
            // Since frame callbacks do not exist on X11, just check for event queue.
            None => return self.event_queue.is_empty(),
        };

        // Check for pending frame callbacks on Wayland.
        let events_dispatched = wayland_event_queue
            .dispatch_pending(&mut (), |_, _, _| {})
            .expect("failed to dispatch event queue");

        self.event_queue.is_empty() && events_dispatched == 0
    }

    /// Return `true` if `event_queue` is empty, `false` otherwise.
    #[inline]
    #[cfg(any(not(feature = "wayland"), target_os = "macos", windows))]
    fn event_queue_empty(&mut self) -> bool {
        self.event_queue.is_empty()
    }

    /// Run the event loop.
    pub fn run<T>(&mut self, terminal: Arc<FairMutex<Term<T>>>, mut event_loop: EventLoop<Event>)
    where
        T: EventListener,
    {
        let mut scheduler = Scheduler::new();

        // Start the initial cursor blinking timer.
        if self.config.cursor.style().blinking {
            let event: Event = TerminalEvent::CursorBlinkingChange(true).into();
            self.event_queue.push(event.into());
        }

        // NOTE: Since this takes a pointer to the winit event loop, it MUST be dropped first.
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        let mut clipboard = unsafe { Clipboard::new(event_loop.wayland_display()) };
        #[cfg(any(not(feature = "wayland"), target_os = "macos", windows))]
        let mut clipboard = Clipboard::new();

        event_loop.run_return(|event, event_loop, control_flow| {
            if self.config.ui_config.debug.print_events {
                info!("glutin event: {:?}", event);
            }

            // Ignore all events we do not care about.
            if Self::skip_event(&event) {
                return;
            }

            match event {
                // Check for shutdown.
                GlutinEvent::UserEvent(Event::Terminal(TerminalEvent::Exit)) => {
                    *control_flow = ControlFlow::Exit;
                    return;
                },
                // Process events.
                GlutinEvent::RedrawEventsCleared => {
                    *control_flow = match scheduler.update(&mut self.event_queue) {
                        Some(instant) => ControlFlow::WaitUntil(instant),
                        None => ControlFlow::Wait,
                    };

                    if self.event_queue_empty() {
                        return;
                    }
                },
                // Remap DPR change event to remove lifetime.
                GlutinEvent::WindowEvent {
                    event: WindowEvent::ScaleFactorChanged { scale_factor, new_inner_size },
                    ..
                } => {
                    *control_flow = ControlFlow::Poll;
                    let size = (new_inner_size.width, new_inner_size.height);
                    self.event_queue.push(Event::DprChanged(scale_factor, size).into());
                    return;
                },
                // Transmute to extend lifetime, which exists only for `ScaleFactorChanged` event.
                // Since we remap that event to remove the lifetime, this is safe.
                event => unsafe {
                    *control_flow = ControlFlow::Poll;
                    self.event_queue.push(mem::transmute(event));
                    return;
                },
            }

            let mut terminal = terminal.lock();

            let mut display_update_pending = DisplayUpdate::default();
            let old_is_searching = self.search_state.history_index.is_some();

            let context = ActionContext {
                terminal: &mut terminal,
                notifier: &mut self.notifier,
                mouse: &mut self.mouse,
                clipboard: &mut clipboard,
                received_count: &mut self.received_count,
                suppress_chars: &mut self.suppress_chars,
                modifiers: &mut self.modifiers,
                message_buffer: &mut self.message_buffer,
                display_update_pending: &mut display_update_pending,
                display: &mut self.display,
                font_size: &mut self.font_size,
                config: &mut self.config,
                scheduler: &mut scheduler,
                search_state: &mut self.search_state,
                cli_options: &self.cli_options,
                dirty: &mut self.dirty,
                event_loop,
            };
            let mut processor = input::Processor::new(context);

            for event in self.event_queue.drain(..) {
                Processor::handle_event(event, &mut processor);
            }

            // Process DisplayUpdate events.
            if display_update_pending.dirty {
                self.submit_display_update(&mut terminal, old_is_searching, display_update_pending);
            }

            // Skip rendering on Wayland until we get frame event from compositor.
            #[cfg(not(any(target_os = "macos", windows)))]
            if !self.display.is_x11 && !self.display.window.should_draw.load(Ordering::Relaxed) {
                return;
            }

            if self.dirty || self.mouse.hint_highlight_dirty {
                self.dirty |= self.display.update_highlighted_hints(
                    &terminal,
                    &self.config,
                    &self.mouse,
                    self.modifiers,
                );
                self.mouse.hint_highlight_dirty = false;
            }

            if self.dirty {
                self.dirty = false;

                // Request immediate re-draw if visual bell animation is not finished yet.
                if !self.display.visual_bell.completed() {
                    let event: Event = TerminalEvent::Wakeup.into();
                    self.event_queue.push(event.into());

                    *control_flow = ControlFlow::Poll;
                }

                // Redraw screen.
                self.display.draw(terminal, &self.message_buffer, &self.config, &self.search_state);
            }
        });

        // Write ref tests to disk.
        if self.config.ui_config.debug.ref_test {
            self.write_ref_test_results(&terminal.lock());
        }
    }

    /// Handle events from glutin.
    ///
    /// Doesn't take self mutably due to borrow checking.
    fn handle_event<T>(
        event: GlutinEvent<'_, Event>,
        processor: &mut input::Processor<T, ActionContext<'_, N, T>>,
    ) where
        T: EventListener,
    {
        match event {
            GlutinEvent::UserEvent(event) => match event {
                Event::DprChanged(scale_factor, (width, height)) => {
                    let display_update_pending = &mut processor.ctx.display_update_pending;

                    // Push current font to update its DPR.
                    let font = processor.ctx.config.ui_config.font.clone();
                    display_update_pending.set_font(font.with_size(*processor.ctx.font_size));

                    // Resize to event's dimensions, since no resize event is emitted on Wayland.
                    display_update_pending.set_dimensions(PhysicalSize::new(width, height));

                    processor.ctx.window().dpr = scale_factor;
                    *processor.ctx.dirty = true;
                },
                Event::Message(message) => {
                    processor.ctx.message_buffer.push(message);
                    processor.ctx.display_update_pending.dirty = true;
                    *processor.ctx.dirty = true;
                },
                Event::SearchNext => processor.ctx.goto_match(None),
                Event::ConfigReload(path) => Self::reload_config(&path, processor),
                Event::Scroll(scroll) => processor.ctx.scroll(scroll),
                Event::BlinkCursor => {
                    processor.ctx.display.cursor_hidden ^= true;
                    *processor.ctx.dirty = true;
                },
                Event::Terminal(event) => match event {
                    TerminalEvent::Title(title) => {
                        let ui_config = &processor.ctx.config.ui_config;
                        if ui_config.window.dynamic_title {
                            processor.ctx.window().set_title(&title);
                        }
                    },
                    TerminalEvent::ResetTitle => {
                        let ui_config = &processor.ctx.config.ui_config;
                        if ui_config.window.dynamic_title {
                            processor.ctx.display.window.set_title(&ui_config.window.title);
                        }
                    },
                    TerminalEvent::Wakeup => *processor.ctx.dirty = true,
                    TerminalEvent::Bell => {
                        // Set window urgency.
                        if processor.ctx.terminal.mode().contains(TermMode::URGENCY_HINTS) {
                            let focused = processor.ctx.terminal.is_focused;
                            processor.ctx.window().set_urgent(!focused);
                        }

                        // Ring visual bell.
                        processor.ctx.display.visual_bell.ring();

                        // Execute bell command.
                        if let Some(bell_command) = &processor.ctx.config.ui_config.bell.command {
                            start_daemon(bell_command.program(), bell_command.args());
                        }
                    },
                    TerminalEvent::ClipboardStore(clipboard_type, content) => {
                        processor.ctx.clipboard.store(clipboard_type, content);
                    },
                    TerminalEvent::ClipboardLoad(clipboard_type, format) => {
                        let text = format(processor.ctx.clipboard.load(clipboard_type).as_str());
                        processor.ctx.write_to_pty(text.into_bytes());
                    },
                    TerminalEvent::ColorRequest(index, format) => {
                        let text = format(processor.ctx.display.colors[index]);
                        processor.ctx.write_to_pty(text.into_bytes());
                    },
                    TerminalEvent::PtyWrite(text) => processor.ctx.write_to_pty(text.into_bytes()),
                    TerminalEvent::MouseCursorDirty => processor.reset_mouse_cursor(),
                    TerminalEvent::Exit => (),
                    TerminalEvent::CursorBlinkingChange(_) => {
                        processor.ctx.update_cursor_blinking();
                    },
                },
            },
            GlutinEvent::RedrawRequested(_) => *processor.ctx.dirty = true,
            GlutinEvent::WindowEvent { event, window_id, .. } => {
                match event {
                    WindowEvent::CloseRequested => processor.ctx.terminal.exit(),
                    WindowEvent::Resized(size) => {
                        // Minimizing the window sends a Resize event with zero width and
                        // height. But there's no need to ever actually resize to this.
                        // ConPTY has issues when resizing down to zero size and back.
                        #[cfg(windows)]
                        if size.width == 0 && size.height == 0 {
                            return;
                        }

                        processor.ctx.display_update_pending.set_dimensions(size);
                        *processor.ctx.dirty = true;
                    },
                    WindowEvent::KeyboardInput { input, is_synthetic: false, .. } => {
                        processor.key_input(input);
                    },
                    WindowEvent::ModifiersChanged(modifiers) => {
                        processor.modifiers_input(modifiers)
                    },
                    WindowEvent::ReceivedCharacter(c) => processor.received_char(c),
                    WindowEvent::MouseInput { state, button, .. } => {
                        processor.ctx.window().set_mouse_visible(true);
                        processor.mouse_input(state, button);
                        *processor.ctx.dirty = true;
                    },
                    WindowEvent::CursorMoved { position, .. } => {
                        processor.ctx.window().set_mouse_visible(true);
                        processor.mouse_moved(position);
                    },
                    WindowEvent::MouseWheel { delta, phase, .. } => {
                        processor.ctx.window().set_mouse_visible(true);
                        processor.mouse_wheel_input(delta, phase);
                    },
                    WindowEvent::Focused(is_focused) => {
                        if window_id == processor.ctx.window().window_id() {
                            processor.ctx.terminal.is_focused = is_focused;
                            *processor.ctx.dirty = true;

                            if is_focused {
                                processor.ctx.window().set_urgent(false);
                            } else {
                                processor.ctx.window().set_mouse_visible(true);
                            }

                            processor.ctx.update_cursor_blinking();
                            processor.on_focus_change(is_focused);
                        }
                    },
                    WindowEvent::DroppedFile(path) => {
                        let path: String = path.to_string_lossy().into();
                        processor.ctx.write_to_pty((path + " ").into_bytes());
                    },
                    WindowEvent::CursorLeft { .. } => {
                        processor.ctx.mouse.inside_text_area = false;

                        if processor.ctx.display().highlighted_hint.is_some() {
                            *processor.ctx.dirty = true;
                        }
                    },
                    WindowEvent::KeyboardInput { is_synthetic: true, .. }
                    | WindowEvent::TouchpadPressure { .. }
                    | WindowEvent::ScaleFactorChanged { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::AxisMotion { .. }
                    | WindowEvent::HoveredFileCancelled
                    | WindowEvent::Destroyed
                    | WindowEvent::ThemeChanged(_)
                    | WindowEvent::HoveredFile(_)
                    | WindowEvent::Touch(_)
                    | WindowEvent::Moved(_) => (),
                }
            },
            GlutinEvent::Suspended { .. }
            | GlutinEvent::NewEvents { .. }
            | GlutinEvent::DeviceEvent { .. }
            | GlutinEvent::MainEventsCleared
            | GlutinEvent::RedrawEventsCleared
            | GlutinEvent::Resumed
            | GlutinEvent::LoopDestroyed => (),
        }
    }

    /// Check if an event is irrelevant and can be skipped.
    fn skip_event(event: &GlutinEvent<'_, Event>) -> bool {
        match event {
            GlutinEvent::WindowEvent { event, .. } => matches!(
                event,
                WindowEvent::KeyboardInput { is_synthetic: true, .. }
                    | WindowEvent::TouchpadPressure { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::AxisMotion { .. }
                    | WindowEvent::HoveredFileCancelled
                    | WindowEvent::Destroyed
                    | WindowEvent::HoveredFile(_)
                    | WindowEvent::Touch(_)
                    | WindowEvent::Moved(_)
            ),
            GlutinEvent::Suspended { .. }
            | GlutinEvent::NewEvents { .. }
            | GlutinEvent::MainEventsCleared
            | GlutinEvent::LoopDestroyed => true,
            _ => false,
        }
    }

    /// Reload the configuration files from disk.
    fn reload_config<T>(path: &Path, processor: &mut input::Processor<T, ActionContext<'_, N, T>>)
    where
        T: EventListener,
    {
        if !processor.ctx.message_buffer.is_empty() {
            processor.ctx.message_buffer.remove_target(LOG_TARGET_CONFIG);
            processor.ctx.display_update_pending.dirty = true;
        }

        let config = match config::reload(path, processor.ctx.cli_options) {
            Ok(config) => config,
            Err(_) => return,
        };

        processor.ctx.display.update_config(&config);
        processor.ctx.terminal.update_config(&config);

        // Reload cursor if its thickness has changed.
        if (processor.ctx.config.cursor.thickness() - config.cursor.thickness()).abs()
            > f32::EPSILON
        {
            processor.ctx.display_update_pending.set_cursor_dirty();
        }

        if processor.ctx.config.ui_config.font != config.ui_config.font {
            // Do not update font size if it has been changed at runtime.
            if *processor.ctx.font_size == processor.ctx.config.ui_config.font.size() {
                *processor.ctx.font_size = config.ui_config.font.size();
            }

            let font = config.ui_config.font.clone().with_size(*processor.ctx.font_size);
            processor.ctx.display_update_pending.set_font(font);
        }

        // Update display if padding options were changed.
        let window_config = &processor.ctx.config.ui_config.window;
        if window_config.padding(1.) != config.ui_config.window.padding(1.)
            || window_config.dynamic_padding != config.ui_config.window.dynamic_padding
        {
            processor.ctx.display_update_pending.dirty = true;
        }

        // Live title reload.
        if !config.ui_config.window.dynamic_title
            || processor.ctx.config.ui_config.window.title != config.ui_config.window.title
        {
            processor.ctx.window().set_title(&config.ui_config.window.title);
        }

        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if processor.ctx.event_loop.is_wayland() {
            processor.ctx.window().set_wayland_theme(&config.ui_config.colors);
        }

        // Set subpixel anti-aliasing.
        #[cfg(target_os = "macos")]
        crossfont::set_font_smoothing(config.ui_config.font.use_thin_strokes);

        // Disable shadows for transparent windows on macOS.
        #[cfg(target_os = "macos")]
        processor.ctx.window().set_has_shadow(config.ui_config.background_opacity() >= 1.0);

        // Update hint keys.
        processor.ctx.display.hint_state.update_alphabet(config.ui_config.hints.alphabet());

        *processor.ctx.config = config;

        // Update cursor blinking.
        processor.ctx.update_cursor_blinking();

        *processor.ctx.dirty = true;
    }

    /// Submit the pending changes to the `Display`.
    fn submit_display_update<T>(
        &mut self,
        terminal: &mut Term<T>,
        old_is_searching: bool,
        display_update_pending: DisplayUpdate,
    ) where
        T: EventListener,
    {
        // Compute cursor positions before resize.
        let num_lines = terminal.screen_lines();
        let cursor_at_bottom = terminal.grid().cursor.point.line + 1 == num_lines;
        let origin_at_bottom = if terminal.mode().contains(TermMode::VI) {
            terminal.vi_mode_cursor.point.line == num_lines - 1
        } else {
            self.search_state.direction == Direction::Left
        };

        self.display.handle_update(
            terminal,
            &mut self.notifier,
            &self.message_buffer,
            self.search_state.history_index.is_some(),
            &self.config,
            display_update_pending,
        );

        let new_is_searching = self.search_state.history_index.is_some();
        if !old_is_searching && new_is_searching {
            // Scroll on search start to make sure origin is visible with minimal viewport motion.
            let display_offset = terminal.grid().display_offset();
            if display_offset == 0 && cursor_at_bottom && !origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(1));
            } else if display_offset != 0 && origin_at_bottom {
                terminal.scroll_display(Scroll::Delta(-1));
            }
        }
    }

    /// Write the ref test results to the disk.
    fn write_ref_test_results<T>(&self, terminal: &Term<T>) {
        // Dump grid state.
        let mut grid = terminal.grid().clone();
        grid.initialize_all();
        grid.truncate();

        let serialized_grid = json::to_string(&grid).expect("serialize grid");

        let serialized_size = json::to_string(&self.display.size_info).expect("serialize size");

        let serialized_config = format!("{{\"history_size\":{}}}", grid.history_size());

        File::create("./grid.json")
            .and_then(|mut f| f.write_all(serialized_grid.as_bytes()))
            .expect("write grid.json");

        File::create("./size.json")
            .and_then(|mut f| f.write_all(serialized_size.as_bytes()))
            .expect("write size.json");

        File::create("./config.json")
            .and_then(|mut f| f.write_all(serialized_config.as_bytes()))
            .expect("write config.json");
    }
}

#[derive(Debug, Clone)]
pub struct EventProxy(EventLoopProxy<Event>);

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<Event>) -> Self {
        EventProxy(proxy)
    }

    /// Send an event to the event loop.
    pub fn send_event(&self, event: Event) {
        let _ = self.0.send_event(event);
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TerminalEvent) {
        let _ = self.0.send_event(Event::Terminal(event));
    }
}
