//! Onglets « Clipper » et « Audio » de la fenêtre principale. Textes, dimensions et calculs repris de
//! l'ancienne page (mêmes formules de débit et de taille de fichier).
use crate::front::{Front, HK, bytes, fr, pretty, thousands};
use crate::gfx::{Col, F, R, hex, pal, r, scene, text_w};
use crate::widgets::{Btn, Note, Seg, SegOpt, Slide, Ui, id, idn, so, son};
use serde_json::json;

const CLASSES: [u32; 4] = [720, 1080, 1440, 2160];
const RES_LABELS: [&str; 4] = ["720p", "1080p", "1440p", "2160p"];

fn class_w(c: u32) -> u32 {
    (c as f32 * 16.0 / 9.0).round() as u32
}

pub fn enc_label(e: &str) -> String {
    match e {
        "h264_nvenc" => "NVENC H.264 (compatible partout)",
        "hevc_nvenc" => "NVENC HEVC (≈ 30 % plus léger)",
        "av1_nvenc" => "NVENC AV1 (le plus léger, lecture récente)",
        "h264_amf" => "AMF H.264 (puce AMD)",
        "hevc_amf" => "AMF HEVC (puce AMD)",
        "av1_amf" => "AMF AV1 (puce AMD)",
        "h264_qsv" => "QSV H.264 (puce Intel)",
        "hevc_qsv" => "QSV HEVC (puce Intel)",
        "av1_qsv" => "QSV AV1 (puce Intel)",
        "libx264" => "x264 (processeur, déconseillé en jeu)",
        x => x,
    }
    .to_string()
}

pub fn vendor(e: &str) -> Option<&'static str> {
    if e.ends_with("_nvenc") {
        Some("nvidia")
    } else if e.ends_with("_amf") {
        Some("amd")
    } else if e.ends_with("_qsv") {
        Some("intel")
    } else {
        None
    }
}

pub const ITEMS: [(&str, &str); 9] =
    [("fps", "FPS"), ("low1", "1% low"), ("frametime", "Temps d'image"), ("cpu", "CPU"), ("gpu", "GPU"), ("cpu_temp", "Temp. CPU"), ("gpu_temp", "Temp. GPU"), ("vram", "VRAM"), ("ram", "RAM")];

pub fn ov_sample(k: &str) -> Option<(&'static str, &'static str, bool)> {
    Some(match k {
        "fps" => ("FPS", "142", true),
        "low1" => ("1% LOW", "98", false),
        "frametime" => ("MS/IMG", "7,0 ms", false),
        "cpu" => ("CPU", "23 %", false),
        "gpu" => ("GPU", "87 %", false),
        "cpu_temp" => ("CPU °C", "68", false),
        "gpu_temp" => ("GPU °C", "71", false),
        "vram" => ("VRAM", "5,1/8 Go", false),
        "ram" => ("RAM", "11/16 Go", false),
        _ => return None,
    })
}

impl Front {
    fn avail(&self, c: u32) -> bool {
        class_w(c) <= self.native.0 + 1
    }
    fn native_class(&self) -> u32 {
        CLASSES.into_iter().filter(|c| self.avail(*c)).last().unwrap_or(720)
    }
    fn out_dims(&self, c: u32) -> (u32, u32) {
        let w = class_w(c).min(self.native.0);
        (w, (w as f32 * self.native.1 as f32 / self.native.0 as f32 / 2.0).round() as u32 * 2)
    }
    /// Débit plafond en Mb/s (même formule que capture::bitrate_kbps).
    pub fn mbps(&self) -> f32 {
        let base = match self.cfg.clip.height {
            720 => 10.0,
            1440 => 35.0,
            2160 => 70.0,
            _ => 20.0,
        };
        let codec = if self.cfg.encoder.starts_with("hevc") || self.cfg.encoder.starts_with("av1") { 0.7 } else { 1.0 };
        base * if self.cfg.clip.fps <= 30 { 0.65 } else { 1.0 } * self.cfg.quality.clamp(0.5, 2.0) * codec
    }
    /// NVENC / x264 en qualité constante : le débit est un plafond, la dépense réelle tourne autour de 40 %.
    pub fn typical(&self) -> f32 {
        if self.cfg.encoder.ends_with("_nvenc") || self.cfg.encoder == "libx264" { 0.4 } else { 1.0 }
    }
    pub fn chip_eff(&self) -> Option<String> {
        let has = |x: &str| self.gpus.iter().any(|g| g.0 == x);
        let c = self.cfg.encode_chip.as_str();
        if has(c) {
            return Some(c.into());
        }
        if c == "igpu" {
            if let Some(i) = ["amd", "intel"].into_iter().find(|x| has(x)) {
                return Some(i.into());
            }
        }
        if has("nvidia") { Some("nvidia".into()) } else { self.gpus.first().map(|g| g.0.clone()) }
    }
    pub fn enc_ok(&self, e: &str) -> bool {
        match (vendor(e), self.chip_eff()) {
            (Some(v), Some(c)) => v == c,
            _ => true,
        }
    }

