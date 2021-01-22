# Changelog

All notable changes to Alacritty are documented in this file.
The sections should follow the order `Packaging`, `Added`, `Changed`, `Fixed` and `Removed`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## 0.8.0-dev

### Packaging

- Updated shell completions
- Added ARM executable to prebuilt macOS binaries

### Added

- IME composition preview not appearing on Windows

### Fixed

- Crash due to assertion failure on 32-bit architectures

## 0.7.1

### Fixed

- Jumping between matches in backward vi search

## 0.7.0

### Added

- Support for `~/` at the beginning of configuration file imports
- New `cursor.style.blinking` option to set the default blinking state
- New `cursor.blink_interval` option to configure the blinking frequency
- Support for cursor blinking escapes (`CSI ? 12 h`, `CSI ? 12 l` and `CSI Ps SP q`)
- IME support on Windows
- Urgency support on Windows
- Customizable keybindings for search
- History for search mode, bound to ^P/^N/Up/Down by default
- Default binding to cancel search on Ctrl+C
- History position indicator for search and vi mode

### Changed

- Nonexistent config imports are ignored instead of raising an error
- Value for disabling logging with `config.log_level` is `Off` instead of `None`
- Missing glyph symbols are no longer drawn for zerowidth characters

### Fixed

- Wide characters sometimes being cut off
- Preserve vi mode across terminal `reset`
- Escapes `CSI Ps b` and `CSI Ps Z` with large parameters locking up Alacritty
- Dimming colors which use the indexed `CSI 38 : 5 : Ps m` notation
- Slow rendering performance with a lot of cells with underline/strikeout attributes
- Performance of scrolling regions with offset from the bottom
- Extra mouse buttons are no longer ignored on Wayland
- Numpad arrow keys are now properly recognized on Wayland
- Compilation when targetting aarch64-apple-darwin
- Window not being completely opaque on Windows
- Window being always on top during alt-tab on Windows
- Cursor position not reported to apps when mouse is moved with button held outside of window
- No live config update when starting Alacritty with a broken configuration file
- PTY not drained to the end with the `--hold` flag enabled
- High CPU usage on BSD with live config reload enabled
- Alacritty not discarding invalid escape sequences starting with ESC
- Crash due to clipboard not being properly released on Wayland
- Shadow artifacts when resizing transparent windows on macOS
- Missing glyph symbols not being rendered for missing glyphs on macOS and Windows
- Underline cursor being obscured by underline
- Cursor not being rendered with a lot of unicode glyphs visible
- IME input swallowed after triggering a key binding
- Crash on Wayland due to non-standard fontconfig configuration
- Search without vi mode not jumping properly between all matches

### Removed

- The following CLI arguments have been removed in favor of the `--option` flag:
    * `--persistent-logging`
    * `--live-config-reload`
    * `--no-live-config-reload`
    * `--dimensions`
    * `--position`
- `live-shader-reload` feature
- Config option `dynamic_title`, you should use `window.dynamic_title` instead
- Config option `scrolling.faux_multiplier`, which was replaced by escape `CSI ? 1007 h/l`
- WinPTY support on Windows

## 0.6.0

### Packaging

- Minimum Rust version has been bumped to 1.43.0
- The snapcraft.yaml file has been removed
- Updated `setab`/`setaf` capabilities in `alacritty-direct` to use colons
- WinPTY is now enabled only when targeting MSVC
- Deprecated the WinPTY backend feature, disabling it by default

### Added

- Secondary device attributes escape (`CSI > 0 c`)
- Support for colon separated SGR 38/48
- New Ctrl+C binding to cancel search and leave vi mode
- Escapes for double underlines (`CSI 4 : 2 m`) and underline reset (`CSI 4 : 0 m`)
- Configuration file option for sourcing other files (`import`)
- CLI parameter `--option`/`-o` to override any configuration field
- Escape sequences to report text area size in pixels (`CSI 14 t`) and in characters (`CSI 18 t`)
- Support for single line terminals dimensions
- Right clicking on Wayland's client side decorations will show application menu
- Escape sequences to enable and disable window urgency hints (`CSI ? 1042 h`, `CSI ? 1042 l`)

### Changed

- Cursors are now inverted when their fixed color is similar to the cell's background
- Use the working directory of the terminal foreground process, instead of the shell's working
    directory, for `SpawnNewInstance` action
