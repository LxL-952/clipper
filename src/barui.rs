//! Barre Alt+Z, dessinée nativement dans une fenêtre à transparence par pixel : la barre, une notification,
//! les derniers clips et un lecteur sur place. Mêmes commandes et mêmes messages que l'ancienne page.
use crate::config::Config;
use crate::front::{dur, pretty};
use crate::gfx::{Col, F, R, hex, pal, r, text_w};
use crate::player::Player;
use crate::widgets::{Btn, Ui, ease, id, idn};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Clip {
    pub path: PathBuf,
    pub name: String,
    pub stem: String,
    pub secs: f64,
    pub audio: usize,
    pub video: bool,
}

pub struct BarUi {
    pub cfg: Config,
    pub hk: [Option<String>; 4],
    pub native: (u32, u32),
    pub overlay_on: bool,
    pub recording: bool,
    pub rec_t0: Instant,
    pub out: Vec<Value>,
    pub shown: bool,
    /// Début de l'animation d'entrée, ou de sortie.
    anim_in: Instant,
    anim_out: Option<Instant>,
    clips: Vec<Clip>,
    cur: Option<PathBuf>,
    pub player: Player,
    thumbs: HashMap<String, Option<tiny_skia::Pixmap>>,
    toast: Option<(String, bool, Option<PathBuf>, Instant)>,
    busy: bool,
    pend: Map<String, Value>,
    flush_at: Option<Instant>,
    pub next: Option<Duration>,
}

const BUF: [u32; 6] = [60, 90, 120, 180, 300, 600];
const RES: [u32; 4] = [720, 1080, 1440, 2160];
/// Pistes audio : préréglages parcourus au clic (muet, tout, PC + micro, les trois).
const AUD: [&[&str]; 4] = [&[], &["mix"], &["game", "mic"], &["mix", "game", "mic"]];

