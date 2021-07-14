//! TTY related functionality.

use std::borrow::Cow;
#[cfg(not(target_os = "macos"))]
use std::env;
use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::{
    io::{AsRawFd, FromRawFd, RawFd},
    process::CommandExt,
};
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use libc::{self, c_int, pid_t, winsize, TIOCSCTTY};
use log::error;
use mio::unix::EventedFd;
use nix::pty::openpty;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use nix::sys::termios::{self, InputFlags, SetArg};
use signal_hook::consts::signal::*;
use signal_hook_mio::v0_6::Signals;

use crate::config::{Config, Program};
use crate::event::OnResize;
use crate::grid::Dimensions;
use crate::term::SizeInfo;
use crate::tty::{ChildEvent, EventedPty, EventedReadWrite};

/// Process ID of child process.
///
/// Necessary to put this in static storage for `SIGCHLD` to have access.
static PID: AtomicUsize = AtomicUsize::new(0);

/// File descriptor of terminal master.
static FD: AtomicI32 = AtomicI32::new(-1);

macro_rules! die {
    ($($arg:tt)*) => {{
        error!($($arg)*);
        std::process::exit(1);
    }}
}

pub fn child_pid() -> pid_t {
    PID.load(Ordering::Relaxed) as pid_t
}

pub fn master_fd() -> RawFd {
    FD.load(Ordering::Relaxed) as RawFd
}

/// Get raw fds for master/slave ends of a new PTY.
fn make_pty(size: winsize) -> (RawFd, RawFd) {
    let mut win_size = size;
    win_size.ws_xpixel = 0;
    win_size.ws_ypixel = 0;

    let ends = openpty(Some(&win_size), None).expect("openpty failed");

    (ends.master, ends.slave)
}

/// Really only needed on BSD, but should be fine elsewhere.
fn set_controlling_terminal(fd: c_int) {
    let res = unsafe {
        // TIOSCTTY changes based on platform and the `ioctl` call is different
        // based on architecture (32/64). So a generic cast is used to make sure
        // there are no issues. To allow such a generic cast the clippy warning
        // is disabled.
        #[allow(clippy::cast_lossless)]
        libc::ioctl(fd, TIOCSCTTY as _, 0)
    };

    if res < 0 {
        die!("ioctl TIOCSCTTY failed: {}", io::Error::last_os_error());
    }
}

#[derive(Debug)]
struct Passwd<'a> {
    name: &'a str,
    passwd: &'a str,
    uid: libc::uid_t,
    gid: libc::gid_t,
    gecos: &'a str,
    dir: &'a str,
    shell: &'a str,
}

/// Return a Passwd struct with pointers into the provided buf.
///
/// # Unsafety
///
/// If `buf` is changed while `Passwd` is alive, bad thing will almost certainly happen.
fn get_pw_entry(buf: &mut [i8; 1024]) -> Passwd<'_> {
    // Create zeroed passwd struct.
    let mut entry: MaybeUninit<libc::passwd> = MaybeUninit::uninit();

    let mut res: *mut libc::passwd = ptr::null_mut();

    // Try and read the pw file.
    let uid = unsafe { libc::getuid() };
    let status = unsafe {
        libc::getpwuid_r(uid, entry.as_mut_ptr(), buf.as_mut_ptr() as *mut _, buf.len(), &mut res)
    };
    let entry = unsafe { entry.assume_init() };

    if status < 0 {
        die!("getpwuid_r failed");
    }

    if res.is_null() {
        die!("pw not found");
    }

    // Sanity check.
    assert_eq!(entry.pw_uid, uid);

    // Build a borrowed Passwd struct.
    Passwd {
        name: unsafe { CStr::from_ptr(entry.pw_name).to_str().unwrap() },
        passwd: unsafe { CStr::from_ptr(entry.pw_passwd).to_str().unwrap() },
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        gecos: unsafe { CStr::from_ptr(entry.pw_gecos).to_str().unwrap() },
        dir: unsafe { CStr::from_ptr(entry.pw_dir).to_str().unwrap() },
        shell: unsafe { CStr::from_ptr(entry.pw_shell).to_str().unwrap() },
    }
}