- Fallback to normal underline for unsupported underline types in `CSI 4 : ? m` escapes
- The user's background color is now used as the foreground for the render timer
- Use yellow/red from the config for error and warning messages instead of fixed colors
- Existing CLI parameters are now passed to instances spawned using `SpawnNewInstance`
- Wayland's Client side decorations now use the search bar colors
- Reduce memory usage by up to at least 30% with a full scrollback buffer
- The number of zerowidth characters per cell is no longer limited to 5
- `SpawnNewInstance` is now using the working directory of the terminal foreground process on macOS

### Fixed

- Incorrect window location with negative `window.position` config options
- Slow rendering performance with HiDPI displays, especially on macOS
- Keys swallowed during search when pressing them right before releasing backspace
- Crash when a wrapped line is rotated into the last line
- Selection wrapping to the top when selecting below the error/warning bar
- Pasting into clients only supporting `UTF8_STRING` mime type on Wayland
- Crash when copying/pasting with neither pointer nor keyboard focus on Wayland
- Crash due to fd leak on Wayland
- IME window position with fullwidth characters in the search bar
- Selection expanding over 2 characters when scrolled in history with fullwidth characters in use
- Selection scrolling not starting when mouse is over the message bar
- Incorrect text width calculation in message bar when the message contains multibyte characters
- Remapped caps lock to escape not triggering escape bindings on Wayland
- Crash when setting overly long title on Wayland
- Switching in and out of various window states, like Fullscreen, not persisting window size on Wayland
- Crash when providing 0 for `XCURSOR_SIZE` on Wayland
- Gap between window and server side decorations on KWIN Wayland
- Wayland's client side decorations not working after tty switch
- `Fullscreen` startup mode not working on Wayland
- Window not being rescaled when changing DPR of the current monitor on Wayland
- Crash in some cases when pointer isn't presented upon startup on Wayland
- IME not working on Wayland
- Crash on startup on GNOME since its 3.37.90 version on Wayland
- Touchpad scrolling scrolled less than it should on macOS/Wayland on scaled outputs
- Incorrect modifiers at startup on X11
- `Add` and `Subtract` keys are now named `NumpadAdd` and `NumpadSubtract` respectively
- Feature checking when cross compiling between different operating systems
- Crash when writing to the clipboard fails on Wayland
- Crash with large negative `font.offset.x/y`
- Visual bell getting stuck on the first frame
- Zerowidth characters in the last column of the line

## 0.5.0

### Packaging

- Minimum Rust version has been bumped to 1.41.0
- Prebuilt Linux binaries have been removed
- Added manpage, terminfo, and completions to macOS application bundle
- On Linux/BSD the build will fail without Fontconfig installed, instead of building it from source
- Minimum FreeType version has been bumped to 2.8 on Linux/BSD

### Added

- Default Command+N keybinding for SpawnNewInstance on macOS
- Vi mode for regex search, copying text, and opening links
- `CopySelection` action which copies into selection buffer on Linux/BSD
- Option `cursor.thickness` to set terminal cursor thickness
- Font fallback on Windows
- Support for Fontconfig embolden and matrix options
- Opt-out compilation flag `winpty` to disable WinPTY support
- Scrolling during selection when mouse is at top/bottom of window
- Expanding existing selections using single, double and triple click with the right mouse button
- Support for `gopher` and `gemini` URLs
- Unicode 13 support
- Option to run command on bell which can be set in `bell.command`
- Fallback to program specified in `$SHELL` variable on Linux/BSD if it is present
- Ability to make selections while search is active

### Changed

- Block cursor is no longer inverted at the start/end of a selection
- Preserve selection on non-LMB or mouse mode clicks
- Wayland client side decorations are now based on config colorscheme
- Low resolution window decoration icon on Windows
- Mouse bindings for additional buttons need to be specified as a number not a string
- Don't hide cursor on modifier press with `mouse.hide_when_typing` enabled
- `Shift + Backspace` now sends `^?` instead of `^H`
- Default color scheme is now `Tomorrow Night` with the bright colors of `Tomorrow Night Bright`
- Set IUTF8 termios flag for improved UTF8 input support
- Dragging files into terminal now adds a space after each path
- Default binding replacement conditions
- Adjusted selection clearing granularity to more accurately match content
- To use the cell's text color for selection with a modified background, the `color.selection.text`
    variable must now be set to `CellForeground` instead of omitting it
- URLs are no longer highlighted without a clearly delimited scheme
- Renamed config option `visual_bell` to `bell`
- Moved config option `dynamic_title` to `window.dynamic_title`
- When searching without vi mode, matches are only selected once search is cancelled

