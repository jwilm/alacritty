//! Alacritty - The GPU Enhanced Terminal.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]
#![cfg_attr(feature = "cargo-clippy", deny(warnings))]
#![cfg_attr(all(test, feature = "bench"), feature(test))]
// With the default subsystem, 'console', windows creates an additional console
// window for the program.
// This is silently ignored on non-windows systems.
// See https://msdn.microsoft.com/en-us/library/4cc7ya5b.aspx for more details.
#![windows_subsystem = "windows"]

#[cfg(not(any(feature = "x11", feature = "wayland", target_os = "macos", windows)))]
compile_error!(r#"at least one of the "x11"/"wayland" features must be enabled"#);

#[cfg(target_os = "macos")]
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::sync::Arc;

use glutin::event_loop::EventLoop as GlutinEventLoop;
use log::{error, info};
#[cfg(windows)]
use winapi::um::wincon::{AttachConsole, FreeConsole, ATTACH_PARENT_PROCESS};

use alacritty_terminal::event_loop::{self, EventLoop, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty;

mod cli;
mod clipboard;
mod config;
mod cursor;
mod daemon;
mod display;
mod event;
mod input;
#[cfg(target_os = "macos")]
mod locale;
mod logging;
mod message_bar;
mod meter;
#[cfg(windows)]
mod panic;
mod renderer;
mod scheduler;
mod url;
mod window;

#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
mod wayland_theme;

mod gl {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

use crate::cli::Options;
use crate::config::monitor;
use crate::config::Config;
use crate::display::Display;
use crate::event::{Event, EventProxy, Processor};
use crate::message_bar::MessageBuffer;

#[macro_use]
extern crate memoffset;

fn main() {
    #[cfg(windows)]
    panic::attach_handler();

    // When linked with the windows subsystem windows won't automatically attach
    // to the console of the parent process, so we do it explicitly. This fails
    // silently if the parent has no console.
    #[cfg(windows)]
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    // Load command line options.
    let options = Options::new();

    // Setup glutin event loop.
    let window_event_loop = GlutinEventLoop::<Event>::with_user_event();

    // Initialize the logger as soon as possible as to capture output from other subsystems.
    let log_file = logging::initialize(&options, window_event_loop.create_proxy())
        .expect("Unable to initialize logger");

    // Load configuration file.
    let config = config::load(&options);

    // Update the log level from config.
    log::set_max_level(config.ui_config.debug.log_level);

    // Switch to home directory.
    #[cfg(target_os = "macos")]
    env::set_current_dir(dirs::home_dir().unwrap()).unwrap();
    // Set locale.
    #[cfg(target_os = "macos")]
    locale::set_locale_environment();

    // Store if log file should be deleted before moving config.
    let persistent_logging = config.ui_config.debug.persistent_logging;

    // Run Alacritty.
    if let Err(err) = run(window_event_loop, config, options) {
        error!("Alacritty encountered an unrecoverable error:\n\n\t{}\n", err);
        std::process::exit(1);
    }

    // Clean up logfile.
    if let Some(log_file) = log_file {
        if !persistent_logging && fs::remove_file(&log_file).is_ok() {
            let _ = writeln!(io::stdout(), "Deleted log file at \"{}\"", log_file.display());
        }
    }
}

/// Run Alacritty.
///
/// Creates a window, the terminal state, PTY, I/O event loop, input processor,
/// config change monitor, and runs the main display loop.
fn run(
    window_event_loop: GlutinEventLoop<Event>,
    config: Config,
    options: Options,
) -> Result<(), Box<dyn Error>> {
    info!("Welcome to Alacritty");

    info!("Configuration files loaded from:");
    for path in &config.ui_config.config_paths {
        info!("  \"{}\"", path.display());
    }

    // Set environment variables.
    tty::setup_env(&config);

    let event_proxy = EventProxy::new(window_event_loop.create_proxy());

    // Create a display.
    //
    // The display manages a window and can draw the terminal.
    let display = Display::new(&config, &window_event_loop)?;

    info!(
        "PTY dimensions: {:?} x {:?}",
        display.size_info.screen_lines(),
        display.size_info.cols()
    );

    // Create the terminal.
    //
    // This object contains all of the state about what's being displayed. It's
    // wrapped in a clonable mutex since both the I/O loop and display need to
    // access it.
    let terminal = Term::new(&config, display.size_info, event_proxy.clone());
    let terminal = Arc::new(FairMutex::new(terminal));

    // Create the PTY.
    //
    // The PTY forks a process to run the shell on the slave side of the
    // pseudoterminal. A file descriptor for the master side is retained for
    // reading/writing to the shell.
    let pty = tty::new(&config, &display.size_info, display.window.x11_window_id());

    // Create the pseudoterminal I/O loop.
    //
    // PTY I/O is ran on another thread as to not occupy cycles used by the
    // renderer and input processing. Note that access to the terminal state is
    // synchronized since the I/O loop updates the state, and the display
    // consumes it periodically.
    let event_loop = EventLoop::new(
        Arc::clone(&terminal),
        event_proxy.clone(),
        pty,
        config.hold,
        config.ui_config.debug.ref_test,
    );

    // The event loop channel allows write requests from the event processor
    // to be sent to the pty loop and ultimately written to the pty.
    let loop_tx = event_loop.channel();

    // Create a config monitor when config was loaded from path.
    //
    // The monitor watches the config file for changes and reloads it. Pending
    // config changes are processed in the main loop.
    if config.ui_config.live_config_reload() {
        monitor::watch(config.ui_config.config_paths.clone(), event_proxy);
    }

    // Setup storage for message UI.
    let message_buffer = MessageBuffer::new();

    // Event processor.
    let mut processor = Processor::new(
        event_loop::Notifier(loop_tx.clone()),
        message_buffer,
        config,
        display,
        options,
    );

    // Kick off the I/O thread.
    let io_thread = event_loop.spawn();

    info!("Initialisation complete");

    // Start event loop and block until shutdown.
    processor.run(terminal, window_event_loop);

    // This explicit drop is needed for Windows, ConPTY backend. Otherwise a deadlock can occur.
    // The cause:
    //   - Drop for ConPTY will deadlock if the conout pipe has already been dropped.
    //   - The conout pipe is dropped when the io_thread is joined below (io_thread owns PTY).
    //   - ConPTY is dropped when the last of processor and io_thread are dropped, because both of
    //     them own an Arc<ConPTY>.
    //
    // The fix is to ensure that processor is dropped first. That way, when io_thread (i.e. PTY)
    // is dropped, it can ensure ConPTY is dropped before the conout pipe in the PTY drop order.
    //
    // FIXME: Change PTY API to enforce the correct drop order with the typesystem.
    drop(processor);

    // Shutdown PTY parser event loop.
    loop_tx.send(Msg::Shutdown).expect("Error sending shutdown to PTY event loop");
    io_thread.join().expect("join io thread");

    // FIXME patch notify library to have a shutdown method.
    // config_reloader.join().ok();

    // Without explicitly detaching the console cmd won't redraw it's prompt.
    #[cfg(windows)]
    unsafe {
        FreeConsole();
    }

    info!("Goodbye");

    Ok(())
}
