//! Onglets « Vidéos », « Pilotes » et « Outils », et l'habillage du lecteur (commandes par-dessus l'image).
use crate::front::{Front, Tab, bytes, dur, led_col, thousands, when};
use crate::gfx::{F, R, hex, md, pal, r, scene, text_w};
use crate::widgets::{Btn, Note, Seg, Ui, id, idn, so};
use serde_json::json;

const RATES: [f32; 5] = [1.0, 1.5, 2.0, 0.25, 0.5];

fn dur_words(s: u32) -> String {
    if s < 60 {
        format!("{s} secondes")
    } else if s % 60 != 0 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{} minute{}", s / 60, if s > 60 { "s" } else { "" })
    }
}

impl Front {
    /// Cadre d'une boîte en ligne (.inline) : à dessiner avant son contenu, hauteur connue.
    fn inline_box(&self, ui: &mut Ui, q: R) {
        ui.c.rrect(q, 7.0, pal::BODY);
        ui.c.border(q, 7.0, 1.0, pal::LINE2);
    }

    /// Étiquette CLIP / REC d'une miniature.
    fn badge(&self, ui: &mut Ui, x: f32, y: f32, rec: bool) -> f32 {
        let t = if rec { "REC" } else { "CLIP" };
        let f = F::s(10.0, 600).ls(0.1);
        let w = 6.0 + 5.0 + 5.0 + text_w(t, f) + 5.0;
        ui.c.rrect(r(x, y, w, 17.0), 3.0, hex(0x080a08).a(0.82));
        ui.led(x + 8.5, y + 8.5, 5.0, if rec { pal::REC } else { pal::SIGNAL });
        ui.c.text(x + 16.0, y + 0.5, 17.0, t, f, pal::TEXT);
        w
    }

