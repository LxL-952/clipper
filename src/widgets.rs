//! Contrôles de l'interface, en mode immédiat : chaque image redessine tout et traite la souris au passage.
//! Rien n'est gardé entre deux images hormis `UiState` (contrôle pressé, animations, défilement, menu ouvert).
//! Dimensions, couleurs et rayons repris des règles CSS de l'ancienne page.
use crate::gfx::{Canvas, Col, F, R, Span, hex, md, pal, r, text_w};
use std::collections::HashMap;
use std::time::Instant;

/// Entrées reçues depuis la dernière image.
#[derive(Default, Clone)]
pub struct Input {
    /// Position de la souris en pixels logiques de la fenêtre ; hors fenêtre : (-1, -1).
    pub mouse: (f32, f32),
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub rpressed: bool,
    pub dbl: bool,
    /// Molette, en crans (positif = vers le haut).
    pub wheel: f32,
    /// Touches enfoncées (codes W3C : « KeyK », « Escape », « ArrowLeft », « F5 »…).
    pub keys: Vec<String>,
    pub text: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Input {
    pub fn key(&self, code: &str) -> bool {
        self.keys.iter().any(|k| k == code)
    }
    /// Remet à zéro ce qui ne dure qu'une image.
    pub fn end_frame(&mut self) {
        self.pressed = false;
        self.released = false;
        self.rpressed = false;
        self.dbl = false;
        self.wheel = 0.0;
        self.keys.clear();
        self.text.clear();
    }
}

struct Anim {
    from: f32,
    to: f32,
    t0: Instant,
    v: f32,
}

/// Liste déroulante ouverte.
pub struct Popup {
    pub id: u64,
    /// Rectangle du champ, en coordonnées de la fenêtre.
    pub anchor: R,
    pub opts: Vec<(String, bool)>,
    pub sel: Option<usize>,
    /// Ouverte pendant cette image : l'appui qui l'a ouverte ne la referme pas.
    pub fresh: bool,
}

#[derive(Default)]
pub struct UiState {
    /// Contrôle sur lequel le bouton de la souris a été enfoncé.
    pub active: u64,
    anims: HashMap<u64, Anim>,
    pub scroll: HashMap<u64, f32>,
    /// Position visée par la molette (la position affichée y glisse).
    pub scroll_to: HashMap<u64, f32>,
    /// Une animation est en cours : une autre image est nécessaire.
    pub animating: bool,
    /// Curseur « main » demandé pendant cette image.
    pub hand: bool,
    /// Zone d'un calque (menu, liste) dessiné à l'image précédente : le reste de la fenêtre ignore la souris.
    pub block: Option<R>,
    pub popup: Option<Popup>,
    /// Choix fait dans la liste déroulante : (champ, index), lu par le champ à l'image suivante.
    pub picked: Option<(u64, usize)>,
    /// Champ de texte actif.
    pub focus: u64,
    /// Glissement de la barre de défilement : (zone, écart souris / haut du curseur).
    drag_scroll: Option<(u64, f32)>,
    in_overlay: bool,
}

pub struct Ui<'a> {
    pub c: &'a mut Canvas,
    pub i: &'a Input,
    pub s: &'a mut UiState,
    pub now: Instant,
}

/// Secondes depuis le lancement (animations cycliques : roue d'attente, curseur de texte).
fn clock() -> f32 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f32()
}

pub fn id(s: &str) -> u64 {
    // FNV-1a : stable, sans dépendance.
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn idn(s: &str, n: usize) -> u64 {
    id(s) ^ (n as u64 + 1).wrapping_mul(0x9e3779b97f4a7c15)
}

/// Courbe d'animation de l'interface (cubic-bezier(.16,1,.3,1) : départ vif, arrivée douce).
pub fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(5)
}

impl<'a> Ui<'a> {
    /// Début d'image : à appeler avant tout contrôle.
    pub fn begin(&mut self) {
        self.s.animating = false;
        self.s.hand = false;
        self.s.in_overlay = false;
        if self.s.popup.is_none() {
            self.s.block = None;
        }
    }

