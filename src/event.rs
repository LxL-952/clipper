//! Messages de l'app (main.rs) vers la fenêtre (`Front::on`) et la barre Alt+Z (`BarUi::on`) : le pendant de
//! cmd.rs dans l'autre sens. Avant : des objets JSON `{"type": "…"}`, reste de l'ancien `window.__on` de la
//! page WebView2 ; une faute de frappe dans un nom ou un champ ne se voyait qu'à l'exécution (message ignoré,
//! champ lu vide). Le compilateur vérifie maintenant chaque message, chaque champ, et que chaque fenêtre
//! traite ce qu'elle doit.
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Couleur et rôle d'une notification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Toast {
    Ok,
    Error,
    /// Clip enregistré (ouvrable d'un clic).
    Clip,
    /// REC enregistré (ouvrable d'un clic).
    Rec,
}

impl Toast {
    /// Nom utilisé par le dessin des notifications (front.rs).
    pub fn name(self) -> &'static str {
        match self {
            Toast::Ok => "ok",
            Toast::Error => "error",
            Toast::Clip => "clip",
            Toast::Rec => "rec",
        }
    }
}

/// Raccourcis actifs, en texte lisible (« Alt+X ») ; `None` : aucun.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hotkeys {
    pub clip: Option<String>,
    pub rec: Option<String>,
    pub bar: Option<String>,
    pub overlay: Option<String>,
    pub clipping: Option<String>,
}

/// Une vidéo du dossier de sortie (onglet Vidéos, derniers clips de la barre).
#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    pub path: PathBuf,
    pub name: String,
    /// Clé de la miniature (library::thumb_key), stable au renommage.
    pub thumb: String,
    pub rec: bool,
    pub bytes: u64,
    pub secs: f64,
    /// Date de modification, en secondes depuis 1970.
    pub date: u64,
    /// Le fichier a une piste d'images.
    pub video: bool,
    /// Pistes de son à faire entendre (mélangées par le lecteur).
    pub audio: usize,
}

/// Une catégorie de l'outil de nettoyage, mesurée (tools.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct CleanScan {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub files: u64,
    pub admin: bool,
    /// Cochée par défaut.
    pub default: bool,
    pub warn: String,
}

/// Étape de l'installation d'un pilote NVIDIA.
#[derive(Clone, Debug, PartialEq)]
pub enum DriverStep {
    Download { done: u64, total: u64 },
    Install,
    /// Nouvelle version vérifiée.
    Done,
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Nouvelle version disponible (page de mise à jour).
    Update(Option<String>),
    /// Installateur de la nouvelle version déjà téléchargé.
    UpdateReady,
    /// Installation de la mise à jour : lancée (`ok`) ou échouée, avec le texte à montrer.
    UpdateState { ok: bool, text: String },
    Config(Box<Config>),
    Hotkeys { keys: Hotkeys, conflicts: Vec<String> },
    /// Barre affichée : état du moment (puis animation d'entrée).
    Show { config: Box<Config>, keys: Hotkeys, native: (u32, u32), overlay_on: bool, recording: bool, rec_secs: u64 },
    /// Barre fermée : animation de sortie, lecture arrêtée.
    Hide,
    /// REC en cours ou non, depuis tant de secondes.
    State { recording: bool, rec_secs: u64 },
    Toast { kind: Toast, text: String, path: Option<PathBuf> },
    /// Dossier choisi pour le réglage `key` (« out_dir » ou « buffer_dir »), et sa place libre.
    Folder { key: String, path: PathBuf, free_bytes: u64 },
    Disk { path: PathBuf, free_bytes: u64 },
    /// Niveaux du son pour les vumètres (0 à 1).
    Levels { game: f32, mic: f32 },
    Gpu { name: String, driver: String },
    /// Lien de l'installateur du dernier pilote (gardé par main.rs pour `DriverInstall`).
    DriverUrl(String),
    /// Aucune carte NVIDIA.
    NoGpu,
    /// Pilote vérifié. `status` : « update », « ok » ou « error ».
    Driver { status: String, installed: String, latest: String, error: String },
    DriverProgress(DriverStep),
    Clean(Vec<CleanScan>),
    CleanDone { freed: u64, skipped: u64 },
    /// Mode administrateur demandé : accepté par Windows (`ok`) ou non ; `elevated` : Clipper l'est déjà.
    Admin { ok: bool, on: bool, elevated: Option<bool> },
    Autostart(bool),
    /// Mémoire libre avant et après le nettoyage, et totale (octets).
    Ram { before: u64, after: u64, total: u64 },
    Overlay(bool),
    /// Une miniature faite en tâche de fond : les vidéos encore sans image la relisent.
    ThumbsReady,
    Videos(Vec<Video>),
    /// Les derniers clips (barre).
    Recent(Vec<Video>),
    /// Onglet à afficher (fenêtre déjà ouverte).
    Tab(String),
}
