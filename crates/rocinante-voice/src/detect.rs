//! Can this machine listen? `/talk` checks before downloading anything, and
//! says exactly why not when it cannot: a remote session, an old CPU, no
//! microphone, or (Linux) no recording tool.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A microphone `/talk` can record from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mic {
    /// What the device calls itself, for the "listening on …" notice.
    pub name: String,
    pub(crate) backend: Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    /// The OS audio API (Core Audio, WASAPI) through cpal.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Native,
    /// `parecord`: PulseAudio, or PipeWire's Pulse layer.
    #[cfg(target_os = "linux")]
    Pulse,
    /// `arecord`: plain ALSA.
    #[cfg(target_os = "linux")]
    Alsa,
}

/// Why `/talk` cannot listen here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// An SSH session: the microphone would be the server's.
    Remote,
    /// The CPU lacks instructions the speech model's build needs.
    Cpu(String),
    /// No input device (detail says what was checked).
    NoMicrophone(String),
    /// Linux without `parecord` or `arecord`.
    NoRecorder,
    Unsupported,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Remote => f.write_str(
                "/talk can't work over SSH: it would listen to this server's microphone, not \
                 yours. Run rocinante on the computer you're speaking into.",
            ),
            Self::Cpu(why) => write!(f, "/talk can't run here: {why}"),
            Self::NoMicrophone(detail) => write!(
                f,
                "/talk found no microphone ({detail}). Connect or enable one, then /talk again."
            ),
            Self::NoRecorder => f.write_str(
                "/talk needs a recording tool on Linux: `parecord` (PulseAudio or PipeWire — \
                 package pulseaudio-utils) or `arecord` (package alsa-utils). Install one, then \
                 /talk again.",
            ),
            Self::Unsupported => f.write_str("/talk isn't supported on this platform yet."),
        }
    }
}

/// Run every check; the first failure wins.
pub fn check() -> Result<Mic, Unavailable> {
    if is_remote(|k| std::env::var(k).ok()) {
        return Err(Unavailable::Remote);
    }
    if let Some(missing) = rocinante_llama::missing_cpu_features() {
        return Err(Unavailable::Cpu(format!(
            "this CPU lacks {missing}, which Rocinante's speech recognition needs (x86-64 from \
             2013 on)"
        )));
    }
    find_mic()
}

/// Speakers `/talk` can read replies through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Speakers {
    pub name: String,
    pub(crate) backend: Backend,
}

/// Find an output device; `Err` says why there is none (replies then stay
/// on screen only).
pub fn find_speakers() -> Result<Speakers, String> {
    if is_remote(|k| std::env::var(k).ok()) {
        return Err("over SSH the speakers would be the server's".into());
    }
    find_output()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn find_output() -> Result<Speakers, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("the system has no default output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "default speakers".into());
    Ok(Speakers {
        name,
        backend: Backend::Native,
    })
}

#[cfg(target_os = "linux")]
fn find_output() -> Result<Speakers, String> {
    if which("pacat").is_some()
        && let Some(out) = run("pactl", &["list", "short", "sinks"])
    {
        let sinks = pulse_sinks(&out);
        if sinks.is_empty() {
            return Err("the sound server lists no outputs".into());
        }
        let name = run("pactl", &["get-default-sink"])
            .map(|s| s.trim().to_string())
            .filter(|s| sinks.contains(s))
            .unwrap_or_else(|| sinks[0].clone());
        return Ok(Speakers {
            name,
            backend: Backend::Pulse,
        });
    }
    if which("aplay").is_some() {
        return match run("aplay", &["-l"]) {
            Some(out) if alsa_has_cards(&out) => Ok(Speakers {
                name: "ALSA default output".into(),
                backend: Backend::Alsa,
            }),
            _ => Err("ALSA lists no playback hardware".into()),
        };
    }
    Err("no playback tool: install pulseaudio-utils (pacat) or alsa-utils (aplay)".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn find_output() -> Result<Speakers, String> {
    Err("not supported on this platform".into())
}

/// Output sink names from `pactl list short sinks` (a headless server has
/// none, or only PipeWire's dummy `auto_null`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pulse_sinks(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "auto_null")
        .map(String::from)
        .collect()
}

/// SSH sets these in the remote shell (and tmux/screen inherit them).
pub(crate) fn is_remote(var: impl Fn(&str) -> Option<String>) -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|k| var(k).is_some_and(|v| !v.trim().is_empty()))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn find_mic() -> Result<Mic, Unavailable> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| {
        Unavailable::NoMicrophone("the system has no default input device".into())
    })?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "default microphone".into());
    Ok(Mic {
        name,
        backend: Backend::Native,
    })
}

