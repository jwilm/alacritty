#![allow(clippy::enum_glob_use)]

use std::fmt::{self, Debug, Display};

use bitflags::bitflags;
use glutin::event::VirtualKeyCode::*;
use glutin::event::{ModifiersState, MouseButton, VirtualKeyCode};
use serde::de::Error as SerdeError;
use serde::de::{self, MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use serde_yaml::Value as SerdeValue;

use alacritty_config_derive::ConfigDeserialize;

use alacritty_terminal::config::Program;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;

use crate::config::ui_config::Hint;

/// Describes a state and action to take in that state.
///
/// This is the shared component of `MouseBinding` and `KeyBinding`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding<T> {
    /// Modifier keys required to activate binding.
    pub mods: ModifiersState,

    /// String to send to PTY if mods and mode match.
    pub action: Action,

    /// Binding mode required to activate binding.
    pub mode: BindingMode,

    /// Excluded binding modes where the binding won't be activated.
    pub notmode: BindingMode,

    /// This property is used as part of the trigger detection code.
    ///
    /// For example, this might be a key like "G", or a mouse button.
    pub trigger: T,
}

/// Bindings that are triggered by a keyboard key.
pub type KeyBinding = Binding<Key>;

/// Bindings that are triggered by a mouse button.
pub type MouseBinding = Binding<MouseButton>;

impl<T: Eq> Binding<T> {
    #[inline]
    pub fn is_triggered_by(&self, mode: BindingMode, mods: ModifiersState, input: &T) -> bool {
        // Check input first since bindings are stored in one big list. This is
        // the most likely item to fail so prioritizing it here allows more
        // checks to be short circuited.
        self.trigger == *input
            && self.mods == mods
            && mode.contains(self.mode)
            && !mode.intersects(self.notmode)
    }

    #[inline]
    pub fn triggers_match(&self, binding: &Binding<T>) -> bool {
        // Check the binding's key and modifiers.
        if self.trigger != binding.trigger || self.mods != binding.mods {
            return false;
        }

        let selfmode = if self.mode.is_empty() { BindingMode::all() } else { self.mode };
        let bindingmode = if binding.mode.is_empty() { BindingMode::all() } else { binding.mode };

        if !selfmode.intersects(bindingmode) {
            return false;
        }

        // The bindings are never active at the same time when the required modes of one binding
        // are part of the forbidden bindings of the other.
        if self.mode.intersects(binding.notmode) || binding.mode.intersects(self.notmode) {
            return false;
        }

        true
    }
}

#[derive(ConfigDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write an escape sequence.
    #[config(skip)]
    Esc(String),

    /// Run given command.
    #[config(skip)]
    Command(Program),

    /// Regex keyboard hints.
    #[config(skip)]
    Hint(Hint),

    /// Move vi mode cursor.
    #[config(skip)]
    ViMotion(ViMotion),

    /// Perform vi mode action.
    #[config(skip)]
    Vi(ViAction),

    /// Perform search mode action.
    #[config(skip)]
    Search(SearchAction),

    /// Perform mouse binding exclusive action.
    #[config(skip)]
    Mouse(MouseAction),

    /// Paste contents of system clipboard.
    Paste,

    /// Store current selection into clipboard.
    Copy,

    #[cfg(not(any(target_os = "macos", windows)))]
    /// Store current selection into selection buffer.
    CopySelection,

    /// Paste contents of selection buffer.
    PasteSelection,

    /// Increase font size.
    IncreaseFontSize,

    /// Decrease font size.
    DecreaseFontSize,

    /// Reset font size to the config value.
    ResetFontSize,

    /// Scroll exactly one page up.
    ScrollPageUp,

    /// Scroll exactly one page down.
    ScrollPageDown,

    /// Scroll half a page up.
    ScrollHalfPageUp,

    /// Scroll half a page down.
    ScrollHalfPageDown,

    /// Scroll one line up.
    ScrollLineUp,

    /// Scroll one line down.
    ScrollLineDown,

    /// Scroll all the way to the top.
    ScrollToTop,

    /// Scroll all the way to the bottom.
    ScrollToBottom,

    /// Clear the display buffer(s) to remove history.
    ClearHistory,

    /// Hide the Alacritty window.
    Hide,

    /// Hide all windows other than Alacritty on macOS.
    #[cfg(target_os = "macos")]
    HideOtherApplications,

    /// Minimize the Alacritty window.
    Minimize,

    /// Quit Alacritty.
    Quit,

    /// Clear warning and error notices.
    ClearLogNotice,

    /// Spawn a new instance of Alacritty.
    SpawnNewInstance,

    /// Toggle fullscreen.
    ToggleFullscreen,

    /// Toggle simple fullscreen on macOS.
    #[cfg(target_os = "macos")]
    ToggleSimpleFullscreen,

    /// Clear active selection.
    ClearSelection,

    /// Toggle vi mode.
    ToggleViMode,

    /// Allow receiving char input.
    ReceiveChar,

    /// Start a forward buffer search.
    SearchForward,

    /// Start a backward buffer search.
    SearchBackward,

    /// No action.
    None,
}

impl From<&'static str> for Action {
    fn from(s: &'static str) -> Action {
        Action::Esc(s.into())
    }
}

