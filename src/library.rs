//! Bibliothèque de clips : miniatures et planches d'aperçus (fabrication, clés, chemins, annonce « prête »).
//! Avant dans capture.rs, qui ne s'en sert qu'après l'assemblage d'un clip ; main.rs, front.rs et preview.rs
//! recomposaient chacun une partie des chemins.
use crate::config::Config;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Écart entre deux images de la planche d'aperçus : 2 s (une image clé sur deux secondes), plus pour un
/// long REC (100 images au plus).
pub fn strip_step(secs: f64) -> f64 {
    (secs / 100.0).max(2.0)
}

/// Planche d'aperçus d'un clip (« nom~p.jpg » à côté de sa miniature) : une image de 160x90 toutes les
/// `strip_step` secondes, 10 par rangée. Seules les images clés sont décodées (une toutes les 2 s) :
/// mesuré sur 48 s en 1080p60, 0,5 s de processeur une seule fois, 38 Ko. Priorité « inactive ».
pub fn make_strip(cfg: &Config, clip: &Path) -> bool {
    let secs = crate::capture::probe(cfg, clip).secs;
    if secs <= 0.0 {
        return false;
    }
    let iv = strip_step(secs);
    let rows = ((secs / iv).floor() as u32 + 1).div_ceil(10);
    let mut c = crate::capture::ffmpeg(cfg);
    c.creation_flags(crate::proc::hidden(crate::proc::Priority::Idle));
    c.args(["-loglevel", "quiet", "-skip_frame", "nokey", "-i"]).arg(clip);
    c.args(["-an", "-sn", "-vf", &format!("fps=1/{iv},scale=160:90:force_original_aspect_ratio=decrease,pad=160:90:(ow-iw)/2:(oh-ih)/2,tile=10x{rows}"), "-frames:v", "1", "-q:v", "6", "-y"]);
    c.arg(strip_path(clip));
    let Ok(mut ch) = c.spawn() else { return false };
    ch.wait().is_ok_and(|s| s.success())
}

/// Clé des miniatures d'un clip : sa date de création (pas de 100 ns), qui ne change pas quand on le renomme.
/// Avant : le nom du fichier. Deux clips renommés pareil (dans deux dossiers de jeu) partageaient une miniature,
/// et un clip renommé avant la fin de sa miniature (faite après la partie) la perdait.
pub fn thumb_key(clip: &Path) -> String {
    use std::os::windows::fs::MetadataExt;
    std::fs::metadata(clip).map_or_else(|_| clip.file_stem().unwrap_or_default().to_string_lossy().into_owned(), |m| format!("c{:x}", m.creation_time()))
}

/// Fabrique la miniature d'un clip (image à 1 s, sinon la première), à priorité « inactive ».
pub fn make_thumb(cfg: &Config, clip: &Path) -> bool {
    // Une seule fabrication à la fois par clip : l'onglet Vidéos, rafraîchi à l'annonce du clip, la demandait
    // aussi (deux ffmpeg écrivant le même fichier).
    static BUSY: Mutex<Vec<PathBuf>> = Mutex::new(vec![]);
    {
        let mut b = BUSY.lock().unwrap();
        if b.iter().any(|p| p == clip) {
            return false;
        }
        b.push(clip.to_path_buf());
    }
    let ok = make_thumb_now(cfg, clip);
    BUSY.lock().unwrap().retain(|p| p != clip);
    ok
}

/// Fonction prévenue quand une miniature est prête (main.rs : l'onglet Vidéos et la barre la relisent).
pub static ON_THUMB: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

pub fn thumbs_ready() {
    if let Some(f) = ON_THUMB.get() {
        f();
    }
}

fn make_thumb_now(cfg: &Config, clip: &Path) -> bool {
    let out = thumb_path(clip);
    let _ = std::fs::create_dir_all(dir());
    for at in ["1", "0"] {
        let mut t = crate::capture::ffmpeg(cfg);
        t.creation_flags(crate::proc::hidden(crate::proc::Priority::Idle));
        t.args(["-loglevel", "quiet", "-threads", "1", "-ss", at, "-i"]).arg(clip).args(["-frames:v", "1", "-vf", "scale=320:-2", "-y"]).arg(&out);
        if crate::capture::run_now(t) && out.exists() {
            return true;
        }
    }
    false
}