    /// Fin d'image : relâche le contrôle pressé.
    pub fn end(&mut self) {
        if self.i.released || !self.i.down {
            self.s.active = 0;
            self.s.drag_scroll = None;
        }
    }

    /// Souris dans le repère du contenu (défilement compris).
    pub fn mouse(&self) -> (f32, f32) {
        (self.i.mouse.0 - self.c.ox, self.i.mouse.1 - self.c.oy)
    }

    /// La souris survole `q` (dans la découpe en cours, et pas sous un calque).
    pub fn hot(&self, q: R) -> bool {
        let m = self.mouse();
        if !q.has(m) || !self.c.clip_rect().has(m) {
            return false;
        }
        self.s.in_overlay || self.s.block.is_none_or(|b| !b.has(self.i.mouse)) && self.s.popup.is_none()
    }

    /// Clic complet (enfoncé puis relâché) sur `q`.
    pub fn click(&mut self, wid: u64, q: R) -> bool {
        let hot = self.hot(q);
        if hot {
            self.s.hand = true;
            if self.i.pressed {
                self.s.active = wid;
            }
        }
        hot && self.i.released && self.s.active == wid
    }

    pub fn held(&self, wid: u64) -> bool {
        self.s.active == wid && self.i.down
    }

    /// Valeur animée vers `target` en `secs` secondes.
    pub fn anim(&mut self, wid: u64, target: f32, secs: f32) -> f32 {
        let now = self.now;
        let a = self.s.anims.entry(wid).or_insert(Anim { from: target, to: target, t0: now, v: target });
        if a.to != target {
            (a.from, a.to, a.t0) = (a.v, target, now);
        }
        let t = if secs <= 0.0 { 1.0 } else { now.duration_since(a.t0).as_secs_f32() / secs };
        a.v = a.from + (a.to - a.from) * ease(t);
        if t < 1.0 {
            self.s.animating = true;
        } else {
            a.v = a.to;
        }
        a.v
    }

    /// Les contrôles dessinés dans `f` forment un calque : ils reçoivent la souris même si un calque bloque le reste.
    pub fn overlay<T>(&mut self, f: impl FnOnce(&mut Ui) -> T) -> T {
        let (ox, oy) = (self.c.ox, self.c.oy);
        let clip = self.c.set_clip(None);
        (self.c.ox, self.c.oy) = (0.0, 0.0);
        let was = std::mem::replace(&mut self.s.in_overlay, true);
        let out = f(self);
        self.s.in_overlay = was;
        (self.c.ox, self.c.oy) = (ox, oy);
        self.c.restore_clip(clip);
        out
    }

    // ---------- texte ----------

    /// Paragraphe balisé (`**gras**`, `` `mono` ``) ; renvoie la hauteur.
    pub fn hint(&mut self, x: f32, y: f32, w: f32, text: &str) -> f32 {
        let sp = md(text, F::s(12.0, 400), pal::TEXT3, 500, pal::TEXT2);
        self.c.para(x, y, w, 17.4, &sp, true)
    }

    /// Petit titre en capitales espacées (.sc).
    pub fn sc(&mut self, x: f32, y: f32, lh: f32, text: &str, c: Col, wt: u16) -> f32 {
        self.c.text(x, y, lh, &text.to_uppercase(), F::s(11.0, wt).ls(0.13), c)
    }

    /// Pastille ronde (.led).
    pub fn led(&mut self, cx: f32, cy: f32, d: f32, c: Col) {
        self.c.circle(cx, cy, d / 2.0, c);
    }

    // ---------- boutons ----------

    pub fn btn_w(label: &str, icon: bool, st: Btn) -> f32 {
        let (pad, gap, ic, f) = st.metrics();
        2.0 * pad + text_w(label, f) + if icon { ic + if label.is_empty() { 0.0 } else { gap } } else { 0.0 } + 2.0
    }