impl From<ViAction> for Action {
    fn from(action: ViAction) -> Self {
        Self::Vi(action)
    }
}

impl From<ViMotion> for Action {
    fn from(motion: ViMotion) -> Self {
        Self::ViMotion(motion)
    }
}

impl From<SearchAction> for Action {
    fn from(action: SearchAction) -> Self {
        Self::Search(action)
    }
}

impl From<MouseAction> for Action {
    fn from(action: MouseAction) -> Self {
        Self::Mouse(action)
    }
}

/// Display trait used for error logging.
impl Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::ViMotion(motion) => motion.fmt(f),
            Action::Vi(action) => action.fmt(f),
            Action::Mouse(action) => action.fmt(f),
            _ => write!(f, "{:?}", self),
        }
    }
}

/// Vi mode specific actions.
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum ViAction {
    /// Toggle normal vi selection.
    ToggleNormalSelection,
    /// Toggle line vi selection.
    ToggleLineSelection,
    /// Toggle block vi selection.
    ToggleBlockSelection,
    /// Toggle semantic vi selection.
    ToggleSemanticSelection,
    /// Jump to the beginning of the next match.
    SearchNext,
    /// Jump to the beginning of the previous match.
    SearchPrevious,
    /// Jump to the next start of a match to the left of the origin.
    SearchStart,
    /// Jump to the next end of a match to the right of the origin.
    SearchEnd,
    /// Launch the URL below the vi mode cursor.
    Open,
}

/// Search mode specific actions.
#[allow(clippy::enum_variant_names)]
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum SearchAction {
    /// Move the focus to the next search match.
    SearchFocusNext,
    /// Move the focus to the previous search match.
    SearchFocusPrevious,
    /// Confirm the active search.
    SearchConfirm,
    /// Cancel the active search.
    SearchCancel,
    /// Reset the search regex.
    SearchClear,
    /// Delete the last word in the search regex.
    SearchDeleteWord,
    /// Go to the previous regex in the search history.
    SearchHistoryPrevious,
    /// Go to the next regex in the search history.
    SearchHistoryNext,
}

/// Mouse binding specific actions.
#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum MouseAction {
    /// Expand the selection to the current mouse cursor position.
    ExpandSelection,
}

macro_rules! bindings {
    (
        KeyBinding;
        $(
            $key:ident
            $(,$mods:expr)*
            $(,+$mode:expr)*
            $(,~$notmode:expr)*
            ;$action:expr
        );*
        $(;)*
    ) => {{
        bindings!(
            KeyBinding;
            $(
                Key::Keycode($key)
                $(,$mods)*
                $(,+$mode)*
                $(,~$notmode)*
                ;$action
            );*
        )
    }};
    (
        $ty:ident;
        $(
            $key:expr
            $(,$mods:expr)*
            $(,+$mode:expr)*
            $(,~$notmode:expr)*
            ;$action:expr
        );*
        $(;)*
    ) => {{
        let mut v = Vec::new();

        $(
            let mut _mods = ModifiersState::empty();
            $(_mods = $mods;)*
            let mut _mode = BindingMode::empty();
            $(_mode.insert($mode);)*
            let mut _notmode = BindingMode::empty();
            $(_notmode.insert($notmode);)*

            v.push($ty {
                trigger: $key,
                mods: _mods,
                mode: _mode,
                notmode: _notmode,
                action: $action.into(),
            });
        )*

        v
    }};
}

pub fn default_mouse_bindings() -> Vec<MouseBinding> {
    bindings!(
        MouseBinding;
        MouseButton::Right;                    MouseAction::ExpandSelection;
        MouseButton::Middle, ~BindingMode::VI; Action::PasteSelection;
    )
}