### Fixed

- Selection not cleared when switching between main and alt grid
- Freeze when application is invisible on Wayland
- Paste from some apps on Wayland
- Slow startup with Nvidia binary drivers on some X11 systems
- Display not scrolling when printing new lines while scrolled in history
- Regression in font rendering on macOS
- Scroll down escape (`CSI Ps T`) incorrectly pulling lines from history
- Dim escape (`CSI 2 m`) support for truecolor text
- Incorrectly deleted lines when increasing width with a prompt wrapped using spaces
- Documentation for class in `--help` missing information on setting general class
- Linewrap tracking when switching between primary and alternate screen buffer
- Preservation of the alternate screen's saved cursor when swapping to primary screen and back
- Reflow of cursor during resize
- Cursor color escape ignored when its color is set to inverted in the config
- Fontconfig's `autohint` and `hinting` options being ignored
- Ingoring of default FreeType properties
- Alacritty crashing at startup when the configured font does not exist
- Font size rounding error
- Opening URLs while search is active

### Removed

- Environment variable `RUST_LOG` for selecting the log level
- Deprecated `window.start_maximized` config field
- Deprecated `render_timer` config field
- Deprecated `persistent_logging` config field

## 0.4.3

### Fixed

- Tabstops not being reset with `reset`
- Fallback to `LC_CTYPE=UTF-8` on macOS without valid system locale
- Resize lag on launch under some X11 wms
- Increased input latency due to vsync behavior on X11
- Emoji colors blending with terminal background
- Fix escapes prematurely terminated by terminators in unicode glyphs
- Incorrect location when clicking inside an unfocused window on macOS
- Startup mode `Maximized` on Windows
- Crash when writing a fullwidth character in the last column with auto-wrap mode disabled
- Crashing at startup on Windows

## 0.4.2

### Packaging

- Minimum Rust version has been bumped to 1.37.0
- Added Rust features `x11` and `wayland` to pick backends, with both enabled by default
- Capitalized the Alacritty.desktop file

### Added

- Live config reload for `window.title`

### Changed

- Pressing additional modifiers for mouse bindings will no longer trigger them
- Renamed `WINIT_HIDPI_FACTOR` environment variable to `WINIT_X11_SCALE_FACTOR`
- Print an error instead of crashing, when startup working directory is invalid
- Line selection will now expand across wrapped lines
- The default value for `draw_bold_text_with_bright_colors` is now `false`
- Mirror OSC query terminators instead of always using BEL
- Increased Beam, Underline, and Hollow Block cursors' line widths
- Dynamic title is not disabled anymore when `window.title` is set in config

### Fixed

- Incorrect default config path in `--help` on Windows and macOS
- Semantic selection stopping at full-width glyphs
- Full-width glyphs cut off in last column
- Crash when starting on some X11 systems
- Font size resetting when Alacritty is moved between screens
- Limited payload length in clipboard escape (used for Tmux copy/paste)
- Alacritty not ignoring keyboard events for changing WM focus on X11
- Regression which added a UNC path prefix to the working directory on Windows
- CLI parameters discarded when config is reload
- Blurred icons in KDE task switcher (alacritty.ico is now high-res)
- Consecutive builds failing on macOS due to preexisting `/Application` symlink
- Block selection starting from first column after beginning leaves the scrollback
- Incorrect selection status of the first cell when selection is off screen
- Backwards bracket selection
- Stack overflow when printing shader creation error
- Underline position for bitmap fonts
- Selection rotating outside of scrolling region
- Throughput performance problems caused by excessive font metric queries
- Unicode throughput performance on Linux/BSD
- Resize of bitmap fonts
- Crash when using bitmap font with `embeddedbitmap` set to `false`
- Inconsistent fontconfig fallback
- Handling of OpenType variable fonts
- Expansion of block-selection on partially selected full-width glyphs
- Minimize action only works with decorations on macOS
- Window permanently vanishing after hiding on macOS
- Handling of URLs with single quotes
- Parser reset between DCS escapes
- Parser stopping at unknown DEC private modes/SGR character attributes
- Block selection appending duplicate newlines when last column is selected
- Bitmap fonts being a bit smaller than they should be in some cases
- Config reload creating alternate screen history instead of updating scrollback
- Crash on Wayland compositors supporting `wl_seat` version 7+
- Message bar not hiding after fixing wrong color value in config
- Tabstops cleared on resize
- Tabstops not breaking across lines
- Crash when parsing DCS escape with more than 16 parameters
- Ignoring of slow touchpad scrolling
- Selection invisible when starting above viewport and ending below it
- Clipboard not working after TTY switch on Wayland
- Crash when pasting non UTF-8 string advertised as UTF-8 string on Wayland
- Incorrect modifiers tracking on X11 and macOS, leading to 'sticky' modifiers
- Crash when starting on Windows with missing dark mode support
- Variables `XCURSOR_THEME` and `XCURSOR_SIZE` ignored on Wayland
- Low resolution mouse cursor and decorations on HiDPI Wayland outputs
- Decorations visible when in fullscreen on Wayland
- Window size not persisted correctly after fullscreening on macOS
- Crash on startup with some locales on X11
- Shrinking terminal height in alt screen deleting primary screen content