    /// Bouton (.btn). Renvoie vrai au clic.
    pub fn btn(&mut self, wid: u64, q: R, label: &str, icon: Option<&'static str>, st: Btn) -> bool {
        let clicked = !st.disabled && self.click(wid, q);
        let hot = !st.disabled && self.hot(q);
        let q = if hot && self.held(wid) { r(q.x, q.y + 1.0, q.w, q.h) } else { q };
        let (bg, line, fg) = match (st.kind, hot) {
            (BtnKind::Normal, false) => (pal::RAISE, pal::LINE2, pal::TEXT),
            (BtnKind::Normal, true) => (pal::RAISE_H, pal::BTN_LINE_H, pal::TEXT),
            (BtnKind::Primary, false) => (pal::SIGNAL, pal::SIGNAL, pal::SIGNAL_INK),
            (BtnKind::Primary, true) => (hex(0xff9150), hex(0xff9150), pal::SIGNAL_INK),
            (BtnKind::Ghost, false) => (Col(0, 0, 0, 0), Col(0, 0, 0, 0), pal::TEXT2),
            (BtnKind::Ghost, true) => (pal::RAISE, Col(0, 0, 0, 0), pal::TEXT),
            (BtnKind::Danger, false) => (hex(0x3a1814), hex(0x6b2a22), hex(0xffc9c2)),
            (BtnKind::Danger, true) => (hex(0x4a1d18), hex(0x86342a), hex(0xffc9c2)),
        };
        let o = if st.disabled { 0.45 } else { 1.0 };
        self.c.rrect(q, 5.0, bg.a(o));
        self.c.border(q, 5.0, 1.0, line.a(o));
        if st.kind != BtnKind::Ghost {
            // Reflet d'un pixel en haut (inset 0 1px 0).
            let hl = if st.kind == BtnKind::Primary { 0.25 } else { 0.04 };
            self.c.rect(r(q.x + 2.0, q.y + 1.0, q.w - 4.0, 1.0), pal::WHITE.a(hl * o));
        }
        let (pad, gap, ic, f) = st.metrics();
        let tw = text_w(label, f);
        let total = tw + if icon.is_some() { ic + if label.is_empty() { 0.0 } else { gap } } else { 0.0 };
        let mut x = if st.left { q.x + pad } else { q.x + (q.w - total) / 2.0 };
        if let Some(name) = icon {
            self.c.icon(name, x, q.y + (q.h - ic) / 2.0, ic, fg.a(o));
            x += ic + gap;
        }
        self.c.text(x, q.y, q.h, label, f, fg.a(o));
        clicked
    }

    /// Bouton carré sans fond (barre de titre, fermeture d'une notification).
    pub fn wbtn(&mut self, wid: u64, q: R, icon: &'static str, danger: bool) -> bool {
        let clicked = self.click(wid, q);
        let hot = self.hot(q);
        if hot {
            self.c.rrect(q, 5.0, if danger { hex(0x5a1f1a) } else { pal::RAISE });
        }
        let fg = if hot { if danger { pal::WHITE } else { pal::TEXT } } else { pal::TEXT3 };
        self.c.icon(icon, q.x + (q.w - 18.0) / 2.0, q.y + (q.h - 18.0) / 2.0, 18.0, fg);
        clicked
    }

    /// Interrupteur (.sw), 44x24. Renvoie vrai au clic.
    pub fn switch(&mut self, wid: u64, x: f32, y: f32, on: bool, disabled: bool) -> bool {
        let q = r(x, y, 44.0, 24.0);
        let clicked = !disabled && self.click(wid, q);
        let t = self.anim(wid, if on { 1.0 } else { 0.0 }, 0.25);
        let o = if disabled { 0.5 } else { 1.0 };
        self.c.rrect(q, 5.0, pal::WELL.a(o));
        self.c.rrect(q, 5.0, pal::SIGNAL_SOFT.a(t * o));
        self.c.border(q, 5.0, 1.0, pal::LINE2.mix(pal::SIGNAL, t).a(o));
        let k = r(x + 3.0 + 20.0 * t, y + 3.0, 18.0, 18.0);
        self.c.rrect(r(k.x, k.y + 1.0, k.w, k.h), 3.0, pal::BLACK.a(0.35 * o));
        self.c.grad(k, 3.0, true, &[(0.0, hex(0x58654f).mix(hex(0xffa066), t).a(o)), (1.0, hex(0x3d4838).mix(pal::SIGNAL, t).a(o))]);
        let la = (0.35 - 0.10 * t) * o;
        self.c.rect(r(k.x + 6.0, k.y, 1.0, 18.0), pal::BLACK.a(la));
        self.c.rect(r(k.x + 10.0, k.y, 1.0, 18.0), pal::BLACK.a(la));
        clicked
    }

