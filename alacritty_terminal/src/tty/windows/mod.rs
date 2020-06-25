use std::ffi::OsStr;
use std::io;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::sync::mpsc::TryRecvError;

use crate::config::{Config, Program};
use crate::event::OnResize;
use crate::term::SizeInfo;
use crate::tty::windows::child::ChildExitWatcher;
use crate::tty::{ChildEvent, EventedPty, EventedReadWrite};

#[cfg(feature = "winpty")]
mod automatic_backend;
mod child;
mod conpty;
#[cfg(feature = "winpty")]
mod winpty;

#[cfg(not(feature = "winpty"))]
use conpty::Conpty as Backend;
#[cfg(not(feature = "winpty"))]
use mio_anonymous_pipes::{EventedAnonRead as ReadPipe, EventedAnonWrite as WritePipe};

#[cfg(feature = "winpty")]
use automatic_backend::{
    EventedReadablePipe as ReadPipe, EventedWritablePipe as WritePipe, PtyBackend as Backend,
};

pub struct Pty {
    // XXX: Backend is required to be the first field, to ensure correct drop order. Dropping
    // `conout` before `backend` will cause a deadlock (with Conpty).
    backend: Backend,
    conout: ReadPipe,
    conin: WritePipe,
    read_token: mio::Token,
    write_token: mio::Token,
    child_event_token: mio::Token,
    child_watcher: ChildExitWatcher,
}

#[cfg(not(feature = "winpty"))]
pub fn new<C>(config: &Config<C>, size: &SizeInfo, window_id: Option<usize>) -> Pty {
    conpty::new(config, size, window_id).expect("Failed to create ConPTY backend")
}

#[cfg(feature = "winpty")]
pub fn new<C>(config: &Config<C>, size: &SizeInfo, window_id: Option<usize>) -> Pty {
    automatic_backend::new(config, size, window_id)
}

impl Pty {
    fn new(
        backend: impl Into<Backend>,
        conout: impl Into<ReadPipe>,
        conin: impl Into<WritePipe>,
        child_watcher: ChildExitWatcher,
    ) -> Self {
        Self {
            backend: backend.into(),
            conout: conout.into(),
            conin: conin.into(),
            read_token: 0.into(),
            write_token: 0.into(),
            child_event_token: 0.into(),
            child_watcher,
        }
    }
}

impl EventedReadWrite for Pty {
    type Reader = ReadPipe;
    type Writer = WritePipe;

    #[inline]
    fn register(
        &mut self,
        poll: &mio::Poll,
        token: &mut dyn Iterator<Item = mio::Token>,
        interest: mio::Ready,
        poll_opts: mio::PollOpt,
    ) -> io::Result<()> {
        self.read_token = token.next().unwrap();
        self.write_token = token.next().unwrap();

        if interest.is_readable() {
            poll.register(&self.conout, self.read_token, mio::Ready::readable(), poll_opts)?
        } else {
            poll.register(&self.conout, self.read_token, mio::Ready::empty(), poll_opts)?
        }
        if interest.is_writable() {
            poll.register(&self.conin, self.write_token, mio::Ready::writable(), poll_opts)?
        } else {
            poll.register(&self.conin, self.write_token, mio::Ready::empty(), poll_opts)?
        }

        self.child_event_token = token.next().unwrap();
        poll.register(
            self.child_watcher.event_rx(),
            self.child_event_token,
            mio::Ready::readable(),
            poll_opts,
        )?;

        Ok(())
    }

    #[inline]
    fn reregister(
        &mut self,
        poll: &mio::Poll,
        interest: mio::Ready,
        poll_opts: mio::PollOpt,
    ) -> io::Result<()> {
        if interest.is_readable() {
            poll.reregister(&self.conout, self.read_token, mio::Ready::readable(), poll_opts)?;
        } else {
            poll.reregister(&self.conout, self.read_token, mio::Ready::empty(), poll_opts)?;
        }
        if interest.is_writable() {
            poll.reregister(&self.conin, self.write_token, mio::Ready::writable(), poll_opts)?;
        } else {
            poll.reregister(&self.conin, self.write_token, mio::Ready::empty(), poll_opts)?;
        }

        poll.reregister(
            self.child_watcher.event_rx(),
            self.child_event_token,
            mio::Ready::readable(),
            poll_opts,
        )?;

        Ok(())
    }

    #[inline]
    fn deregister(&mut self, poll: &mio::Poll) -> io::Result<()> {
        poll.deregister(&self.conout)?;
        poll.deregister(&self.conin)?;
        poll.deregister(self.child_watcher.event_rx())?;
        Ok(())
    }

    #[inline]
    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.conout
    }

    #[inline]
    fn read_token(&self) -> mio::Token {
        self.read_token
    }

    #[inline]
    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.conin
    }

    #[inline]
    fn write_token(&self) -> mio::Token {
        self.write_token
    }
}