### Removed

- Config option `auto_scroll`, which is now always disabled
- Config option `tabspaces`, which is now fixed at `8`

## 0.4.1

### Packaging

- Added compatibility logo variants for environments which can't render the default SVG

### Added

- Terminal escape bindings with combined modifiers for Delete and Insert
- /Applications symlink into OS X DMG for easier installation
- Colored emojis on Linux/BSD
- Value `randr` for `WINIT_HIDPI_FACTOR`, to ignore `Xft.dpi` and scale based on screen dimensions
- `Minimize` key binding action, bound to `cmd + m` on macOS

### Changed

- On Windows, the ConPTY backend will now be used by default if available
- The `enable_experimental_conpty_backend` config option has been replaced with `winpty_backend`

### Fixed

- URLs not truncated with non-matching single quote
- Absolute file URLs (`file:///home`) not recognized because of leading `/`
- Clipboard escape `OSC 52` not working with empty clipboard parameter
- Direct escape input on Windows using alt
- Incorrect window size on X11 when waking up from suspend
- Width of Unicode 11/12 emojis
- Minimize on windows causing layout issues
- Performance bottleneck when clearing colored rows
- Vague startup crash messages on Windows with WinPTY backend
- Deadlock on Windows when closing Alacritty using the title bar "X" button (ConPTY backend)
- Crash on `clear` when scrolled up in history
- Entire screen getting underlined/stroke out when running `clear`
- Slow startup on some Wayland compositors
- Padding not consistently visible on macOS
- Decorations ignoring Windows dark theme
- Crash on macOS when starting maximized without decorations
- Resize cursor not showing up on Wayland
- Maximized windows spawning behind system panel on Gnome Wayland

### Removed

- Support for 8-bit C1 escape sequences

## 0.4.0

### Packaging

- Minimum Rust version has been bumped to 1.36.0
- Config is not generated anymore, please consider distributing the alacritty.yml as documentation
- Removed Alacritty terminfo from .deb in favor of ncurses provided one

### Added

- Block selection mode when Control is held while starting a selection
- Allow setting general window class on X11 using CLI or config (`window.class.general`)
- Config option `window.gtk_theme_variant` to set GTK theme variant
- Completions for `--class` and `-t` (short title)
- Change the mouse cursor when hovering over the message bar and its close button
- Support combined bold and italic text (with `font.bold_italic` to customize it)
- Extra bindings for F13-F20
- Terminal escape bindings with combined modifiers
- Bindings for ScrollToTop and ScrollToBottom actions
- `ReceiveChar` key binding action to insert the key's text character
- New CLI flag `--hold` for keeping Alacritty opened after its child process exits
- Escape sequence to save and restore window title from stack
- Alternate scroll escape sequence (`CSI ? 1007 h` / `CSI ? 1007 l`)
- Print name of launch command if Alacritty failed to execute it
- Live reload font settings from config
- UTF-8 mouse mode escape sequence (`CSI ? 1005 h` / `CSI ? 1005 l`)
- Escape for reading clipboard (`OSC 52 ; <s / p / c> ; ? BEL`)
- Set selection clipboard (`OSC 52 ; <s / p> ; <BASE64> BEL`)

### Changed

- On Windows, query DirectWrite for recommended anti-aliasing settings
- Scroll lines out of the visible region instead of deleting them when clearing the screen

### Fixed