    /// Groupe segmenté (.seg). `fill` : largeur imposée, options de même largeur. Renvoie l'option cliquée.
    pub fn seg(&mut self, wid: u64, x: f32, y: f32, opts: &[SegOpt], sel: Option<usize>, st: Seg) -> (Option<usize>, f32) {
        let (pad, gap) = if st.fill.is_some() { (4.0, 5.0) } else { (10.0, 7.0) };
        let dot = if st.dot { 5.0 + gap } else { 0.0 };
        let f_of = |o: &SegOpt| if o.mono { F::m(12.5, 400) } else { F::s(13.0, 400) };
        let widths: Vec<f32> = match st.fill {
            Some(w) => {
                let each = (w - 8.0 - 2.0 * (opts.len() as f32 - 1.0)) / opts.len() as f32;
                vec![each; opts.len()]
            }
            None => opts.iter().map(|o| (2.0 * pad + dot + text_w(o.label, f_of(o))).ceil()).collect(),
        };
        let total = widths.iter().sum::<f32>() + 8.0 + 2.0 * (opts.len() as f32 - 1.0);
        let q = r(x, y, total, 38.0);
        self.c.rrect(q, 6.0, pal::WELL);
        self.c.border(q, 6.0, 1.0, pal::LINE);
        let mut cx = x + 4.0;
        let mut out = None;
        for (i, o) in opts.iter().enumerate() {
            let cell = r(cx, y + 4.0, widths[i], 30.0);
            let on = sel == Some(i);
            let hot = o.enabled && self.hot(cell);
            if o.enabled && self.click(idn("seg", i) ^ wid, cell) {
                out = Some(i);
            }
            if on {
                self.c.rrect(r(cell.x, cell.y + 1.0, cell.w, cell.h), 4.0, pal::BLACK.a(0.45));
                self.c.rrect(cell, 4.0, pal::RAISE);
                self.c.rect(r(cell.x + 2.0, cell.y, cell.w - 4.0, 1.0), pal::WHITE.a(0.05));
            }
            let fg = if !o.enabled { pal::OFF } else if on && st.res { pal::SIGNAL } else if on || hot { pal::TEXT } else { pal::TEXT2 };
            let f = f_of(o);
            let tw = text_w(o.label, f);
            let mut tx = cell.x + (cell.w - tw - dot) / 2.0;
            if st.dot {
                let (dx, dy) = (tx + 2.5, cell.cy());
                if !o.enabled {
                    self.c.border(r(dx - 2.5, dy - 2.5, 5.0, 5.0), 2.5, 1.0, pal::OFF);
                } else {
                    self.c.circle(dx, dy, 2.5, if on { pal::SIGNAL } else { pal::LINE2 });
                }
                tx += dot;
            }
            self.c.text(tx, cell.y, cell.h, o.label, f, fg);
            cx += widths[i] + 2.0;
        }
        (out, total)
    }

    /// Liste déroulante (.sel). Renvoie l'index choisi.
    pub fn select(&mut self, wid: u64, q: R, opts: &[(String, bool)], sel: Option<usize>) -> Option<usize> {
        let hot = self.hot(q);
        self.c.rrect(q, 6.0, pal::WELL);
        self.c.border(q, 6.0, 1.0, if hot || self.s.popup.as_ref().is_some_and(|p| p.id == wid) { pal::BTN_LINE_H } else { pal::LINE2 });
        let label = sel.and_then(|i| opts.get(i)).map_or("", |o| o.0.as_str());
        self.c.text_cut(q.x + 11.0, q.y, q.h, q.w - 45.0, label, F::s(13.0, 400), pal::TEXT);
        self.c.icon("down", q.right() - 23.0, q.y + (q.h - 12.0) / 2.0, 12.0, pal::TEXT2);
        // Ouverte dès l'appui (comme une liste Windows), pas au relâchement.
        if hot {
            self.s.hand = true;
            if self.i.pressed && !opts.is_empty() {
                let anchor = r(q.x + self.c.ox, q.y + self.c.oy, q.w, q.h);
                self.s.popup = Some(Popup { id: wid, anchor, opts: opts.to_vec(), sel, fresh: true });
            }
        }
        match self.s.picked {
            Some((pid, i)) if pid == wid => {
                self.s.picked = None;
                Some(i)
            }
            _ => None,
        }
    }

