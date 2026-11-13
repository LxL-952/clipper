//! Aperçu au survol de la barre de temps (lecteur de l'app et de la barre Alt+Z) : une petite image du
//! point survolé, prise dans la planche d'aperçus du clip (capture::make_strip). Aucun décodage vidéo au
//! survol : la planche est faite une fois (à l'enregistrement, ou au premier survol d'un ancien clip, en
//! tâche de fond à priorité inactive), puis relue du disque une fois par clip.
use crate::config::Config;
use crate::front::dur;
use crate::gfx::{F, R, pal, r};
use crate::widgets::Ui;
use std::path::Path;
use std::time::{Duration, Instant};

/// Taille d'une image de la planche, en pixels.
const TW: u32 = 160;
const TH: u32 = 90;

#[derive(Default)]
pub struct Preview {
    stem: String,
    img: Option<tiny_skia::Pixmap>,
    /// Planche demandée (fabrication lancée) et dernière relecture du disque en l'attendant.
    asked: bool,
    looked: Option<Instant>,
}

impl Preview {
    /// Dessine l'aperçu du point `t` au-dessus de `bar` (la barre de temps), centré sur `x` et gardé dans
    /// `area`. Vrai : planche en cours de fabrication, redessiner bientôt pour la montrer.
    pub fn show(&mut self, ui: &mut Ui, cfg: &Config, clip: &Path, secs: f64, t: f64, x: f32, bar: R, area: R) -> bool {
        let stem = clip.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        if stem != self.stem {
            *self = Preview { stem, ..Default::default() };
        }
        if self.img.is_none() {
            if self.looked.is_some_and(|l| l.elapsed() < Duration::from_millis(300)) {
                return true;
            }
            self.looked = Some(Instant::now());
            self.img = std::fs::read(crate::capture::strip_path(clip)).ok().and_then(|b| crate::front::decode_jpeg(&b));
            if self.img.is_none() {
                if !self.asked {
                    self.asked = true;
                    let (c, p) = (cfg.clone(), clip.to_path_buf());
                    std::thread::spawn(move || crate::capture::make_strip(&c, &p));
                }
                return true;
            }
        }
        let Some(img) = &self.img else { return false };
        let cols = (img.width() / TW).max(1);
        let n = cols * (img.height() / TH).max(1);
        let i = ((t / crate::capture::strip_step(secs)).round() as u32).min(n - 1);
        let (sx, sy) = ((i % cols) * TW, (i / cols) * TH);
        // Une image de la planche, recopiée pixel pour pixel (160x90 à l'écran, quelle que soit l'échelle).
        let mut tile = vec![0u8; (TW * TH * 4) as usize];
        let stride = (img.width() * 4) as usize;
        for row in 0..TH as usize {
            let s0 = (sy as usize + row) * stride + sx as usize * 4;
            tile[row * TW as usize * 4..(row + 1) * TW as usize * 4].copy_from_slice(&img.data()[s0..s0 + TW as usize * 4]);
        }
        let s = ui.c.s;
        // 160x90 pixels d'écran, mais au plus 42 % de la largeur du lecteur (le petit lecteur de l'app :
        // l'aperçu en couvrait plus de la moitié). Réduit alors une fois, en bilinéaire.
        let w = (TW as f32 / s).min(area.w * 0.42).round();
        let h = (w * 9.0 / 16.0).round();
        let q = r((x - w / 2.0).clamp(area.x + 4.0, area.right() - 4.0 - w), bar.y - 8.0 - h - 20.0, w, h);
        let lab = r(q.x, q.bottom(), w, 20.0);
        // Dessiné sur place (pas en calque : le calque oublie le défilement de la colonne du lecteur).
        let frame = r(q.x - 2.0, q.y - 2.0, w + 4.0, h + 24.0);
        ui.c.shadow(frame, 6.0, 4.0, 10.0, pal::BLACK.a(0.45));
        ui.c.rrect(frame, 6.0, pal::RAISE);
        ui.c.border(frame, 6.0, 1.0, pal::LINE2);
        let (pw, ph) = ((w * s).round() as u32, (h * s).round() as u32);
        if (pw, ph) == (TW, TH) {
            ui.c.blit(&tile, TW, TH, q.x, q.y);
        } else if let Some(small) = tiny_skia::Pixmap::from_vec(tile, tiny_skia::IntSize::from_wh(TW, TH).unwrap()).and_then(|t| crate::gfx::fit_cover(&t, pw, ph)) {
            ui.c.blit(small.data(), pw, ph, q.x, q.y);
        }
        ui.c.text_in(lab, 0, &dur(t), F::m(11.5, 500), pal::TEXT);
        false
    }
}