    /// Lecteur : image, puis commandes aux couleurs de l'app (barre de position, lecture, vitesse, son, plein écran).
    pub fn player_view(&mut self, ui: &mut Ui, q: R, rad: f32) {
        let now = ui.now;
        ui.c.rrect(q, rad, pal::BLACK);
        if rad > 0.0 {
            ui.c.outline(q, rad, 1.0, pal::LINE2);
        }
        self.player.set_size(q.w * ui.c.s, q.h * ui.c.s);
        let open = self.player.file.is_some();
        // En attendant la 1re image du clip : sa miniature (plus de cadre noir « Préparation… » qui
        // clignotait à chaque clic), puis fondu vers la vidéo.
        let fade = self.player.fade();
        let mut thumb = open && fade < 1.0 && self.player_thumb(ui, q);
        let has = open && self.player.draw(ui.c, q, fade);
        if open && !has {
            thumb = self.player_thumb(ui, q);
        }
        if (has || thumb) && rad > 0.0 {
            ui.c.corners(q, rad, pal::PANEL);
            ui.c.outline(q, rad, 1.0, pal::LINE2);
        }
        if !has && !thumb {
            let t = if open { "Préparation de la lecture…" } else if self.cur().is_some() { "" } else if self.vids.as_ref().is_some_and(|v| !v.is_empty()) { "Choisissez une vidéo à droite." } else { "" };
            ui.c.text_in(q, 0, t, F::s(12.5, 400), pal::TEXT3);
        }
        if !open {
            return;
        }
        let inside = q.has(ui.mouse());
        if inside && (ui.i.mouse != self.p_mouse || ui.i.pressed) {
            self.p_idle = now;
            self.p_mouse = ui.i.mouse;
        }
        let playing = self.player.playing;
        let idle = playing && ui.s.active == 0 && (!inside || now.duration_since(self.p_idle).as_secs_f32() > 2.2);
        let a = ui.anim(id("p-idle"), if idle { 0.0 } else { 1.0 }, 0.25);
        if playing && !idle {
            self.wake_in(std::time::Duration::from_millis(300));
        }
        let cq = |lo: f32, pct: f32, hi: f32| (q.w * pct / 100.0).clamp(lo, hi);
        let (pt, px, pb) = (cq(12.0, 5.0, 22.0), cq(4.0, 1.6, 8.0), cq(2.0, 1.0, 6.0));
        let (bh, bw, ic, fs) = (cq(24.0, 6.6, 32.0), cq(24.0, 7.0, 34.0), cq(14.0, 3.8, 18.0), cq(10.0, 2.5, 12.5));
        let pc = r(q.x, q.bottom() - (pt + 18.0 + bh + pb), q.w, pt + 18.0 + bh + pb);
        let (d, pos) = (self.player.dur.max(0.001), self.player.pos());
        let mut used = false;
        if a > 0.01 {
            let old = ui.c.set_clip(Some(q.inter(&ui.c.clip_rect())));
            ui.c.grad(pc, 0.0, true, &[(0.0, hex(0x080b08).a(0.0)), (0.55, hex(0x080b08).a(0.88 * a)), (1.0, hex(0x080b08).a(0.88 * a))]);
            // barre de position
            let sk = r(pc.x + px, pc.y + pt, pc.w - 2.0 * px, 18.0);
            let hot = ui.hot(sk) || ui.s.active == id("pSeek");
            if ui.hot(sk) {
                ui.s.hand = true;
                if ui.i.pressed {
                    ui.s.active = id("pSeek");
                }
            }
            let mut p = (pos / d) as f32;
            if ui.s.active == id("pSeek") {
                used = true;
                p = ((ui.mouse().0 - sk.x) / sk.w).clamp(0.0, 1.0);
                // Glissement : images fixes seules, la dernière position gagne ; la lecture reprend au lâcher.
                // Avant : un saut complet (vidéo + son relancés) à chaque image du geste.
                let t = p as f64 * d;
                if (t - pos).abs() > 0.03 || !self.p_scrub {
                    self.player.scrub(t);
                }
                self.p_scrub = true;
            } else if self.p_scrub {
                self.p_scrub = false;
                self.player.scrub_end();
            }
            let th = if hot { 5.0 } else { 3.0 };
            let tr = r(sk.x, sk.cy() - th / 2.0, sk.w, th);
            ui.c.rrect(tr, 2.0, pal::TEXT.a(0.14 * a));
            ui.c.rrect(r(tr.x, tr.y, tr.w * p, th), 2.0, pal::SIGNAL.a(a));
            if hot {
                ui.c.circle(tr.x + tr.w * p, sk.cy(), 9.0, pal::SIGNAL.a(0.25 * a));
                ui.c.circle(tr.x + tr.w * p, sk.cy(), 6.0, pal::SIGNAL.a(a));
            }
            // rangée de commandes
            let ry = sk.bottom();
            let pbtn = |ui: &mut Ui, wid: u64, x: f32, w: f32, icon: Option<&'static str>, label: &str| -> bool {
                let b = r(x, ry, w, bh);
                let hit = ui.click(wid, b);
                if ui.hot(b) {
                    ui.c.rrect(b, 5.0, pal::TEXT.a(0.09 * a));
                }
                match icon {
                    Some(n) => ui.c.icon(n, b.x + (b.w - ic) / 2.0, b.y + (b.h - ic) / 2.0, ic, pal::TEXT.a(a)),
                    None => ui.c.text_in(b, 0, label, F::m(fs, 500), pal::TEXT.a(a)),
                }
                hit
            };
            let mut x = pc.x + px;
            if pbtn(ui, id("pPlay"), x, bw, Some(if playing { "pause" } else { "play" }), "") {
                self.player.toggle();
                used = true;
            }
            x += bw + cq(2.0, 1.0, 6.0);
            let f = F::m(fs, 400);
            x += ui.c.text(x, ry, bh, &dur(pos), f.wt(500), pal::TEXT.a(a));
            ui.c.text(x, ry, bh, &format!(" / {}", dur(self.player.dur)), f, pal::TEXT3.a(a));
            let mut rx = pc.right() - px - bw;
            if pbtn(ui, id("pFull"), rx, bw, Some(if self.fullscreen { "unfull" } else { "full" }), "") {
                self.set_fullscreen(!self.fullscreen);
                used = true;
            }
            if q.w > 440.0 {
                let vw = cq(48.0, 14.0, 90.0);
                rx -= vw + 4.0;
                let vr = r(rx, ry + (bh - 24.0) / 2.0, vw, 24.0);
                if ui.hot(vr) {
                    ui.s.hand = true;
                    if ui.i.pressed {
                        ui.s.active = id("pVol");
                    }
                }
                let mut v = if self.player.muted { 0.0 } else { self.player.vol };
                if ui.s.active == id("pVol") {
                    used = true;
                    v = ((ui.mouse().0 - vr.x - 5.0) / (vr.w - 10.0)).clamp(0.0, 1.0);
                    self.player.set_volume(v, v == 0.0);
                    if ui.i.released {
                        let _ = std::fs::write(crate::config::dir().join("volume.txt"), v.to_string());
                    }
                }
                let tr = r(vr.x, vr.cy() - 1.5, vr.w, 3.0);
                ui.c.rrect(tr, 1.5, pal::TEXT.a(0.18 * a));
                ui.c.rrect(r(tr.x, tr.y, tr.w * v, 3.0), 1.5, pal::TEXT.a(a));
                ui.c.circle(vr.x + 5.0 + (vr.w - 10.0) * v, vr.cy(), 5.0, pal::TEXT.a(a));
            }
            rx -= bw + 2.0;
            let quiet = self.player.muted || self.player.vol == 0.0;
            if pbtn(ui, id("pMute"), rx, bw, Some(if quiet { "mute" } else { "spk" }), "") {
                let (v, m) = if quiet { (if self.player.vol == 0.0 { 0.5 } else { self.player.vol }, false) } else { (self.player.vol, true) };
                self.player.set_volume(v, m);
                used = true;
            }
            if q.w > 300.0 {
                rx -= bw + 2.0;
                let label = format!("{}×", self.player.rate.to_string().replace('.', ","));
                if pbtn(ui, id("pRate"), rx, bw, None, &label) {
                    let i = RATES.iter().position(|x| (*x - self.player.rate).abs() < 0.01).unwrap_or(0);
                    self.player.set_rate(RATES[(i + 1) % RATES.len()]);
                    used = true;
                }
            }
            // gros bouton de lecture, à l'arrêt
            if !playing {
                let sz = cq(40.0, 11.0, 60.0);
                let big = r(q.cx() - sz / 2.0, q.cy() - sz / 2.0, sz, sz);
                let k = ui.anim(id("pBig"), if ui.hot(big) { 1.06 } else { 1.0 }, 0.15);
                ui.c.circle(big.cx(), big.cy(), sz / 2.0 * k, pal::SIGNAL.a(a));
                let isz = sz * 0.45 * k;
                ui.c.icon("play", big.cx() - isz / 2.0 + sz * 0.03, big.cy() - isz / 2.0, isz, pal::SIGNAL_INK.a(a));
                if ui.click(id("pBigB"), big) {
                    self.player.toggle();
                    used = true;
                }
            }
            // Aperçu du point survolé sur la barre de temps (dessiné en dernier, par-dessus les commandes).
            if hot {
                let mx = ui.mouse().0.clamp(sk.x, sk.right());
                let t = ((mx - sk.x) / sk.w).clamp(0.0, 1.0) as f64 * d;
                if let Some(path) = self.player.file.clone() {
                    if self.preview.show(ui, &self.cfg, &path, self.player.dur, t, mx, sk, q) {
                        self.wake_in(std::time::Duration::from_millis(300));
                    }
                }
            }
            ui.c.restore_clip(old);
        }
        // Clic sur l'image : lecture / pause. Double-clic : plein écran.
        if !used && ui.hot(q) && !pc.has(ui.mouse()) {
            if ui.i.dbl {
                self.set_fullscreen(!self.fullscreen);
            } else if ui.click(id("pImg"), q) {
                self.player.toggle();
            }
        }
        // Raccourcis (hors champ de texte) : Espace/K lecture, ← → 5 s, , . image par image, F plein écran, M son.
        if ui.s.focus == 0 && self.hk_wait.is_none() && !ui.i.ctrl && !ui.i.alt {
            for k in ui.i.keys.clone() {
                match k.as_str() {
                    "Space" | "KeyK" => self.player.toggle(),
                    "ArrowLeft" => self.player.seek(pos - 5.0),
                    "ArrowRight" => self.player.seek(pos + 5.0),
                    "Comma" | "Period" => {
                        self.player.pause();
                        self.player.seek(pos + if k == "Comma" { -1.0 / 60.0 } else { 1.0 / 60.0 });
                    }
                    "KeyF" => self.set_fullscreen(!self.fullscreen),
                    "KeyM" => {
                        let m = !self.player.muted;
                        self.player.set_volume(self.player.vol, m);
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn tab_videos(&mut self, ui: &mut Ui, body: R) {
        // ---------- colonne de gauche : lecteur et actions ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-vid"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        let pq = r(x, y, w, (w * 9.0 / 16.0).round());
        self.player_view(ui, pq, 6.0);
        y = pq.bottom() + 14.0;
        if let Some((name, rec, secs, size, date, video, path)) = self.cur().map(|v| (v.name.clone(), v.rec, v.secs, v.bytes, v.date, v.video, v.path.clone())) {
            let sp = vec![crate::gfx::Span { t: name.clone(), f: F::s(15.0, 600).ls(-0.01), c: pal::TEXT }];
            y += ui.c.para(x, y, w, 19.5, &sp, true) + 10.0;
            let mut mx = x + self.badge(ui, x, y, rec) + 12.0;
            mx += ui.c.text(mx, y, 17.4, &dur(secs), F::m(12.0, 400), pal::TEXT2) + 12.0;
            mx += ui.c.text(mx, y, 17.4, &bytes(size), F::m(12.0, 400), pal::TEXT2) + 12.0;
            ui.c.text(mx, y, 17.4, &when(date), F::s(12.0, 400), pal::TEXT2);
            y += 17.4 + 10.0;
            let open_folder = |me: &mut Front| me.send(json!({"cmd": "video_action", "action": "open_folder", "path": path}));
            if !video {
                let bw = Ui::btn_w("Ouvrir le dossier", true, Btn::N);
                let (nh, (ax, ay)) = ui.note(x, y, w, Note::Err, "warn", "**Lecture impossible ici.** Ce fichier ne contient aucune image (clip AV1 enregistré avant la version 1.3.0, irrécupérable). Vous pouvez le supprimer.", 39.0);
                if ui.btn(id("vErrOpen"), r(ax, ay + 7.0, bw, 32.0), "Ouvrir le dossier", Some("folder"), Btn::N) {
                    open_folder(self);
                }
                y += nh + 10.0;
            }
            if let Some(mut text) = self.ren.take() {
                // Renommer : champ + boutons
                let q = r(x, y, w, 12.0 + 16.0 + 10.0 + 34.0 + 10.0 + 32.0 + 12.0);
                self.inline_box(ui, q);
                ui.sc(q.x + 12.0, q.y + 12.0, 16.0, "Nouveau nom", pal::TEXT3, 500);
                let enter = ui.text_field(id("renIn"), r(q.x + 12.0, q.y + 38.0, q.w - 24.0, 34.0), &mut text);
                let b1 = Ui::btn_w("Renommer", false, Btn::P);
                let ok = ui.btn(id("renOk"), r(q.x + 12.0, q.y + 82.0, b1, 32.0), "Renommer", None, Btn::P) || enter;
                let no = ui.btn(id("renNo"), r(q.x + 20.0 + b1, q.y + 82.0, Ui::btn_w("Annuler", false, Btn::G), 32.0), "Annuler", None, Btn::G) || ui.i.key("Escape");
                if ok && !text.trim().is_empty() {
                    self.send(json!({"cmd": "video_action", "action": "rename", "path": path, "name": text.trim()}));
                    ui.s.focus = 0;
                } else if no {
                    ui.s.focus = 0;
                } else {
                    self.ren = Some(text);
                }
                y = q.bottom();
            } else if self.del_box {
                let perm = self.cfg.delete_permanent;
                let (qt, ht, bt) = if perm {
                    ("Supprimer définitivement cette vidéo ?", "Elle ne passera pas par la Corbeille : impossible de la récupérer.", "Supprimer")
                } else {
                    ("Mettre cette vidéo à la corbeille ?", "Vous pourrez la récupérer depuis la corbeille de Windows.", "Mettre à la corbeille")
                };
                let hsp = md(ht, F::s(12.0, 400), pal::TEXT3, 500, pal::TEXT2);
                let hh = ui.c.para(0.0, 0.0, w - 24.0, 17.4, &hsp, false);
                let q = r(x, y, w, 12.0 + 19.0 + hh + 10.0 + 18.0 + 12.0 + 32.0 + 12.0);
                self.inline_box(ui, q);
                ui.c.text(q.x + 12.0, q.y + 12.0, 19.0, qt, F::s(13.0, 600), pal::TEXT);
                ui.c.para(q.x + 12.0, q.y + 31.0, w - 24.0, 17.4, &hsp, true);
                let cy = q.y + 41.0 + hh;
                if ui.check(id("delAsk"), q.x + 12.0, cy + 2.0, 14.0, self.del_ask, false) {
                    self.del_ask = !self.del_ask;
                }
                ui.c.text(q.x + 34.0, cy, 18.0, "Ne plus me demander", F::s(12.5, 400), pal::TEXT2);
                let by = cy + 30.0;
                let b1 = Ui::btn_w(bt, true, Btn::D);
                if ui.btn(id("delYes"), r(q.x + 12.0, by, b1, 32.0), bt, Some("trash"), Btn::D) {
                    if self.del_ask {
                        self.set("delete_confirm", json!(false));
                    }
                    self.do_delete();
                }
                if ui.btn(id("delNo"), r(q.x + 20.0 + b1, by, Ui::btn_w("Annuler", false, Btn::G), 32.0), "Annuler", None, Btn::G) {
                    self.del_box = false;
                }
                y = q.bottom();
            } else {
                let mut bx = x;
                let mut by = y;
                let place = |bx: &mut f32, by: &mut f32, bw: f32| -> R {
                    if *bx + bw > x + w + 0.5 {
                        (*bx, *by) = (x, *by + 40.0);
                    }
                    let q = r(*bx, *by, bw, 32.0);
                    *bx += bw + 8.0;
                    q
                };
                let q1 = place(&mut bx, &mut by, Ui::btn_w("Renommer", true, Btn::N));
                if ui.btn(id("vRen"), q1, "Renommer", Some("pen"), Btn::N) {
                    self.ren = Some(name.rsplit_once('.').map_or(name.clone(), |(a, _)| a.to_string()));
                    ui.s.focus = id("renIn");
                }
                let q2 = place(&mut bx, &mut by, Ui::btn_w("Ouvrir le dossier", true, Btn::N));
                if ui.btn(id("vOpen"), q2, "Ouvrir le dossier", Some("folder"), Btn::N) {
                    open_folder(self);
                }
                let q3 = place(&mut bx, &mut by, Ui::btn_w("Supprimer", true, Btn::G));
                if ui.btn(id("vDel"), q3, "Supprimer", Some("trash"), Btn::G) {
                    self.ask_delete();
                }
                y = by + 32.0;
            }
        }
        ui.scroll_end(id("mon-vid"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite : grille ----------
        let list = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let count = match &self.vids {
            None => "Chargement…".to_string(),
            Some(l) if l.is_empty() => "Vidéos".to_string(),
            Some(l) => format!("{} vidéo{} · {}", l.len(), if l.len() > 1 { "s" } else { "" }, bytes(l.iter().map(|v| v.bytes).sum())),
        };
        ui.sc(list.x + 22.0, list.y + 14.0, 32.0, &count, pal::TEXT3, 500);
        let bw = Ui::btn_w("Actualiser", true, Btn::G);
        if ui.btn(id("vRefresh"), r(list.right() - 22.0 - bw, list.y + 14.0, bw, 32.0), "Actualiser", Some("refresh"), Btn::G) {
            self.send(json!({"cmd": "videos"}));
        }
        ui.c.rect(r(list.x, list.y + 58.0, list.w, 1.0), pal::LINE);
        let view = r(list.x, list.y + 59.0, list.w, list.h - 59.0);
        if self.vids.as_ref().is_some_and(|l| l.is_empty()) {
            let (ex, mut ey) = (view.x + 32.0, view.y + 40.0);
            ui.c.text(ex, ey, 24.0, "Aucun clip pour l'instant", F::s(17.0, 600).ls(-0.015), pal::TEXT);
            ey += 34.0;
            let t = format!(
                "En jeu, appuyez sur **{}** pour garder les dernières {}, ou sur **{}** pour lancer un REC. Les vidéos apparaîtront ici.",
                self.hk[0].clone().unwrap_or_default(),
                dur_words(self.cfg.buffer_secs),
                self.hk[1].clone().unwrap_or_default()
            );
            let sp = md(&t, F::s(13.0, 400), pal::TEXT2, 600, pal::TEXT);
            ey += ui.c.para(ex, ey, (view.w - 64.0).min(376.0), 18.9, &sp, true) + 10.0;
            if ui.btn(id("vEmpty"), r(ex, ey, Ui::btn_w("Voir le dossier des clips", true, Btn::N), 32.0), "Voir le dossier des clips", Some("folder"), Btn::N) {
                let p = self.cfg.out_dir.clone();
                self.send(json!({"cmd": "video_action", "action": "open_folder", "path": p}));
            }
            return;
        }
        let clip = ui.scroll_begin(id("vgrid"), view);
        let (gx, gw) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let cols = (((gw + 14.0) / 174.0).floor() as usize).max(1);
        let cw = (gw - 14.0 * (cols as f32 - 1.0)) / cols as f32;
        let th = (cw * 9.0 / 16.0).round();
        let ch = th + 6.0 + 18.0 + 6.0 + 17.0;
        let n = self.vids.as_ref().map_or(0, |l| l.len());
        let vis = ui.c.clip_rect();
        let (mut pick, mut ctx) = (None, None);
        for i in 0..n {
            let q = r(gx + (cw + 14.0) * (i % cols) as f32, view.y + 16.0 + (ch + 14.0) * (i / cols) as f32, cw, ch);
            if q.bottom() < vis.y || q.y > vis.bottom() {
                continue; // hors de la vue : ni dessin ni miniature décodée
            }
            let (stem, name, rec, secs, date, size, sel) = {
                let v = &self.vids.as_ref().unwrap()[i];
                (v.stem.clone(), v.name.clone(), v.rec, v.secs, v.date, v.bytes, self.v_sel.as_ref() == Some(&v.path))
            };
            let t = r(q.x, q.y, cw, th);
            let hot = ui.hot(q);
            ui.c.rrect(t, 5.0, hex(0x0c1210));
            let (pw, ph) = ((t.w * ui.c.s).round() as u32, (t.h * ui.c.s).round() as u32);
            let drawn = match self.thumb(&stem, pw, ph) {
                Some(img) => {
                    ui.c.blit(img.data(), img.width(), img.height(), t.x, t.y);
                    ui.c.corners(t, 5.0, pal::BODY);
                    true
                }
                None => false,
            };
            if !drawn {
                scene(ui.c, t, 5.0);
            }
            if sel {
                ui.c.outline(t, 5.0, 2.0, pal::SIGNAL);
            } else {
                ui.c.outline(t, 5.0, 1.0, if hot { pal::TEXT3 } else { pal::LINE2 });
            }
            self.badge(ui, t.x + 6.0, t.y + 6.0, rec);
            let d = dur(secs);
            let dw = text_w(&d, F::m(11.0, 400)) + 10.0;
            ui.c.rrect(r(t.right() - 6.0 - dw, t.bottom() - 6.0 - 20.0, dw, 20.0), 3.0, hex(0x080a08).a(0.82));
            ui.c.text(t.right() - 1.0 - dw, t.bottom() - 26.0, 20.0, &d, F::m(11.0, 400), pal::TEXT);
            ui.c.text_cut(q.x, t.bottom() + 6.0, 18.0, cw, &name, F::s(12.5, 500), pal::TEXT);
            ui.c.text_cut(q.x, t.bottom() + 30.0, 17.0, cw, &format!("{} · {}", when(date), bytes(size)), F::m(11.5, 400), pal::TEXT3);
            if ui.click(idn("vc", i), q) {
                pick = Some(i);
            }
            if hot && ui.i.rpressed {
                ctx = Some(i);
            }
        }
        let rows = n.div_ceil(cols) as f32;
        ui.scroll_end(id("vgrid"), view, 16.0 + rows * (ch + 14.0) + 10.0, clip);
        if let Some(i) = pick {
            let p = self.vids.as_ref().unwrap()[i].path.clone();
            if self.v_sel.as_ref() != Some(&p) {
                self.pick(Some(p));
            }
        }
        if let Some(i) = ctx {
            self.ctx = Some((self.vids.as_ref().unwrap()[i].path.clone(), ui.i.mouse));
        }
    }

    pub fn tab_drivers(&mut self, ui: &mut Ui, body: R) {
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        let branch = if self.cfg.driver_branch == "studio" { "Studio" } else { "Game Ready" };
        if let Some((name, driver)) = self.gpu.clone() {
            let sp = vec![crate::gfx::Span { t: name, f: F::s(17.0, 600).ls(-0.015), c: pal::TEXT }];
            let nh = ui.c.para(0.0, 0.0, w - 28.0, 22.1, &sp, false);
            let ph = 4.0 + 16.0 + nh + 3.0 + 17.4 + 12.0 + 1.0 + 3.0 * 40.0 + 4.0;
            let q = r(x, y, w, ph);
            ui.c.rrect(q, 7.0, pal::BODY);
            ui.c.border(q, 7.0, 1.0, pal::LINE2);
            ui.c.circle(q.x + 10.0, q.y + 12.0, 3.0, hex(0x2f382c));
            ui.c.circle(q.right() - 10.0, q.y + 12.0, 3.0, hex(0x2f382c));
            let (ix, iw) = (x + 14.0, w - 28.0);
            ui.c.para(ix, y + 20.0, iw, 22.1, &sp, true);
            ui.c.text(ix, y + 23.0 + nh, 17.4, "Pilote lu dans Windows", F::s(12.0, 400), pal::TEXT3);
            let mut ry = y + 20.0 + nh + 3.0 + 17.4 + 12.0;
            ui.c.rect(r(ix, ry, iw, 1.0), pal::LINE);
            ry += 1.0;
            let scr = format!("{}×{}", self.native.0, self.native.1);
            for (i, (l, v, mono)) in [("Pilote installé", driver.as_str(), true), ("Branche", branch, false), ("Écran capturé", scr.as_str(), true)].iter().enumerate() {
                ui.c.text(ix, ry, 40.0, &l.to_uppercase(), F::s(11.0, 500).ls(0.11), pal::TEXT3);
                let f = if *mono { F::m(13.0, 500) } else { F::s(13.0, 500) };
                ui.c.text(ix + iw - text_w(v, f), ry, 40.0, v, f, pal::TEXT);
                ry += 40.0;
                if i < 2 {
                    ui.c.rect(r(ix, ry - 1.0, iw, 1.0), pal::LINE);
                }
            }
            y += ph + 14.0;
        }
        ui.hint(x, y, w, "Téléchargement uniquement depuis `*.nvidia.com`, taille vérifiée avant l'installation. Windows demande l'autorisation pour installer.");

        // ---------- colonne de droite ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("drv"), view);
        let (x, w) = (view.x + 26.0, view.w - 52.0 - 10.0);
        let mut y = view.y + 24.0;
        if self.gpu.is_none() {
            ui.c.text(x, y + 8.0, 24.0, "Aucune carte NVIDIA détectée", F::s(17.0, 600).ls(-0.015), pal::TEXT);
            let sp = md("Cet onglet met à jour les pilotes NVIDIA uniquement. Pour une carte AMD ou Intel, utilisez AMD Adrenalin ou l'assistant Intel. Si vous avez une carte NVIDIA, vérifiez que son pilote est installé : `nvidia-smi` doit répondre.", F::s(13.0, 400), pal::TEXT2, 600, pal::TEXT);
            ui.c.para(x, y + 42.0, w.min(440.0), 18.9, &sp, true);
            ui.scroll_end(id("drv"), view, 200.0, clip);
            return;
        }
        let d = &self.drv;
        let (state, led) = match (d.phase.as_str(), d.status.as_str()) {
            ("done", _) => (format!("Pilote {} installé", d.latest), "ok"),
            (_, "ok") => ("À jour".to_string(), "ok"),
            (_, "update") => (format!("Mise à jour disponible ({})", d.latest), "warn"),
            (_, "error") => ("Vérification impossible".to_string(), "rec"),
            _ => ("Vérification auprès de NVIDIA…".to_string(), ""),
        };
        ui.led(x + 3.5, y + 13.0, 7.0, led_col(led));
        ui.c.text(x + 17.0, y, 26.0, &state, F::s(18.0, 600).ls(-0.015), pal::TEXT);
        y += 26.0 + 20.0;
        let update = d.status == "update" && d.phase != "done";
        if update {
            let (old, new) = (d.installed.clone(), d.latest.clone());
            let f = F::m(30.0, 500).ls(-0.02);
            ui.sc(x, y, 16.0, "Installé", pal::TEXT3, 500);
            ui.c.text(x, y + 20.0, 30.0, &old, f, pal::TEXT3);
            let ax = x + text_w(&old, f).max(60.0) + 18.0;
            ui.c.icon("arrow", ax, y + 21.0, 28.0, pal::SIGNAL);
            ui.sc(ax + 46.0, y, 16.0, "Disponible", pal::TEXT3, 500);
            ui.c.text(ax + 46.0, y + 20.0, 30.0, &new, f, pal::TEXT);
            y += 50.0 + 20.0;
        }
        ui.c.text(x, y, 38.0, "Branche", F::s(13.0, 400), pal::TEXT2);
        let bx = x + text_w("Branche", F::s(13.0, 400)) + 14.0;
        let (hit, _) = ui.seg(id("branch"), bx, y, &[so("Game Ready"), so("Studio")], Some(if self.cfg.driver_branch == "studio" { 1 } else { 0 }), Seg::D);
        if let Some(i) = hit {
            self.cfg.driver_branch = if i == 1 { "studio".into() } else { "game_ready".into() };
            self.check_driver();
        }
        y += 38.0 + 20.0;
        y += ui.hint(x, y, w, if self.cfg.driver_branch == "studio" { "Studio : versions validées plus longtemps, pour les logiciels de création." } else { "Game Ready : optimisé pour les jeux sortis récemment." }) + 20.0;
        if self.drv.status == "error" {
            // « Vérifiez la connexion » seulement pour une erreur réseau, pas pour un code de l'installateur.
            let msg = if self.drv.error.is_empty() {
                "NVIDIA n'a pas répondu. Vérifiez la connexion Internet.".to_string()
            } else if self.drv.error.contains("Réseau") || self.drv.error.contains("Téléchargement") {
                format!("{} Vérifiez la connexion Internet.", self.drv.error)
            } else {
                self.drv.error.clone()
            };
            let (nh, (ax, ay)) = ui.note(x, y, w, Note::Err, "warn", &msg, 39.0);
            if ui.btn(id("dRetry"), r(ax, ay + 7.0, Ui::btn_w("Réessayer", true, Btn::N), 32.0), "Réessayer", Some("refresh"), Btn::N) {
                self.check_driver();
            }
            y += nh + 20.0;
        }
        if self.drv.status == "update" || !self.drv.phase.is_empty() {
            let (ix, iw) = (x + 16.0, w - 32.0);
            let y0 = y;
            let mut dy = y + 16.0;
            let phase = self.drv.phase.clone();
            if phase == "done" {
                dy += ui.note(ix, dy, iw, Note::Info, "check", "**Pilote installé.** Redémarrez Windows quand vous voulez pour terminer.", 0.0).0;
            } else if !phase.is_empty() {
                let ph = match phase.as_str() {
                    "download" => 0,
                    "verify" => 1,
                    _ => 2,
                };
                let ratio = if ph > 0 { 1.0 } else if self.drv.total > 0 { (self.drv.done as f32 / self.drv.total as f32).min(1.0) } else { 0.0 };
                let k = ui.anim(id("meter"), ratio, 0.3);
                let m = r(ix, dy, iw, 12.0);
                ui.c.outline(m, 2.0, 1.0, pal::LINE2);
                ui.c.rrect(m, 2.0, pal::WELL);
                ui.c.rect(r(m.x, m.y, m.w * k, 12.0), pal::SIGNAL);
                for i in 1..20 {
                    ui.c.rect(r(m.x + m.w * i as f32 / 20.0 - 1.0, m.y, 1.0, 12.0), pal::WELL.a(0.75));
                }
                dy += 12.0 + 10.0;
                let pct = match ph {
                    0 => format!("{} %", (ratio * 100.0).round()),
                    1 => "Vérification…".into(),
                    _ => "Installation…".into(),
                };
                ui.c.text(ix, dy, 17.4, &pct, F::m(12.0, 400), pal::TEXT2);
                if self.drv.total > 0 {
                    let mo = format!("{} / {}", bytes(self.drv.done), bytes(self.drv.total));
                    ui.c.text(ix + iw - text_w(&mo, F::m(12.0, 400)), dy, 17.4, &mo, F::m(12.0, 400), pal::TEXT2);
                }
                dy += 17.4 + 10.0;
                for (j, s) in ["Téléchargement depuis nvidia.com", "Vérification de la taille", "Installation (l'écran peut clignoter)"].iter().enumerate() {
                    let (lc, tc) = if j < ph { (pal::OK, pal::TEXT2) } else if j == ph { (pal::SIGNAL, pal::TEXT) } else { (pal::LINE2, pal::TEXT3) };
                    ui.led(ix + 3.5, dy + 9.5, 7.0, lc);
                    ui.c.text(ix + 17.0, dy, 19.0, s, F::s(13.0, 400), tc);
                    dy += 19.0 + 8.0;
                }
                dy -= 8.0;
            } else if self.drv.confirm {
                let q0 = dy;
                let mut cy = dy + 12.0;
                let lims: [(&'static str, &str); 3] = [
                    ("screen", "L'écran va **clignoter ou devenir noir** quelques secondes pendant l'installation."),
                    ("warn", "Fermez vos jeux. Un **redémarrage** sera conseillé à la fin."),
                    ("lock", "Windows va demander l'autorisation (UAC)."),
                ];
                // Hauteur mesurée d'abord (le cadre se dessine avant son contenu).
                let lh: f32 = lims.iter().map(|(_, t)| { let sp = md(t, F::s(12.5, 400), pal::TEXT2, 500, pal::TEXT); ui.c.para(0.0, 0.0, iw - 48.0, 18.1, &sp, false) + 8.0 }).sum();
                self.inline_box(ui, r(ix, q0, iw, 12.0 + 19.0 + 10.0 + lh + 2.0 + 32.0 + 12.0));
                let t = format!("Installer le pilote {} ?", self.drv.latest);
                ui.c.text(ix + 12.0, cy, 19.0, &t, F::s(13.0, 600), pal::TEXT);
                cy += 29.0;
                for (icon, t) in lims {
                    cy += self.lim(ui, ix + 12.0, cy, iw - 24.0, icon, t) + 8.0;
                }
                cy += 2.0;
                let b1 = Ui::btn_w("Installer maintenant", false, Btn::P);
                if ui.btn(id("dlYes"), r(ix + 12.0, cy, b1, 32.0), "Installer maintenant", None, Btn::P) {
                    self.drv.confirm = false;
                    self.drv.phase = "download".into();
                    (self.drv.done, self.drv.total) = (0, 0);
                    self.send(json!({"cmd": "driver_install"}));
                }
                if ui.btn(id("dlNo"), r(ix + 20.0 + b1, cy, Ui::btn_w("Annuler", false, Btn::G), 32.0), "Annuler", None, Btn::G) {
                    self.drv.confirm = false;
                }
                dy = cy + 32.0 + 12.0;
            } else {
                if ui.btn(id("dlBtn"), r(ix, dy, Ui::btn_w("Télécharger et installer", true, Btn::P), 32.0), "Télécharger et installer", Some("dl"), Btn::P) {
                    self.drv.confirm = true;
                }
                dy += 32.0;
            }
            dy += 16.0;
            ui.c.border(r(x, y0, w, dy - y0), 8.0, 1.0, pal::LINE);
            y = dy;
        }
        ui.scroll_end(id("drv"), view, y + 24.0 - view.y, clip);
    }

    pub fn tab_tools(&mut self, ui: &mut Ui, body: R) {
        // ---------- colonne de gauche : mémoire ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-tools"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        ui.sc(x, y, 16.0, "Nettoyer la RAM", pal::TEXT2, 500);
        y += 30.0;
        y += ui.hint(x, y, w, "Vide la mémoire des programmes et la liste « standby » de Windows, comme « Clean memory » de QuickCPU. Aucun programme n'est ouvert, donc aucun contact avec le jeu.") + 14.0;
        if let Some((before, after, total)) = self.tools.ram {
            let cw = (w - 10.0) / 2.0;
            for (i, (l, v)) in [("Libre avant", before), ("Libre après", after)].iter().enumerate() {
                let q = r(x + (cw + 10.0) * i as f32, y, cw, 68.0);
                ui.c.rrect(q, 6.0, pal::BODY);
                ui.c.border(q, 6.0, 1.0, pal::LINE);
                ui.sc(q.x + 12.0, q.y + 10.0, 16.0, l, pal::TEXT3, 500);
                ui.c.text(q.x + 12.0, q.y + 30.0, 29.0, &bytes(*v), F::m(20.0, 500), pal::TEXT);
            }
            y += 78.0;
            let g = after as i64 - before as i64;
            let t = format!("{}{}", if g > 0 { format!("+{} de mémoire libre", bytes(g as u64)) } else { "Pas de mémoire en plus à libérer".into() }, if total > 0 { format!(" · total {}", bytes(total)) } else { String::new() });
            ui.c.text(x, y, 19.0, &t, F::s(13.0, 400), pal::TEXT2);
            y += 19.0 + 14.0;
        }
        let bw = Ui::btn_w("Nettoyer la RAM", false, Btn::P);
        if ui.btn(id("ramBtn"), r(x, y, bw, 32.0), "Nettoyer la RAM", None, Btn::P.off(self.tools.ram_wait)) {
            self.tools.ram_wait = true;
            self.send(json!({"cmd": "ram_clean"}));
        }
        if self.tools.ram_wait {
            ui.spin(x + bw + 15.0, y + 16.0);
            ui.c.text(x + bw + 28.0, y, 32.0, "En attente de Windows…", F::s(12.0, 400), pal::TEXT3);
        }
        y += 32.0 + 14.0;
        y += ui.hint(x, y, w, "Windows demande l'autorisation administrateur à chaque clic. Les applications rechargent ensuite leurs pages : petit ralentissement possible. Utile surtout quand la mémoire standby gonfle ; jamais automatique en pleine partie.");
        ui.scroll_end(id("mon-tools"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite : fichiers temporaires ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("set-tools"), view);
        let (x, w) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let (open, mut cy) = self.grp(ui, "tools:clean", "Fichiers temporaires et caches", x, view.y + 4.0, w);
        if open {
            if self.tools.items.is_none() {
                cy += ui.hint(x, cy, w, "L'analyse mesure chaque catégorie sans rien supprimer. Vous choisissez ensuite quoi effacer. Les fichiers en cours d'utilisation sont ignorés.") + 10.0;
                let bw = Ui::btn_w("Analyser", false, Btn::P);
                if ui.btn(id("clScan"), r(x, cy, bw, 32.0), "Analyser", None, Btn::P.off(self.tools.scanning)) {
                    self.tools.scanning = true;
                    self.send(json!({"cmd": "clean_scan"}));
                }
                if self.tools.scanning {
                    ui.spin(x + bw + 15.0, cy + 16.0);
                    ui.c.text(x + bw + 28.0, cy, 32.0, "Analyse…", F::s(12.0, 400), pal::TEXT3);
                }
                cy += 32.0;
            } else {
                ui.c.rect(r(x, cy, w, 1.0), pal::LINE);
                cy += 1.0;
                let n = self.tools.items.as_ref().map_or(0, |l| l.len());
                let mut open_id = None;
                for i in 0..n {
                    let (name, size, files, admin, warn, on, cid) = {
                        let it = &self.tools.items.as_ref().unwrap()[i];
                        (it.name.clone(), it.bytes, it.files, it.admin, it.warn.clone(), it.on, it.id.clone())
                    };
                    let wsp = vec![crate::gfx::Span { t: warn.clone(), f: F::s(12.0, 400), c: pal::WARN }];
                    let wh = if warn.is_empty() { 0.0 } else { ui.c.para(0.0, 0.0, w - 28.0 - 21.0, 17.4, &wsp, false) + 2.0 };
                    let rh = 10.0 + 19.0 + 2.0 + 24.0 + wh + 10.0;
                    let row = r(x, cy, w, rh);
                    let bwo = Ui::btn_w("Ouvrir", true, Btn::G.sm());
                    let ob = r(x + w - bwo, cy + 31.0, bwo, 24.0);
                    if ui.btn(idn("clOpen", i), ob, "Ouvrir", Some("folder"), Btn::G.sm()) {
                        open_id = Some(cid);
                    } else if size > 0 && !ob.has(ui.mouse()) && ui.click(idn("clRow", i), row) {
                        self.tools.items.as_mut().unwrap()[i].on = !on;
                    }
                    ui.check(idn("clChk", i), x, cy + 20.0, 16.0, on, size == 0);
                    let nx = x + 28.0 + ui.c.text(x + 28.0, cy + 10.0, 19.0, &name, F::s(13.0, 500), pal::TEXT);
                    if admin {
                        let f = F::s(10.0, 600).ls(0.08);
                        let tq = r(nx + 8.0, cy + 12.0, text_w("ADMIN", f) + 10.0, 15.0);
                        ui.c.border(tq, 3.0, 1.0, pal::LINE2);
                        ui.c.text(tq.x + 5.0, tq.y + 0.5, 15.0, "ADMIN", f, pal::TEXT2);
                    }
                    let sz = bytes(size);
                    ui.c.text(x + w - text_w(&sz, F::m(13.0, 400)), cy + 10.0, 19.0, &sz, F::m(13.0, 400), pal::TEXT);
                    ui.c.text(x + 28.0, cy + 31.0, 24.0, &format!("{} fichiers", thousands(files)), F::m(12.0, 400), pal::TEXT3);
                    if !warn.is_empty() {
                        ui.c.icon("warn", x + 28.0, cy + 59.0, 14.0, pal::WARN);
                        ui.c.para(x + 49.0, cy + 57.0, w - 49.0, 17.4, &wsp, true);
                    }
                    cy += rh;
                    ui.c.rect(r(x, cy - 1.0, w, 1.0), pal::LINE);
                }
                if let Some(cid) = open_id {
                    self.send(json!({"cmd": "clean_open", "id": cid}));
                }
                let items = self.tools.items.as_ref().unwrap();
                let total: u64 = items.iter().filter(|i| i.on).map(|i| i.bytes).sum();
                let ids: Vec<String> = items.iter().filter(|i| i.on).map(|i| i.id.clone()).collect();
                let adm = items.iter().any(|i| i.on && i.admin);
                cy += 14.0;
                let lx = x + ui.sc(x, cy, 32.0, "Sélection", pal::TEXT3, 500) + 6.0;
                ui.c.text(lx, cy, 32.0, &bytes(total), F::m(20.0, 500), pal::TEXT);
                let gl = if self.tools.running { "Suppression…" } else { "Supprimer la sélection" };
                let b2 = Ui::btn_w(gl, false, Btn::P);
                if ui.btn(id("clGo"), r(x + w - b2, cy, b2, 32.0), gl, None, Btn::P.off(ids.is_empty() || self.tools.running)) {
                    self.tools.confirm = true;
                }
                let b1 = Ui::btn_w("Réanalyser", true, Btn::G);
                if ui.btn(id("clRescan"), r(x + w - b2 - 8.0 - b1, cy, b1, 32.0), "Réanalyser", Some("refresh"), Btn::G.off(self.tools.scanning)) {
                    self.tools.scanning = true;
                    self.tools.done = None;
                    self.send(json!({"cmd": "clean_scan"}));
                }
                cy += 32.0;
                if self.tools.confirm {
                    cy += 12.0;
                    let note = format!("{}Les fichiers supprimés ne passent pas par la corbeille. Les fichiers en cours d'utilisation sont ignorés.", if adm { "Windows demandera l'autorisation administrateur. " } else { "" });
                    let sp = md(&note, F::s(12.0, 400), pal::TEXT3, 500, pal::TEXT2);
                    let nh = ui.c.para(0.0, 0.0, w - 24.0, 17.4, &sp, false);
                    let q = r(x, cy, w, 12.0 + 19.0 + nh + 10.0 + 32.0 + 12.0);
                    self.inline_box(ui, q);
                    ui.c.text(q.x + 12.0, q.y + 12.0, 19.0, &format!("Supprimer {} ?", bytes(total)), F::s(13.0, 600), pal::TEXT);
                    ui.c.para(q.x + 12.0, q.y + 31.0, w - 24.0, 17.4, &sp, true);
                    let by = q.y + 41.0 + nh;
                    let b1 = Ui::btn_w("Supprimer", true, Btn::D);
                    if ui.btn(id("clYes"), r(q.x + 12.0, by, b1, 32.0), "Supprimer", Some("trash"), Btn::D) {
                        self.tools.confirm = false;
                        self.tools.running = true;
                        self.send(json!({"cmd": "clean_run", "ids": ids}));
                    }
                    if ui.btn(id("clNo"), r(q.x + 20.0 + b1, by, Ui::btn_w("Annuler", false, Btn::G), 32.0), "Annuler", None, Btn::G) {
                        self.tools.confirm = false;
                    }
                    cy = q.bottom();
                }
                if let Some(t) = self.tools.done.clone() {
                    cy += 12.0;
                    cy += ui.note(x, cy, w, Note::Info, "check", &t, 0.0).0;
                }
            }
            cy += 16.0;
            cy += ui.note(x, cy, w, Note::Info, "info", "**Le dossier Prefetch n'est pas dans la liste, exprès.** Windows s'en sert pour lancer vos applications plus vite. Le vider les ralentit : c'est un mythe des vidéos d'optimisation.", 0.0).0;
        }
        let y = self.grp_end(ui, open, x, cy, w, true);
        ui.scroll_end(id("set-tools"), view, y + 28.0 - view.y, clip);
        let _ = Tab::Tools;
    }
}
