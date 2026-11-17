//! The model files `/talk` needs, fetched on first use into
//! `~/.rocinante/voice/` and checked against pinned SHA-256 hashes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// One downloadable model file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// The Silero voice-activity model: tells speech from silence and noise.
pub const VAD: ModelFile = ModelFile {
    file: "ggml-silero-v6.2.0.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin",
    sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    bytes: 885_098,
};

/// Phonon 31M English (Gradium, Apache-2.0): the voice that reads replies
/// aloud. Files pinned to one revision of the Hugging Face repo; the 4-bit
/// weights sound the same as the 8-bit ones and are 9 MB smaller.
pub const PHONON_DIR: &str = "phonon-31m-en";
pub const PHONON: [ModelFile; 4] = [
    ModelFile {
        file: "phonon-31m-en/config.json",
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/config.json",
        sha256: "b5d0f88353638370d94035b7d73d7d6577c0554d78f8d7ecfd40490aae39ae85",
        bytes: 4_634,
    },
    ModelFile {
        file: "phonon-31m-en/tokenizer.json",
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/tokenizer.json",
        sha256: "691a00068fa7b989f2f3e5ae5daa2d1a5bb319f2d5360deafcfb19a8bb6e7936",
        bytes: 277_967,
    },
    ModelFile {
        file: "phonon-31m-en/model.q4k.gguf",
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/model.q4k.gguf",
        sha256: "b89e867e995b45835cbe30564fe360178fb39f1fe457c13e1e559c2e4260e4f3",
        bytes: 55_075_168,
    },
    ModelFile {
        file: "phonon-31m-en/default-voice.safetensors",
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/default-voice.safetensors",
        sha256: "dc537478eaadc1a5257fcda2e3501ab81e14c111a5d95a5838f350db2afd0f1a",
        bytes: 256_136,
    },
];

/// English whisper.cpp models, smallest first (`[voice] stt_model`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttModel {
    /// Fastest; for older machines. Misses more words.
    Tiny,
    /// The default: accurate for conversation, well under a second per
    /// sentence on a laptop CPU.
    #[default]
    Base,
    /// Best accuracy; about 3× slower than base.
    Small,
}

impl SttModel {
    pub const NAMES: [&'static str; 3] = ["tiny.en", "base.en", "small.en"];

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "tiny" | "tiny.en" => Some(Self::Tiny),
            "base" | "base.en" => Some(Self::Base),
            "small" | "small.en" => Some(Self::Small),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Tiny => "tiny.en",
            Self::Base => "base.en",
            Self::Small => "small.en",
        }
    }

    pub fn file(self) -> ModelFile {
        match self {
            Self::Tiny => ModelFile {
                file: "ggml-tiny.en-q5_1.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q5_1.bin",
                sha256: "c77c5766f1cef09b6b7d47f21b546cbddd4157886b3b5d6d4f709e91e66c7c2b",
                bytes: 32_166_155,
            },
            Self::Base => ModelFile {
                file: "ggml-base.en-q5_1.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin",
                sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
                bytes: 59_721_011,
            },
            Self::Small => ModelFile {
                file: "ggml-small.en-q5_1.bin",
                url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en-q5_1.bin",
                sha256: "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30",
                bytes: 190_098_681,
            },
        }
    }
}

/// `~/.rocinante/voice`.
pub fn dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join("voice"))
}

/// The file's path in `dir` when it is already there in full. Size is the
/// check: the hash was verified before the file got its final name.
pub fn installed(dir: &Path, model: &ModelFile) -> Option<PathBuf> {
    let path = dir.join(model.file);
    let len = std::fs::metadata(&path).ok()?.len();
    (len == model.bytes).then_some(path)
}

/// Bytes still to download for `models` (0 = all present).
pub fn missing_bytes(dir: &Path, models: &[ModelFile]) -> u64 {
    models
        .iter()
        .filter(|m| installed(dir, m).is_none())
        .map(|m| m.bytes)
        .sum()
}

