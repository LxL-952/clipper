use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct Quality {
    /// Classe de hauteur : 1080, 1440 ou 2160 (la largeur suit le ratio de l'écran).
    pub height: u32,
    /// 30 ou 60.
    pub fps: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub buffer_secs: u32,
    pub clip: Quality,
    pub rec: Quality,
    pub encoder: String,
    /// Multiplicateur de débit, 0.5 à 1.5.
    pub quality: f32,
    /// "mp4", "mkv" ou "mov" : commun au Clip et au REC.
    pub container: String,
    /// "none", "mixed" (1 piste) ou "split" (2 pistes).
    pub audio_mode: String,
    pub out_device: Option<String>,
    pub mic_device: Option<String>,
    pub game_volume: f32,
    pub mic_volume: f32,
    pub mic_muted: bool,
    pub key_clip: String,
    pub key_rec: String,
    pub key_bar: String,
    pub out_dir: PathBuf,
    pub ffmpeg: String,
    /// "game_ready" ou "studio".
    pub driver_branch: String,
    /// "auto", "dgpu" (carte graphique) ou "igpu" (puce intégrée AMD/Intel).
    pub encode_chip: String,
    /// Index de sortie DXGI pour ddagrab.
    pub monitor: u32,
    /// "off", "16:9" ou "16:10".
    pub stretch: String,
    pub desktop_capture: bool,
    pub buffer_dir: PathBuf,
    /// "hidden", "hotkey" ou "always".
    pub overlay_mode: String,
    pub key_overlay: String,
    pub overlay_items: Vec<String>,
    /// "tl", "tr", "bl" ou "br".
    pub overlay_corner: String,
    /// "s", "m" ou "l".
    pub overlay_size: String,
    /// 0.0 à 1.0.
    pub overlay_opacity: f32,
}

impl Default for Config {
    fn default() -> Self {
        let videos = std::env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Videos"))
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            buffer_secs: 180,
            clip: Quality { height: 1080, fps: 60 },
            rec: Quality { height: 1080, fps: 60 },
            encoder: "h264_nvenc".into(),
            quality: 1.0,
            container: "mp4".into(),
            audio_mode: "mixed".into(),
            out_device: None,
            mic_device: None,
            game_volume: 1.0,
            mic_volume: 1.0,
            mic_muted: false,
            key_clip: "Alt+KeyX".into(),
            key_rec: "Alt+KeyR".into(),
            key_bar: "Alt+KeyZ".into(),
            out_dir: videos.join("Clipper"),
            ffmpeg: "ffmpeg".into(),
            driver_branch: "game_ready".into(),
            encode_chip: "auto".into(),
            monitor: 0,
            stretch: "off".into(),
            desktop_capture: true,
            buffer_dir: std::env::temp_dir().join("Clipper").join("buf"),
            overlay_mode: "hotkey".into(),
            key_overlay: "Alt+KeyF".into(),
            overlay_items: vec!["fps".into(), "frametime".into(), "cpu".into(), "gpu".into()],
            overlay_corner: "tl".into(),
            overlay_size: "m".into(),
            overlay_opacity: 0.7,
        }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Clipper")
}

impl Config {
    /// Fichier absent ou illisible : valeurs par défaut (jamais de panic au démarrage).
    pub fn load() -> Self {
        std::fs::read_to_string(dir().join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let tmp = dir().join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, dir().join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_keeps_defaults() {
        let c: Config = serde_json::from_str(r#"{"buffer_secs":60,"container":"mkv"}"#).unwrap();
        assert_eq!(c.buffer_secs, 60);
        assert_eq!(c.container, "mkv");
        assert_eq!(c.clip, Quality { height: 1080, fps: 60 });
    }
}