- GUI programs launched by Alacritty starting in the background on X11
- Text Cursor position when scrolling
- Performance issues while resizing Alacritty
- First unfullscreen action ignored on window launched in fullscreen mode
- The window is now filled with the background color before displaying
- Cells sometimes not getting cleared correctly
- X11 clipboard hanging when mime type is set
- On macOS, Alacritty will now fallback to Menlo if a font specified in the config cannot be loaded
- Debug ref tests are now written to disk regardless of shutdown method
- Cursor color setting with escape sequence
- Override default bindings with subset terminal mode match
- On Linux, respect fontconfig's `embeddedbitmap` configuration option
- Selecting trailing tab with semantic expansion
- URL parser incorrectly handling Markdown URLs and angled brackets
- Intermediate bytes of CSI sequences not checked
- Wayland clipboard integration
- Use text mouse cursor when mouse mode is temporarily disabled with shift
- Wayland primary selection clipboard not storing text when selection is stopped outside of the window
- Block URL highlight while a selection is active
- Bindings for Alt + F1-F12
- Discard scrolling region escape with bottom above top
- Opacity always applying to cells with their background color matching the teriminal background
- Allow semicolons when setting titles using an OSC
- Background always opaque on X11
- Skipping redraws on PTY update
- Not redrawing while resizing on Windows/macOS
- Decorations `none` launching an invisible window on Windows
- Alacritty turning transparent when opening another window on macOS with chunkwm
- Startup mode `Maximized` having no effect on Windows
- Inserting Emojis using `Super+.` or compose sequences on Windows
- Change mouse cursor depending on mode with Wayland
- Hide mouse cursor when typing if the `mouse.hide_when_typing` option is set on Wayland
- Glitches when DPI changes on Windows
- Crash when resuming after suspension
- Crash when trying to start on X11 with a Wayland compositor running
- Crash with a virtual display connected on X11
- Use `\` instead of `\\` as path separators on Windows for logging config file location
- Underline/strikeout drawn above visual bell
- Terminal going transparent during visual bell
- Selection not being cleared when sending chars through a binding
- Mouse protocols/encodings not being mutually exclusive within themselves
- Escape `CSI Ps M` deleting lines above cursor when at the bottom of the viewport
- Cell reset not clearing underline, strikeout and foreground color
- Escape `CSI Ps c` honored with a wrong `Ps`
- Ignore `ESC` escapes with invalid intermediates
- Blank lines after each line when pasting from GTK apps on Wayland

### Removed

- Bindings for Super/Command + F1-F12
- Automatic config generation
- Deprecated `scrolling.faux_multiplier`, the alternate scroll escape can now be used to disable it
    and `scrolling.multiplier` controls the number of scrolled lines

## 0.3.3

### Packaging

- Add appstream metadata, located at /extra/linux/io.alacritty.Alacritty.xml
- The xclip dependency has been removed
- On macOS, Alacritty now requests NSSystemAdministrationUsageDescription to
   avoid permission failures
- Minimum Rust version has been bumped to 1.32.0

### Added

- Added ToggleFullscreen action
- On macOS, there's a ToggleSimpleFullscreen action which allows switching to
    fullscreen without occupying another space
- A new window option `window.startup_mode` which controls how the window is created
- `_NET_WM_ICON` property is set on X11 now, allowing for WMs to show icons in titlebars
- Current Git commit hash to `alacritty --version`
- Config options `window.title` and `window.class`
- Config option `working_directory`
- Config group `debug` with the options `debug.log_level`, `debug.print_events`
    and `debug.ref_test`
- Select until next matching bracket when double-clicking a bracket
- Added foreground/background escape code request sequences
- Escape sequences now support 1, 3, and 4 digit hex colors

### Changed

- On Windows, Alacritty will now use the native DirectWrite font API
- The `start_maximized` window option is now `startup_mode: Maximized`
- Cells with identical foreground and background will now show their text upon selection/inversion
- Default Window padding to 0x0
- Moved config option `render_timer` and `persistent_logging` to the `debug` group
- When the cursor is in the selection, it will be inverted again, making it visible

### Fixed

- Double-width characters in URLs only being highlit on the left half
- PTY size not getting updated when message bar is shown
- Text Cursor disappearing
- Incorrect positioning of zero-width characters over double-width characters
- Mouse mode generating events when the cell has not changed
- Selections not automatically expanding across double-width characters
- On macOS, automatic graphics switching has been enabled again
- Text getting recognized as URLs without slashes separating the scheme
- URL parser dropping trailing slashes from valid URLs
- UTF-8 BOM skipped when reading config file
- Terminfo backspace escape sequence (`kbs`)

### Removed

- Deprecated `mouse.faux_scrollback_lines` config field
- Deprecated `custom_cursor_colors` config field
- Deprecated `hide_cursor_when_typing` config field
- Deprecated `cursor_style` config field
- Deprecated `unfocused_hollow_cursor` config field
- Deprecated `dimensions` config field

## Version 0.3.2

### Fixed

- Panic on startup when using Conpty on Windows

## Version 0.3.1

### Added

- Added ScrollLineUp and ScrollLineDown actions for scrolling line by line
- Native clipboard support on X11 and Wayland

### Changed

- Alacritty now has a fixed minimum supported Rust version of 1.31.0

### Fixed

- Reset scrolling region when the RIS escape sequence is received
- Subprocess spawning on macos
- Unnecessary resize at startup
- Text getting blurry after live-reloading shaders with padding active
- Resize events are not send to the shell anymore if dimensions haven't changed
- Minor performance issues with underline and strikeout checks
- Rare bug which would extend underline and strikeout beyond the end of line
- Cursors not spanning two lines when over double-width characters
- Incorrect cursor dimensions if the font offset isn't `0`

## Version 0.3.0

### Packaging

- On Linux, the .desktop file now uses `Alacritty` as icon name, which can be
    found at `extra/logo/alacritty-term.svg`

### Added

- MSI installer for Windows is now available
- New default key bindings Alt+Home, Alt+End, Alt+PageUp and Alt+PageDown
- Dynamic title support on Windows
- Ability to specify starting position with the `--position` flag
- New configuration field `window.position` allows specifying the starting position
- Added the ability to change the selection color
- Text will reflow instead of truncating when resizing Alacritty
- Underline text and change cursor when hovering over URLs with required modifiers pressed

### Changed

- Clicking on non-alphabetical characters in front of URLs will no longer open them
- Command keybindings on Windows will no longer open new cmd.exe console windows
- On macOS, automatic graphics switching has been temporarily disabled due to a macos bug

### Fixed

- Fix panic which could occur when quitting Alacritty on Windows if using the Conpty backend
- Automatic copying of selection to clipboard when mouse is released outside of Alacritty
- Scrollback history live reload only working when shrinking lines
- Crash when decreasing scrollback history in config while scrolled in history
- Resetting the terminal while in the alt screen will no longer disable scrollback
- Cursor jumping around when leaving alt screen while not in the alt screen
- Text lingering around when resetting while scrolled up in the history
- Terminfo support for extended capabilities
- Allow mouse presses and beginning of mouse selection in padding
- Windows: Conpty backend could close immediately on startup in certain situations
- FreeBSD: SpawnNewInstance will now open new instances in the shell's current
    working directory as long as linprocfs(5) is mounted on `/compat/linux/proc`
- Fix lingering Alacritty window after child process has exited
- Growing the terminal while scrolled up will no longer move the content down
- Support for alternate keyboard layouts on macOS
- Slow startup time on some X11 systems
- The AltGr key no longer sends escapes (like Alt)
- Fixes increase/decrease font-size keybindings on international keyboards
- On Wayland, the `--title` flag will set the Window title now
- Parsing issues with URLs starting in the first or ending in the last column
- URLs stopping at double-width characters
- Fix `start_maximized` option on X11
- Error when parsing URLs ending with Unicode outside of the ascii range
- On Windows, focusing a Window will no longer start a selection

## Version 0.2.9

### Changed

- Accept fonts which are smaller in width or height than a single pixel

### Fixed

- Incorrect font spacing after moving Alacritty between displays

## Version 0.2.8

### Added

- Window class on Wayland is set to `Alacritty` by default
- Log file location is stored in the `ALACRITTY_LOG` environment variable
- Close button has been added to the error/warning messages

### Changed

- Improve scrolling accuracy with devices sending fractional updates (like touchpads)
- `scrolling.multiplier` now affects normal scrolling with touchpads
- Error/Warning bar doesn't overwrite the terminal anymore
- Full error/warning messages are displayed
- Config error messages are automatically removed when the config is fixed
- Scroll history on Shift+PgUp/PgDown when scrollback history is available

### Fixed

- Resolved off-by-one issue with erasing characters in the last column
- Excessive polling every 100ms with `live_config_reload` enabled
- Unicode characters at the beginning of URLs are now properly ignored
- Remove error message when reloading an empty config
- Allow disabling URL launching by setting the value of `mouse.url.launcher` to `None`
- Corrected the `window.decorations` config documentation for macOS
- Fix IME position on HiDPI displays
- URLs not opening while terminal is scrolled
- Reliably remove log file when Alacritty is closed and persistent logging is disabled
- Remove selections when clearing the screen partially (scrolling horizontally in less)
- Crash/Freeze when shrinking the font size too far
- Documentation of the `--dimensions` flag have been updated to display the correct default

### Removed

- `clear` doesn't remove error/warning messages anymore

## Version 0.2.7

### Fixed

- Crash when trying to start Alacritty on Windows

## Version 0.2.6

### Added

- New `alt_send_esc` option for controlling if alt key should send escape sequences

### Changed

- All options in the configuration file are now optional

### Fixed

- Replaced `Command` with `Super` in the Linux and Windows config documentation
- Prevent semantic and line selection from starting with the right or middle mouse button
- Prevent Alacritty from crashing when started on a system without any free space
- Resolve issue with high CPU usage after moving Alacritty between displays
- Characters will no longer be deleted when using ncurses with the hard tab optimization
- Crash on non-linux operating systems when using the `SpawnNewInstance` action

### Removed

- Windows and macOS configuration files (`alacritty.yml` is now platform independent)

## Version 0.2.5

### Added

- New configuration field `visual_bell.color` allows changing the visual bell color
- Crashes on Windows are now also reported with a popup in addition to stderr
- Windows: New configuration field `enable_experimental_conpty_backend` which enables support
    for the Pseudoconsole API (ConPTY) added in Windows 10 October 2018 (1809) update
- New mouse and key action `SpawnNewInstance` for launching another instance of Alacritty

### Changed

- Log messages are now consistent in style, and some have been removed
- Windows configuration location has been moved from %USERPROFILE%\alacritty.yml
    to %APPDATA%\alacritty\alacritty.yml
- Windows default shell is now PowerShell instead of cmd
- URL schemes have been limited to http, https, mailto, news, file, git, ssh and ftp

### Fixed

- Fix color issue in ncurses programs by updating terminfo pairs from 0x10000 to 0x7FFF
- Fix panic after quitting Alacritty on macOS
- Tabs are no longer replaced by spaces when copying them to the clipboard
- Alt modifier is no longer sent separately from the modified key
- Various Windows issues, like color support and performance, through the new ConPTY
- Fixed rendering non default mouse cursors in terminal mouse mode (linux)
- Fix the `Copy` `mouse_bindings` action ([#1963](https://github.com/alacritty/alacritty/issues/1963))
- URLs are only launched when left-clicking
- Removal of extra characters (like `,`) at the end of URLs has been improved
- Single quotes (`'`) are removed from URLs when there is no matching opening quote
- Precompiled binaries now work with macOS versions before 10.13 (10.11 and above)