/// Miniatures absentes (cache effacé, clip enregistré par une autre copie de Clipper, ancien clip) : refaites
/// une à une en tâche de fond, jamais pendant un jeu plein écran. Avant, une miniature manquante le restait
/// pour toujours. Annoncées (`thumbs_ready`) si au moins une a été faite. Une seule tournée à la fois.
pub fn make_missing_thumbs(cfg: &Config, clips: &[PathBuf]) {
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::Relaxed) {
        return;
    }
    let mut made = 0;
    for clip in clips.iter().filter(|c| !thumb_path(c).exists()) {
        while crate::foreground::fullscreen_foreground() {
            std::thread::sleep(Duration::from_secs(5));
        }
        made += usize::from(make_thumb(cfg, clip));
    }
    if made > 0 {
        crate::log(&format!("miniatures refaites : {made}"));
        thumbs_ready();
    }
    BUSY.store(false, Ordering::Relaxed);
}

/// Dossier des miniatures et planches (à côté de la configuration).
fn dir() -> PathBuf {
    crate::config::dir().join("thumbs")
}

/// Miniature d'une clé (`thumb_key`) : seule règle de nommage, aussi utilisée par l'interface.
pub fn thumb_of_key(key: &str) -> PathBuf {
    dir().join(format!("{key}.jpg"))
}

/// Miniature d'un clip (onglet Vidéos, barre).
pub fn thumb_path(clip: &Path) -> PathBuf {
    thumb_of_key(&thumb_key(clip))
}

/// Fichier de la planche d'aperçus d'un clip.
pub fn strip_path(clip: &Path) -> PathBuf {
    dir().join(format!("{}~p.jpg", thumb_key(clip)))
}

/// Miniature et planche d'un clip, à relever AVANT de le supprimer (la clé se lit sur le fichier).
pub fn images(clip: &Path) -> [PathBuf; 2] {
    [thumb_path(clip), strip_path(clip)]
}

/// Efface la miniature et la planche relevées par `images` (clip supprimé).
pub fn forget(images: &[PathBuf; 2]) {
    for f in images {
        let _ = std::fs::remove_file(f);
    }
}

/// Miniature et planche d'avant la clé par date (nommées comme le fichier) : renommées une fois à la clé.
pub fn migrate_thumbs(clip: &Path) {
    migrate_in(&dir(), clip);
}

fn migrate_in(dir: &Path, clip: &Path) {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let key = thumb_key(clip);
    if key == stem {
        return;
    }
    for suffix in [".jpg", "~p.jpg"] {
        let (old, new) = (dir.join(format!("{stem}{suffix}")), dir.join(format!("{key}{suffix}")));
        if !new.exists() && old.exists() {
            let _ = std::fs::rename(old, new);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_clip_falls_back_to_its_file_name() {
        assert_eq!(thumb_key(Path::new(r"C:\nulle-part\Clip 2026.mp4")), "Clip 2026");
    }

    #[test]
    fn key_is_the_creation_date_and_survives_a_rename() {
        let d = std::env::temp_dir().join("clipper-library-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (a, b) = (d.join("avant.mp4"), d.join("après.mp4"));
        std::fs::write(&a, b"x").unwrap();
        let key = thumb_key(&a);
        assert!(key.starts_with('c'), "{key}");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(thumb_key(&b), key);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn old_name_based_images_move_to_the_key() {
        let d = std::env::temp_dir().join("clipper-library-migrate");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let clip = d.join("Clip 1.mp4");
        std::fs::write(&clip, b"x").unwrap();
        std::fs::write(d.join("Clip 1.jpg"), b"t").unwrap();
        std::fs::write(d.join("Clip 1~p.jpg"), b"p").unwrap();
        migrate_in(&d, &clip);
        let key = thumb_key(&clip);
        assert!(d.join(format!("{key}.jpg")).exists() && d.join(format!("{key}~p.jpg")).exists());
        assert!(!d.join("Clip 1.jpg").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
