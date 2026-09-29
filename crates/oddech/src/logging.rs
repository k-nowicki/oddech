//! The log goes to the terminal and to a file, so that a program started
//! from the desktop menu still leaves a record: `oddech.log` in the state
//! directory (`~/.local/state/oddech/`), with the previous run kept as
//! `oddech.log.1`.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;

const DEFAULT_FILTER: &str = "info,wgpu=warn,naga=warn,eframe=warn,egui_wgpu=warn,whisper_rs=warn";

/// Where the log of the current run is written.
pub fn log_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "oddech")?;
    let dir = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
    Some(dir.join("oddech.log"))
}

/// Sets up logging (filter from `RUST_LOG`) and logs panics.
pub fn init() {
    let file = log_path().and_then(|path| {
        fs::create_dir_all(path.parent()?).ok()?;
        let _ = fs::rename(&path, path.with_extension("log.1"));
        File::create(&path).ok()
    });
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(DEFAULT_FILTER))
        .target(env_logger::Target::Pipe(Box::new(Tee { file })))
        .init();
    if let Some(path) = log_path() {
        log::info!("oddech {}, log: {}", env!("CARGO_PKG_VERSION"), path.display());
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        default_hook(info);
    }));
}

/// Writes to standard error and, if it could be opened, the log file.
struct Tee {
    file: Option<File>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        if let Some(f) = &mut self.file {
            // A full disk must not stop the program.
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = io::stderr().flush();
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }
        Ok(())
    }
}