## Version 0.2.4

### Added

- Option for evenly spreading extra padding around the terminal (`window.dynamic_padding`)
- Option for maximizing alacritty on start (`window.start_maximized`)
- Display notice about errors and warnings inside Alacritty
- Log all messages to both stderr and a log file in the system's temporary directory
- New configuration option `persistent_logging` and CLI flag `--persistent-logging`,
    for keeping the log file after closing Alacritty
- `ClearLogNotice` action for removing the warning and error message
- Terminal bells on macOS will now request the user's attention in the window
- Alacritty now requests privacy permissions on macOS

### Changed

- Extra padding is not evenly spread around the terminal by default anymore
- When the config file is empty, Alacritty now logs an info instead of an error message

### Fixed

- Fixed a bad type conversion which could cause underflow on a window resize
- Alacritty now spawns a login shell on macOS, as with Terminal.app and iTerm2
- Fixed zombie processes sticking around after launching URLs
- Zero-width characters are now properly rendered without progressing the cursor

## Version 0.2.3

### Fixed

- Mouse cursor alignment issues and truncated last line caused by incorrect padding calculations

## Version 0.2.2

### Added

- Add support for Windows
- Add terminfo capabilities advertising support for changing the window title
- Allow using scancodes in the key_bindings section
- When `mouse.url.launcher` is set, clicking on URLs will now open them with the specified program
- New `mouse.url.modifiers` option to specify keyboard modifiers for opening URLs on click
- Binaries for macOS, Windows and Debian-based systems are now published with GitHub releases
- The keys F16-F24 have been added as options for key bindings
- DEB file adds Alacritty as option to `update-alternatives --config x-terminal-emulator`