    /// Dessine la liste ouverte, par-dessus tout. À appeler en fin d'image.
    pub fn popups(&mut self) {
        let Some(mut p) = self.s.popup.take() else { return };
        let fresh = std::mem::take(&mut p.fresh);
        let (_, wh) = self.c.size();
        let row = 28.0;
        let h = (p.opts.len() as f32 * row + 8.0).min(wh - 16.0);
        let y = if p.anchor.bottom() + 4.0 + h > wh - 8.0 { (p.anchor.y - 4.0 - h).max(8.0) } else { p.anchor.bottom() + 4.0 };
        let q = r(p.anchor.x, y, p.anchor.w, h);
        self.s.block = Some(q);
        let mut keep = true;
        let picked = self.overlay(|ui| {
            ui.c.shadow(q, 7.0, 6.0, 14.0, pal::BLACK.a(0.5));
            ui.c.rrect(q, 7.0, pal::BODY);
            ui.c.border(q, 7.0, 1.0, pal::LINE2);
            let mut out = None;
            for (i, (label, enabled)) in p.opts.iter().enumerate() {
                let cell = r(q.x + 4.0, q.y + 4.0 + i as f32 * row, q.w - 8.0, row);
                if cell.bottom() > q.bottom() {
                    break;
                }
                let hot = *enabled && cell.has(ui.i.mouse);
                if hot {
                    ui.s.hand = true;
                    ui.c.rrect(cell, 4.0, pal::RAISE_H);
                } else if p.sel == Some(i) {
                    ui.c.rrect(cell, 4.0, pal::RAISE);
                }
                ui.c.text_cut(cell.x + 8.0, cell.y, cell.h, cell.w - 16.0, label, F::s(13.0, 400), if *enabled { pal::TEXT } else { pal::OFF });
                if hot && ui.i.released {
                    out = Some(i);
                }
            }
            out
        });
        if let Some(i) = picked {
            self.s.picked = Some((p.id, i));
            keep = false;
        }
        // Clic ailleurs ou Échap : la liste se ferme sans rien changer.
        if (!fresh && self.i.pressed && !q.has(self.i.mouse)) || self.i.key("Escape") {
            keep = false;
        }
        if keep {
            self.s.popup = Some(p);
        } else {
            self.s.block = None;
            self.s.animating = true; // une image de plus : le champ lit le choix
        }
    }

    /// Curseur (input range) : piste de 4 px, poignée 12x22. `q` = zone de 32 px de haut.
    pub fn slider(&mut self, wid: u64, q: R, v: f32, min: f32, max: f32, step: f32) -> Slide {
        let hot = self.hot(q);
        if hot {
            self.s.hand = true;
            if self.i.pressed {
                self.s.active = wid;
            }
        }
        let mut out = Slide { v, changed: false, done: false };
        if self.s.active == wid {
            let t = ((self.mouse().0 - q.x - 6.0) / (q.w - 12.0)).clamp(0.0, 1.0);
            let nv = (((min + t * (max - min)) / step).round() * step).clamp(min, max);
            out.changed = (nv - v).abs() > step / 100.0;
            out.v = nv;
            out.done = self.i.released;
        }
        let t = ((out.v - min) / (max - min)).clamp(0.0, 1.0);
        let track = r(q.x, q.cy() - 2.0, q.w, 4.0);
        self.c.outline(track, 1.0, 1.0, pal::LINE2);
        self.c.rrect(track, 1.0, pal::WELL);
        self.c.rrect(r(track.x, track.y, track.w * t, 4.0), 1.0, pal::SIGNAL);
        let k = r(q.x + (q.w - 12.0) * t, q.cy() - 11.0, 12.0, 22.0);
        self.c.rrect(r(k.x, k.y + 2.0, k.w, k.h), 3.0, pal::BLACK.a(0.4));
        self.c.grad(k, 3.0, true, &[(0.0, hex(0x3e4a3a)), (1.0, hex(0x2a3327))]);
        self.c.border(k, 3.0, 1.0, hex(0x56634f));
        self.c.rect(r(k.x + 5.0, k.y + 1.0, 1.0, 20.0), pal::TEXT);
        out
    }

