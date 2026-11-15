//! Commandes envoyées à l'app (main.rs, `App::ipc`) par la fenêtre, la barre Alt+Z, l'icône de notification et
//! les fils de fond. Avant : des objets JSON `{"cmd": "…"}`, reste de l'ancienne page WebView2 ; une faute de
//! frappe dans un nom ou un champ ne se voyait qu'à l'exécution (« commande UI inconnue », champ lu vide).
//! Le compilateur vérifie maintenant chaque commande et chacun de ses champs.
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Réglages modifiés (clés « clip.height » acceptées), appliqués d'un coup.
    SetMany(Map<String, Value>),
    /// Vidéo en plein écran : la fenêtre suit.
    Fullscreen(bool),
    Maximize(bool),
    Minimize,
    /// Fenêtre principale fermée ; barre cachée, en rendant le focus au jeu si `refocus`.
    Close { refocus: bool },
    /// Déplacement de la fenêtre (barre de titre).
    Drag,
    /// Fenêtre principale ouverte depuis la barre, sur un onglet ou non.
    OpenSettings(Option<String>),
    /// Fin de l'animation de sortie de la barre n° `gen`.
    BarHidden(u64),
    OverlayToggle,
    Clip,
    Rec,
    /// Liste des vidéos (onglet Vidéos) ; `Recent` : les 4 dernières (barre).
    Videos,
    Recent,
    Video(VideoAct, PathBuf),
    DeleteVideos(Vec<PathBuf>),
    /// Choix d'un dossier (clé de réglage : « out_dir » ou « buffer_dir »).
    PickFolder(String),
    DiskFree(PathBuf),
    /// Niveaux audio pour les vumètres.
    Levels,
    UpdateFound(String),
    UpdateInstall,
    InstallFfmpeg,
    FfmpegInstalled(bool),
    /// Pilote NVIDIA : vérification sur la branche (« studio » ou « game_ready »), installation.
    DriverCheck(String),
    DriverInstall,
    CleanScan,
    CleanRun(Vec<String>),
    /// Catégorie de nettoyage ouverte dans l'Explorateur.
    CleanOpen(String),
    AdminMode(bool),
    Autostart(bool),
    RelaunchAdmin,
    RamClean,
}

/// Action sur un fichier vidéo du dossier de sortie.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoAct {
    /// Lecteur par défaut de Windows.
    Open,
    /// Explorateur, fichier sélectionné (dossier de sortie vide : le dossier lui-même).
    OpenFolder,
    Delete,
    Rename(String),
}