    /// Curseur avec sa valeur à droite (.fader) et sa règle. Renvoie (résultat, hauteur).
    pub fn fader(&mut self, ui: &mut Ui, wid: u64, x: f32, y: f32, w: f32, v: f32, range: (f32, f32, f32), out: (&str, &str), labels: Option<[&str; 3]>) -> (Slide, f32) {
        let (a, b) = (text_w(out.0, F::m(12.5, 400)), text_w(out.1, F::m(11.0, 400)));
        // Place de la valeur mesurée (avant : 92 px fixes, « ×1,00 · max 20 Mb/s » chevauchait le curseur et
        // touchait le bord). Arrondie par paliers de 16 px : le curseur ne change pas de taille à chaque cran.
        let rw = ((a + b).max(92.0) / 16.0).ceil() * 16.0;
        let sw = w - rw - 12.0;
        let s = ui.slider(wid, r(x, y, sw, 32.0), v, range.0, range.1, range.2);
        ui.ruler(x, y + 26.0, sw);
        ui.c.text(x + w - a - b, y, 32.0, out.0, F::m(12.5, 400), pal::TEXT);
        ui.c.text(x + w - b, y + 1.0, 32.0, out.1, F::m(11.0, 400), pal::TEXT3);
        let mut h = 32.0;
        if let Some(l) = labels {
            let f = F::m(11.0, 400);
            ui.c.text(x, y + 34.0, 16.0, l[0], f, pal::TEXT3);
            ui.c.text(x + (sw - text_w(l[1], f)) / 2.0, y + 34.0, 16.0, l[1], f, pal::TEXT3);
            ui.c.text(x + sw - text_w(l[2], f), y + 34.0, 16.0, l[2], f, pal::TEXT3);
            h = 50.0;
        }
        (s, h)
    }