### Changed

- The `colors.cursor.text` and `colors.cursor.cursor` fields are optional now
- Moved `cursor_style` to `cursor.style`
- Moved `unfocused_hollow_cursor` to `cursor.unfocused_hollow`
- Moved `hide_cursor_when_typing` to `mouse.hide_when_typing`
- Mouse bindings now ignore additional modifiers
- Extra padding is now spread evenly around the terminal grid
- DEB file installs to `usr/bin` instead of `usr/local/bin`

### Fixed

- Fixed erroneous results when using the `indexed_colors` config option
- Fixed rendering cursors other than rectangular with the RustType backend
- Selection memory leak and glitches in the alternate screen buffer
- Invalid default configuration on macOS and Linux
- Middle mouse pasting if mouse mode is enabled
- Selections now properly update as you scroll the scrollback buffer while selecting
- NUL character at the end of window titles
- DPI Scaling when moving windows across monitors
- On macOS, issues with Command-[KEY] and Control-Tab keybindings have been fixed
- Incorrect number of columns/lines when using the `window.dimensions` option
- On Wayland, windows will no longer be spawned outside of the visible region
- Resizing of windows without decorations
- On Wayland, key repetition works again
- On macOS, Alacritty will now use the integrated GPU again when available
- On Linux, the `WINIT_HIDPI_FACTOR` environment variable can be set from the config now