impl BarUi {
    pub fn new(cfg: &Config, wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> BarUi {
        let now = Instant::now();
        BarUi {
            cfg: cfg.clone(),
            hk: Default::default(),
            native: (1920, 1080),
            overlay_on: false,
            recording: false,
            rec_t0: now,
            out: vec![],
            shown: false,
            anim_in: now,
            anim_out: None,
            clips: vec![],
            cur: None,
            player: Player::new(&cfg.ffmpeg, wake),
            thumbs: HashMap::new(),
            toast: None,
            busy: false,
            pend: Map::new(),
            flush_at: None,
            next: None,
        }
    }

    fn send(&mut self, v: Value) {
        self.out.push(v);
    }

    fn set(&mut self, key: &str, value: Value) {
        let mut v = serde_json::to_value(&self.cfg).unwrap_or_default();
        let mut slot = &mut v;
        for part in key.split('.') {
            match slot.get_mut(part) {
                Some(s) => slot = s,
                None => return,
            }
        }
        *slot = value.clone();
        if let Ok(c) = serde_json::from_value::<Config>(v) {
            self.cfg = c;
        }
        // Affichage immédiat ; clics rapprochés regroupés en un seul envoi (une seule relance de la capture).
        self.pend.insert(key.into(), value);
        self.flush_at = Some(Instant::now() + Duration::from_millis(450));
    }

    fn flush(&mut self) {
        self.flush_at = None;
        if !self.pend.is_empty() {
            let values = Value::Object(std::mem::take(&mut self.pend));
            self.send(json!({"cmd": "set_many", "values": values}));
        }
    }

    fn close_player(&mut self) {
        self.player.close();
        self.cur = None;
    }

    fn play(&mut self, path: PathBuf) {
        let Some((secs, audio, video)) = self.clips.iter().find(|c| c.path == path).map(|c| (c.secs, c.audio, c.video)) else { return };
        self.cur = Some(path.clone());
        if video {
            self.player.open(&path, secs, audio);
            self.player.toggle(); // lecture tout de suite
        } else {
            self.player.close();
        }
    }

    pub fn on(&mut self, m: &Value) {
        match m["type"].as_str().unwrap_or("") {
            // Barre affichée : état du moment, animation d'entrée, derniers clips.
            "show" => {
                if let Ok(c) = serde_json::from_value(m["config"].clone()) {
                    self.cfg = c;
                }
                let hk = |k: &str| m["hotkeys"][k].as_str().map(String::from);
                self.hk = [hk("clip"), hk("rec"), hk("bar"), hk("overlay")];
                self.native = (m["native"][0].as_u64().unwrap_or(1920) as u32, m["native"][1].as_u64().unwrap_or(1080) as u32);
                self.overlay_on = m["overlay_on"].as_bool().unwrap_or(false);
                self.recording = m["recording"].as_bool().unwrap_or(false);
                self.rec_t0 = Instant::now() - Duration::from_secs(m["rec_secs"].as_u64().unwrap_or(0));
                (self.shown, self.anim_in, self.anim_out, self.busy, self.toast) = (true, Instant::now(), None, false, None);
                self.pend.clear();
                self.send(json!({"cmd": "recent"}));
            }
            // Fermeture : animation de sortie, lecture arrêtée.
            "hide" => {
                self.flush();
                self.shown = false;
                self.anim_out = Some(Instant::now());
                self.close_player();
            }
            "state" => {
                self.recording = m["recording"].as_bool().unwrap_or(false);
                self.rec_t0 = Instant::now() - Duration::from_secs(m["rec_secs"].as_u64().unwrap_or(0));
            }
            "config" => {
                if self.pend.is_empty() {
                    if let Ok(c) = serde_json::from_value(m["config"].clone()) {
                        self.cfg = c;
                    }
                }
            }
            "hotkeys" => {
                let hk = |k: &str| m["hotkeys"][k].as_str().map(String::from);
                self.hk = [hk("clip"), hk("rec"), hk("bar"), hk("overlay")];
            }
            "recent" => {
                self.clips = m["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| {
                        let path = PathBuf::from(v["path"].as_str().unwrap_or(""));
                        Clip {
                            stem: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                            path,
                            name: v["name"].as_str().unwrap_or("").into(),
                            secs: v["secs"].as_f64().unwrap_or(0.0),
                            audio: v["audio"].as_u64().unwrap_or(0) as usize,
                            video: v["video"].as_bool().unwrap_or(true),
                        }
                    })
                    .collect();
                self.thumbs.retain(|k, _| self.clips.iter().any(|c| &c.stem == k));
            }
            "toast" => {
                self.busy = false;
                if self.shown {
                    let path = m["path"].as_str().map(PathBuf::from);
                    self.toast = Some((m["text"].as_str().unwrap_or("").into(), m["kind"] == "error", path.clone(), Instant::now()));
                    if path.is_some() {
                        self.send(json!({"cmd": "recent"}));
                    }
                }
            }
            "overlay" => self.overlay_on = m["on"].as_bool().unwrap_or(false),
            _ => {}
        }
    }

    fn wake_in(&mut self, d: Duration) {
        self.next = Some(self.next.map_or(d, |n| n.min(d)));
    }

    /// Bouton de la barre : icône, valeur en gras, légende en police fixe dessous. Renvoie (clic, largeur).
    fn bbtn(&self, ui: &mut Ui, wid: u64, x: f32, y: f32, icon: Option<&'static str>, v: &str, k: &str, st: (Col, Col, Option<Col>, f32)) -> (bool, f32) {
        let (fg, kc, bg, minw) = st;
        let vf = if bg.is_none() && fg == pal::REC { F::m(13.0, 600) } else { F::s(13.0, 600) };
        let tw = text_w(v, vf).max(text_w(k, F::m(10.0, 500).ls(0.02)));
        let w = (22.0 + 17.0 + 8.0 + tw).ceil().max(minw);
        let q = r(x, y, w, 48.0);
        let hot = ui.hot(q);
        let hit = ui.click(wid, q);
        match bg {
            Some(b) => ui.c.rrect(q, 7.0, if hot { hex(0xff9148) } else { b }),
            None if hot => ui.c.rrect(q, 7.0, if ui.held(wid) { pal::LINE2 } else { pal::RAISE_H }),
            None => {}
        }
        match icon {
            Some(n) => ui.c.icon(n, q.x + 11.0, q.y + 15.5, 17.0, fg),
            None => ui.led(q.x + 19.5, q.y + 24.0, 9.0, if fg == pal::REC { pal::REC } else { pal::TEXT3 }),
        }
        ui.c.text(q.x + 36.0, q.y + 9.0, 15.0, v, vf, fg);
        ui.c.text(q.x + 36.0, q.y + 26.0, 12.0, k, F::m(10.0, 500).ls(0.02), kc);
        (hit, w)
    }