#[cfg(target_os = "linux")]
fn find_mic() -> Result<Mic, Unavailable> {
    let parecord = which("parecord").is_some();
    let arecord = which("arecord").is_some();
    if parecord && let Some(out) = run("pactl", &["list", "short", "sources"]) {
        let sources = pulse_sources(&out);
        if sources.is_empty() {
            return Err(Unavailable::NoMicrophone(
                "the sound server lists no input sources".into(),
            ));
        }
        let name = run("pactl", &["get-default-source"])
            .map(|s| s.trim().to_string())
            .filter(|s| sources.contains(s))
            .unwrap_or_else(|| sources[0].clone());
        return Ok(Mic {
            name,
            backend: Backend::Pulse,
        });
    }
    if arecord {
        return match run("arecord", &["-l"]) {
            Some(out) if alsa_has_cards(&out) => Ok(Mic {
                name: "ALSA default capture device".into(),
                backend: Backend::Alsa,
            }),
            _ => Err(Unavailable::NoMicrophone(
                "ALSA lists no capture hardware".into(),
            )),
        };
    }
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    if cards.trim().is_empty() || cards.contains("no soundcards") {
        return Err(Unavailable::NoMicrophone(
            "this machine has no sound card".into(),
        ));
    }
    Err(Unavailable::NoRecorder)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn find_mic() -> Result<Mic, Unavailable> {
    Err(Unavailable::Unsupported)
}

/// Input source names from `pactl list short sources`, minus the
/// `.monitor` sources (what the speakers play, not a microphone).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pulse_sources(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.ends_with(".monitor"))
        .map(String::from)
        .collect()
}

/// `arecord -l` lists `card N: …` lines when capture hardware exists.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn alsa_has_cards(out: &str) -> bool {
    out.lines().any(|l| l.trim_start().starts_with("card "))
}

/// A program on PATH.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// Run a short query command; its stdout, or None if it is missing, fails,
/// or takes over 3 s (a wedged sound server must not hang `/talk`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    use std::io::Read;
                    let _ = stdout.read_to_string(&mut out);
                }
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_sessions_are_remote() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(is_remote(env(&[(
            "SSH_CONNECTION",
            "10.0.0.2 5000 10.0.0.1 22"
        )])));
        assert!(is_remote(env(&[("SSH_TTY", "/dev/pts/3")])));
        assert!(!is_remote(env(&[("SSH_TTY", "  ")])));
        assert!(!is_remote(env(&[("TERM", "xterm")])));
    }

    #[test]
    fn pulse_sources_skip_monitors() {
        let out = "0\talsa_output.pci-0000_00_1f.3.analog-stereo.monitor\tmodule-alsa-card.c\ts16le 2ch 44100Hz\tSUSPENDED\n\
                   1\talsa_input.usb-Blue_Yeti-00.analog-stereo\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tRUNNING\n";
        assert_eq!(
            pulse_sources(out),
            vec!["alsa_input.usb-Blue_Yeti-00.analog-stereo".to_string()]
        );
        // A headless server: only the null sink's monitor.
        assert!(
            pulse_sources("0\tauto_null.monitor\tmodule-null-sink.c\ts16le 2ch 44100Hz\tIDLE\n")
                .is_empty()
        );
        assert!(pulse_sources("").is_empty());
    }

    #[test]
    fn pulse_sinks_skip_the_null_sink() {
        let out = "0\tauto_null\tmodule-null-sink.c\ts16le 2ch 44100Hz\tIDLE\n\
                   1\talsa_output.pci-0000_00_1f.3.analog-stereo\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tSUSPENDED\n";
        assert_eq!(
            pulse_sinks(out),
            vec!["alsa_output.pci-0000_00_1f.3.analog-stereo".to_string()]
        );
        assert!(
            pulse_sinks("0\tauto_null\tmodule-null-sink.c\ts16le 2ch 44100Hz\tIDLE\n").is_empty()
        );
    }

    #[test]
    fn alsa_capture_listing() {
        let out = "**** List of CAPTURE Hardware Devices ****\n\
                   card 0: PCH [HDA Intel PCH], device 0: ALC3246 Analog [ALC3246 Analog]\n";
        assert!(alsa_has_cards(out));
        assert!(!alsa_has_cards(
            "arecord: device_list:279: no soundcards found...\n"
        ));
    }

    #[test]
    fn unavailable_messages_say_what_to_do() {
        assert!(Unavailable::Remote.to_string().contains("SSH"));
        assert!(Unavailable::NoRecorder.to_string().contains("alsa-utils"));
        assert!(
            Unavailable::NoMicrophone("x".into())
                .to_string()
                .contains("/talk again")
        );
    }
}
