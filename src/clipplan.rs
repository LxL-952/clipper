//! Règles d'assemblage d'un clip, sans disque ni ffmpeg (capture.rs, `save` et `concat`) : combien
//! couper au début pour tomber pile sur la durée du buffer, quelle durée attendre du fichier fini, et
//! comment donner les segments à ffmpeg. Les bugs de durée passés (clips de 62 s) vivaient ici.
use std::path::{Path, PathBuf};

/// Secondes du début à masquer (liste d'édition MP4/MOV, sans réencodage). Les segments commencent par une
/// image clé toutes les 2 s : un clip emportait jusqu'à 2 s de plus que le buffer (1:01, 1:02).
/// `media` : durée exacte des images emportées ; `trimmable` : un Clip, hors MKV (pas de liste d'édition).
/// Moins de 20 ms d'excédent (une image) : rien à couper.
pub fn trim(media: Option<f64>, target: f64, trimmable: bool) -> f64 {
    match media {
        Some(m) if trimmable && m > target + 0.02 => m - target,
        _ => 0.0,
    }
}

/// Clip le plus court : deux appuis à moins de 5 s donnent quand même un clip regardable (un peu de reprise).
pub const MIN_CLIP: f64 = 5.0;

/// Durée d'un clip : la durée du buffer, sans reprendre ce que le clip précédent a déjà emporté. Clip à
/// 10:00, nouveau clip à 10:15 : 15 s, pas une minute dont 45 s déjà vues. `last` : instant du clip
/// précédent sur l'horloge du buffer. En MKV (pas de liste d'édition), le début n'est pas coupé : jusqu'à
/// 2 s de reprise (un segment).
pub fn window(buffer: f64, last: Option<f64>, now: f64) -> f64 {
    last.map_or(buffer, |l| (now - l).max(MIN_CLIP)).min(buffer)
}

/// Durée que le fichier fini doit annoncer (contrôlée par `verify`) : la cible si on a coupé, sinon la
/// durée exacte des images, à défaut celle mesurée sur l'horloge du buffer.
pub fn expected(media: Option<f64>, clock_secs: f64, trim: f64, target: f64) -> f64 {
    if trim > 0.0 { target } else { media.unwrap_or(clock_secs) }
}

/// Entrée de ffmpeg pour les segments d'un clip, une liste par capture (ffmpeg relancé entre deux).
#[derive(Debug, PartialEq)]
pub enum Input {
    /// Une seule capture en .ts, un seul dossier : lus comme un seul flux (protocole concat), noms courts,
    /// ffmpeg lancé dans `dir`. 2 min assemblées en 0,6 s au lieu de 3,3 s avec le démultiplexeur concat.
    Stream { dir: PathBuf, names: Vec<String> },
    /// Plusieurs captures (chacune repart de zéro dans ses horodatages) ou autre format : fichier de liste
    /// pour le démultiplexeur concat, une ligne par capture en .ts, une par fichier sinon.
    List(String),
}

pub fn input(parts: &[Vec<PathBuf>]) -> Input {
    let ts = parts.iter().flatten().all(|p| p.extension().is_some_and(|e| e == "ts"));
    let dir = parts.first().and_then(|g| g.first()).and_then(|p| p.parent()).filter(|d| parts.iter().flatten().all(|p| p.parent() == Some(*d)));
    match dir {
        Some(d) if ts && parts.len() == 1 => {
            Input::Stream { dir: d.to_path_buf(), names: parts[0].iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect() }
        }
        _ => {
            let quote = |p: &Path| p.display().to_string().replace('\'', "'\\''");
            Input::List(if ts {
                parts.iter().map(|g| format!("file 'concat:{}'\n", g.iter().map(|p| quote(p)).collect::<Vec<_>>().join("|"))).collect()
            } else {
                parts.iter().flatten().map(|p| format!("file '{}'\n", quote(p))).collect()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_clip_does_not_repeat_the_previous_one() {
        assert_eq!(window(60.0, None, 600.0), 60.0, "premier clip : tout le buffer");
        assert_eq!(window(60.0, Some(600.0), 615.0), 15.0, "15 s après : les 15 s seulement");
        assert_eq!(window(60.0, Some(600.0), 700.0), 60.0, "longtemps après : tout le buffer");
        assert_eq!(window(60.0, Some(600.0), 601.0), MIN_CLIP, "double appui : 5 s");
        assert_eq!(window(3.0, Some(600.0), 601.0), 3.0, "jamais plus que le buffer");
    }

    #[test]
    fn clip_is_trimmed_to_the_buffer_length() {
        assert!((trim(Some(61.87), 60.0, true) - 1.87).abs() < 1e-9);
        assert_eq!(trim(Some(60.01), 60.0, true), 0.0, "une image d'écart : rien");
        assert_eq!(trim(Some(58.0), 60.0, true), 0.0, "buffer pas encore plein");
        assert_eq!(trim(Some(61.87), 60.0, false), 0.0, "REC ou MKV : jamais coupé");
        assert_eq!(trim(None, 60.0, true), 0.0, "durée illisible : on ne devine pas");
    }

    #[test]
    fn expected_duration_follows_the_trim() {
        assert_eq!(expected(Some(61.87), 62.0, 1.87, 60.0), 60.0);
        assert_eq!(expected(Some(41.0), 41.2, 0.0, 60.0), 41.0);
        assert_eq!(expected(None, 41.2, 0.0, 60.0), 41.2);
    }

    #[test]
    fn one_capture_is_read_as_one_stream() {
        let d = PathBuf::from(r"C:\buf");
        let parts = vec![vec![d.join("0000000001.ts"), d.join("0000000002.ts"), d.join("tail-3.ts")]];
        assert_eq!(input(&parts), Input::Stream { dir: d, names: vec!["0000000001.ts".into(), "0000000002.ts".into(), "tail-3.ts".into()] });
    }

    #[test]
    fn several_captures_get_one_list_line_each() {
        let d = PathBuf::from(r"C:\buf");
        let parts = vec![vec![d.join("1.ts"), d.join("2.ts")], vec![d.join("7.ts")]];
        assert_eq!(input(&parts), Input::List("file 'concat:C:\\buf\\1.ts|C:\\buf\\2.ts'\nfile 'concat:C:\\buf\\7.ts'\n".into()));
    }

    #[test]
    fn mkv_segments_are_listed_one_by_one_and_quotes_escaped() {
        let d = PathBuf::from(r"C:\l'a");
        let parts = vec![vec![d.join("1.mkv"), d.join("2.mkv")]];
        assert_eq!(input(&parts), Input::List("file 'C:\\l'\\''a\\1.mkv'\nfile 'C:\\l'\\''a\\2.mkv'\n".into()));
    }
}
