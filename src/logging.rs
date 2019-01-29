// Copyright 2016 Joe Wilm, The Alacritty Project Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
//! Logging for alacritty.
//!
//! The main executable is supposed to call `initialize()` exactly once during
//! startup. All logging messages are written to stdout, given that their
//! log-level is sufficient for the level configured in `cli::Options`.
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, LineWriter, Stdout, Write};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use log::{self, Level};
use time;

use crate::cli;
use crate::message_bar::MessageBar;

pub fn initialize(
    options: &cli::Options,
    message_bar: MessageBar,
) -> Result<(), log::SetLoggerError> {
    // Use env_logger if RUST_LOG environment variable is defined. Otherwise,
    // use the alacritty-only logger.
    if ::std::env::var("RUST_LOG").is_ok() {
        ::env_logger::try_init()?;
    } else {
        let logger = Logger::new(options.log_level, message_bar);
        log::set_boxed_logger(Box::new(logger))?;
    }
    Ok(())
}

pub struct Logger {
    level: log::LevelFilter,
    logfile: Mutex<OnDemandLogFile>,
    stdout: Mutex<LineWriter<Stdout>>,
    message_bar: Mutex<MessageBar>,
}

impl Logger {
    // False positive, see: https://github.com/rust-lang-nursery/rust-clippy/issues/734
    #[allow(clippy::new_ret_no_self)]
    fn new(level: log::LevelFilter, message_bar: MessageBar) -> Self {
        log::set_max_level(level);

        let logfile = Mutex::new(OnDemandLogFile::new());
        let stdout = Mutex::new(LineWriter::new(io::stdout()));
        let message_bar = Mutex::new(message_bar);

        Logger {
            level,
            logfile,
            stdout,
            message_bar,
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) && record.target().starts_with("alacritty") {
            let now = time::strftime("%F %R", &time::now()).unwrap();

            let msg = if record.level() >= Level::Trace {
                format!(
                    "[{}] [{}] [{}:{}] {}\n",
                    now,
                    record.level(),
                    record.file().unwrap_or("?"),
                    record
                        .line()
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "?".into()),
                    record.args()
                )
            } else {
                format!("[{}] [{}] {}\n", now, record.level(), record.args())
            };

            if let Ok(ref mut logfile) = self.logfile.lock() {
                let _ = logfile.write_all(msg.as_ref());

                if let Ok(ref mut message_bar) = self.message_bar.lock() {
                    let msg = format!("Error! See log at {}", logfile.path.to_string_lossy());
                    match record.level() {
                        Level::Error => {
                            let _ = message_bar.push(msg, crate::RED);
                        }
                        Level::Warn => {
                            let _ = message_bar.push(msg, crate::YELLOW);
                        }
                        _ => (),
                    }
                }
            }

            if let Ok(ref mut stdout) = self.stdout.lock() {
                let _ = stdout.write_all(msg.as_ref());
            }
        }
    }

    fn flush(&self) {}
}

struct OnDemandLogFile {
    file: Option<LineWriter<File>>,
    created: Arc<AtomicBool>,
    path: PathBuf,
}

impl Drop for OnDemandLogFile {
    fn drop(&mut self) {
        // TODO: Check for persistent logging again
        if self.created.load(Ordering::Relaxed) && fs::remove_file(&self.path).is_ok() {
            let _ = writeln!(io::stdout(), "Deleted log file at {:?}", self.path);
        }
    }
}

impl OnDemandLogFile {
    fn new() -> Self {
        let mut path = env::temp_dir();
        path.push(format!("Alacritty-{}.log", process::id()));

        OnDemandLogFile {
            path,
            file: None,
            created: Arc::new(AtomicBool::new(false)),
        }
    }

    fn file(&mut self) -> Result<&mut LineWriter<File>, io::Error> {
        // Allow to recreate the file if it has been deleted at runtime
        if self.file.is_some() && !self.path.as_path().exists() {
            self.file = None;
        }

        // Create the file if it doesn't exist yet
        if self.file.is_none() {
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.path);

            match file {
                Ok(file) => {
                    self.file = Some(io::LineWriter::new(file));
                    self.created.store(true, Ordering::Relaxed);
                    let _ = writeln!(io::stdout(), "Created log file at {:?}", self.path);
                }
                Err(e) => {
                    let _ = writeln!(io::stdout(), "Unable to create log file: {}", e);
                    return Err(e);
                }
            }
        }

        Ok(self.file.as_mut().unwrap())
    }
}

impl Write for OnDemandLogFile {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.file()?.write(buf)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        self.file()?.flush()
    }
}
