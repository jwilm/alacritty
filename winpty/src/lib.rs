#[macro_use]
extern crate bitflags;
extern crate widestring;
extern crate winpty_sys;

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::result::Result;
use std::os::windows::io::RawHandle;
use std::ptr::{null, null_mut};
use fmt::{Display, Formatter};

use winpty_sys::*;

use widestring::WideCString;

pub enum ErrorCodes {
    Success,
    OutOfMemory,
    SpawnCreateProcessFailed,
    LostConnection,
    AgentExeMissing,
    Unspecified,
    AgentDied,
    AgentTimeout,
    AgentCreationFailed,
}
pub enum MouseMode {
    None,
    Auto,
    Force,
}
bitflags!(
    pub struct SpawnFlags: u64 {
        const AUTO_SHUTDOWN = 0x1;
        const EXIT_AFTER_SHUTDOWN = 0x2;
    }
);
bitflags!(
    pub struct ConfigFlags: u64 {
        const CONERR = 0x1;
        const PLAIN_OUTPUT = 0x2;
        const COLOR_ESCAPES = 0x4;
    }
);

#[derive(Debug)]
pub struct Err<'a> {
    ptr: &'a mut winpty_error_t,
    code: u32,
    message: String,
}

// Check to see whether winpty gave us an error
fn check_err<'a>(e: *mut winpty_error_t) -> Option<Err<'a>> {
    let err = unsafe {
        let raw = winpty_error_msg(e);
        Err {
            ptr: &mut *e,
            code: winpty_error_code(e),
            message: String::from_utf16_lossy(std::slice::from_raw_parts(raw, wcslen(raw))),
        }
    };
    if err.code != 0 {
        Some(err)
    } else {
        None
    }
}

impl<'a> Drop for Err<'a> {
    fn drop(&mut self) {
        unsafe {
            winpty_error_free(self.ptr);
        }
    }
}
impl<'a> Display for Err<'a> {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        write!(f, "Code: {}, Message: {}", self.code, self.message)
    }
}
impl<'a> Error for Err<'a> {
    fn description(&self) -> &str {
        &self.message
    }
}

#[derive(Debug)]
/// Winpty agent config
pub struct Config<'a>(&'a mut winpty_config_t);

impl<'a, 'b> Config<'a> {
    pub fn new(flags: ConfigFlags) -> Result<Self, Err<'b>> {
        let mut err = null_mut() as *mut winpty_error_t;
        let config = unsafe { winpty_config_new(flags.bits(), &mut err) };

        if let Some(err) = check_err(err) {
            Result::Err(err)
        } else {
            unsafe { Ok(Config(&mut *config)) }
        }
    }

    /// Set the initial size of the console window
    pub fn set_initial_size(&mut self, cols: i32, rows: i32) {
        unsafe {
            winpty_config_set_initial_size(self.0, cols, rows);
        }
    }

    /// Set the mouse mode
    pub fn set_mouse_mode(&mut self, mode: MouseMode) {
        let m = match mode {
            MouseMode::None => 0,
            MouseMode::Auto => 1,
            MouseMode::Force => 2,
        };
        unsafe {
            winpty_config_set_mouse_mode(self.0, m);
        }
    }

    /// Amount of time to wait for the agent to startup and to wait for any given
    /// agent RPC request.  Must be greater than 0.  Can be INFINITE.
    // Might be a better way to represent this while still retaining infinite capability?
    // Enum?
    pub fn set_agent_timeout(&mut self, timeout: u32) {
        unsafe {
            winpty_config_set_agent_timeout(self.0, timeout);
        }
    }
}

impl<'a> Drop for Config<'a> {
    fn drop(&mut self) {
        unsafe {
            winpty_config_free(self.0);
        }
    }
}

#[derive(Debug)]
/// A struct representing the winpty agent process
pub struct Winpty<'a>(&'a mut winpty_t);

