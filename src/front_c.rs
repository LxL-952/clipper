//! Onglets « Statistiques » (overlay en jeu) et « Avancé » (démarrage, administrateur, puce d'encodage,
//! mode étiré, limites connues), et la carte « Clipping / Capture du bureau » de l'onglet Clipper.
//! Mêmes réglages et mêmes textes qu'avant, regroupés : rien des statistiques ne reste dans les autres onglets.
use crate::front::Front;
use crate::front_a::{ITEMS, ov_sample, vendor};
use crate::gfx::{F, R, hex, pal, r, scene, text_w};
use crate::widgets::{Btn, Note, Ui, id, idn, para_h};
use serde_json::json;

/// Colonne des libellés à gauche des contrôles (même valeur que l'onglet Clipper).
const LAB: f32 = 132.0;

impl Front {
    /// Titre de section fixe (non repliable) : 16 px, graisse 600, filet au-dessus sauf pour la première.
    fn sect(&mut self, ui: &mut Ui, title: &str, x: f32, y: f32, w: f32, first: bool) -> f32 {
        let mut y = y;
        if !first {
            ui.c.rect(r(x, y, w, 1.0), pal::LINE);
            y += 1.0;
        }
        ui.c.text(x, y + 18.0, 26.0, title, F::s(16.0, 600).ls(-0.01), pal::TEXT);
        y + 18.0 + 26.0 + 12.0
    }

    /// Rangée de pastilles à cocher qui passe à la ligne dans `fw`. Renvoie (clé cliquée, bas de la rangée).
    fn chip_rows(&mut self, ui: &mut Ui, key: &str, fx: f32, y: f32, fw: f32, items: &[(String, bool, bool)]) -> (Option<usize>, f32) {
        let (mut px, mut py) = (fx, y);
        let mut hit = None;
        for (i, (l, on, off)) in items.iter().enumerate() {
            let cw = (37.0 + text_w(l, F::s(12.5, 400)) + 2.0).ceil();
            if px + cw > fx + fw + 0.5 && px > fx {
                (px, py) = (fx, py + 38.0);
            }
            let (clicked, w) = ui.chip(idn(key, i), px, py, l, *on, *off);
            if clicked {
                hit = Some(i);
            }
            px += w + 6.0;
        }
        (hit, py + 32.0)
    }

    /// Aperçu de l'overlay dans le paysage `m` : position, taille, opacité, mesures et courbes choisies.
    fn ov_preview(&mut self, ui: &mut Ui, m: R) {
        if self.cfg.overlay_mode == "hidden" {
            ui.c.text_in(r(m.x, m.y + 8.0, m.w, 20.0), 0, "Statistiques masquées", F::s(12.5, 500), pal::TEXT2);
            return;
        }
        let fs = match self.cfg.overlay_size.as_str() {
            "s" => 9.0,
            "l" => 11.5,
            _ => 10.0,
        };
        let f = F::m(fs, 400);
        let rows: Vec<_> = self.cfg.overlay_items.iter().filter_map(|k| ov_sample(k).map(|s| (k.clone(), s))).collect();
        if rows.is_empty() {
            return;
        }
        let graph = |k: &str| self.cfg.overlay_graphs.iter().any(|g| g == k);
        let gw = if rows.iter().any(|(k, _)| graph(k)) { fs * 4.0 } else { 0.0 };
        let lh = fs * 1.35;
        let bw = rows.iter().map(|(_, (a, b, _))| text_w(a, f) + 10.0 + text_w(b, f)).fold(0.0, f32::max) + 14.0 + gw;
        let bh = rows.len() as f32 * lh + 10.0;
        let cn = self.cfg.overlay_corner.as_str();
        let tx = match cn.chars().nth(1) {
            Some('r') => m.right() - 22.0 - bw,
            Some('c') => m.x + (m.w - bw) / 2.0,
            _ => m.x + 22.0,
        };
        let ty = if cn.starts_with('b') { m.bottom() - 32.0 - bh } else { m.y + 22.0 };
        // Positions arrondies au pixel : texte net pendant et après le glissement de l'aperçu.
        let ax = ui.anim(id("ov-x"), tx, 0.3).round();
        let ay = ui.anim(id("ov-y"), ty, 0.3).round();
        ui.c.rrect(r(ax, ay, bw, bh), 3.0, hex(0x080a08).a(self.cfg.overlay_opacity));
        for (i, (k, (a, b, hi))) in rows.iter().enumerate() {
            let ry = ay + 5.0 + i as f32 * lh;
            ui.c.text(ax + 7.0, ry, lh, a, f, pal::TEXT2);
            ui.c.text(ax + bw - 7.0 - gw - text_w(b, f), ry, lh, b, f, if *hi { pal::OK } else { pal::TEXT });
            if graph(k) {
                // Courbe d'exemple (comme Afterburner), à droite de la valeur.
                let (gx, gy, gh) = (ax + bw - 5.0 - gw + 4.0, ry + 3.0, lh - 6.0);
                let n = 12;
                for j in 0..n {
                    let v = |t: f32| 0.5 + 0.35 * ((t * 1.7 + i as f32).sin() * 0.7 + (t * 4.3).sin() * 0.3);
                    let (t0, t1) = (j as f32, j as f32 + 1.0);
                    let (x0, x1) = (gx + (gw - 8.0) * t0 / n as f32, gx + (gw - 8.0) * t1 / n as f32);
                    ui.c.line(x0, gy + gh * (1.0 - v(t0)), x1, gy + gh * (1.0 - v(t1)), 1.0, pal::SIGNAL);
                }
            }
        }
    }