pub fn default_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = bindings!(
        KeyBinding;
        Copy;  Action::Copy;
        Copy,  +BindingMode::VI; Action::ClearSelection;
        Paste, ~BindingMode::VI; Action::Paste;
        L, ModifiersState::CTRL; Action::ClearLogNotice;
        L,    ModifiersState::CTRL,  ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x0c".into());
        Tab,  ModifiersState::SHIFT, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[Z".into());
        Back, ModifiersState::ALT,   ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b\x7f".into());
        Back, ModifiersState::SHIFT, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x7f".into());
        Home,     ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollToTop;
        End,      ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollToBottom;
        PageUp,   ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollPageUp;
        PageDown, ModifiersState::SHIFT, ~BindingMode::ALT_SCREEN; Action::ScrollPageDown;
        Home,     ModifiersState::SHIFT, +BindingMode::ALT_SCREEN,
            ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[1;2H".into());
        End,      ModifiersState::SHIFT, +BindingMode::ALT_SCREEN,
            ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[1;2F".into());
        PageUp,   ModifiersState::SHIFT, +BindingMode::ALT_SCREEN,
            ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[5;2~".into());
        PageDown, ModifiersState::SHIFT, +BindingMode::ALT_SCREEN,
            ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[6;2~".into());
        Home,  +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOH".into());
        Home,  ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[H".into());
        End,   +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOF".into());
        End,   ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[F".into());
        Up,    +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOA".into());
        Up,    ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[A".into());
        Down,  +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOB".into());
        Down,  ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[B".into());
        Right, +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOC".into());
        Right, ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[C".into());
        Left,  +BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1bOD".into());
        Left,  ~BindingMode::APP_CURSOR, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[D".into());
        Back,        ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x7f".into());
        Insert,      ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[2~".into());
        Delete,      ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[3~".into());
        PageUp,      ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[5~".into());
        PageDown,    ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[6~".into());
        F1,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOP".into());
        F2,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOQ".into());
        F3,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOR".into());
        F4,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1bOS".into());
        F5,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[15~".into());
        F6,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[17~".into());
        F7,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[18~".into());
        F8,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[19~".into());
        F9,          ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[20~".into());
        F10,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[21~".into());
        F11,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[23~".into());
        F12,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[24~".into());
        F13,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[25~".into());
        F14,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[26~".into());
        F15,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[28~".into());
        F16,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[29~".into());
        F17,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[31~".into());
        F18,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[32~".into());
        F19,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[33~".into());
        F20,         ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\x1b[34~".into());
        NumpadEnter, ~BindingMode::VI, ~BindingMode::SEARCH; Action::Esc("\n".into());
        Space, ModifiersState::SHIFT | ModifiersState::CTRL, ~BindingMode::SEARCH;
            Action::ToggleViMode;
        Space, ModifiersState::SHIFT | ModifiersState::CTRL, +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollToBottom;
        Escape,                        +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ClearSelection;
        I,                             +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ToggleViMode;
        I,                             +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollToBottom;
        C,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ToggleViMode;
        Y,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollLineUp;
        E,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollLineDown;
        G,                             +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollToTop;
        G,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollToBottom;
        B,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollPageUp;
        F,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollPageDown;
        U,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollHalfPageUp;
        D,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ScrollHalfPageDown;
        Y,                             +BindingMode::VI, ~BindingMode::SEARCH; Action::Copy;
        Y,                             +BindingMode::VI, ~BindingMode::SEARCH;
            Action::ClearSelection;
        Slash,                         +BindingMode::VI, ~BindingMode::SEARCH;
            Action::SearchForward;
        Slash,  ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            Action::SearchBackward;
        V,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::ToggleNormalSelection;
        V,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::ToggleLineSelection;
        V,      ModifiersState::CTRL,  +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::ToggleBlockSelection;
        V,      ModifiersState::ALT,   +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::ToggleSemanticSelection;
        N,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::SearchNext;
        N,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::SearchPrevious;
        Return,                        +BindingMode::VI, ~BindingMode::SEARCH;
            ViAction::Open;
        K,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Up;
        J,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Down;
        H,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Left;
        L,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Right;
        Up,                            +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Up;
        Down,                          +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Down;
        Left,                          +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Left;
        Right,                         +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Right;
        Key0,                          +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::First;
        Key4,   ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Last;
        Key6,   ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::FirstOccupied;
        H,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::High;
        M,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Middle;
        L,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Low;
        B,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::SemanticLeft;
        W,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::SemanticRight;
        E,                             +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::SemanticRightEnd;
        B,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::WordLeft;
        W,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::WordRight;
        E,      ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::WordRightEnd;
        Key5,   ModifiersState::SHIFT, +BindingMode::VI, ~BindingMode::SEARCH;
            ViMotion::Bracket;
        Return,                        +BindingMode::SEARCH, +BindingMode::VI;
            SearchAction::SearchConfirm;
        Escape,                        +BindingMode::SEARCH; SearchAction::SearchCancel;
        C,      ModifiersState::CTRL,  +BindingMode::SEARCH; SearchAction::SearchCancel;
        U,      ModifiersState::CTRL,  +BindingMode::SEARCH; SearchAction::SearchClear;
        W,      ModifiersState::CTRL,  +BindingMode::SEARCH; SearchAction::SearchDeleteWord;
        P,      ModifiersState::CTRL,  +BindingMode::SEARCH; SearchAction::SearchHistoryPrevious;
        N,      ModifiersState::CTRL,  +BindingMode::SEARCH; SearchAction::SearchHistoryNext;
        Up,                            +BindingMode::SEARCH; SearchAction::SearchHistoryPrevious;
        Down,                          +BindingMode::SEARCH; SearchAction::SearchHistoryNext;
        Return,                        +BindingMode::SEARCH, ~BindingMode::VI;
            SearchAction::SearchFocusNext;
        Return, ModifiersState::SHIFT, +BindingMode::SEARCH, ~BindingMode::VI;
            SearchAction::SearchFocusPrevious;
    );

    //   Code     Modifiers
    // ---------+---------------------------
    //    2     | Shift
    //    3     | Alt
    //    4     | Shift + Alt
    //    5     | Control
    //    6     | Shift + Control
    //    7     | Alt + Control
    //    8     | Shift + Alt + Control
    // ---------+---------------------------
    //
    // from: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-PC-Style-Function-Keys
    let mut modifiers = vec![
        ModifiersState::SHIFT,
        ModifiersState::ALT,
        ModifiersState::SHIFT | ModifiersState::ALT,
        ModifiersState::CTRL,
        ModifiersState::SHIFT | ModifiersState::CTRL,
        ModifiersState::ALT | ModifiersState::CTRL,
        ModifiersState::SHIFT | ModifiersState::ALT | ModifiersState::CTRL,
    ];

    for (index, mods) in modifiers.drain(..).enumerate() {
        let modifiers_code = index + 2;
        bindings.extend(bindings!(
            KeyBinding;
            Delete, mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[3;{}~", modifiers_code));
            Up,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}A", modifiers_code));
            Down,   mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}B", modifiers_code));
            Right,  mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}C", modifiers_code));
            Left,   mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}D", modifiers_code));
            F1,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}P", modifiers_code));
            F2,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}Q", modifiers_code));
            F3,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}R", modifiers_code));
            F4,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[1;{}S", modifiers_code));
            F5,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[15;{}~", modifiers_code));
            F6,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[17;{}~", modifiers_code));
            F7,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[18;{}~", modifiers_code));
            F8,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[19;{}~", modifiers_code));
            F9,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[20;{}~", modifiers_code));
            F10,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[21;{}~", modifiers_code));
            F11,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[23;{}~", modifiers_code));
            F12,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[24;{}~", modifiers_code));
            F13,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[25;{}~", modifiers_code));
            F14,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[26;{}~", modifiers_code));
            F15,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[28;{}~", modifiers_code));
            F16,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[29;{}~", modifiers_code));
            F17,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[31;{}~", modifiers_code));
            F18,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[32;{}~", modifiers_code));
            F19,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[33;{}~", modifiers_code));
            F20,    mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                Action::Esc(format!("\x1b[34;{}~", modifiers_code));
        ));

        // We're adding the following bindings with `Shift` manually above, so skipping them here.
        if modifiers_code != 2 {
            bindings.extend(bindings!(
                KeyBinding;
                Insert,   mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                    Action::Esc(format!("\x1b[2;{}~", modifiers_code));
                PageUp,   mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                    Action::Esc(format!("\x1b[5;{}~", modifiers_code));
                PageDown, mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                    Action::Esc(format!("\x1b[6;{}~", modifiers_code));
                End,      mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                    Action::Esc(format!("\x1b[1;{}F", modifiers_code));
                Home,     mods, ~BindingMode::VI, ~BindingMode::SEARCH;
                    Action::Esc(format!("\x1b[1;{}H", modifiers_code));
            ));
        }
    }

    bindings.extend(platform_key_bindings());

    bindings
}

#[cfg(not(any(target_os = "macos", test)))]
fn common_keybindings() -> Vec<KeyBinding> {
    bindings!(
        KeyBinding;
        V,        ModifiersState::CTRL | ModifiersState::SHIFT, ~BindingMode::VI; Action::Paste;
        C,        ModifiersState::CTRL | ModifiersState::SHIFT; Action::Copy;
        F,        ModifiersState::CTRL | ModifiersState::SHIFT, ~BindingMode::SEARCH;
            Action::SearchForward;
        B,        ModifiersState::CTRL | ModifiersState::SHIFT, ~BindingMode::SEARCH;
            Action::SearchBackward;
        C,        ModifiersState::CTRL | ModifiersState::SHIFT,
            +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        Insert,   ModifiersState::SHIFT, ~BindingMode::VI; Action::PasteSelection;
        Key0,     ModifiersState::CTRL;  Action::ResetFontSize;
        Equals,   ModifiersState::CTRL;  Action::IncreaseFontSize;
        Plus,     ModifiersState::CTRL;  Action::IncreaseFontSize;
        NumpadAdd,      ModifiersState::CTRL;  Action::IncreaseFontSize;
        Minus,          ModifiersState::CTRL;  Action::DecreaseFontSize;
        NumpadSubtract, ModifiersState::CTRL;  Action::DecreaseFontSize;
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows", test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    common_keybindings()
}

#[cfg(all(target_os = "windows", not(test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = bindings!(
        KeyBinding;
        Return, ModifiersState::ALT; Action::ToggleFullscreen;
    );
    bindings.extend(common_keybindings());
    bindings
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    bindings!(
        KeyBinding;
        Key0,           ModifiersState::LOGO; Action::ResetFontSize;
        Equals,         ModifiersState::LOGO; Action::IncreaseFontSize;
        Plus,           ModifiersState::LOGO; Action::IncreaseFontSize;
        NumpadAdd,      ModifiersState::LOGO; Action::IncreaseFontSize;
        Minus,          ModifiersState::LOGO; Action::DecreaseFontSize;
        NumpadSubtract, ModifiersState::LOGO; Action::DecreaseFontSize;
        Insert, ModifiersState::SHIFT, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x1b[2;2~".into());
        K, ModifiersState::LOGO, ~BindingMode::VI, ~BindingMode::SEARCH;
            Action::Esc("\x0c".into());
        K, ModifiersState::LOGO, ~BindingMode::VI, ~BindingMode::SEARCH;  Action::ClearHistory;
        V, ModifiersState::LOGO, ~BindingMode::VI; Action::Paste;
        N, ModifiersState::LOGO; Action::SpawnNewInstance;
        F, ModifiersState::CTRL | ModifiersState::LOGO; Action::ToggleFullscreen;
        C, ModifiersState::LOGO; Action::Copy;
        C, ModifiersState::LOGO, +BindingMode::VI, ~BindingMode::SEARCH; Action::ClearSelection;
        H, ModifiersState::LOGO; Action::Hide;
        H, ModifiersState::LOGO | ModifiersState::ALT; Action::HideOtherApplications;
        M, ModifiersState::LOGO; Action::Minimize;
        Q, ModifiersState::LOGO; Action::Quit;
        W, ModifiersState::LOGO; Action::Quit;
        F, ModifiersState::LOGO, ~BindingMode::SEARCH; Action::SearchForward;
        B, ModifiersState::LOGO, ~BindingMode::SEARCH; Action::SearchBackward;
    )
}

// Don't return any bindings for tests since they are commented-out by default.
#[cfg(test)]
pub fn platform_key_bindings() -> Vec<KeyBinding> {
    vec![]
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Key {
    Scancode(u32),
    Keycode(VirtualKeyCode),
}

impl<'a> Deserialize<'a> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let value = SerdeValue::deserialize(deserializer)?;
        match u32::deserialize(value.clone()) {
            Ok(scancode) => Ok(Key::Scancode(scancode)),
            Err(_) => {
                let keycode = VirtualKeyCode::deserialize(value).map_err(D::Error::custom)?;
                Ok(Key::Keycode(keycode))
            },
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ModeWrapper {
    pub mode: BindingMode,
    pub not_mode: BindingMode,
}

bitflags! {
    /// Modes available for key bindings.
    pub struct BindingMode: u8 {
        const APP_CURSOR          = 0b0000_0001;
        const APP_KEYPAD          = 0b0000_0010;
        const ALT_SCREEN          = 0b0000_0100;
        const VI                  = 0b0000_1000;
        const SEARCH              = 0b0001_0000;
    }
}

impl BindingMode {
    pub fn new(mode: &TermMode, search: bool) -> BindingMode {
        let mut binding_mode = BindingMode::empty();
        binding_mode.set(BindingMode::APP_CURSOR, mode.contains(TermMode::APP_CURSOR));
        binding_mode.set(BindingMode::APP_KEYPAD, mode.contains(TermMode::APP_KEYPAD));
        binding_mode.set(BindingMode::ALT_SCREEN, mode.contains(TermMode::ALT_SCREEN));
        binding_mode.set(BindingMode::VI, mode.contains(TermMode::VI));
        binding_mode.set(BindingMode::SEARCH, search);
        binding_mode
    }
}

impl Default for ModeWrapper {
    fn default() -> Self {
        Self { mode: BindingMode::empty(), not_mode: BindingMode::empty() }
    }
}

impl<'a> Deserialize<'a> for ModeWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        struct ModeVisitor;

        impl<'a> Visitor<'a> for ModeVisitor {
            type Value = ModeWrapper;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a combination of AppCursor | AppKeypad | Alt | Vi, possibly with negation (~)",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<ModeWrapper, E>
            where
                E: de::Error,
            {
                let mut res =
                    ModeWrapper { mode: BindingMode::empty(), not_mode: BindingMode::empty() };

                for modifier in value.split('|') {
                    match modifier.trim().to_lowercase().as_str() {
                        "appcursor" => res.mode |= BindingMode::APP_CURSOR,
                        "~appcursor" => res.not_mode |= BindingMode::APP_CURSOR,
                        "appkeypad" => res.mode |= BindingMode::APP_KEYPAD,
                        "~appkeypad" => res.not_mode |= BindingMode::APP_KEYPAD,
                        "alt" => res.mode |= BindingMode::ALT_SCREEN,
                        "~alt" => res.not_mode |= BindingMode::ALT_SCREEN,
                        "vi" => res.mode |= BindingMode::VI,
                        "~vi" => res.not_mode |= BindingMode::VI,
                        "search" => res.mode |= BindingMode::SEARCH,
                        "~search" => res.not_mode |= BindingMode::SEARCH,
                        _ => return Err(E::invalid_value(Unexpected::Str(modifier), &self)),
                    }
                }

                Ok(res)
            }
        }
        deserializer.deserialize_str(ModeVisitor)
    }
}

struct MouseButtonWrapper(MouseButton);

impl MouseButtonWrapper {
    fn into_inner(self) -> MouseButton {
        self.0
    }
}

impl<'a> Deserialize<'a> for MouseButtonWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        struct MouseButtonVisitor;

        impl<'a> Visitor<'a> for MouseButtonVisitor {
            type Value = MouseButtonWrapper;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("Left, Right, Middle, or a number from 0 to 65536")
            }

            fn visit_u64<E>(self, value: u64) -> Result<MouseButtonWrapper, E>
            where
                E: de::Error,
            {
                match value {
                    0..=65536 => Ok(MouseButtonWrapper(MouseButton::Other(value as u16))),
                    _ => Err(E::invalid_value(Unexpected::Unsigned(value), &self)),
                }
            }

            fn visit_str<E>(self, value: &str) -> Result<MouseButtonWrapper, E>
            where
                E: de::Error,
            {
                match value {
                    "Left" => Ok(MouseButtonWrapper(MouseButton::Left)),
                    "Right" => Ok(MouseButtonWrapper(MouseButton::Right)),
                    "Middle" => Ok(MouseButtonWrapper(MouseButton::Middle)),
                    _ => Err(E::invalid_value(Unexpected::Str(value), &self)),
                }
            }
        }

        deserializer.deserialize_any(MouseButtonVisitor)
    }
}

/// Bindings are deserialized into a `RawBinding` before being parsed as a
/// `KeyBinding` or `MouseBinding`.
#[derive(PartialEq, Eq)]
struct RawBinding {
    key: Option<Key>,
    mouse: Option<MouseButton>,
    mods: ModifiersState,
    mode: BindingMode,
    notmode: BindingMode,
    action: Action,
}

impl RawBinding {
    fn into_mouse_binding(self) -> Result<MouseBinding, Self> {
        if let Some(mouse) = self.mouse {
            Ok(Binding {
                trigger: mouse,
                mods: self.mods,
                action: self.action,
                mode: self.mode,
                notmode: self.notmode,
            })
        } else {
            Err(self)
        }
    }

    fn into_key_binding(self) -> Result<KeyBinding, Self> {
        if let Some(key) = self.key {
            Ok(KeyBinding {
                trigger: key,
                mods: self.mods,
                action: self.action,
                mode: self.mode,
                notmode: self.notmode,
            })
        } else {
            Err(self)
        }
    }
}

impl<'a> Deserialize<'a> for RawBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        const FIELDS: &[&str] = &["key", "mods", "mode", "action", "chars", "mouse", "command"];

        enum Field {
            Key,
            Mods,
            Mode,
            Action,
            Chars,
            Mouse,
            Command,
        }

        impl<'a> Deserialize<'a> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Field, D::Error>
            where
                D: Deserializer<'a>,
            {
                struct FieldVisitor;

                impl<'a> Visitor<'a> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("binding fields")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Field, E>
                    where
                        E: de::Error,
                    {
                        match value.to_ascii_lowercase().as_str() {
                            "key" => Ok(Field::Key),
                            "mods" => Ok(Field::Mods),
                            "mode" => Ok(Field::Mode),
                            "action" => Ok(Field::Action),
                            "chars" => Ok(Field::Chars),
                            "mouse" => Ok(Field::Mouse),
                            "command" => Ok(Field::Command),
                            _ => Err(E::unknown_field(value, FIELDS)),
                        }
                    }
                }

                deserializer.deserialize_str(FieldVisitor)
            }
        }

        struct RawBindingVisitor;
        impl<'a> Visitor<'a> for RawBindingVisitor {
            type Value = RawBinding;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("binding specification")
            }

            fn visit_map<V>(self, mut map: V) -> Result<RawBinding, V::Error>
            where
                V: MapAccess<'a>,
            {
                let mut mods: Option<ModifiersState> = None;
                let mut key: Option<Key> = None;
                let mut chars: Option<String> = None;
                let mut action: Option<Action> = None;
                let mut mode: Option<BindingMode> = None;
                let mut not_mode: Option<BindingMode> = None;
                let mut mouse: Option<MouseButton> = None;
                let mut command: Option<Program> = None;

                use de::Error;

                while let Some(struct_key) = map.next_key::<Field>()? {
                    match struct_key {
                        Field::Key => {
                            if key.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("key"));
                            }

                            let val = map.next_value::<SerdeValue>()?;
                            if val.is_u64() {
                                let scancode = val.as_u64().unwrap();
                                if scancode > u64::from(std::u32::MAX) {
                                    return Err(<V::Error as Error>::custom(format!(
                                        "Invalid key binding, scancode too big: {}",
                                        scancode
                                    )));
                                }
                                key = Some(Key::Scancode(scancode as u32));
                            } else {
                                let k = Key::deserialize(val).map_err(V::Error::custom)?;
                                key = Some(k);
                            }
                        },
                        Field::Mods => {
                            if mods.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mods"));
                            }

                            mods = Some(map.next_value::<ModsWrapper>()?.into_inner());
                        },
                        Field::Mode => {
                            if mode.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mode"));
                            }

                            let mode_deserializer = map.next_value::<ModeWrapper>()?;
                            mode = Some(mode_deserializer.mode);
                            not_mode = Some(mode_deserializer.not_mode);
                        },
                        Field::Action => {
                            if action.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("action"));
                            }

                            let value = map.next_value::<SerdeValue>()?;

                            action = if let Ok(vi_action) = ViAction::deserialize(value.clone()) {
                                Some(vi_action.into())
                            } else if let Ok(vi_motion) = ViMotion::deserialize(value.clone()) {
                                Some(vi_motion.into())
                            } else if let Ok(search_action) =
                                SearchAction::deserialize(value.clone())
                            {
                                Some(search_action.into())
                            } else if let Ok(mouse_action) = MouseAction::deserialize(value.clone())
                            {
                                Some(mouse_action.into())
                            } else {
                                match Action::deserialize(value.clone()).map_err(V::Error::custom) {
                                    Ok(action) => Some(action),
                                    Err(err) => {
                                        let value = match value {
                                            SerdeValue::String(string) => string,
                                            SerdeValue::Mapping(map) if map.len() == 1 => {
                                                match map.into_iter().next() {
                                                    Some((
                                                        SerdeValue::String(string),
                                                        SerdeValue::Null,
                                                    )) => string,
                                                    _ => return Err(err),
                                                }
                                            },
                                            _ => return Err(err),
                                        };
                                        return Err(V::Error::custom(format!(
                                            "unknown keyboard action `{}`",
                                            value
                                        )));
                                    },
                                }
                            };
                        },
                        Field::Chars => {
                            if chars.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("chars"));
                            }

                            chars = Some(map.next_value()?);
                        },
                        Field::Mouse => {
                            if chars.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("mouse"));
                            }

                            mouse = Some(map.next_value::<MouseButtonWrapper>()?.into_inner());
                        },
                        Field::Command => {
                            if command.is_some() {
                                return Err(<V::Error as Error>::duplicate_field("command"));
                            }

                            command = Some(map.next_value::<Program>()?);
                        },
                    }
                }

                let mode = mode.unwrap_or_else(BindingMode::empty);
                let not_mode = not_mode.unwrap_or_else(BindingMode::empty);
                let mods = mods.unwrap_or_else(ModifiersState::default);

                let action = match (action, chars, command) {
                    (Some(action @ Action::ViMotion(_)), None, None)
                    | (Some(action @ Action::Vi(_)), None, None) => {
                        if !mode.intersects(BindingMode::VI) || not_mode.intersects(BindingMode::VI)
                        {
                            return Err(V::Error::custom(format!(
                                "action `{}` is only available in vi mode, try adding `mode: Vi`",
                                action,
                            )));
                        }
                        action
                    },
                    (Some(action @ Action::Search(_)), None, None) => {
                        if !mode.intersects(BindingMode::SEARCH) {
                            return Err(V::Error::custom(format!(
                                "action `{}` is only available in search mode, try adding `mode: \
                                 Search`",
                                action,
                            )));
                        }
                        action
                    },
                    (Some(action @ Action::Mouse(_)), None, None) => {
                        if mouse.is_none() {
                            return Err(V::Error::custom(format!(
                                "action `{}` is only available for mouse bindings",
                                action,
                            )));
                        }
                        action
                    },
                    (Some(action), None, None) => action,
                    (None, Some(chars), None) => Action::Esc(chars),
                    (None, None, Some(cmd)) => Action::Command(cmd),
                    _ => {
                        return Err(V::Error::custom(
                            "must specify exactly one of chars, action or command",
                        ))
                    },
                };

                if mouse.is_none() && key.is_none() {
                    return Err(V::Error::custom("bindings require mouse button or key"));
                }

                Ok(RawBinding { mode, notmode: not_mode, action, key, mouse, mods })
            }
        }

        deserializer.deserialize_struct("RawBinding", FIELDS, RawBindingVisitor)
    }
}

impl<'a> Deserialize<'a> for MouseBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let raw = RawBinding::deserialize(deserializer)?;
        raw.into_mouse_binding()
            .map_err(|_| D::Error::custom("expected mouse binding, got key binding"))
    }
}

impl<'a> Deserialize<'a> for KeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        let raw = RawBinding::deserialize(deserializer)?;
        raw.into_key_binding()
            .map_err(|_| D::Error::custom("expected key binding, got mouse binding"))
    }
}

/// Newtype for implementing deserialize on glutin Mods.
///
/// Our deserialize impl wouldn't be covered by a derive(Deserialize); see the
/// impl below.
#[derive(Debug, Copy, Clone, Hash, Default, Eq, PartialEq)]
pub struct ModsWrapper(pub ModifiersState);

impl ModsWrapper {
    pub fn into_inner(self) -> ModifiersState {
        self.0
    }
}

impl<'a> de::Deserialize<'a> for ModsWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'a>,
    {
        struct ModsVisitor;

        impl<'a> Visitor<'a> for ModsVisitor {
            type Value = ModsWrapper;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("None or a subset of Shift|Control|Super|Command|Alt|Option")
            }

            fn visit_str<E>(self, value: &str) -> Result<ModsWrapper, E>
            where
                E: de::Error,
            {
                let mut res = ModifiersState::empty();
                for modifier in value.split('|') {
                    match modifier.trim().to_lowercase().as_str() {
                        "command" | "super" => res.insert(ModifiersState::LOGO),
                        "shift" => res.insert(ModifiersState::SHIFT),
                        "alt" | "option" => res.insert(ModifiersState::ALT),
                        "control" => res.insert(ModifiersState::CTRL),
                        "none" => (),
                        _ => return Err(E::invalid_value(Unexpected::Str(modifier), &self)),
                    }
                }

                Ok(ModsWrapper(res))
            }
        }

        deserializer.deserialize_str(ModsVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use glutin::event::ModifiersState;

    type MockBinding = Binding<usize>;

    impl Default for MockBinding {
        fn default() -> Self {
            Self {
                mods: Default::default(),
                action: Action::None,
                mode: BindingMode::empty(),
                notmode: BindingMode::empty(),
                trigger: Default::default(),
            }
        }
    }

    #[test]
    fn binding_matches_itself() {
        let binding = MockBinding::default();
        let identical_binding = MockBinding::default();

        assert!(binding.triggers_match(&identical_binding));
        assert!(identical_binding.triggers_match(&binding));
    }

    #[test]
    fn binding_matches_different_action() {
        let binding = MockBinding::default();
        let different_action =
            MockBinding { action: Action::ClearHistory, ..MockBinding::default() };

        assert!(binding.triggers_match(&different_action));
        assert!(different_action.triggers_match(&binding));
    }

    #[test]
    fn mods_binding_requires_strict_match() {
        let superset_mods = MockBinding { mods: ModifiersState::all(), ..MockBinding::default() };
        let subset_mods = MockBinding { mods: ModifiersState::ALT, ..MockBinding::default() };

        assert!(!superset_mods.triggers_match(&subset_mods));
        assert!(!subset_mods.triggers_match(&superset_mods));
    }

    #[test]
    fn binding_matches_identical_mode() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_without_mode_matches_any_mode() {
        let b1 = MockBinding::default();
        let b2 = MockBinding {
            mode: BindingMode::APP_KEYPAD,
            notmode: BindingMode::ALT_SCREEN,
            ..MockBinding::default()
        };

        assert!(b1.triggers_match(&b2));
    }

    #[test]
    fn binding_with_mode_matches_empty_mode() {
        let b1 = MockBinding {
            mode: BindingMode::APP_KEYPAD,
            notmode: BindingMode::ALT_SCREEN,
            ..MockBinding::default()
        };
        let b2 = MockBinding::default();

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_modes() {
        let b1 = MockBinding {
            mode: BindingMode::ALT_SCREEN | BindingMode::APP_KEYPAD,
            ..MockBinding::default()
        };
        let b2 = MockBinding { mode: BindingMode::APP_KEYPAD, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_partial_intersection() {
        let b1 = MockBinding {
            mode: BindingMode::ALT_SCREEN | BindingMode::APP_KEYPAD,
            ..MockBinding::default()
        };
        let b2 = MockBinding {
            mode: BindingMode::APP_KEYPAD | BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_mismatches_notmode() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { notmode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        assert!(!b1.triggers_match(&b2));
        assert!(!b2.triggers_match(&b1));
    }

    #[test]
    fn binding_mismatches_unrelated() {
        let b1 = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };
        let b2 = MockBinding { mode: BindingMode::APP_KEYPAD, ..MockBinding::default() };

        assert!(!b1.triggers_match(&b2));
        assert!(!b2.triggers_match(&b1));
    }

    #[test]
    fn binding_matches_notmodes() {
        let subset_notmodes = MockBinding {
            notmode: BindingMode::VI | BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };
        let superset_notmodes =
            MockBinding { notmode: BindingMode::APP_CURSOR, ..MockBinding::default() };

        assert!(subset_notmodes.triggers_match(&superset_notmodes));
        assert!(superset_notmodes.triggers_match(&subset_notmodes));
    }

    #[test]
    fn binding_matches_mode_notmode() {
        let b1 = MockBinding {
            mode: BindingMode::VI,
            notmode: BindingMode::APP_CURSOR,
            ..MockBinding::default()
        };
        let b2 = MockBinding { notmode: BindingMode::APP_CURSOR, ..MockBinding::default() };

        assert!(b1.triggers_match(&b2));
        assert!(b2.triggers_match(&b1));
    }

    #[test]
    fn binding_trigger_input() {
        let binding = MockBinding { trigger: 13, ..MockBinding::default() };

        let mods = binding.mods;
        let mode = binding.mode;

        assert!(binding.is_triggered_by(mode, mods, &13));
        assert!(!binding.is_triggered_by(mode, mods, &32));
    }

    #[test]
    fn binding_trigger_mods() {
        let binding = MockBinding {
            mods: ModifiersState::ALT | ModifiersState::LOGO,
            ..MockBinding::default()
        };

        let superset_mods = ModifiersState::all();
        let subset_mods = ModifiersState::empty();

        let t = binding.trigger;
        let mode = binding.mode;

        assert!(binding.is_triggered_by(mode, binding.mods, &t));
        assert!(!binding.is_triggered_by(mode, superset_mods, &t));
        assert!(!binding.is_triggered_by(mode, subset_mods, &t));
    }

    #[test]
    fn binding_trigger_modes() {
        let binding = MockBinding { mode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        let t = binding.trigger;
        let mods = binding.mods;

        assert!(!binding.is_triggered_by(BindingMode::VI, mods, &t));
        assert!(binding.is_triggered_by(BindingMode::ALT_SCREEN, mods, &t));
        assert!(binding.is_triggered_by(BindingMode::ALT_SCREEN | BindingMode::VI, mods, &t));
    }

    #[test]
    fn binding_trigger_notmodes() {
        let binding = MockBinding { notmode: BindingMode::ALT_SCREEN, ..MockBinding::default() };

        let t = binding.trigger;
        let mods = binding.mods;

        assert!(binding.is_triggered_by(BindingMode::VI, mods, &t));
        assert!(!binding.is_triggered_by(BindingMode::ALT_SCREEN, mods, &t));
        assert!(!binding.is_triggered_by(BindingMode::ALT_SCREEN | BindingMode::VI, mods, &t));
    }
}