impl<'a, 'b> Winpty<'a> {
    /// Starts the agent. This process will connect to the agent 
    /// over a control pipe, and the agent will open data pipes 
    /// (e.g. CONIN and CONOUT).
    pub fn open(cfg: &Config) -> Result<Self, Err<'b>> {
        let mut err = null_mut() as *mut winpty_error_t;
        unsafe {
            let winpty = winpty_open(cfg.0, &mut err);
            let err = check_err(err);
            if let Some(err) = err {
                Result::Err(err)
            } else {
                Ok(Winpty(&mut *winpty))
            }
        }
    }

    /// Returns the handle to the winpty agent process
    pub fn raw_handle(&mut self) -> RawHandle {
        unsafe { winpty_agent_process(self.0) }
    }

    /// Returns the name of the input pipe.
    /// Pipe is half-duplex.
    pub fn conin_name(&mut self) -> PathBuf {
        unsafe {
            let raw = winpty_conin_name(self.0);
            PathBuf::from(&String::from_utf16_lossy(std::slice::from_raw_parts(
                raw,
                wcslen(raw),
            )))
        }
    }

    /// Returns the name of the output pipe.
    /// Pipe is half-duplex.
    pub fn conout_name(&mut self) -> PathBuf {
        unsafe {
            let raw = winpty_conout_name(self.0);
            PathBuf::from(&String::from_utf16_lossy(std::slice::from_raw_parts(
                raw,
                wcslen(raw),
            )))
        }
    }

    /// Returns the name of the error pipe.
    /// The name will only be valid if ConfigFlags::CONERR was specified.
    /// Pipe is half-duplex.
    pub fn conerr_name(&mut self) -> PathBuf {
        unsafe {
            let raw = winpty_conerr_name(self.0);
            PathBuf::from(&String::from_utf16_lossy(std::slice::from_raw_parts(
                raw,
                wcslen(raw),
            )))
        }
    }

    /// Change the size of the Windows console window.
    pub fn set_size(&mut self, cols: usize, rows: usize) -> Result<(), Err> {
        let mut err = null_mut() as *mut winpty_error_t;

        unsafe {
            winpty_set_size(self.0, cols as i32, rows as i32, &mut err);
        }

        if let Some(err) = check_err(err) {
            Result::Err(err)
        } else {
            Ok(())
        }
    }

    /// Gets a list of processes attached to the console.
    /// Currently unimplemented
    // TODO: Implement
    pub fn console_process_list(&mut self) -> Result<Vec<u32>, Err> {
        unimplemented!();
    }

    /// Spawns the new process.
    ///
    /// spawn can only be called once per Winpty object.  If it is called
    /// before the output data pipe(s) is/are connected, then collected output is
    /// buffered until the pipes are connected, rather than being discarded.
    /// (https://blogs.msdn.microsoft.com/oldnewthing/20110107-00/?p=11803)
    // Decide whether this should return a new object and if so should it have the pipe methods
    // TODO: Support getting the process and thread handle of the spawned process (Not the agent)
    // TODO: Support returning the error from CreateProcess
    pub fn spawn(
        &mut self,
        cfg: &SpawnConfig,
    ) -> Result<(), Err> {
        let mut err = null_mut() as *mut winpty_error_t;

        unsafe {
            let ok = winpty_spawn(
                self.0,
                cfg.0 as *const winpty_spawn_config_s,
                null_mut(), // Process handle
                null_mut(), // Thread handle
                null_mut(), // Create process error
                &mut err,
            );
            if ok == 0 { return Ok(());}
        }

        if let Some(err) = check_err(err) {
            Result::Err(err)
        } else {
            Ok(())
        }
    }
}
unsafe impl<'a> Sync for Winpty<'a> {}
unsafe impl<'a> Send for Winpty<'a> {}

impl<'a> Drop for Winpty<'a> {
    fn drop(&mut self) {
        unsafe {
            winpty_free(self.0);
        }
    }
}

#[derive(Debug)]
/// Information about a process for winpty to spawn
pub struct SpawnConfig<'a>(&'a mut winpty_spawn_config_t);

impl<'a, 'b> SpawnConfig<'a> {
    /// Creates a new spawnconfig
    pub fn new(
        spawnflags: SpawnFlags,
        appname: Option<&str>,
        cmdline: Option<&str>,
        cwd: Option<&str>,
        end: Option<&str>,
    ) -> Result<Self, Err<'b>> {
        let mut err = null_mut() as *mut winpty_error_t;
        let (appname, cmdline, cwd, end) = (
            appname.map_or(null(), |s| WideCString::from_str(s).unwrap().into_raw()),
            cmdline.map_or(null(), |s| WideCString::from_str(s).unwrap().into_raw()),
            cwd.map_or(null(), |s| WideCString::from_str(s).unwrap().into_raw()),
            end.map_or(null(), |s| WideCString::from_str(s).unwrap().into_raw()),
        );

        let spawn_config = unsafe {
            winpty_spawn_config_new(spawnflags.bits(), appname, cmdline, cwd, end, &mut err)
        };

        // Required to free the strings
        unsafe {
            if appname != null() {
                WideCString::from_raw(appname as *mut u16);
            }
            if cmdline != null() {
                WideCString::from_raw(cmdline as *mut u16);
            }
            if cwd != null() {
                WideCString::from_raw(cwd as *mut u16);
            }
            if end != null() {
                WideCString::from_raw(end as *mut u16);
            }
        }

        if let Some(err) = check_err(err) {
            Result::Err(err)
        } else {
            unsafe { Ok(SpawnConfig(&mut *spawn_config)) }
        }
    }
}
impl<'a> Drop for SpawnConfig<'a> {
    fn drop(&mut self) {
        unsafe {
            winpty_spawn_config_free(self.0);
        }
    }
}