pub struct Pty {
    child: Child,
    fd: File,
    token: mio::Token,
    signals: Signals,
    signals_token: mio::Token,
}

#[cfg(target_os = "macos")]
fn default_shell(pw: &Passwd<'_>) -> Program {
    let shell_name = pw.shell.rsplit('/').next().unwrap();
    let argv = vec![String::from("-c"), format!("exec -a -{} {}", shell_name, pw.shell)];

    Program::WithArgs { program: "/bin/bash".to_owned(), args: argv }
}

#[cfg(not(target_os = "macos"))]
fn default_shell(pw: &Passwd<'_>) -> Program {
    Program::Just(env::var("SHELL").unwrap_or_else(|_| pw.shell.to_owned()))
}

/// Create a new TTY and return a handle to interact with it.
pub fn new<C>(config: &Config<C>, size: &SizeInfo, window_id: Option<usize>) -> Pty {
    let (master, slave) = make_pty(size.to_winsize());

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Ok(mut termios) = termios::tcgetattr(master) {
        // Set character encoding to UTF-8.
        termios.input_flags.set(InputFlags::IUTF8, true);
        let _ = termios::tcsetattr(master, SetArg::TCSANOW, &termios);
    }

    let mut buf = [0; 1024];
    let pw = get_pw_entry(&mut buf);

    let shell = match config.shell.as_ref() {
        Some(shell) => Cow::Borrowed(shell),
        None => Cow::Owned(default_shell(&pw)),
    };

    let mut builder = Command::new(shell.program());
    for arg in shell.args() {
        builder.arg(arg);
    }

    // Setup child stdin/stdout/stderr as slave fd of PTY.
    // Ownership of fd is transferred to the Stdio structs and will be closed by them at the end of
    // this scope. (It is not an issue that the fd is closed three times since File::drop ignores
    // error on libc::close.).
    builder.stdin(unsafe { Stdio::from_raw_fd(slave) });
    builder.stderr(unsafe { Stdio::from_raw_fd(slave) });
    builder.stdout(unsafe { Stdio::from_raw_fd(slave) });

    // Setup shell environment.
    builder.env("LOGNAME", pw.name);
    builder.env("USER", pw.name);
    builder.env("HOME", pw.dir);

    // Set $SHELL environment variable on macOS, since login does not do it for us.
    #[cfg(target_os = "macos")]
    builder.env("SHELL", config.shell.as_ref().map(|sh| sh.program()).unwrap_or(pw.shell));

    if let Some(window_id) = window_id {
        builder.env("WINDOWID", format!("{}", window_id));
    }

    unsafe {
        builder.pre_exec(move || {
            // Create a new process group.
            let err = libc::setsid();
            if err == -1 {
                die!("Failed to set session id: {}", io::Error::last_os_error());
            }

            set_controlling_terminal(slave);

            // No longer need slave/master fds.
            libc::close(slave);
            libc::close(master);

            libc::signal(SIGCHLD, libc::SIG_DFL);
            libc::signal(SIGHUP, libc::SIG_DFL);
            libc::signal(SIGINT, libc::SIG_DFL);
            libc::signal(SIGQUIT, libc::SIG_DFL);
            libc::signal(SIGTERM, libc::SIG_DFL);
            libc::signal(SIGALRM, libc::SIG_DFL);

            Ok(())
        });
    }

    // Handle set working directory option.
    if let Some(dir) = &config.working_directory {
        builder.current_dir(dir);
    }

    // Prepare signal handling before spawning child.
    let signals = Signals::new(&[SIGCHLD]).expect("error preparing signal handling");

    match builder.spawn() {
        Ok(child) => {
            // Remember master FD and child PID so other modules can use it.
            PID.store(child.id() as usize, Ordering::Relaxed);
            FD.store(master, Ordering::Relaxed);

            unsafe {
                // Maybe this should be done outside of this function so nonblocking
                // isn't forced upon consumers. Although maybe it should be?
                set_nonblocking(master);
            }

            let mut pty = Pty {
                child,
                fd: unsafe { File::from_raw_fd(master) },
                token: mio::Token::from(0),
                signals,
                signals_token: mio::Token::from(0),
            };
            pty.on_resize(size);
            pty
        },
        Err(err) => die!("Failed to spawn command '{}': {}", shell.program(), err),
    }
}