    /// Règle graduée sous un curseur (.ruler) : un trait tous les 10 %.
    pub fn ruler(&mut self, x: f32, y: f32, w: f32) {
        for i in 0..=10 {
            self.c.rect(r(x + 6.0 + (w - 12.0 - 1.0) * i as f32 / 10.0, y, 1.0, 5.0), pal::LINE2);
        }
    }

    /// Pastille à cocher (.chip). Renvoie (clic, largeur).
    pub fn chip(&mut self, wid: u64, x: f32, y: f32, label: &str, on: bool, off: bool) -> (bool, f32) {
        let f = F::s(12.5, 400);
        let w = (20.0 + 10.0 + 7.0 + text_w(label, f) + 2.0).ceil();
        let q = r(x, y, w, 32.0);
        let clicked = !off && self.click(wid, q);
        let o = if off { 0.4 } else { 1.0 };
        if on {
            self.c.rrect(q, 5.0, pal::RAISE.a(o));
        }
        self.c.border(q, 5.0, 1.0, if on { hex(0x56634f) } else { pal::LINE2 }.a(o));
        let b = r(x + 11.0, y + 11.0, 10.0, 10.0);
        if on {
            self.c.rrect(b, 2.0, pal::SIGNAL.a(o));
            self.c.border(b.inset(1.0), 1.0, 2.0, pal::RAISE.a(o));
            self.c.border(b, 2.0, 1.0, pal::SIGNAL.a(o));
        } else {
            self.c.rrect(b, 2.0, pal::WELL.a(o));
            self.c.border(b, 2.0, 1.0, pal::LINE2.a(o));
        }
        self.c.text(x + 28.0, y, 32.0, label, f, if on { pal::TEXT } else { pal::TEXT2 }.a(o));
        (clicked, w)
    }

    /// Case à cocher simple (.chk / listes). 14 ou 16 px.
    pub fn check(&mut self, wid: u64, x: f32, y: f32, size: f32, on: bool, disabled: bool) -> bool {
        let q = r(x, y, size, size);
        let clicked = !disabled && self.click(wid, q.inset(-3.0));
        let o = if disabled { 0.4 } else { 1.0 };
        if on {
            self.c.rrect(q, 3.0, pal::SIGNAL.a(o));
            self.c.icon("check", x + 1.0, y + 1.0, size - 2.0, pal::SIGNAL_INK.a(o));
        } else {
            self.c.rrect(q, 3.0, pal::WELL.a(o));
            self.c.border(q, 3.0, 1.0, pal::TEXT3.a(o));
        }
        clicked
    }

    /// Encadré (.note) : icône + texte balisé. `extra` : hauteur réservée sous le texte (boutons).
    /// Renvoie (hauteur totale, origine de la zone réservée).
    pub fn note(&mut self, x: f32, y: f32, w: f32, kind: Note, icon: &'static str, text: &str, extra: f32) -> (f32, (f32, f32)) {
        let sp = md(text, F::s(12.5, 400), pal::TEXT2, 600, pal::TEXT);
        let tw = w - 22.0 - 16.0 - 9.0;
        let th = self.c.para(0.0, 0.0, tw, 18.1, &sp, false);
        let h = (th + extra + 18.0).ceil();
        let q = r(x, y, w, h);
        let (bg, line, ic) = match kind {
            Note::Warn => (pal::WARN.a(0.08), pal::WARN.a(0.22), pal::WARN),
            Note::Info => (Col(0, 0, 0, 0), pal::LINE, pal::TEXT3),
            Note::Err => (pal::REC.a(0.08), pal::REC.a(0.3), pal::REC),
        };
        self.c.rrect(q, 6.0, bg);
        self.c.border(q, 6.0, 1.0, line);
        self.c.icon(icon, x + 11.0, y + 10.0, 16.0, ic);
        self.c.para(x + 36.0, y + 9.0, tw, 18.1, &sp, true);
        (h, (x + 36.0, y + 9.0 + th))
    }