    pub fn tab_stats(&mut self, ui: &mut Ui, body: R) {
        // ---------- colonne de gauche : aperçu ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-stats"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        ui.sc(x, y, 16.0, "Aperçu", pal::TEXT2, 500);
        y += 26.0;
        let m = r(x, y, w, (w * 10.0 / 16.0).round());
        scene(ui.c, m, 6.0);
        ui.c.outline(m, 6.0, 1.0, pal::LINE2);
        self.ov_preview(ui, m);
        y = m.bottom() + 14.0;
        let ovk = self.hk[3].clone().unwrap_or_else(|| crate::front::pretty(&self.cfg.key_overlay));
        let state = match self.cfg.overlay_mode.as_str() {
            "hidden" => "Aucune statistique à l'écran.".to_string(),
            "always" => "Visible en permanence par-dessus le jeu.".to_string(),
            _ => format!("Affichez ou masquez pendant le jeu avec **{ovk}**."),
        };
        y += ui.hint(x, y, w, &state) + 12.0;
        y += ui.hint(x, y, w, "Fenêtre Windows séparée, rien n'est injecté dans le jeu. Invisible en plein écran exclusif. Pas incrustée dans les clips.");
        ui.scroll_end(id("mon-stats"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite : réglages ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("set-stats"), view);
        let (x, w) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let (fx, fw) = (x + LAB, w - LAB);
        let mut y = view.y + 18.0;
        // Bandeau d'information, toujours en tête : sans droits, FPS et temps d'image restent vides.
        y += ui.note(x, y, w, Note::Warn, "lock", "**FPS et temps d'image demandent les droits administrateur** (ou le groupe « Utilisateurs du journal de performances »). Sans eux, seuls CPU, GPU, températures et mémoire s'affichent.", 0.0).0;
        if !self.elevated {
            y += 8.0;
            y += ui.hint(x, y, w, "Onglet **Avancé** : « Démarrer en administrateur » les active sans demande à chaque lancement.");
        }

        // Affichage
        y = self.sect(ui, "Affichage et raccourci", x, y + 6.0, w, true);
        self.lab(ui.c, x, y, 38.0, "Affichage", None);
        self.seg_cfg(ui, "overlay_mode", fx, y, &[("Masqué", json!("hidden")), ("Avec raccourci", json!("hotkey")), ("Toujours", json!("always"))], false);
        y += 38.0 + 8.0;
        let t = match self.cfg.overlay_mode.as_str() {
            "hidden" => "Aucune statistique à l'écran : rien n'est mesuré, aucun coût.".to_string(),
            "always" => "Visible en permanence par-dessus le jeu.".to_string(),
            _ => format!("Affichez ou masquez pendant le jeu avec {ovk}. Masqué, rien n'est mesuré ni dessiné."),
        };
        y += ui.hint(fx, y, fw, &t) + 8.0;
        // Raccourci de l'overlay (déplacé ici depuis « Raccourcis »).
        y = self.hk_row(ui, 3, x, y, w);
        y += 4.0;

        // Mesures
        y = self.sect(ui, "Mesures", x, y + 12.0, w, false);
        for (which, label, key) in [(0, "Mesures", "overlay_items"), (1, "Graphique", "overlay_graphs")] {
            let list = if which == 0 { self.cfg.overlay_items.clone() } else { self.cfg.overlay_graphs.clone() };
            let items: Vec<(String, bool, bool)> = ITEMS
                .iter()
                .map(|(k, l)| {
                    let on = list.iter().any(|x| x == k);
                    // Graphique : seulement pour une mesure affichée. Temp. CPU sans capteur : décochable, pas cochable.
                    let off = if which == 1 { !self.cfg.overlay_items.iter().any(|x| x == k) } else { *k == "cpu_temp" && !self.cpu_temp && !on };
                    (l.to_string(), on, off)
                })
                .collect();
            let (hit, bottom) = self.chip_rows(ui, key, fx, y, fw, &items);
            self.lab(ui.c, x, y, 32.0, label, None);
            if let Some(i) = hit {
                let k = ITEMS[i].0;
                let mut on: Vec<&str> = list.iter().map(String::as_str).collect();
                if on.contains(&k) { on.retain(|x| *x != k) } else { on.push(k) }
                let ordered: Vec<&str> = ITEMS.iter().map(|i| i.0).filter(|i| on.contains(i)).collect();
                self.set(key, json!(ordered));
            }
            y = bottom + 8.0;
            let help = if which == 0 {
                "FPS et MS/IMG (temps moyen d'une image, 1000 ÷ FPS) comptent les images du frame gen (DLSS, FSR) comme le compteur NVIDIA. Le **1 % low** est calculé sur les 10 dernières secondes : la moyenne des 1 % d'images les plus lentes, comme Afterburner et CapFrameX."
            } else {
                "Courbe des 15 dernières secondes à côté de la valeur, comme Afterburner. Seulement pour une mesure affichée."
            };
            y += ui.hint(fx, y, fw, help) + 14.0;
        }

        // Apparence
        y = self.sect(ui, "Position et apparence", x, y, w, false);
        self.lab(ui.c, x, y, 75.0, "Position", None);
        let cb = r(fx, y, 134.0, 75.0);
        ui.c.rrect(cb, 6.0, pal::WELL);
        ui.c.border(cb, 6.0, 1.0, pal::LINE);
        let mut pick = None;
        for (i, v) in ["tl", "tc", "tr", "bl", "bc", "br"].iter().enumerate() {
            let cell = r(fx + 4.0 + (i % 3) as f32 * 43.0, y + 4.0 + (i / 3) as f32 * 35.0, 40.0, 32.0);
            let on = self.cfg.overlay_corner == *v;
            if on || ui.hot(cell) {
                ui.c.rrect(cell, 3.0, pal::RAISE);
            }
            if ui.click(idn("corner", i), cell) {
                pick = Some(*v);
            }
            let mx = match i % 3 {
                0 => cell.x + 6.0,
                1 => cell.x + (cell.w - 9.0) / 2.0,
                _ => cell.right() - 15.0,
            };
            let my = if i / 3 == 0 { cell.y + 6.0 } else { cell.bottom() - 13.0 };
            ui.c.rrect(r(mx, my, 9.0, 7.0), 1.0, if on { pal::SIGNAL } else { pal::LINE2 });
        }
        if let Some(v) = pick {
            self.set("overlay_corner", json!(v));
        }
        ui.hint(fx + 134.0 + 14.0, y + 4.0, fw - 148.0, "Coins et milieux de l'écran du jeu, à la bonne échelle sur chaque écran (100 %, 125 %, 150 %).");
        y += 75.0 + 14.0;
        self.lab(ui.c, x, y, 38.0, "Taille", None);
        self.seg_cfg(ui, "overlay_size", fx, y, &[("Petite", json!("s")), ("Moyenne", json!("m")), ("Grande", json!("l"))], false);
        y += 38.0 + 14.0;
        self.lab(ui.c, x, y, 34.0, "Opacité du fond", None);
        let pct = (self.cfg.overlay_opacity * 100.0).round();
        let (s, _) = self.fader(ui, id("op"), fx, y + 1.0, fw, pct, (0.0, 100.0, 5.0), (&format!("{pct:.0}"), " %"), None);
        if s.changed {
            self.cfg.overlay_opacity = s.v / 100.0;
        }
        if s.done {
            self.set("overlay_opacity", json!(s.v / 100.0));
        }
        y += 34.0 + 14.0;

        // Cartes et mise à jour
        y = self.sect(ui, "Cartes et mise à jour", x, y, w, false);
        // Cartes mesurées (GPU %, température, VRAM) : une ou plusieurs, puce intégrée comprise.
        let names = self.gpu_names.clone();
        if names.len() > 1 {
            // Aucune cochée = la carte NVIDIA (ou la première) : affichée cochée.
            let chosen: Vec<String> = if self.cfg.overlay_gpus.iter().any(|g| names.contains(g)) {
                self.cfg.overlay_gpus.iter().filter(|g| names.contains(g)).cloned().collect()
            } else {
                names.iter().find(|n| n.contains("NVIDIA")).or(names.first()).cloned().into_iter().collect()
            };
            let items: Vec<(String, bool, bool)> = names.iter().map(|n| (n.strip_prefix("NVIDIA ").unwrap_or(n).to_string(), chosen.contains(n), false)).collect();
            let (hit, bottom) = self.chip_rows(ui, "gpus", fx, y, fw, &items);
            self.lab(ui.c, x, y, 32.0, "Cartes mesurées", None);
            if let Some(i) = hit {
                let n = names[i].clone();
                let mut on = chosen.clone();
                if on.contains(&n) {
                    // Toujours au moins une carte.
                    if on.len() > 1 {
                        on.retain(|g| *g != n);
                    }
                } else {
                    on.push(n);
                }
                let ordered: Vec<&String> = names.iter().filter(|g| on.contains(g)).collect();
                self.set("overlay_gpus", json!(ordered));
            }
            y = bottom + 8.0;
        }
        let t = if names.is_empty() {
            "GPU, température GPU et VRAM : aucune carte graphique trouvée."
        } else {
            "GPU, température GPU et VRAM : une ligne par carte cochée. Température : cartes NVIDIA seulement (« n/d » pour une puce AMD ou Intel, illisible sans le logiciel du fabricant)."
        };
        y += ui.hint(if names.len() > 1 { fx } else { x }, y, if names.len() > 1 { fw } else { w }, t) + 16.0;
        self.lab(ui.c, x, y, 38.0, "Mise à jour", Some("des chiffres"));
        self.seg_cfg(ui, "overlay_refresh_ms", fx, y, &[("0,25 s", json!(250)), ("0,5 s", json!(500)), ("1 s", json!(1000)), ("2 s", json!(2000))], true);
        y += 38.0 + 8.0;
        y += ui.hint(fx, y, fw, "Les chiffres et l'overlay ne sont redessinés qu'à cet intervalle, jamais entre deux : 0,25 s reste quasi gratuit, 1 s ou 2 s le rend encore plus discret.");
        ui.scroll_end(id("set-stats"), view, y + 28.0 - view.y, clip);
    }

    pub fn tab_advanced(&mut self, ui: &mut Ui, body: R) {
        let ce = self.chip_eff();
        let chip_name = self.gpus.iter().find(|g| Some(&g.0) == ce.as_ref()).map_or("Processeur".to_string(), |g| g.1.clone());
        let stretch = match self.cfg.stretch.as_str() {
            "16:9" => "16:9",
            "16:10" => "16:10",
            _ => "Désactivé",
        };
        // ---------- colonne de gauche : état ----------
        ui.c.rect(r(body.x, body.y, 330.0, body.h), pal::PANEL);
        ui.c.rect(r(body.x + 329.0, body.y, 1.0, body.h), pal::LINE);
        let view = r(body.x, body.y, 329.0, body.h);
        let clip = ui.scroll_begin(id("mon-adv"), view);
        let (x, w) = (body.x + 16.0, 297.0);
        let mut y = body.y + 16.0;
        ui.sc(x, y, 16.0, "État", pal::TEXT2, 500);
        y += 26.0;
        let rows: [(&str, String, bool); 5] = [
            ("Administrateur", if self.elevated { "Oui".into() } else { "Non".into() }, self.elevated),
            ("Démarrage avec Windows", if self.autostart { "Oui".into() } else { "Non".into() }, self.autostart),
            ("Puce d'encodage", chip_name.clone(), true),
            ("Mode étiré", stretch.to_string(), self.cfg.stretch != "off"),
            ("Version", env!("CARGO_PKG_VERSION").to_string(), true),
        ];
        let q = r(x, y, w, rows.len() as f32 * 40.0 + 8.0);
        ui.c.rrect(q, 7.0, pal::BODY);
        ui.c.border(q, 7.0, 1.0, pal::LINE2);
        let mut ry = y + 4.0;
        for (i, (l, v, on)) in rows.iter().enumerate() {
            ui.c.text(x + 14.0, ry, 40.0, l, F::s(12.5, 400), pal::TEXT2);
            let f = F::s(12.5, 600);
            let vw = text_w(v, f).min(w - 150.0);
            ui.c.text_cut(x + w - 14.0 - vw, ry, 40.0, w - 150.0, v, f, if *on { pal::TEXT } else { pal::TEXT3 });
            ry += 40.0;
            if i + 1 < rows.len() {
                ui.c.rect(r(x + 14.0, ry - 0.5, w - 28.0, 1.0), pal::LINE);
            }
        }
        y = q.bottom() + 14.0;
        y += ui.hint(x, y, w, "Réglages rarement changés. Tout est enregistré tout de suite et gardé d'un lancement à l'autre.");
        ui.scroll_end(id("mon-adv"), view, y + 16.0 - body.y, clip);

        // ---------- colonne de droite : réglages ----------
        let view = r(body.x + 330.0, body.y, body.w - 330.0, body.h);
        let clip = ui.scroll_begin(id("set-adv"), view);
        let (x, w) = (view.x + 22.0, view.w - 44.0 - 10.0);
        let mut y = view.y + 4.0;

        y = self.sect(ui, "Démarrage et fenêtre", x, y, w, true);
        let (hit, hh) = self.sw_row(ui, id("autoSw"), x, y, w, "Ouvrir au démarrage", "Clipper se lance avec Windows, réduit dans la zone de notification.", self.autostart, false);
        if hit {
            self.autostart = !self.autostart;
            let v = self.autostart;
            self.send(json!({"cmd": "autostart", "value": v}));
        }
        y = self.sw_sep(ui.c, x, y + hh, w);
        let tray = self.cfg.minimize_to_tray;
        let (hit, hh) = self.sw_row(ui, id("traySw"), x, y, w, "Réduire dans la zone de notification", "Le bouton « – » range Clipper près de l'horloge (flèche en bas à droite), sans icône dans la barre des tâches.", tray, false);
        if hit {
            self.set("minimize_to_tray", json!(!tray));
        }
        y = self.sw_sep(ui.c, x, y + hh, w);
        let on = self.cfg.admin_mode;
        let hint = if self.admin_busy {
            "En attente de l'autorisation Windows…"
        } else if on && self.elevated {
            "Actif : Clipper tourne en administrateur, sans demande à chaque lancement. Les FPS de l'overlay fonctionnent."
        } else if on {
            "Activé. Relancez Clipper pour passer en administrateur."
        } else {
            "Windows demande l'autorisation une seule fois. Ensuite, plus aucune demande au lancement (FPS de l'overlay, nettoyage)."
        };
        let (hit, hh) = self.sw_row(ui, id("adminSw"), x, y, w, "Démarrer en administrateur", hint, on, self.admin_busy);
        if hit {
            self.admin_busy = true;
            self.send(json!({"cmd": "admin_mode", "value": !on}));
        }
        y += hh;
        if on && !self.elevated {
            y += 10.0;
            if ui.btn(id("adminRel"), r(x, y, Ui::btn_w("Relancer en administrateur", true, Btn::N), 32.0), "Relancer en administrateur", Some("refresh"), Btn::N) {
                self.send(json!({"cmd": "relaunch_admin"}));
            }
            y += 32.0;
        }
        y += 20.0;

        y = self.sect(ui, "Encodage et compatibilité", x, y, w, false);
        // Puce d'encodage
        let (o, mut dy) = self.details(ui, "adv:chip", "Puce d'encodage", &chip_name, false, x, y, w);
        if o {
            let (ix, iw) = (x + 12.0, w - 24.0);
            // Noms de cartes : libellés dynamiques, dessinés comme un groupe segmenté.
            let names: Vec<String> = if self.gpus.is_empty() { vec!["Processeur".into()] } else { self.gpus.iter().map(|g| g.1.clone()).collect() };
            let mut sx = ix + 4.0;
            let total: f32 = names.iter().map(|n| (32.0 + text_w(n, F::s(13.0, 400))).ceil() + 2.0).sum::<f32>() + 6.0;
            let bx = r(ix, dy, total.min(iw), 38.0);
            ui.c.rrect(bx, 6.0, pal::WELL);
            ui.c.border(bx, 6.0, 1.0, pal::LINE);
            let mut pick = None;
            for (i, n) in names.iter().enumerate() {
                let cwid = (32.0 + text_w(n, F::s(13.0, 400))).ceil();
                let cell = r(sx, dy + 4.0, cwid, 30.0);
                let on = self.gpus.get(i).is_none_or(|g| Some(&g.0) == ce.as_ref());
                if on {
                    ui.c.rrect(cell, 4.0, pal::RAISE);
                }
                if !self.gpus.is_empty() && ui.click(idn("chip", i), cell) {
                    pick = Some(i);
                }
                ui.c.circle(cell.x + 12.5, cell.cy(), 2.5, if on { pal::SIGNAL } else { pal::LINE2 });
                ui.c.text(cell.x + 22.0, cell.y, 30.0, n, F::s(13.0, 400), if on || ui.hot(cell) { pal::TEXT } else { pal::TEXT2 });
                sx += cwid + 2.0;
            }
            if let Some(i) = pick {
                let gid = self.gpus[i].0.clone();
                self.set("encode_chip", json!(gid));
                if !self.enc_ok(&self.cfg.encoder.clone()) {
                    let e = self.encoders.iter().find(|e| vendor(e) == Some(gid.as_str()) && e.starts_with("h264")).or_else(|| self.encoders.iter().find(|e| self.enc_ok(e))).cloned();
                    if let Some(e) = e {
                        self.set("encoder", json!(e));
                    }
                }
            }
            dy += 38.0 + 12.0;
            dy += ui.hint(ix, dy, iw, "Les encodeurs des autres puces sont grisés dans « Encodeur » (onglet Clipper). x264 (processeur) reste toujours possible.");
            if self.gpus.len() >= 2 {
                dy += 12.0;
                dy += ui.note(ix, dy, iw, Note::Info, "info", "NVENC est déjà une puce séparée des cœurs qui font tourner le jeu (impact typique `1–3 %` de FPS). La puce intégrée libère NVENC mais coûte environ `6 %` de processeur et de la bande passante mémoire. **Si votre jeu est limité par le processeur, restez sur la carte graphique.**", 0.0).0;
            }
            dy += 14.0;
        }
        y = self.details_end(ui, x, y, dy, w) + 10.0;
        // Mode étiré
        let (o, mut dy) = self.details(ui, "adv:str", "Mode étiré (4:3 étiré)", stretch, false, x, y, w);
        if o {
            let (ix, iw) = (x + 12.0, w - 24.0);
            self.seg_cfg(ui, "stretch", ix, dy, &[("Désactivé", json!("off")), ("Étirer en 16:9", json!("16:9")), ("Étirer en 16:10", json!("16:10"))], false);
            dy += 38.0 + 12.0;
            dy += ui.hint(ix, dy, iw, "Si vous jouez en `1440×1080` étiré, le clip est élargi pour ressembler à ce que vous voyez. Lisible partout (Discord, YouTube).");
            if self.cfg.stretch != "off" {
                dy += 12.0;
                dy += ui.note(ix, dy, iw, Note::Warn, "warn", "**Sans coût, sans perte.** L'image garde tous ses pixels ; le fichier est marqué 16:9 et les lecteurs (VLC, Premiere, navigateur, Discord) l'étirent à l'affichage.", 0.0).0;
            }
            dy += 14.0;
        }
        y = self.details_end(ui, x, y, dy, w) + 10.0;
        // Limites connues
        let gh = (self.mbps() * self.typical() + self.cfg.audio_tracks.len() as f32 * 0.16) * 3600.0 / 8.0 / 1000.0;
        let lims: [(&'static str, String); 5] = [
            ("screen", "La barre et les statistiques sont invisibles en **plein écran exclusif**. Si le jeu capture la souris, utilisez les raccourcis : ils marchent toujours.".into()),
            ("disk", format!("Le buffer écrit environ **{} Go par heure** sur le disque du buffer, en boucle.", crate::front::fr(gh as f64, 1))),
            ("lock", "Les **FPS** de l'overlay demandent les droits administrateur.".into()),
            ("res", "Une résolution autre que celle de l'écran passe par un redimensionnement **processeur**.".into()),
            ("chip", "PC hybride : si l'écran est branché sur la puce intégrée, l'encodage NVIDIA sans copie peut échouer ; Clipper bascule alors seul.".into()),
        ];
        let (o, mut dy) = self.details(ui, "adv:lim", "Limites connues", &lims.len().to_string(), true, x, y, w);
        if o {
            let (ix, iw) = (x + 12.0, w - 24.0);
            for (icon, t) in &lims {
                dy += self.lim(ui, ix, dy, iw, icon, t) + 8.0;
            }
            dy += 4.0;
            dy += ui.hint(ix, dy, iw, "Clipper n'utilise que des API Windows publiques (comme OBS ou Discord) et ne touche jamais au processus du jeu. Personne ne peut garantir l'absence de ban : chaque éditeur décide seul.");
            dy += 14.0;
        }
        y = self.details_end(ui, x, y, dy, w);
        ui.scroll_end(id("set-adv"), view, y + 28.0 - view.y, clip);
    }

    /// Carte « Clipping » et « Capture du bureau » (colonne de gauche de l'onglet Clipper). Chaque rangée :
    /// titre et interrupteur centrés sur la même ligne, texte d'aide dessous sur toute la largeur utile,
    /// filet entre les deux, marges égales. Renvoie le y sous la carte.
    pub fn capture_card(&mut self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        const PAD: f32 = 14.0;
        let hint_f = |t: &str| crate::gfx::md(t, F::s(12.5, 400), pal::TEXT3, 600, pal::TEXT2);
        let clip_hint = if self.cfg.clipping { "Activé : les dernières minutes sont gardées pour Clip et REC." } else { "Désactivé : aucune capture, aucun encodage, 0 % processeur et GPU. Clip et REC ne font rien." };
        let desk_hint = if self.cfg.desktop_capture { "Activée : le buffer tourne en permanence, même sur le bureau." } else { "Désactivée : le buffer ne tourne que pendant un jeu plein écran. 0 % GPU sur le bureau." };
        let tw = w - 2.0 * PAD;
        let rows = [
            ("clipping", "Clipping", clip_hint, self.cfg.clipping, false),
            ("desktop_capture", "Capture du bureau", desk_hint, self.cfg.desktop_capture, false),
        ];
        let hs: Vec<f32> = rows.iter().map(|r| para_h(ui.c, tw, 18.0, &hint_f(r.2))).collect();
        let row_h = |h: f32| 24.0 + 8.0 + h;
        let total = PAD + row_h(hs[0]) + PAD + 1.0 + PAD + row_h(hs[1]) + PAD;
        let q = r(x, y, w, total);
        ui.c.rrect(q, 8.0, pal::BODY);
        ui.c.border(q, 8.0, 1.0, pal::LINE2);
        let mut ry = y + PAD;
        for (i, (key, title, hint, on, off)) in rows.into_iter().enumerate() {
            // Ligne de 24 px : titre centré verticalement sur la hauteur de l'interrupteur.
            ui.led(x + PAD + 3.5, ry + 12.0, 7.0, if off { pal::LINE2 } else if on { pal::OK } else { pal::TEXT3 });
            ui.c.text(x + PAD + 15.0, ry, 24.0, title, F::s(13.5, 600), if off { pal::TEXT3 } else { pal::TEXT });
            if ui.switch(id(key), x + w - PAD - 44.0, ry, on, off) {
                self.set(key, json!(!on));
            }
            ui.c.para(x + PAD, ry + 32.0, tw, 18.0, &hint_f(hint), true);
            ry += row_h(hs[i]) + PAD;
            if i == 0 {
                ui.c.rect(r(x + PAD, ry, w - 2.0 * PAD, 1.0), pal::LINE);
                ry += 1.0 + PAD;
            }
        }
        q.bottom()
    }
}