### Removed

- The `custom_cursor_colors` config field was deleted, remove the `colors.cursor.*` options
  to achieve the same behavior as setting it to `false`
- The `scale_with_dpi` configuration value has been removed, on Linux the env
    variable `WINIT_HIDPI_FACTOR=1` can be set instead to disable DPI scaling

## Version 0.2.1

### Added

- Implement the `hidden` escape sequence (`echo -e "\e[8mTEST"`)
- Add support for macOS systemwide dark mode
- Set the environment variable `COLORTERM="truecolor"` to advertise 24-bit color support
- On macOS, there are two new values for the config option `window.decorations`:
    - `transparent` - This makes the title bar transparent and allows the
        viewport to extend to the top of the window.
    - `buttonless` - Similar to transparent but also removed the buttons.
- Add support for changing the colors from 16 to 256 in the `indexed_colors` config section
- Add `save_to_clipboard` configuration option for copying selected text to the system clipboard
- New terminfo entry, `alacritty-direct`, that advertises 24-bit color support
- Add support for CSI sequences Cursor Next Line (`\e[nE`) and Cursor Previous Line (`\e[nF`)

### Changed

- Inverse/Selection color is now modelled after XTerm/VTE instead of URxvt to improve consistency
- First click on unfocused Alacritty windows is no longer ignored on platforms other than macOS
- Reduce memory usage significantly by only initializing part of the scrollback buffer at startup
- The `alacritty` terminfo entry no longer requires the `xterm` definition to be
  present on the system
- The default `TERM` value is no longer static; the `alacritty` entry is used if
  available, otherwise the `xterm-256color` entry is used instead
- The values `true` and `false` for the config option `window.decorations` have been replaced with
    `full` and `none`

### Fixed

- Rendering now occurs without the terminal locked which improves performance
- Clear screen properly before rendering of content to prevent various graphical glitches
- Fix build failure on 32-bit systems
- Windows started as unfocused now show the hollow cursor if the setting is enabled
- Empty lines in selections are now properly copied to the clipboard
- Selection start point lagging behind initial cursor position
- Rendering of selections which start above the visible area and end below it
- Bracketed paste mode now filters escape sequences beginning with \x1b

### Removed

- The terminfo entry `alacritty-256color`. It is replaced by the `alacritty`
  entry (which also advertises 256 colors)

## Version 0.2.0

### Added

- Add a scrollback history buffer (10_000 lines by default)
- CHANGELOG has been added for documenting relevant user-facing changes
- Add `ClearHistory` key binding action and the `Erase Saved Lines` control sequence
- When growing the window height, Alacritty will now try to load additional lines out of the
  scrollback history
- Support the dim foreground color (`echo -e '\033[2mDimmed Text'`)
- Add support for the LCD-V pixel mode (vertical screens)
- Pressing enter on the numpad should now insert a newline
- The mouse bindings now support keyboard modifiers (shift/ctrl/alt/super)
- Add support for the bright foreground color
- Support for setting foreground, background colors in one escape sequence

### Changed

- Multiple key/mouse bindings for a single key will now all be executed instead of picking one and
  ignoring the rest
- Improve text scrolling performance (affects applications like `yes`, not scrolling the history)

### Fixed

- Clear the visible region when the RIS escape sequence (`echo -ne '\033c'`) is received
- Prevent logger from crashing Alacritty when stdout/stderr is not available
- Fix a crash when sending the IL escape sequence with a large number of lines