impl EventedPty for Pty {
    fn child_event_token(&self) -> mio::Token {
        self.child_event_token
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        match self.child_watcher.event_rx().try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(ChildEvent::Exited),
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, size: &SizeInfo) {
        self.backend.on_resize(size)
    }
}

fn cmdline<C>(config: &Config<C>) -> String {
    let default_shell = Program::Just("powershell".to_owned());
    let shell = config.shell.as_ref().unwrap_or(&default_shell);

    once(shell.program().as_ref())
        .chain(shell.args().iter().map(|a| a.as_ref()))
        .map(|arg| quote_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote an argument for a Windows command line.
///
/// The full rules are described [in this article][parsing-args] in terms of parsing rather than
/// quoting, but the basic idea is that we need to add quotes around the argument and escape any
/// quotes inside the argument.
///
/// [parsing-args]:
/// https://docs.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments
///
/// The escaping rules are a bit insane. Each quote must be escaped with a leading backslash.
/// Backslashes must *only* be escaped if they are in a run of backslashes preceding a quote. In
/// that case, each backslash in the run must be escaped with its own backslash. All other
/// backslashes must *not* be escaped and will be interpreted literally.
///
/// # Avoiding Quoting
///
/// If we determine the argument is &ldquo;simple enough&rdquo;, that is, the argument
///   * is empty,
///   * doesn't contain a quote, and
///   * doesn't contain whitespace
/// then we don't actually need to escape anything. It's better in that case to leave the argument
/// unescaped so that programs that handle their arguments &ldquo;oddly&rdquo; have a better chance
/// of correctly parsing them.
///
/// Note that a string containing backslashes, but still meeting the above requirements, *does not*
/// need to be escaped.
///
/// # Examples
///
/// Note in the following examples that `r#"..."#` defines a raw string, so only the characters
/// between those delimiters are input to or output from the function.
///
/// ```rust
/// # use alacritty_terminal::tty::windows::quote_argument;
/// assert_eq!(quote_argument(r#"a b c"#), r#""a b c""#);
/// assert_eq!(quote_argument(r#"a"bc"#), r#""a\"bc""#);
/// assert_eq!(quote_argument(r#"a\"bc"#), r#""a\\\"bc""#);
/// assert_eq!(quote_argument(r#"a\\"bc"#), r#""a\\\\\"bc""#);
/// assert_eq!(quote_argument(r#"\abc""#), r#""\abc\""#);
/// assert_eq!(quote_argument(r#"a\\bc""#), r#""a\\bc\""#);
/// assert_eq!(quote_argument(r#"a\b"c"#), r#""a\b\"c""#);
/// assert_eq!(quote_argument(r#""abc\"#), r#""\"abc\\""#);
///
/// // Simple enough, left unescaped.
/// assert_eq!(quote_argument(r#"abc"#), r#"abc"#);
/// assert_eq!(quote_argument(r#"\abc"#), r#"\abc"#);
/// assert_eq!(quote_argument(r#"a\\bc"#), r#"a\\bc"#);
/// assert_eq!(quote_argument(r#"abc\"#), r#"abc\"#);
/// ```
fn quote_argument(arg: &str) -> String {
    // If this argument is simple enough, get out of Dodge.
    if arg.is_empty() && !arg.contains('"') && !arg.chars().any(char::is_whitespace) {
        return arg.to_owned();
    }

    // Allocate the output string, which will require *at least* as much space as the input.
    let mut output = String::with_capacity(arg.len());
    // Keep track of the number of backslashes we've seen in the current run of backslashes. If
    // zero, then we're not currently in a run of backslashes.
    let mut backslash_count: usize = 0;

    // Push the opening quote.
    output.push('"');

    for c in arg.chars() {
        if c == '\\' {
            backslash_count += 1;
        } else if c == '"' {
            // If we have a backslash run, it was actually preceding a quote, so action is
            // required. We need to double the run, plus add an extra backslash to actually escape
            // the quote (hence the inclusive bound).
            for _ in 0..=backslash_count {
                output.push('\\');
            }
            // And now we're not in a run anymore.
            backslash_count = 0;
        } else {
            backslash_count = 0;
        }

        output.push(c);
    }

    if backslash_count > 0 {
        // If we made it out of the loop in a backslash run, we need to double it because it's
        // going to be preceding a quote once we add the closing quote. This time we *do not* add
        // an extra backslash, because we do not want to escape the closing quote.
        for _ in 0..backslash_count {
            output.push('\\');
        }
    }

    // Push the closing quote.
    output.push('"');

    output
}

/// Converts the string slice into a Windows-standard representation for "W"-
/// suffixed function variants, which accept UTF-16 encoded string values.
pub fn win32_string<S: AsRef<OsStr> + ?Sized>(value: &S) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(once(0)).collect()
}