impl EventedReadWrite for Pty {
    type Reader = File;
    type Writer = File;

    #[inline]
    fn register(
        &mut self,
        poll: &mio::Poll,
        token: &mut dyn Iterator<Item = mio::Token>,
        interest: mio::Ready,
        poll_opts: mio::PollOpt,
    ) -> io::Result<()> {
        self.token = token.next().unwrap();
        poll.register(&EventedFd(&self.fd.as_raw_fd()), self.token, interest, poll_opts)?;

        self.signals_token = token.next().unwrap();
        poll.register(
            &self.signals,
            self.signals_token,
            mio::Ready::readable(),
            mio::PollOpt::level(),
        )
    }

    #[inline]
    fn reregister(
        &mut self,
        poll: &mio::Poll,
        interest: mio::Ready,
        poll_opts: mio::PollOpt,
    ) -> io::Result<()> {
        poll.reregister(&EventedFd(&self.fd.as_raw_fd()), self.token, interest, poll_opts)?;

        poll.reregister(
            &self.signals,
            self.signals_token,
            mio::Ready::readable(),
            mio::PollOpt::level(),
        )
    }

    #[inline]
    fn deregister(&mut self, poll: &mio::Poll) -> io::Result<()> {
        poll.deregister(&EventedFd(&self.fd.as_raw_fd()))?;
        poll.deregister(&self.signals)
    }

    #[inline]
    fn reader(&mut self) -> &mut File {
        &mut self.fd
    }

    #[inline]
    fn read_token(&self) -> mio::Token {
        self.token
    }

    #[inline]
    fn writer(&mut self) -> &mut File {
        &mut self.fd
    }

    #[inline]
    fn write_token(&self) -> mio::Token {
        self.token
    }
}

impl EventedPty for Pty {
    #[inline]
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.signals.pending().next().and_then(|signal| {
            if signal != SIGCHLD {
                return None;
            }

            match self.child.try_wait() {
                Err(e) => {
                    error!("Error checking child process termination: {}", e);
                    None
                },
                Ok(None) => None,
                Ok(_) => Some(ChildEvent::Exited),
            }
        })
    }

    #[inline]
    fn child_event_token(&self) -> mio::Token {
        self.signals_token
    }
}

/// Types that can produce a `libc::winsize`.
pub trait ToWinsize {
    /// Get a `libc::winsize`.
    fn to_winsize(&self) -> winsize;
}

impl<'a> ToWinsize for &'a SizeInfo {
    fn to_winsize(&self) -> winsize {
        winsize {
            ws_row: self.screen_lines() as libc::c_ushort,
            ws_col: self.columns() as libc::c_ushort,
            ws_xpixel: self.width() as libc::c_ushort,
            ws_ypixel: self.height() as libc::c_ushort,
        }
    }
}

impl OnResize for Pty {
    /// Resize the PTY.
    ///
    /// Tells the kernel that the window size changed with the new pixel
    /// dimensions and line/column counts.
    fn on_resize(&mut self, size: &SizeInfo) {
        let win = size.to_winsize();

        let res = unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCSWINSZ, &win as *const _) };

        if res < 0 {
            die!("ioctl TIOCSWINSZ failed: {}", io::Error::last_os_error());
        }
    }
}

unsafe fn set_nonblocking(fd: c_int) {
    use libc::{fcntl, F_GETFL, F_SETFL, O_NONBLOCK};

    let res = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
    assert_eq!(res, 0);
}

#[test]
fn test_get_pw_entry() {
    let mut buf: [i8; 1024] = [0; 1024];
    let _pw = get_pw_entry(&mut buf);
}