    /// Roue d'attente (.spin), 14 px.
    pub fn spin(&mut self, cx: f32, cy: f32) {
        let a = clock() * 7.85;
        self.c.arc(cx, cy, 6.0, 0.0, std::f32::consts::TAU, 2.0, pal::LINE2);
        self.c.arc(cx, cy, 6.0, a, a + 1.57, 2.0, pal::SIGNAL);
        self.s.animating = true;
    }

    // ---------- défilement ----------

    /// Ouvre une zone défilante : découpe + décalage. Le contenu se dessine ensuite en coordonnées
    /// normales à partir de `view.y`. Renvoie la découpe à rendre à `scroll_end`.
    pub fn scroll_begin(&mut self, wid: u64, view: R) -> (i32, i32, i32, i32) {
        let clip = self.c.set_clip(Some(view));
        self.c.oy = -self.s.scroll.get(&wid).copied().unwrap_or(0.0);
        clip
    }

    /// Ferme la zone : molette, bornes, barre de défilement (10 px, comme ::-webkit-scrollbar).
    pub fn scroll_end(&mut self, wid: u64, view: R, content_h: f32, clip: (i32, i32, i32, i32)) {
        self.c.oy = 0.0;
        self.c.restore_clip(clip);
        let max = (content_h - view.h).max(0.0);
        let mut pos = self.s.scroll.get(&wid).copied().unwrap_or(0.0);
        let over = view.has(self.i.mouse) && self.s.popup.is_none() && self.s.block.is_none_or(|b| !b.has(self.i.mouse));
        // Molette : 30 px par cran (42 montaient encore trop d'un petit coup), avec un glissé vers la cible.
        // Position changée ailleurs (retour en haut d'une page…) : la cible la suit, sans animation.
        const STEP: f32 = 30.0;
        let akey = wid ^ 0x5C_0011_u64;
        let shown = self.s.anims.get(&akey).map(|a| a.v);
        let mut target = self.s.scroll_to.get(&wid).copied().unwrap_or(pos);
        let mut snap = shown.is_none_or(|v| (v - pos).abs() > 0.5);
        if snap {
            target = pos;
        }
        if over && self.i.wheel != 0.0 {
            target = (target - self.i.wheel * STEP).clamp(0.0, max);
        }
        if max > 0.0 {
            let th = (view.h * view.h / content_h).max(28.0);
            let track = r(view.right() - 10.0, view.y, 10.0, view.h);
            let ty = view.y + (view.h - th) * (pos / max).clamp(0.0, 1.0);
            if over && self.i.pressed && track.has(self.i.mouse) {
                self.s.drag_scroll = Some((wid, if r(track.x, ty, 10.0, th).has(self.i.mouse) { self.i.mouse.1 - ty } else { th / 2.0 }));
            }
            if let Some((did, off)) = self.s.drag_scroll {
                if did == wid && self.i.down {
                    // Barre tirée à la souris : suit la main, sans retard.
                    target = ((self.i.mouse.1 - off - view.y) / (view.h - th)).clamp(0.0, 1.0) * max;
                    snap = true;
                }
            }
        }
        target = target.clamp(0.0, max);
        self.s.scroll_to.insert(wid, target);
        pos = self.anim(akey, target, if snap { 0.0 } else { 0.16 });
        if max > 0.0 {
            let th = (view.h * view.h / content_h).max(28.0);
            let ty = view.y + (view.h - th) * (pos / max).clamp(0.0, 1.0);
            self.c.rrect(r(view.right() - 7.0, ty + 3.0, 4.0, th - 6.0), 2.0, pal::LINE2);
        }
        self.s.scroll.insert(wid, pos.clamp(0.0, max));
    }