/// Make sure `model` is in `dir`, downloading it if needed. `progress` gets
/// (bytes so far, total) as the download runs. The file is written under a
/// temporary name and only renamed into place after its hash matches.
pub async fn fetch(
    dir: &Path,
    model: &ModelFile,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    if let Some(path) = installed(dir, model) {
        return Ok(path);
    }
    // `file` may name a subfolder (`phonon-31m-en/config.json`).
    let path = dir.join(model.file);
    let parent = path.parent().unwrap_or(dir).to_path_buf();
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| model.file.to_string());
    tokio::fs::create_dir_all(&parent)
        .await
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("rocinante/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("cannot build an HTTP client: {e}"))?;
    let resp = client
        .get(model.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("downloading {} failed: {e}", model.file))?;

    let part = parent.join(format!(".{base}.part"));
    let mut out = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut body = resp.bytes_stream();
    let result: Result<(), String> = async {
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| format!("downloading {} failed: {e}", model.file))?;
            hasher.update(&chunk);
            out.write_all(&chunk)
                .await
                .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
            done += chunk.len() as u64;
            progress(done, model.bytes);
        }
        out.flush()
            .await
            .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let actual = hex(&hasher.finalize());
        if actual != model.sha256 {
            return Err(format!(
                "{} failed its checksum (got {actual}, expected {}) — not installed",
                model.file, model.sha256
            ));
        }
        Ok(())
    }
    .await;
    drop(out);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, &path)
        .await
        .map_err(|e| format!("cannot move {} into place: {e}", model.file))?;
    Ok(path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// "57 MB" style size for notices.
pub fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_names() {
        assert_eq!(SttModel::parse("base.en"), Some(SttModel::Base));
        assert_eq!(SttModel::parse(" Tiny "), Some(SttModel::Tiny));
        assert_eq!(SttModel::parse("small"), Some(SttModel::Small));
        assert_eq!(SttModel::parse("large-v3"), None);
        for name in SttModel::NAMES {
            assert_eq!(SttModel::parse(name).unwrap().name(), name);
        }
    }

    #[test]
    fn pins_look_like_sha256_and_https() {
        let mut all = vec![
            VAD,
            SttModel::Tiny.file(),
            SttModel::Base.file(),
            SttModel::Small.file(),
        ];
        all.extend(PHONON);
        for m in all {
            assert_eq!(m.sha256.len(), 64, "{}", m.file);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.url.starts_with("https://"));
            let base = m.file.rsplit('/').next().unwrap();
            assert!(m.url.ends_with(&format!("/{base}")), "{}", m.file);
            assert!(m.bytes > 0);
        }
        for m in PHONON {
            assert!(m.file.starts_with(&format!("{PHONON_DIR}/")));
        }
    }

    #[test]
    fn installed_checks_the_size() {
        let dir = tempfile::tempdir().unwrap();
        let model = ModelFile {
            file: "m.bin",
            url: "https://example.invalid/m.bin",
            sha256: "00",
            bytes: 3,
        };
        assert_eq!(installed(dir.path(), &model), None);
        assert_eq!(missing_bytes(dir.path(), &[model]), 3);
        std::fs::write(dir.path().join("m.bin"), b"ab").unwrap();
        assert_eq!(installed(dir.path(), &model), None, "partial file");
        std::fs::write(dir.path().join("m.bin"), b"abc").unwrap();
        assert_eq!(
            installed(dir.path(), &model),
            Some(dir.path().join("m.bin"))
        );
        assert_eq!(missing_bytes(dir.path(), &[model]), 0);
    }

    #[test]
    fn megabytes_rounds_up() {
        assert_eq!(megabytes(59_721_011), "60 MB");
        assert_eq!(megabytes(885_098), "1 MB");
    }

    /// Downloads 5 KB from Hugging Face:
    /// `cargo test -p rocinante-voice -- --ignored fetch_into`.
    #[tokio::test]
    #[ignore]
    async fn fetch_into_a_subfolder_verifies_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let config = PHONON[0];
        let mut seen = 0;
        let path = fetch(dir.path(), &config, |done, total| {
            assert_eq!(total, config.bytes);
            seen = done;
        })
        .await
        .unwrap();
        assert_eq!(path, dir.path().join("phonon-31m-en/config.json"));
        assert_eq!(seen, config.bytes);
        assert_eq!(installed(dir.path(), &config), Some(path.clone()));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("config.json")]);
        // A wrong pin is refused and nothing is left behind.
        let bad = ModelFile {
            file: "phonon-31m-en/tokenizer.json",
            sha256: "00",
            ..PHONON[1]
        };
        let err = fetch(dir.path(), &bad, |_, _| {}).await.unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        assert!(installed(dir.path(), &bad).is_none());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