    fn thumb(&mut self, stem: &str, w: u32, h: u32) -> Option<&tiny_skia::Pixmap> {
        let stale = self.thumbs.get(stem).is_none_or(|t| t.as_ref().is_some_and(|p| (p.width(), p.height()) != (w, h)));
        if stale {
            self.thumbs.insert(stem.to_string(), crate::front::load_thumb(stem, w, h));
        }
        self.thumbs.get(stem).and_then(|t| t.as_ref())
    }

    pub fn frame(&mut self, ui: &mut Ui) {
        let now = ui.now;
        self.next = None;
        ui.begin();
        ui.c.clear(Col(0, 0, 0, 0));
        let (w, h) = ui.c.size();
        // Entrée : la barre descend (0,26 s) ; sortie : elle remonte (0,18 s).
        let k = match self.anim_out {
            Some(t) => 1.0 - (now.duration_since(t).as_secs_f32() / 0.18).min(1.0),
            None => ease(now.duration_since(self.anim_in).as_secs_f32() / 0.26),
        };
        if k < 1.0 && (self.shown || self.anim_out.is_some()) {
            ui.s.animating = true;
        }
        ui.c.oy = -68.0 * (1.0 - k);

        // ---------- la barre ----------
        let c = self.cfg.clone();
        let nat_h = self.native.1;
        let res_label = if nat_h >= c.clip.height && nat_h as f32 <= c.clip.height as f32 * 1.12 { format!("{nat_h}p") } else { format!("{}p", c.clip.height) };
        let buf_label = if c.buffer_secs % 60 != 0 { format!("{}:{:02}", c.buffer_secs / 60, c.buffer_secs % 60) } else { format!("{} min", c.buffer_secs / 60) };
        let aud_label = match c.audio_tracks.len() {
            0 => "Muet".to_string(),
            1 => "1 piste".to_string(),
            n => format!("{n} pistes"),
        };
        let rec_secs = self.rec_t0.elapsed().as_secs();
        let rec_label = if self.recording { format!("{:02}:{:02}", rec_secs / 60, rec_secs % 60) } else { "REC".into() };
        let hk = |i: usize| self.hk[i].clone().unwrap_or_default();
        let plain = (pal::TEXT, pal::TEXT3, None, 0.0);
        // Largeur totale d'abord (la barre est centrée), avec les mêmes mesures que le dessin.
        let measure = |v: &str, kk: &str, minw: f32, mono: bool| (22.0 + 17.0 + 8.0 + text_w(v, if mono { F::m(13.0, 600) } else { F::s(13.0, 600) }).max(text_w(kk, F::m(10.0, 500).ls(0.02)))).ceil().max(minw);
        let items: [(String, String, f32, bool); 8] = [
            ("Clip".into(), hk(0), 104.0, false),
            (rec_label.clone(), if self.recording { "arrêter".into() } else { hk(1) }, 104.0, self.recording),
            (buf_label.clone(), "buffer".into(), 0.0, false),
            (res_label.clone(), "résolution".into(), 0.0, false),
            (format!("{} fps", c.clip.fps), "images/s".into(), 0.0, false),
            (aud_label.clone(), "audio".into(), 0.0, false),
            (if self.overlay_on { "Stats on".into() } else { "Stats off".into() }, hk(3), 0.0, false),
            ("Réglages".into(), "Échap ferme".into(), 0.0, false),
        ];
        let bw: f32 = items.iter().map(|i| measure(&i.0, &i.1, i.2, i.3)).sum::<f32>() + 2.0 * 9.0 + 2.0 * 9.0 + 12.0;
        let bar = r(((w - bw) / 2.0).round(), 0.0, bw, 62.0);
        ui.c.shadow(bar, 12.0, 8.0, 14.0, pal::BLACK.a(0.5));
        ui.c.rrect(r(bar.x, bar.y - 12.0, bar.w, bar.h + 12.0), 12.0, pal::BODY.a(0.96));
        ui.c.border(r(bar.x, bar.y - 12.0, bar.w, bar.h + 12.0), 12.0, 1.0, pal::LINE2);
        let (mut x, y) = (bar.x + 6.0, 7.0);
        let sep = |ui: &mut Ui, x: &mut f32| {
            ui.c.rect(r(*x + 4.0, 16.0, 1.0, 30.0), pal::LINE);
            *x += 11.0;
        };
        let clip_st = (pal::SIGNAL_INK, pal::SIGNAL_INK.a(0.72), Some(if self.busy { pal::SIGNAL.a(0.6) } else { pal::SIGNAL }), 104.0);
        let (hit, bw_) = self.bbtn(ui, id("b-clip"), x, y, Some("b-clip"), "Clip", &items[0].1, clip_st);
        if hit && !self.busy {
            self.busy = true;
            self.send(json!({"cmd": "clip"}));
        }
        x += bw_ + 2.0;
        let rec_st = (if self.recording { pal::REC } else { pal::TEXT }, pal::TEXT3, None, 104.0);
        let (hit, bw_) = self.bbtn(ui, id("b-rec"), x, y, None, &rec_label, &items[1].1, rec_st);
        if hit {
            self.send(json!({"cmd": "rec"}));
        }
        x += bw_;
        sep(ui, &mut x);
        let (hit, bw_) = self.bbtn(ui, id("b-buf"), x, y, Some("b-buf"), &buf_label, "buffer", plain);
        if hit {
            let i = BUF.iter().position(|b| *b == c.buffer_secs).map_or(0, |i| (i + 1) % BUF.len());
            self.set("buffer_secs", json!(BUF[i]));
        }
        x += bw_ + 2.0;
        let (hit, bw_) = self.bbtn(ui, id("b-res"), x, y, Some("b-res"), &res_label, "résolution", plain);
        if hit {
            // Même règle que capture.rs : une classe au-dessus de la hauteur native est refusée.
            let mut i = RES.iter().position(|b| *b == c.clip.height).unwrap_or(0);
            for _ in 0..RES.len() {
                i = (i + 1) % RES.len();
                if RES[i] <= nat_h {
                    break;
                }
            }
            self.set("clip.height", json!(RES[i]));
        }
        x += bw_ + 2.0;
        let (hit, bw_) = self.bbtn(ui, id("b-fps"), x, y, Some("b-fps"), &items[4].0, "images/s", plain);
        if hit {
            self.set("clip.fps", json!(if c.clip.fps == 60 { 30 } else { 60 }));
        }
        x += bw_ + 2.0;
        let (hit, bw_) = self.bbtn(ui, id("b-aud"), x, y, Some("b-aud"), &aud_label, "audio", plain);
        if hit {
            let cur: Vec<&str> = c.audio_tracks.iter().map(String::as_str).collect();
            let i = AUD.iter().position(|a| *a == cur.as_slice()).map_or(0, |i| (i + 1) % AUD.len());
            self.set("audio_tracks", json!(AUD[i]));
        }
        x += bw_ + 2.0;
        let st = (if self.overlay_on { pal::OK } else { pal::TEXT }, pal::TEXT3, None, 0.0);
        let (hit, bw_) = self.bbtn(ui, id("b-stats"), x, y, Some("b-stats"), &items[6].0, &items[6].1, st);
        if hit {
            self.send(json!({"cmd": "overlay_toggle"}));
        }
        x += bw_;
        sep(ui, &mut x);
        let (hit, _) = self.bbtn(ui, id("b-set"), x, y, Some("b-set"), "Réglages", "Échap ferme", plain);
        if hit {
            self.send(json!({"cmd": "open_settings"}));
        }

        // ---------- sous la barre ----------
        ui.c.oy = -8.0 * (1.0 - k);
        let mut uy = 72.0;
        let card = |ui: &mut Ui, q: R| {
            ui.c.shadow(q, 10.0, 6.0, 12.0, pal::BLACK.a(0.45));
            ui.c.rrect(q, 10.0, pal::BODY.a(0.96));
            ui.c.border(q, 10.0, 1.0, pal::LINE2);
        };
        // notification
        if let Some((text, err, path, t0)) = self.toast.clone() {
            let left = Duration::from_secs(6).saturating_sub(now.duration_since(t0));
            if left.is_zero() {
                self.toast = None;
            } else {
                self.wake_in(left);
                let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let see = !err && path.is_some();
                let tw = text_w(&text, F::s(13.0, 600)).max(text_w(&name, F::s(11.5, 500))).min(420.0);
                let bwid = if see { Ui::btn_w("Regarder", false, Btn::N) + 10.0 } else { 0.0 };
                let q = r(((w - (24.0 + tw + bwid)) / 2.0).round(), uy, 24.0 + tw + bwid, if name.is_empty() { 38.0 } else { 52.0 });
                card(ui, q);
                ui.c.text(q.x + 14.0, q.y + 9.0, 18.0, &text, F::s(13.0, 600), if err { pal::REC } else { pal::TEXT });
                if !name.is_empty() {
                    ui.c.text_cut(q.x + 14.0, q.y + 28.0, 16.0, tw, &name, F::s(11.5, 500), pal::TEXT2);
                }
                if see && ui.btn(id("t-see"), r(q.right() - bwid, q.y + (q.h - 32.0) / 2.0, bwid - 10.0, 32.0), "Regarder", None, Btn::N) {
                    self.toast = None;
                    self.play(path.unwrap());
                }
                uy += q.h + 10.0;
            }
        }
        // derniers clips
        if !self.clips.is_empty() {
            let n = self.clips.len().min(4);
            let gw = 16.0 + n as f32 * 158.0 + (n as f32 - 1.0) * 8.0;
            let q = r(((w - gw) / 2.0).round(), uy, gw, 8.0 + 16.0 + 4.0 + 92.0 + 5.0 + 17.0 + 4.0 + 8.0);
            card(ui, q);
            ui.c.text(q.x + 12.0, q.y + 8.0, 16.0, "DERNIERS CLIPS", F::m(10.0, 500).ls(0.08), pal::TEXT3);
            let mut pick = None;
            for i in 0..n {
                let (stem, name, secs, sel) = {
                    let cl = &self.clips[i];
                    (cl.stem.clone(), cl.name.clone(), cl.secs, self.cur.as_ref() == Some(&cl.path))
                };
                let cell = r(q.x + 8.0 + i as f32 * 166.0, q.y + 28.0, 158.0, 122.0);
                let hot = ui.hot(cell);
                if hot || sel {
                    ui.c.rrect(cell, 7.0, pal::RAISE_H);
                }
                if ui.click(idn("gal", i), cell) {
                    pick = Some(i);
                }
                let th = r(cell.x + 4.0, cell.y + 4.0, 150.0, 84.0);
                ui.c.rrect(th, 5.0, hex(0x0c0f0b));
                let (pw, ph) = ((th.w * ui.c.s).round() as u32, (th.h * ui.c.s).round() as u32);
                if let Some(img) = self.thumb(&stem, pw, ph) {
                    ui.c.blit(img.data(), img.width(), img.height(), th.x, th.y);
                }
                let d = dur(secs);
                let dw = text_w(&d, F::m(10.5, 500)) + 10.0;
                ui.c.rrect(r(th.right() - 4.0 - dw, th.bottom() - 4.0 - 15.0, dw, 15.0), 4.0, hex(0x080b08).a(0.82));
                ui.c.text(th.right() + 1.0 - dw, th.bottom() - 19.0, 15.0, &d, F::m(10.5, 500), pal::TEXT);
                let short = name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a);
                ui.c.text_cut(cell.x + 4.0, th.bottom() + 5.0, 17.0, 150.0, short, F::s(11.5, 500), if sel { pal::TEXT } else { pal::TEXT2 });
            }
            if let Some(i) = pick {
                let p = self.clips[i].path.clone();
                self.play(p);
            }
            uy += q.h + 10.0;
        }
        // lecteur sur place
        if let Some(path) = self.cur.clone() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let pw = 780.0f32.min(w - 24.0);
            let sh = ((pw - 16.0) * 9.0 / 16.0).min(h - uy - 110.0).max(120.0);
            let sw = (sh * 16.0 / 9.0).min(pw - 16.0);
            let q = r(((w - pw) / 2.0).round(), uy, pw, 8.0 + 32.0 + 8.0 + sh + 8.0 + 30.0 + 8.0);
            card(ui, q);
            let b2 = Ui::btn_w("Fermer", false, Btn::N);
            let b1 = Ui::btn_w("Ouvrir dans Clipper", false, Btn::N);
            ui.c.text_cut(q.x + 14.0, q.y + 8.0, 32.0, pw - 44.0 - b1 - b2, &name, F::s(13.0, 600), pal::TEXT);
            if ui.btn(id("pl-x"), r(q.right() - 8.0 - b2, q.y + 8.0, b2, 32.0), "Fermer", None, Btn::N) {
                self.close_player();
            }
            if ui.btn(id("pl-open"), r(q.right() - 16.0 - b2 - b1, q.y + 8.0, b1, 32.0), "Ouvrir dans Clipper", None, Btn::N) {
                self.send(json!({"cmd": "open_settings", "tab": "videos"}));
            }
            let scr = r(q.x + (pw - sw) / 2.0, q.y + 48.0, sw, sh);
            ui.c.rrect(scr, 6.0, pal::BLACK);
            self.player.set_size(scr.w * ui.c.s, scr.h * ui.c.s);
            let video = self.clips.iter().find(|c| c.path == path).is_none_or(|c| c.video);
            if !video {
                ui.c.text_in(scr, 0, "Ce fichier ne contient aucune image.", F::s(13.0, 500), pal::TEXT2);
            } else if !self.player.draw(ui.c, scr, 6.0) {
                ui.c.text_in(scr, 0, "Préparation de la lecture…", F::s(13.0, 500), pal::TEXT2);
            }
            if ui.click(id("pl-img"), scr) {
                self.player.toggle();
            }
            // commandes
            let cy = scr.bottom() + 8.0;
            let playing = self.player.playing;
            let pb = r(q.x + 10.0, cy, 32.0, 30.0);
            if ui.hot(pb) {
                ui.c.rrect(pb, 6.0, pal::RAISE_H);
            }
            ui.c.icon(if playing { "pause" } else { "play" }, pb.x + 7.5, pb.y + 6.5, 17.0, pal::TEXT);
            if ui.click(id("c-play"), pb) {
                self.player.toggle();
            }
            let (d, pos) = (self.player.dur.max(0.001), self.player.pos());
            let f = F::m(12.0, 500);
            let mut tx = pb.right() + 6.0;
            tx += ui.c.text(tx, cy, 30.0, &dur(pos), f, pal::TEXT);
            tx += ui.c.text(tx, cy, 30.0, &format!(" / {}", dur(self.player.dur)), f, pal::TEXT3) + 10.0;
            let vol = r(q.right() - 10.0 - 70.0, cy + 6.0, 70.0, 18.0);
            let mb = r(vol.x - 38.0, cy, 32.0, 30.0);
            let sk = r(tx, cy + 6.0, mb.x - 8.0 - tx, 18.0);
            if ui.hot(sk) {
                ui.s.hand = true;
                if ui.i.pressed {
                    ui.s.active = id("seek");
                }
            }
            let mut p = (pos / d) as f32;
            if ui.s.active == id("seek") {
                p = ((ui.mouse().0 - sk.x) / sk.w).clamp(0.0, 1.0);
                self.player.seek(p as f64 * d);
            }
            ui.c.rrect(r(sk.x, sk.cy() - 1.5, sk.w, 3.0), 2.0, pal::TEXT.a(0.2));
            ui.c.rrect(r(sk.x, sk.cy() - 1.5, sk.w * p, 3.0), 2.0, pal::SIGNAL);
            ui.c.circle(sk.x + sk.w * p, sk.cy(), 5.5, pal::SIGNAL);
            let quiet = self.player.muted || self.player.vol == 0.0;
            if ui.hot(mb) {
                ui.c.rrect(mb, 6.0, pal::RAISE_H);
            }
            ui.c.icon(if quiet { "mute" } else { "spk1" }, mb.x + 7.5, mb.y + 6.5, 17.0, pal::TEXT);
            if ui.click(id("c-mute"), mb) {
                let (v, m) = if quiet { (if self.player.vol == 0.0 { 0.5 } else { self.player.vol }, false) } else { (self.player.vol, true) };
                self.player.set_volume(v, m);
            }
            if ui.hot(vol) {
                ui.s.hand = true;
                if ui.i.pressed {
                    ui.s.active = id("vol");
                }
            }
            let mut v = if self.player.muted { 0.0 } else { self.player.vol };
            if ui.s.active == id("vol") {
                v = ((ui.mouse().0 - vol.x - 5.0) / (vol.w - 10.0)).clamp(0.0, 1.0);
                self.player.set_volume(v, v == 0.0);
            }
            ui.c.rrect(r(vol.x, vol.cy() - 1.5, vol.w, 3.0), 2.0, pal::TEXT.a(0.2));
            ui.c.rrect(r(vol.x, vol.cy() - 1.5, vol.w * v, 3.0), 2.0, pal::TEXT);
            ui.c.circle(vol.x + 5.0 + (vol.w - 10.0) * v, vol.cy(), 5.5, pal::TEXT);
        }
        ui.c.oy = 0.0;

        // ---------- clavier ----------
        for key in ui.i.keys.clone() {
            match key.as_str() {
                "Escape" if self.cur.is_some() => self.close_player(),
                "Escape" => {
                    self.flush();
                    self.send(json!({"cmd": "close", "refocus": true}));
                }
                "Space" | "KeyK" if self.cur.is_some() => self.player.toggle(),
                "ArrowLeft" if self.cur.is_some() => {
                    let p = self.player.pos();
                    self.player.seek(p - 5.0);
                }
                "ArrowRight" if self.cur.is_some() => {
                    let p = self.player.pos();
                    self.player.seek(p + 5.0);
                }
                "KeyM" if self.cur.is_some() => {
                    let m = !self.player.muted;
                    self.player.set_volume(self.player.vol, m);
                }
                _ => {}
            }
        }
        ui.end();
        // Fondu d'entrée / de sortie : tout le tampon (prémultiplié) est atténué d'un coup.
        let alpha = if self.anim_out.is_some() { k } else { (now.duration_since(self.anim_in).as_secs_f32() / 0.16).min(1.0) };
        if alpha < 1.0 {
            let a = (alpha * 256.0) as u32;
            for b in ui.c.pm.data_mut() {
                *b = ((*b as u32 * a) >> 8) as u8;
            }
            ui.s.animating = self.shown || k > 0.0;
        }
        if let Some(t) = self.flush_at {
            if now >= t { self.flush() } else { self.wake_in(t - now) }
        }
        if self.recording && self.shown {
            self.wake_in(Duration::from_millis(500));
        }
        if self.player.tick() {
            self.wake_in(Duration::from_millis(if self.player.playing { 100 } else { 60 }));
        }
        let _ = pretty;
    }
}