    /// Groupe segmenté lié à un réglage texte. Renvoie la largeur.
    pub fn seg_cfg(&mut self, ui: &mut Ui, key: &'static str, x: f32, y: f32, opts: &[(&'static str, serde_json::Value)], mono: bool) -> f32 {
        let cur = self.cfg_get(key);
        let sel = opts.iter().position(|(_, v)| Some(v) == cur.as_ref() || (v.is_number() && cur.as_ref().and_then(|c| c.as_f64()) == v.as_f64()));
        let so_: Vec<SegOpt> = opts.iter().map(|(l, _)| if mono { son(l) } else { so(l) }).collect();
        let (hit, w) = ui.seg(id(key), x, y, &so_, sel, Seg::D);
        if let Some(i) = hit {
            self.set(key, opts[i].1.clone());
        }
        w
    }

    pub fn tab_clip(&mut self, ui: &mut Ui, body: R) {
        // ---------- colonne de gauche : aperçu, durée du buffer, tailles ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-clip"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        let (cw, ch) = self.out_dims(self.cfg.clip.height);
        let nc = self.native_class();
        {
            let m = r(x, y, w, (w * 10.0 / 16.0).round());
            scene(ui.c, m, 6.0);
            ui.c.outline(m, 6.0, 1.0, pal::LINE2);
            let mk = pal::TEXT.a(0.55);
            for (cx, cy, dx, dy) in [(m.x + 8.0, m.y + 8.0, 1.0, 1.0), (m.right() - 8.0, m.y + 8.0, -1.0, 1.0), (m.x + 8.0, m.bottom() - 8.0, 1.0, -1.0), (m.right() - 8.0, m.bottom() - 8.0, -1.0, -1.0)] {
                ui.c.rect(r(if dx > 0.0 { cx } else { cx - 12.0 }, if dy > 0.0 { cy } else { cy - 1.0 }, 12.0, 1.0), mk);
                ui.c.rect(r(if dx > 0.0 { cx } else { cx - 1.0 }, if dy > 0.0 { cy } else { cy - 12.0 }, 1.0, 12.0), mk);
            }
            // (Aperçu des statistiques à l'écran : onglet Statistiques.)
            let f = F::m(11.0, 400);
            let hud = pal::TEXT.a(0.85);
            ui.led(m.x + 17.5, m.bottom() - 20.0, 7.0, pal::OK);
            let (bm, bs) = (self.cfg.buffer_secs / 60, self.cfg.buffer_secs % 60);
            ui.c.text(m.x + 27.0, m.bottom() - 28.0, 16.0, &format!("−{bm:02}:{bs:02}"), f, hud);
            let fmt = format!("{cw}×{ch} · {}", self.cfg.clip.fps);
            ui.c.text(m.right() - 14.0 - text_w(&fmt, f), m.bottom() - 28.0, 16.0, &fmt, f, hud);
            y = m.bottom() + 14.0;
        }
        // durée du buffer
        {
            ui.sc(x, y, 16.0, "Durée du buffer", pal::TEXT3, 500);
            let (bm, bs) = (self.cfg.buffer_secs / 60, self.cfg.buffer_secs % 60);
            let v = if bs > 0 { format!("{}{bs} s", if bm > 0 { format!("{bm} min ") } else { String::new() }) } else { format!("{bm} min") };
            ui.c.text(x + w - text_w(&v, F::m(12.0, 400)), y, 16.0, &v, F::m(12.0, 400), pal::TEXT);
            y += 24.0;
            let col = (w - 30.0) / 6.0;
            // règle graduée
            ui.c.rect(r(x, y + 9.0, w, 1.0), pal::LINE2);
            let mut tx = 0.0;
            while tx < w {
                ui.c.rect(r(x + tx, y + 6.0, 1.0, 4.0), pal::LINE2);
                tx += 8.0;
            }
            let secs = [600u32, 300, 180, 120, 60, 30];
            let labels = ["10", "5", "3", "2", "1", ":30"];
            if let Some(si) = secs.iter().position(|s| *s == self.cfg.buffer_secs) {
                let target = (x + w - 15.0) - (x + col * si as f32 + col / 2.0);
                let fw = ui.anim(id("tl-fill"), target, 0.35);
                ui.c.rect(r(x + w - 15.0 - fw, y, fw, 10.0), pal::SIGNAL.a(0.26));
                ui.c.rect(r(x + w - 15.0 - fw, y, 2.0, 10.0), pal::SIGNAL);
            }
            ui.c.rect(r(x + w - 15.0, y - 4.0, 1.0, 18.0), pal::TEXT);
            let f = F::m(12.0, 400);
            let mut pick = None;
            for (i, l) in labels.iter().enumerate() {
                let cell = r(x + col * i as f32, y + 12.0, col, 40.0);
                let on = self.cfg.buffer_secs == secs[i];
                let hot = ui.hot(cell);
                if ui.click(idn("det", i), cell) {
                    pick = Some(secs[i]);
                }
                let cxm = cell.cx();
                if on {
                    ui.c.rect(r(cxm - 0.5, y - 4.0, 1.0, 12.0), pal::SIGNAL);
                    let tw = text_w(l, f.wt(500)) + 8.0;
                    ui.c.rrect(r(cxm - tw / 2.0, y + 18.0, tw, 21.4), 3.0, pal::SIGNAL);
                    ui.c.text(cxm - tw / 2.0 + 4.0, y + 18.0, 21.4, l, f.wt(500), pal::SIGNAL_INK);
                } else {
                    ui.c.rect(r(cxm - 0.5, y, 1.0, 8.0), pal::LINE2);
                    ui.c.text(cxm - text_w(l, f) / 2.0, y + 18.0, 21.4, l, f, if hot { pal::TEXT } else { pal::TEXT3 });
                }
            }
            ui.c.text(x + w - 15.0 - text_w("0", f) / 2.0, y + 18.0, 17.4, "0", f, pal::TEXT2);
            if let Some(s) = pick {
                self.set("buffer_secs", json!(s));
            }
            y += 52.0 + 14.0;
        }
        // tailles calculées
        let (cb, k, a) = (self.mbps(), self.typical(), self.cfg.audio_tracks.len() as f32 * 0.16);
        let buf = self.cfg.buffer_secs as f32;
        let gh = (cb * k + a) * 3600.0 / 8.0 / 1000.0;
        {
            let mo = |n: f32| thousands(n.round() as u64);
            let rows: [(&str, String, String); 3] = [
                ("Taille d'un clip", format!("≈ {} Mo", mo((cb * k + a) * buf / 8.0)), if k < 1.0 { format!(" · max {}", mo((cb + a) * buf / 8.0)) } else { String::new() }),
                ("REC", format!("≈ {} Mo", mo((cb * k + a) * 60.0 / 8.0)), "/min".into()),
                ("Écriture disque", format!("≈ {} Go", fr(gh as f64, 1)), "/h".into()),
            ];
            ui.c.rect(r(x, y, w, 1.0), pal::LINE);
            y += 1.0;
            for (l, v, sm) in rows {
                ui.c.text(x, y + 9.0, 22.0, &l.to_uppercase(), F::s(11.0, 500).ls(0.11), pal::TEXT3);
                let (vw, sw) = (text_w(&v, F::m(15.0, 500)), text_w(&sm, F::m(11.0, 500)));
                ui.c.text(x + w - vw - sw, y + 9.0, 22.0, &v, F::m(15.0, 500), pal::TEXT);
                ui.c.text(x + w - sw, y + 11.0, 22.0, &sm, F::m(11.0, 500), pal::TEXT3);
                y += 40.0;
                ui.c.rect(r(x, y - 1.0, w, 1.0), pal::LINE);
            }
            y += 14.0;
        }
        // Interrupteurs « Clipping » et « Capture du bureau », dans une carte : avant, empilés sans marge, le
        // titre du second touchait le texte du premier et les interrupteurs flottaient en haut à droite.
        y = self.capture_card(ui, x, y, w);
        ui.scroll_end(id("mon-clip"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite : réglages ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("set-clip"), view);
        let (x, w) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let (fx, fw) = (x + 132.0, w - 132.0);
        let mut y = view.y + 4.0;
        if !self.ffmpeg_ok {
            let bw = Ui::btn_w("Installer FFmpeg", true, Btn::P);
            let (nh, (ax, ay)) = ui.note(x, y + 14.0, w, Note::Err, "warn", "**FFmpeg est introuvable.** Clipper s'en sert pour lister les encodeurs et les écrans, et pour enregistrer : sans lui, ces listes restent vides et aucun clip n'est possible.", 39.0);
            if ui.btn(id("ffInst"), r(ax, ay + 7.0, bw, 32.0), "Installer FFmpeg", Some("dl"), Btn::P.off(self.ff_installing)) {
                self.ff_installing = true;
                self.send(json!({"cmd": "install_ffmpeg"}));
            }
            if self.ff_installing {
                ui.c.text(ax + bw + 8.0, ay + 7.0, 32.0, "Installation par winget… la fenêtre se rouvrira toute seule.", F::s(12.0, 400), pal::TEXT3);
            }
            y += 14.0 + nh + 4.0;
        }

        // Capture
        let (open, mut cy) = self.grp(ui, "clip:cap", "Capture", x, y, w);
        if open {
            let iw = w - 24.0;
            let hint_txt = "Même qualité pour le Clip et le REC : le REC réutilise le buffer, sans 2ᵉ capture ni coût en plus.";
            let hh = { let sp = crate::gfx::md(hint_txt, F::s(12.5, 400), pal::TEXT3, 600, pal::TEXT2); ui.c.para(0.0, 0.0, iw, 18.0, &sp, false) };
            let card = r(x, cy, w, 12.0 + 19.0 + 10.0 + 62.0 + 10.0 + 17.0 + 10.0 + hh + 12.0);
            ui.c.rrect(card, 7.0, pal::WHITE.a(0.012));
            ui.c.border(card, 7.0, 1.0, pal::LINE);
            let (ix, mut iy) = (x + 12.0, cy + 12.0);
            ui.c.icon("clip", ix, iy + 0.5, 18.0, pal::TEXT);
            let f = F::s(13.0, 600);
            let mut tx = ix + 26.0 + ui.c.text(ix + 26.0, iy, 19.0, "Clip et ", f, pal::TEXT);
            ui.led(tx + 6.0, iy + 9.5, 7.0, pal::REC);
            tx += 15.0;
            ui.c.text(tx, iy, 19.0, "REC", f, pal::TEXT);
            let keys = format!("{} · {}", self.hk[0].clone().unwrap_or_default(), self.hk[1].clone().unwrap_or_default());
            ui.c.text(ix + iw - text_w(&keys, F::m(11.0, 400)), iy, 19.0, &keys, F::m(11.0, 400), pal::TEXT3);
            iy += 29.0;
            let c1 = ((iw - 12.0) * 1.6 / 2.6).floor();
            let c2 = iw - 12.0 - c1;
            ui.sc(ix, iy, 16.0, "Résolution", pal::TEXT3, 500);
            ui.sc(ix + c1 + 12.0, iy, 16.0, "Images/s", pal::TEXT3, 500);
            let opts: Vec<SegOpt> = CLASSES.iter().zip(RES_LABELS).map(|(c, l)| SegOpt { label: l, enabled: self.avail(*c), mono: true }).collect();
            let (hit, _) = ui.seg(id("res"), ix, iy + 24.0, &opts, CLASSES.iter().position(|c| *c == self.cfg.clip.height), Seg { fill: Some(c1), dot: false, res: true });
            if let Some(i) = hit {
                self.set("clip.height", json!(CLASSES[i]));
            }
            let (hit, _) = ui.seg(id("fps"), ix + c1 + 12.0, iy + 24.0, &[son("30"), son("60")], [30, 60].iter().position(|f| *f == self.cfg.clip.fps), Seg { fill: Some(c2), dot: true, res: false });
            if let Some(i) = hit {
                self.set("clip.fps", json!([30, 60][i]));
            }
            iy += 72.0;
            let out = format!("{cw}×{ch}{}", if self.cfg.clip.height == nc { " · natif" } else { "" });
            ui.c.text(ix, iy, 17.0, &out, F::m(12.0, 400), pal::TEXT3);
            iy += 27.0;
            ui.hint(ix, iy, iw, hint_txt);
            cy = card.bottom() + 12.0;
            let above: Vec<String> = CLASSES.iter().filter(|c| !self.avail(**c)).map(|c| format!("{c}p")).collect();
            let (nw, nh_) = self.native;
            let (ow, oh) = self.out_dims(nc);
            let nat = if above.is_empty() {
                format!("Toutes les résolutions tiennent dans votre écran (`{nw}×{nh_}`). {nc}p = taille native, sans copie GPU.")
            } else {
                format!(
                    "{} {} votre écran (`{nw}×{nh_}`). Agrandir n'ajoute aucun détail. {nc}p garde le format de l'écran, soit `{ow}×{oh}` natif, sans copie GPU.",
                    above.join(" et "),
                    if above.len() > 1 { "sont grisés : ils dépassent" } else { "est grisé : il dépasse" }
                )
            };
            cy += ui.note(x, cy, w, Note::Info, "info", &nat, 0.0).0;
            if self.cfg.clip.height != nc && self.gpu_scale != Some(true) {
                let t = format!("**Passage par le processeur.** La réduction sur la carte graphique n'a pas marché sur ce PC : chaque image de l'écran est copiée vers le processeur pour être réduite (jusqu'à un cœur entier à 60 i/s). À {nc}p (taille de l'écran), aucun coût en plus.");
                cy += 10.0;
                cy += ui.note(x, cy, w, Note::Warn, "warn", &t, 0.0).0;
            }
            cy += 14.0;
            self.lab(ui.c, x, cy, 34.0, "Écran capturé", None);
            let opts: Vec<(String, bool)> = self.monitors.iter().map(|m| (m.1.clone(), true)).collect();
            if let Some(i) = ui.select(id("mon"), r(fx, cy, fw, 34.0), &opts, self.monitors.iter().position(|m| m.0 == self.cfg.monitor)) {
                let idx = self.monitors[i].0;
                self.set("monitor", json!(idx));
            }
            cy += 34.0;
        }
        y = self.grp_end(ui, open, x, cy, w, false);

        // Qualité et fichier
        let (open, mut cy) = self.grp(ui, "clip:q", "Qualité et fichier", x, y, w);
        if open {
            // Profil d'encodage : le jeu d'abord par défaut (« Performance maximale »).
            self.lab(ui.c, x, cy, 34.0, "Profil", None);
            const PROFILES: [(&str, &str); 3] = [("perf", "Compétitif (recommandé)"), ("balanced", "Équilibré"), ("quality", "Qualité max")];
            let opts: Vec<(String, bool)> = PROFILES.iter().map(|p| (p.1.to_string(), true)).collect();
            if let Some(i) = ui.select(id("profile"), r(fx, cy, fw, 34.0), &opts, PROFILES.iter().position(|p| p.0 == self.cfg.profile).or(Some(0))) {
                self.set("profile", json!(PROFILES[i].0));
            }
            cy += 34.0 + 6.0;
            // Détail du profil choisi : ce qui est réellement enregistré.
            let k = crate::capture::profile_k(&self.cfg.profile) * self.cfg.quality;
            let mbps = crate::capture::bitrate_kbps(self.cfg.clip.height, self.cfg.clip.fps, &self.cfg.encoder, k) as f32 / 1000.0;
            let what = match self.cfg.profile.as_str() {
                "quality" => "préréglage p2 + AQ spatiale, débit +20 % : plus de détails, fichiers plus lourds, même charge d'encodeur.",
                "balanced" => "préréglage p1 + AQ spatiale, débit +18 % : un peu plus de détails dans l'action rapide.",
                _ => "préréglage p1 + AQ spatiale : environ 8 % du bloc d'encodage (séparé du rendu du jeu), pensé pour Valorant.",
            };
            let line = format!("{}p · {} i/s · {} · jusqu'à {:.0} Mb/s · {what} Si l'encodeur sature, Clipper s'allège tout seul.", self.cfg.clip.height, self.cfg.clip.fps, enc_label(&self.cfg.encoder), mbps);
            cy += ui.hint(x, cy, w, &line);
            cy += 10.0;
            self.lab(ui.c, x, cy, 34.0, "Encodeur", None);
            let opts: Vec<(String, bool)> = if self.encoders.is_empty() { vec![("Aucun encodeur détecté".into(), false)] } else { self.encoders.iter().map(|e| (enc_label(e), self.enc_ok(e))).collect() };
            if let Some(i) = ui.select(id("enc"), r(fx, cy, fw, 34.0), &opts, self.encoders.iter().position(|e| *e == self.cfg.encoder)) {
                let e = self.encoders[i].clone();
                self.set("encoder", json!(e));
            }
            cy += 34.0;
            if self.cfg.encoder == "libx264" {
                cy += 10.0;
                cy += ui.note(x, cy, w, Note::Warn, "warn", "**x264 encode sur le processeur.** En jeu, il prend des cœurs au jeu et peut faire chuter les FPS. À réserver aux PC sans encodeur matériel.", 0.0).0;
            }
            cy += 10.0;
            self.lab(ui.c, x, cy, 50.0, "Qualité", Some("débit max"));
            let out = format!("×{} · {}{}", fr(self.cfg.quality as f64, 2), if k < 1.0 { "max " } else { "" }, fr(cb as f64, 0));
            let (s, fh) = self.fader(ui, id("q"), fx, cy, fw, self.cfg.quality, (0.5, 2.0, 0.05), (&out, " Mb/s"), Some(["×0,5", "×1,25", "×2"]));
            if s.changed {
                self.cfg.quality = s.v;
            }
            if s.done {
                self.set("quality", json!((s.v * 100.0).round() / 100.0));
            }
            cy += fh + 10.0;
            self.lab(ui.c, x, cy, 38.0, "Format", None);
            self.seg_cfg(ui, "container", fx, cy, &[("MP4", json!("mp4")), ("MKV", json!("mkv")), ("MOV", json!("mov"))], false);
            cy += 38.0 + 8.0;
            cy += ui.hint(fx, cy, fw, "Pour le Clip et le REC, sans réencodage. MP4 se lit partout ; MKV reste lisible après une coupure de courant.");
        }
        y = self.grp_end(ui, open, x, cy, w, false);

        // Raccourcis
        let (open, mut cy) = self.grp(ui, "clip:k", "Raccourcis", x, y, w);
        if open {
            // Le raccourci des statistiques (n° 3) est réglé dans l'onglet Statistiques.
            let shown: Vec<usize> = (0..HK.len()).filter(|i| *i != 3).collect();
            for (n, &i) in shown.iter().enumerate() {
                cy = self.hk_row(ui, i, x, cy, w);
                // Filet entre deux raccourcis (avant : « i < 3 », le dernier séparateur manquait).
                if n + 1 < shown.len() {
                    ui.c.rect(r(x, cy - 1.0, w, 1.0), pal::LINE);
                }
            }
            cy += 8.0;
            cy += ui.hint(x, cy, w, "Cliquez « Modifier », puis appuyez sur la combinaison voulue (avec Ctrl, Alt ou Maj). Échap annule.");
        }
        y = self.grp_end(ui, open, x, cy, w, false);

        // Emplacements
        let (open, mut cy) = self.grp(ui, "clip:l", "Emplacements", x, y, w);
        if open {
            let rows = [("folder", "Clips et REC", self.cfg.out_dir.clone(), "out_dir"), ("hdd", "Buffer temporaire", self.cfg.buffer_dir.clone(), "buffer_dir")];
            for (i, (icon, what, path, key)) in rows.iter().enumerate() {
                let bw = Ui::btn_w("Changer…", false, Btn::N);
                let tw = w - 30.0 - 12.0 - bw;
                let p = path.display().to_string();
                let sp = vec![crate::gfx::Span { t: p, f: F::m(12.0, 400), c: pal::TEXT2 }];
                let ph = ui.c.para(0.0, 0.0, tw, 17.4, &sp, false);
                let rh = 10.0 + 19.0 + 3.0 + ph + 3.0 + 17.4 + 10.0;
                ui.c.icon(icon, x, cy + (rh - 18.0) / 2.0, 18.0, pal::TEXT3);
                ui.c.text(x + 30.0, cy + 10.0, 19.0, what, F::s(13.0, 500), pal::TEXT);
                ui.c.para(x + 30.0, cy + 32.0, tw, 17.4, &sp, true);
                let fy = cy + 35.0 + ph;
                let lx = x + 30.0 + ui.c.text(x + 30.0, fy, 17.4, "Espace libre : ", F::s(12.0, 400), pal::TEXT3);
                match self.free.get(path) {
                    Some(f) => ui.c.text(lx, fy, 17.4, &bytes(*f), F::m(12.0, 500), pal::TEXT),
                    None => ui.c.text(lx, fy, 17.4, "…", F::s(12.0, 400), pal::TEXT3),
                };
                if ui.btn(idn("pick", i), r(x + w - bw, cy + (rh - 32.0) / 2.0, bw, 32.0), "Changer…", None, Btn::N) {
                    self.send(json!({"cmd": "pick_folder", "key": key}));
                }
                cy += rh;
                if i == 0 {
                    ui.c.rect(r(x, cy - 1.0, w, 1.0), pal::LINE);
                }
            }
            cy += 6.0;
            cy += ui.hint(x, cy, w, &format!("Le buffer réécrit en boucle (`≈ {} Go/h`) sans s'accumuler. Le placer sur un second disque ménage le SSD système.", fr(gh as f64, 1)));
        }
        y = self.grp_end(ui, open, x, cy, w, false);

        // Interface
        let (open, mut cy) = self.grp(ui, "clip:ui", "Interface", x, y, w);
        if open {
            let rows: [(&str, &str, &str, bool); 5] = [
                ("intro", "Intro à l'ouverture", "Le trombone se clipse sur la page, avec son bruitage (environ 3 s, un clic la passe).", self.cfg.intro),
                ("sounds", "Sons de confirmation", "Deux notes qui montent au début d'un REC, qui descendent à la fin, un clic aigu quand un clip est enregistré, une note grave en cas d'erreur.", self.cfg.sounds),
                ("update_check", "Vérifier les mises à jour", "Au lancement, une seule question à GitHub. Une nouvelle version s'installe en un clic, réglages conservés.", self.cfg.update_check),
                ("delete_permanent", "Supprimer définitivement", "Les vidéos supprimées ne passent pas par la Corbeille : elles ne peuvent pas être récupérées.", self.cfg.delete_permanent),
                ("delete_confirm", "Confirmer avant de supprimer", "Désactivé par « Ne plus me demander » ; réactivez-le ici.", self.cfg.delete_confirm),
            ];
            for (i, (key, title, hint, on)) in rows.iter().enumerate() {
                let (hit, hh) = self.sw_row(ui, id(key), x, cy, w, title, hint, *on, false);
                if hit {
                    self.set(key, json!(!on));
                }
                cy += hh;
                if i < 4 {
                    cy = self.sw_sep(ui.c, x, cy, w);
                }
            }
        }
        y = self.grp_end(ui, open, x, cy, w, true);

        ui.scroll_end(id("set-clip"), view, y + 28.0 - view.y, clip);
    }

    /// Ligne d'un raccourci : libellé, touche active, bouton « Modifier » et conflit éventuel. Renvoie le y suivant.
    pub fn hk_row(&mut self, ui: &mut Ui, i: usize, x: f32, mut y: f32, w: f32) -> f32 {
        let (_, key, label) = HK[i];
        let want = pretty(&self.cfg_get(key).and_then(|v| v.as_str().map(String::from)).unwrap_or_default());
        let act = self.hk[i].clone();
        let wait = self.hk_wait == Some(i);
        ui.c.text(x, y + 6.0, 32.0, label, F::s(13.0, 400), pal::TEXT);
        let bl = if wait { "Annuler" } else { "Modifier" };
        let bw = Ui::btn_w(bl, false, Btn::G);
        if ui.btn(idn("hkedit", i), r(x + w - bw, y + 6.0, bw, 32.0), bl, None, Btn::G) {
            self.hk_wait = if wait { None } else { Some(i) };
        }
        let (kt, kc, kf) = if wait { ("Appuyez…".to_string(), pal::SIGNAL, F::s(12.0, 600).ls(0.06)) } else { match &act { Some(a) => (a.clone(), pal::TEXT, F::s(12.0, 600).ls(0.06)), None => ("aucun".into(), pal::TEXT3, F::s(12.0, 500).ls(0.06)) } };
        let kw = text_w(&kt, kf) + 16.0;
        let kb = r(x + w - bw - 12.0 - kw, y + 11.0, kw, 22.0);
        ui.c.rrect(r(kb.x, kb.y + 1.0, kb.w, kb.h), 4.0, pal::LINE2);
        ui.c.rrect(kb, 4.0, pal::WELL);
        ui.c.border(kb, 4.0, 1.0, if wait { pal::SIGNAL } else { pal::LINE2 });
        ui.c.text(kb.x + 8.0, kb.y + 1.0, 21.0, &kt, kf, kc);
        y += 44.0;
        if self.conflicts.contains(&want) {
            let t = format!(
                "{want} est déjà pris par une autre application (souvent l'overlay NVIDIA ou AMD Adrenalin). {}",
                match &act {
                    Some(a) if *a != want => format!("Clipper utilise {a} à la place."),
                    _ => "Aucun raccourci actif : choisissez-en un autre.".into(),
                }
            );
            ui.c.icon("warn", x, y - 4.0, 14.0, pal::WARN);
            let sp = vec![crate::gfx::Span { t, f: F::s(12.0, 400), c: pal::WARN }];
            y += ui.c.para(x + 21.0, y - 6.0, w - 21.0, 17.4, &sp, true) + 2.0;
        }
        y
    }

    /// Ligne d'une liste à icône (.lims li). Renvoie la hauteur.
    pub fn lim(&mut self, ui: &mut Ui, x: f32, y: f32, w: f32, icon: &'static str, text: &str) -> f32 {
        ui.c.icon(icon, x, y + 1.0, 15.0, pal::TEXT3);
        let sp = crate::gfx::md(text, F::s(12.5, 400), pal::TEXT2, 500, pal::TEXT);
        ui.c.para(x + 24.0, y, w - 24.0, 18.1, &sp, true)
    }

    /// Volet dépliable (details.more) : en-tête de 42 px. Renvoie (ouvert, y du contenu).
    pub fn details(&mut self, ui: &mut Ui, key: &str, title: &str, side: &str, mono: bool, x: f32, y: f32, w: f32) -> (bool, f32) {
        let open = self.fold(key);
        let head = r(x, y, w, 42.0);
        if ui.hot(head) {
            ui.c.rrect(head, 7.0, pal::WHITE.a(0.02));
        }
        if ui.click(id(key), head) {
            self.toggle_fold(key);
        }
        let rot = ui.anim(id(key) ^ 9, if open { 90.0 } else { 0.0 }, 0.2);
        ui.c.icon_rot("chev", x + 12.0, y + 14.0, 14.0, pal::TEXT3, rot);
        ui.c.text(x + 36.0, y, 42.0, title, F::s(13.0, 500), pal::TEXT);
        let f = if mono { F::m(12.0, 400) } else { F::s(12.0, 400) };
        ui.c.text_cut(x + w - 12.0 - text_w(side, f).min(w * 0.5), y, 42.0, w * 0.5, side, f, pal::TEXT3);
        (open, y + 42.0 + if open { 4.0 } else { 0.0 })
    }

    /// Cadre du volet, une fois sa hauteur connue. Renvoie le y sous le volet.
    pub fn details_end(&mut self, ui: &mut Ui, x: f32, y0: f32, y1: f32, w: f32) -> f32 {
        ui.c.border(r(x, y0, w, y1 - y0), 7.0, 1.0, pal::LINE);
        y1
    }

    /// Vu-mètre segmenté (.vu) : vert, jaune au-delà de 66 %, rouge au-delà de 86 %.
    fn vu(&mut self, ui: &mut Ui, wid: u64, q: R, level: f32) {
        let v = ui.anim(wid, level.clamp(0.0, 1.0), 0.08);
        let mut sx = 0.0;
        while sx < q.w {
            let sw = (q.w - sx).min(6.0);
            let p = sx / q.w;
            let col: Col = if p < 0.66 { pal::OK } else if p < 0.86 { pal::WARN } else { pal::REC };
            let lit = ((v * q.w - sx) / sw).clamp(0.0, 1.0);
            ui.c.rect(r(q.x + sx, q.y, sw, q.h), hex(0x262e24));
            if lit > 0.0 {
                ui.c.rect(r(q.x + sx, q.y, sw * lit, q.h), col);
            }
            sx += 8.0;
        }
    }

    pub fn tab_audio(&mut self, ui: &mut Ui, body: R) {
        let tracks = self.cfg.audio_tracks.clone();
        let has = |t: &str| tracks.iter().any(|x| x == t);
        // ---------- colonne de gauche : niveaux en direct ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-aud"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        ui.sc(x, y, 16.0, "Niveaux en direct", pal::TEXT2, 500);
        y += 16.0 + 14.0;
        if !tracks.is_empty() {
            let (fx, fw) = (x + 132.0, w - 132.0);
            self.lab(ui.c, x, y, 34.0, "Environnement", None);
            self.vu(ui, id("vuG"), r(fx, y + 12.0, fw, 10.0), self.levels.0);
            y += 44.0;
            self.lab(ui.c, x, y, 34.0, "Micro", None);
            let muted = self.cfg.mic_muted;
            self.vu(ui, id("vuM"), r(fx, y + 12.0, fw - 102.0, 10.0), if muted { 0.0 } else { self.levels.1 });
            let st = if muted { Btn::D } else { Btn::N };
            if ui.btn(id("mute"), r(fx + fw - 92.0, y + 1.0, 92.0, 32.0), if muted { "Coupé" } else { "Couper" }, Some(if muted { "micoff" } else { "mic" }), st) {
                self.set("mic_muted", json!(!muted));
            }
            y += 34.0 + 14.0;
        }
        let names: Vec<&str> = ["mix", "game", "mic"].into_iter().filter(|t| has(t)).map(|t| match t { "mix" => "Tout", "game" => "Environnement", _ => "Micro" }).collect();
        let am = if tracks.is_empty() { "Clips muets : activez au moins une piste.".to_string() } else { format!("{} piste{} audio dans chaque clip, dans l'ordre : {}.", tracks.len(), if tracks.len() > 1 { "s" } else { "" }, names.join(", ")) };
        y += ui.hint(x, y, w, &am) + 14.0;
        y += ui.hint(x, y, w, "Le son est lu par WASAPI, comme Discord ou OBS : rien n'est injecté dans le jeu. Les niveaux ne sont mesurés que quand cet onglet est ouvert.");
        ui.scroll_end(id("mon-aud"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("set-aud"), view);
        let (x, w) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let (fx, fw) = (x + 132.0, w - 132.0);
        let mut y = view.y + 4.0;
        let (open, mut cy) = self.grp(ui, "aud:a", "Pistes", x, y, w);
        if open {
            let cw = ((w - 20.0) / 3.0).floor();
            let cards = [("mix", "Tout le son", "Environnement et micro mélangés, prêt à partager."), ("game", "Environnement", "Tout le son du PC : jeux, Discord, musique."), ("mic", "Micro", "Votre voix seule.")];
            let hf = F::s(11.5, 400);
            let hs: Vec<f32> = cards.iter().map(|c| { let sp = crate::gfx::md(c.2, hf, pal::TEXT3, 500, pal::TEXT2); ui.c.para(0.0, 0.0, cw - 24.0, 15.5, &sp, false) }).collect();
            let chh = 12.0 + 19.0 + 8.0 + 32.0 + 8.0 + hs.iter().cloned().fold(0.0, f32::max) + 12.0;
            let mut toggled = None;
            for (i, (k, name, hint)) in cards.iter().enumerate() {
                let q = r(x + (cw + 10.0) * i as f32, cy, cw, chh);
                let on = has(k);
                ui.c.rrect(q, 7.0, if on { pal::SIGNAL.a(0.05) } else { pal::WHITE.a(0.012) });
                ui.c.border(q, 7.0, 1.0, if on { pal::SIGNAL_SOFT } else { pal::LINE });
                ui.c.text(q.x + 12.0, q.y + 12.0, 19.0, name, F::s(13.0, 600), pal::TEXT);
                let sw_hit = ui.switch(idn("trk", i), q.x + 12.0, q.y + 43.0, on, false);
                let sp = crate::gfx::md(hint, hf, pal::TEXT3, 500, pal::TEXT2);
                ui.c.para(q.x + 12.0, q.y + 79.0, cw - 24.0, 15.5, &sp, true);
                if sw_hit || ui.click(idn("trkc", i), q) {
                    toggled = Some(*k);
                }
            }
            if let Some(k) = toggled {
                let mut on: Vec<&str> = tracks.iter().map(String::as_str).collect();
                if on.contains(&k) { on.retain(|x| *x != k) } else { on.push(k) }
                let ordered: Vec<&str> = ["mix", "game", "mic"].into_iter().filter(|t| on.contains(t)).collect();
                self.set("audio_tracks", json!(ordered));
            }
            cy += chh;
        }
        y = self.grp_end(ui, open, x, cy, w, tracks.is_empty());
        if !tracks.is_empty() {
            let devs = |list: &[(String, String)]| -> Vec<(String, bool)> { std::iter::once(("Par défaut (Windows)".to_string(), true)).chain(list.iter().map(|d| (d.1.clone(), true))).collect() };
            let (open, mut cy) = self.grp(ui, "aud:g", "Environnement", x, y, w);
            if open {
                ui.c.icon("spk", x, cy + 9.5, 15.0, pal::TEXT2);
                self.lab(ui.c, x + 22.0, cy, 34.0, "Sortie écoutée", None);
                let sel = self.cfg.out_device.as_ref().and_then(|d| self.outputs.iter().position(|o| &o.0 == d)).map_or(0, |i| i + 1);
                if let Some(i) = ui.select(id("outDev"), r(fx, cy, fw, 34.0), &devs(&self.outputs), Some(sel)) {
                    let v = if i == 0 { json!(null) } else { json!(self.outputs[i - 1].0) };
                    self.set("out_device", v);
                }
                cy += 44.0;
                self.lab(ui.c, x, cy, 34.0, "Volume", None);
                let pct = (self.cfg.game_volume * 100.0).round();
                let (s, _) = self.fader(ui, id("gv"), fx, cy + 1.0, fw, pct, (0.0, 100.0, 1.0), (&format!("{pct:.0}"), " %"), None);
                if s.changed {
                    self.cfg.game_volume = s.v / 100.0;
                }
                if s.done {
                    self.set("game_volume", json!(s.v / 100.0));
                }
                cy += 34.0;
            }
            y = self.grp_end(ui, open, x, cy, w, false);
            let (open, mut cy) = self.grp(ui, "aud:m", "Micro", x, y, w);
            if open {
                ui.c.icon("mic", x, cy + 9.5, 15.0, pal::TEXT2);
                self.lab(ui.c, x + 22.0, cy, 34.0, "Entrée", None);
                let sel = self.cfg.mic_device.as_ref().and_then(|d| self.inputs.iter().position(|o| &o.0 == d)).map_or(0, |i| i + 1);
                if let Some(i) = ui.select(id("micDev"), r(fx, cy, fw, 34.0), &devs(&self.inputs), Some(sel)) {
                    let v = if i == 0 { json!(null) } else { json!(self.inputs[i - 1].0) };
                    self.set("mic_device", v);
                }
                cy += 44.0;
                self.lab(ui.c, x, cy, 34.0, "Volume", None);
                let pct = (self.cfg.mic_volume * 100.0).round();
                let (s, _) = self.fader(ui, id("mv"), fx, cy + 1.0, fw, pct, (0.0, 100.0, 1.0), (&format!("{pct:.0}"), " %"), None);
                if s.changed {
                    self.cfg.mic_volume = s.v / 100.0;
                }
                if s.done {
                    self.set("mic_volume", json!(s.v / 100.0));
                }
                cy += 44.0;
                self.lab(ui.c, x, cy, 38.0, "Boost", None);
                self.seg_cfg(ui, "mic_boost_db", fx, cy, &[("Aucun", json!(0.0)), ("+6 dB", json!(6.0)), ("+12 dB", json!(12.0)), ("+20 dB", json!(20.0))], false);
                cy += 38.0 + 6.0;
                cy += ui.hint(fx, cy, fw, "Pour un micro trop faible, en plus du volume. Les cris sont arrondis au lieu de grésiller.") + 16.0;
                let on = self.cfg.mic_denoise;
                let (hit, hh) = self.sw_row(ui, id("mic_denoise"), x, cy, w, "Réduction de bruit", "Retire le ventilateur, le souffle et les bruits de fond du micro, comme Discord (RNNoise, calculé sur votre PC). Coût mesuré : environ 0,7 % d'un cœur du processeur pendant que le micro est enregistré (moins de 0,1 % du processeur entier sur un 8 cœurs) ; rien quand le micro est coupé ou absent des pistes.", on, false);
                if hit {
                    self.set("mic_denoise", json!(!on));
                }
                cy += hh;
            }
            y = self.grp_end(ui, open, x, cy, w, true);
        }
        ui.scroll_end(id("set-aud"), view, y + 28.0 - view.y, clip);
    }
}