    /// Champ de texte sur une ligne (.txt). Renvoie vrai quand Entrée est pressée.
    pub fn text_field(&mut self, wid: u64, q: R, value: &mut String) -> bool {
        if self.i.pressed {
            if self.hot(q) {
                self.s.focus = wid;
            } else if self.s.focus == wid {
                self.s.focus = 0;
            }
        }
        let focus = self.s.focus == wid;
        let mut enter = false;
        if focus {
            for ch in self.i.text.chars().filter(|c| !c.is_control()) {
                value.push(ch);
            }
            for k in &self.i.keys {
                match k.as_str() {
                    "Backspace" => {
                        value.pop();
                    }
                    "Enter" | "NumpadEnter" => enter = true,
                    _ => {}
                }
            }
        }
        self.c.rrect(q, 6.0, pal::WELL);
        self.c.border(q, 6.0, 1.0, if focus { pal::SIGNAL } else { pal::LINE2 });
        let f = F::s(13.0, 400);
        let old = self.c.set_clip(Some(q.inset(2.0).inter(&self.c.clip_rect())));
        let tw = text_w(value, f);
        let x = q.x + 11.0 - (tw - (q.w - 26.0)).max(0.0);
        self.c.text(x, q.y, q.h, value, f, pal::TEXT);
        if focus && clock() % 1.1 < 0.55 {
            self.c.rect(r(x + tw + 1.0, q.y + 9.0, 1.0, q.h - 18.0), pal::SIGNAL);
        }
        self.c.restore_clip(old);
        if focus {
            self.s.animating = true; // curseur clignotant
        }
        enter
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum BtnKind {
    Normal,
    Primary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy)]
pub struct Btn {
    pub kind: BtnKind,
    pub small: bool,
    pub disabled: bool,
    /// Contenu calé à gauche (lignes de menu).
    pub left: bool,
}

impl Btn {
    pub const N: Btn = Btn { kind: BtnKind::Normal, small: false, disabled: false, left: false };
    pub const P: Btn = Btn { kind: BtnKind::Primary, ..Btn::N };
    pub const G: Btn = Btn { kind: BtnKind::Ghost, ..Btn::N };
    pub const D: Btn = Btn { kind: BtnKind::Danger, ..Btn::N };
    pub const fn off(self, d: bool) -> Btn {
        Btn { disabled: d, ..self }
    }
    pub const fn sm(self) -> Btn {
        Btn { small: true, ..self }
    }
    /// (marge intérieure, écart icône-texte, taille d'icône, police)
    fn metrics(&self) -> (f32, f32, f32, F) {
        let wt = if matches!(self.kind, BtnKind::Primary) { 600 } else { 500 };
        if self.small { (8.0, 5.0, 13.0, F::s(12.0, wt)) } else { (12.0, 8.0, 16.0, F::s(13.0, wt)) }
    }
}

pub struct SegOpt {
    pub label: &'static str,
    pub enabled: bool,
    pub mono: bool,
}

pub const fn so(label: &'static str) -> SegOpt {
    SegOpt { label, enabled: true, mono: false }
}

pub const fn son(label: &'static str) -> SegOpt {
    SegOpt { label, enabled: true, mono: true }
}

#[derive(Clone, Copy)]
pub struct Seg {
    pub fill: Option<f32>,
    pub dot: bool,
    /// Résolutions : sans pastille, option choisie en orange.
    pub res: bool,
}

impl Seg {
    pub const D: Seg = Seg { fill: None, dot: true, res: false };
}

pub struct Slide {
    pub v: f32,
    pub changed: bool,
    pub done: bool,
}

#[derive(Clone, Copy)]
pub enum Note {
    Warn,
    Info,
    Err,
}

/// Paragraphe mesuré sans dessin (hauteur d'un texte balisé).
pub fn para_h(c: &mut Canvas, w: f32, lh: f32, spans: &[Span]) -> f32 {
    c.para(0.0, 0.0, w, lh, spans, false)
}
