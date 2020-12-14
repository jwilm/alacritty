use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use log::{debug, error};
use notify::{watcher, DebouncedEvent, RecursiveMode, Watcher};

use alacritty_terminal::thread;

use crate::event::{Event, EventProxy};

struct ConfigPath {
    path: PathBuf,
    symlink: Option<PathBuf>,
}

impl ConfigPath {
    fn get(path: PathBuf) -> Option<Self> {
        // Canonicalize all paths, filtering out the ones that do not exist.
        match fs::canonicalize(&path) {
            Ok(path) => {
                // Keep the symlink in case it is recreated
                let symlink =
                    if path.symlink_metadata().is_ok() { Some(path.clone()) } else { None };

                Some(ConfigPath { path, symlink })
            },
            Err(err) => {
                error!("Unable to canonicalize config path {:?}: {}", path, err);
                None
            },
        }
    }
}

pub fn watch(mut paths: Vec<PathBuf>, event_proxy: EventProxy) {
    // Canonicalize paths and eventually get symlinks
    let config_paths: Vec<ConfigPath> = paths.drain(..).filter_map(ConfigPath::get).collect();

    // Don't monitor config if there is no path to watch.
    if config_paths.is_empty() {
        return;
    }

    // The Duration argument is a debouncing period.
    let (tx, rx) = mpsc::channel();

    let mut watcher = match watcher(tx, Duration::from_millis(10)) {
        Ok(watcher) => watcher,
        Err(err) => {
            error!("Unable to watch config file: {}", err);
            return;
        },
    };

    thread::spawn_named("config watcher", move || {
        let paths = &config_paths
            .iter()
            .map(|config_path| config_path.path.clone())
            .collect::<Vec<PathBuf>>();

        let parents = get_all_unique_parent_directories(paths.as_slice());

        // Watch all configuration file directories.
        for parent in &parents {
            if let Err(err) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
                debug!("Unable to watch config directory {:?}: {}", parent, err);
            }
        }

        loop {
            let event = match rx.recv() {
                Ok(event) => event,
                Err(err) => {
                    debug!("Config watcher channel dropped unexpectedly: {}", err);
                    break;
                },
            };

            match event {
                DebouncedEvent::Rename(..) => continue,
                DebouncedEvent::NoticeRemove(path) | DebouncedEvent::Remove(path) => {
                    let path_is_known = config_paths.iter().any(|config_path| {
                        path == config_path.path || config_path.symlink == Some(path.clone())
                    });
                    if path_has_been_recreated(&path) && path_is_known {
                        if let Err(err) = watcher.watch(&path, RecursiveMode::NonRecursive) {
                            debug!("Unable to watch config directory {:?}: {}", path, err);
                        } else {
                            event_proxy.send_event(Event::ConfigReload(paths[0].clone()));
                        }
                    }
                },
                DebouncedEvent::Write(path)
                | DebouncedEvent::Create(path)
                | DebouncedEvent::Chmod(path) => {
                    if !paths.contains(&&path) {
                        continue;
                    }
                    // Always reload the primary configuration file.
                    event_proxy.send_event(Event::ConfigReload(paths[0].clone()));
                },
                _ => {},
            }
        }
    });
}

fn get_all_unique_parent_directories(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut parents = paths
        .iter()
        .map(|path| {
            let mut path = path.clone();
            path.pop();
            path
        })
        .collect::<Vec<PathBuf>>();
    parents.sort_unstable();
    parents.dedup();
    parents
}

fn path_has_been_recreated(path: &PathBuf) -> bool {
    let mut wait = 0;

    while wait < 5 {
        wait += 1;
        std::thread::sleep(Duration::from_millis(1000));

        if path.exists() {
            return true;
        }
    }

    false
}
