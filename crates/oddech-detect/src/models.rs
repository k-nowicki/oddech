//! Where models and libraries live; download of speech models (`whisper`
//! feature).

use std::path::{Path, PathBuf};

#[cfg(feature = "whisper")]
pub use speech::*;

/// The application directory: models and libraries are part of the
/// application, not of the user, so they live beside the executable
/// (`<app>/models`, `<app>/lib`) wherever it is installed.
///
/// `ODDECH_HOME` overrides it. A development build (`<repo>/target/<profile>/oddech`)
/// uses `<repo>/dist`, which is laid out like an installed application.
pub fn app_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ODDECH_HOME") {
        return PathBuf::from(dir);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    if exe_dir.join("models").is_dir() {
        return exe_dir;
    }
    // target/<profile>/ or target/<profile>/examples/
    exe_dir
        .ancestors()
        .find(|d| d.file_name().is_some_and(|n| n == "target"))
        .and_then(Path::parent)
        .map(|repo| repo.join("dist"))
        .filter(|d| d.is_dir())
        .unwrap_or(exe_dir)
}

pub fn models_dir() -> PathBuf {
    app_dir().join("models")
}

pub fn lib_dir() -> PathBuf {
    app_dir().join("lib")
}

#[cfg(feature = "whisper")]
mod speech {
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde::{Deserialize, Serialize};

    use super::models_dir;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum WhisperModel {
        LargeV3Turbo,
        LargeV3,
    }

    impl WhisperModel {
        pub fn file_name(self) -> &'static str {
            match self {
                Self::LargeV3Turbo => "ggml-large-v3-turbo.bin",
                Self::LargeV3 => "ggml-large-v3.bin",
            }
        }

        pub fn id(self) -> &'static str {
            match self {
                Self::LargeV3Turbo => "large-v3-turbo",
                Self::LargeV3 => "large-v3",
            }
        }

        pub fn url(self) -> String {
            format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", self.file_name())
        }

        /// Approximate size, for the UI before the download starts.
        pub fn approx_bytes(self) -> u64 {
            match self {
                Self::LargeV3Turbo => 1_624_555_275,
                Self::LargeV3 => 3_095_033_483,
            }
        }

        /// Where the model lives (`<app>/models/…`, see [`app_dir`]).
        pub fn path(self) -> PathBuf {
            models_dir().join(self.file_name())
        }

        pub fn is_present(self) -> bool {
            fs::metadata(self.path()).is_ok_and(|m| m.len() > 1_000_000)
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum DownloadError {
        #[error("błąd sieci: {0}")]
        Http(String),
        #[error("błąd zapisu: {0}")]
        Io(#[from] io::Error),
        #[error("przerwano")]
        Cancelled,
    }

    /// Downloads `model` if missing. Resumes a partial `.part` file. `progress`
    /// receives `(downloaded, total)` bytes.
    pub fn ensure_model(
        model: WhisperModel,
        progress: &dyn Fn(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<PathBuf, DownloadError> {
        let path = model.path();
        if model.is_present() {
            return Ok(path);
        }
        fs::create_dir_all(models_dir())?;
        let part = path.with_extension("bin.part");
        let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

        let mut req = ureq::get(&model.url());
        if have > 0 {
            req = req.header("Range", &format!("bytes={have}-"));
        }
        let mut resp = req.call().map_err(|e| DownloadError::Http(e.to_string()))?;
        let resumed = resp.status().as_u16() == 206;
        let remaining: Option<u64> = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let (mut done, total) = if resumed {
            (have, remaining.map_or(model.approx_bytes(), |r| r + have))
        } else {
            (0, remaining.unwrap_or(model.approx_bytes()))
        };
        let mut file = if resumed {
            OpenOptions::new().append(true).open(&part)?
        } else {
            File::create(&part)?
        };
        let mut reader = resp.body_mut().as_reader();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        file.sync_all()?;
        drop(file);
        if done < total {
            return Err(DownloadError::Http(format!("pobrano {done} z {total} bajtów")));
        }
        fs::rename(&part, &path)?;
        Ok(path)
    }
}
