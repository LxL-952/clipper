//! Moteur de dessin de l'interface : rendu logiciel (tiny-skia) dans un tampon BGRA prémultiplié, celui que
//! Windows affiche tel quel (aucune copie, aucune conversion). Texte : polices Geist embarquées, rastérisées
//! par fontdue et gardées en cache par glyphe. Coordonnées en pixels logiques ; `s` = mise à l'échelle de l'écran.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, PixmapRef,
    Point, RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

/// Couleur RVBA non prémultipliée.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Col(pub u8, pub u8, pub u8, pub u8);

pub const fn hex(v: u32) -> Col {
    Col((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
}

impl Col {
    /// Même couleur, opacité multipliée par `a` (0 à 1).
    pub fn a(self, a: f32) -> Col {
        Col(self.0, self.1, self.2, (self.3 as f32 * a.clamp(0.0, 1.0)).round() as u8)
    }
    /// Mélange vers `o` (t = 0 : self, t = 1 : o).
    pub fn mix(self, o: Col, t: f32) -> Col {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)).round() as u8;
        Col(m(self.0, o.0), m(self.1, o.1), m(self.2, o.2), m(self.3, o.3))
    }
    /// Le tampon est en BGRA : rouge et bleu échangés ici, une fois pour toutes.
    fn sk(self) -> Color {
        Color::from_rgba8(self.2, self.1, self.0, self.3)
    }
}

/// Palette de l'interface (variables CSS de l'ancienne page).
pub mod pal {
    use super::{Col, hex};
    pub const GROUND: Col = hex(0x0a0d0a);
    pub const BODY: Col = hex(0x141913);
    pub const PANEL: Col = hex(0x192018);
    pub const WELL: Col = hex(0x0d110c);
    pub const RAISE: Col = hex(0x222a20);
    pub const RAISE_H: Col = hex(0x2a3327);
    pub const LINE: Col = hex(0x2b3429);
    pub const LINE2: Col = hex(0x3c4739);
    pub const TEXT: Col = hex(0xe9ece2);
    pub const TEXT2: Col = hex(0xb3bba9);
    pub const TEXT3: Col = hex(0x8d9786);
    pub const OFF: Col = hex(0x7a8572);
    pub const SIGNAL: Col = hex(0xff7d2e);
    pub const SIGNAL_INK: Col = hex(0x1d0e03);
    pub const SIGNAL_SOFT: Col = Col(255, 125, 46, 41);
    pub const REC: Col = hex(0xff4a3d);
    pub const OK: Col = hex(0x9ed672);
    pub const WARN: Col = hex(0xebc85e);
    pub const BTN_LINE_H: Col = hex(0x4a5646);
    pub const WHITE: Col = hex(0xffffff);
    pub const BLACK: Col = hex(0x000000);
}

/// Rectangle en pixels logiques.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub const fn r(x: f32, y: f32, w: f32, h: f32) -> R {
    R { x, y, w, h }
}

impl R {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn has(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    /// Rétréci de `d` de chaque côté.
    pub fn inset(&self, d: f32) -> R {
        r(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn inter(&self, o: &R) -> R {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        r(x, y, (self.right().min(o.right()) - x).max(0.0), (self.bottom().min(o.bottom()) - y).max(0.0))
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

/// Police : famille (0 = Geist, 1 = Geist Mono), graisse, taille en px, espacement des lettres en em.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct F {
    pub mono: bool,
    pub wt: u16,
    pub px: f32,
    pub ls: f32,
}

impl F {
    pub const fn s(px: f32, wt: u16) -> F {
        F { mono: false, wt, px, ls: 0.0 }
    }
    pub const fn m(px: f32, wt: u16) -> F {
        F { mono: true, wt, px, ls: 0.0 }
    }
    pub const fn ls(self, em: f32) -> F {
        F { ls: em, ..self }
    }
    pub const fn wt(self, wt: u16) -> F {
        F { wt, ..self }
    }
    fn idx(&self) -> usize {
        match (self.mono, self.wt) {
            (false, 0..=449) => 0,
            (false, 450..=549) => 1,
            (false, 550..=649) => 2,
            (false, _) => 3,
            (true, 0..=449) => 4,
            (true, _) => 5,
        }
    }
}

fn fonts() -> &'static [fontdue::Font; 6] {
    static FONTS: OnceLock<[fontdue::Font; 6]> = OnceLock::new();
    FONTS.get_or_init(|| {
        let f = |b: &[u8]| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).expect("police embarquée");
        // Texte : Arial de Windows (lue sur le disque, jamais copiée dans Clipper : sa licence ne le permet pas).
        // Arial n'a que normal et gras : 400/500 -> normal, 600/700 -> gras. Absente : Geist embarquée.
        let sys = |file: &str| {
            let dir = std::env::var_os("WINDIR").map_or(std::path::PathBuf::from(r"C:\Windows"), std::path::PathBuf::from);
            std::fs::read(dir.join("Fonts").join(file)).ok().and_then(|b| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).ok())
        };
        let (reg, bold) = (sys("arial.ttf"), sys("arialbd.ttf"));
        [
            reg.clone().unwrap_or_else(|| f(include_bytes!("../assets/geist-400.ttf"))),
            reg.unwrap_or_else(|| f(include_bytes!("../assets/geist-500.ttf"))),
            bold.clone().unwrap_or_else(|| f(include_bytes!("../assets/geist-600.ttf"))),
            bold.unwrap_or_else(|| f(include_bytes!("../assets/geist-700.ttf"))),
            f(include_bytes!("../assets/geist-mono-400.ttf")),
            f(include_bytes!("../assets/geist-mono-500.ttf")),
        ]
    })
}

struct Glyph {
    w: usize,
    h: usize,
    xmin: i32,
    ymin: i32,
    adv: f32,
    cov: Vec<u8>,
}

thread_local! {
    /// (police, caractère, taille physique en quarts de pixel) -> glyphe rastérisé.
    static GLYPHS: RefCell<HashMap<(u8, char, u32), std::rc::Rc<Glyph>>> = RefCell::new(HashMap::new());
    static ICONS: RefCell<HashMap<&'static str, std::rc::Rc<Vec<(Path, IconKind)>>>> = RefCell::new(HashMap::new());
}

/// Caractère absent de la police (sous-ensemble latin) : même graisse dans l'autre famille, sinon un
/// équivalent simple. Sans cela, « ≈ » ou l'espace fine des milliers s'affichaient comme un carré vide.
fn resolve(fi: usize, ch: char) -> (usize, char) {
    let has = |i: usize, c: char| fonts()[i].lookup_glyph_index(c) != 0;
    if has(fi, ch) {
        return (fi, ch);
    }
    let other = match fi {
        0 | 1 => 4 + fi.min(1),
        2 | 3 => 5,
        4 => 0,
        _ => 1,
    };
    if has(other, ch) {
        return (other, ch);
    }
    let sub = match ch {
        '≈' => '~',
        '\u{202f}' | '\u{a0}' | '\u{2009}' => ' ',
        'ᵉ' => 'e',
        '−' => '-',
        '…' => '.',
        '’' => '\'',
        c => c,
    };
    (fi, sub)
}

fn glyph(fi: usize, ch: char, px: f32) -> std::rc::Rc<Glyph> {
    let (fi, ch) = resolve(fi, ch);
    let key = (fi as u8, ch, (px * 4.0).round() as u32);
    GLYPHS.with(|g| {
        let mut g = g.borrow_mut();
        if g.len() > 6000 {
            g.clear(); // garde-fou mémoire (changement d'échelle, plein écran…)
        }
        g.entry(key)
            .or_insert_with(|| {
                let (m, cov) = fonts()[fi].rasterize(ch, key.2 as f32 / 4.0);
                std::rc::Rc::new(Glyph { w: m.width, h: m.height, xmin: m.xmin, ymin: m.ymin, adv: m.advance_width, cov })
            })
            .clone()
    })
}

/// Largeur d'un texte, en pixels logiques.
pub fn text_w(s: &str, f: F) -> f32 {
    let font = &fonts()[f.idx()];
    let mut w = 0.0;
    for ch in s.chars() {
        w += { let (i, c2) = resolve(f.idx(), ch); let _ = font; fonts()[i].metrics(c2, f.px).advance_width } + f.ls * f.px;
    }
    w
}

/// Morceau de texte d'un paragraphe (gras, chiffres en police fixe…).
#[derive(Clone)]
pub struct Span {
    pub t: String,
    pub f: F,
    pub c: Col,
}

/// Petit balisage des textes de l'interface : `**gras**` et `` `chiffres` `` (police fixe).
/// `bw` / `bc` : graisse et couleur du gras.
pub fn md(text: &str, f: F, c: Col, bw: u16, bc: Col) -> Vec<Span> {
    let mut out = vec![];
    let (mut bold, mut mono) = (false, false);
    let mut cur = String::new();
    let mut it = text.chars().peekable();
    let mut flush = |cur: &mut String, bold: bool, mono: bool, out: &mut Vec<Span>| {
        if !cur.is_empty() {
            let mut ff = if bold { f.wt(bw) } else { f };
            if mono {
                ff = F { mono: true, px: (f.px - 0.5).max(9.0), ..ff };
            }
            out.push(Span { t: std::mem::take(cur), f: ff, c: if bold { bc } else { c } });
        }
    };
    while let Some(ch) = it.next() {
        if ch == '*' && it.peek() == Some(&'*') {
            it.next();
            flush(&mut cur, bold, mono, &mut out);
            bold = !bold;
        } else if ch == '`' {
            flush(&mut cur, bold, mono, &mut out);
            mono = !mono;
        } else {
            cur.push(ch);
        }
    }
    flush(&mut cur, bold, mono, &mut out);
    out
}

#[derive(Clone, Copy, PartialEq)]
pub enum IconKind {
    Stroke(f32),
    Fill,
}

/// Icônes de l'interface (repère 24x24), reprises telles quelles des symboles SVG de l'ancienne page.
/// `R x y l h r` = rectangle arrondi, `O cx cy r` = cercle (ajouts au format des tracés SVG).
fn icon_src(name: &str) -> &'static [(&'static str, IconKind)] {
    use IconKind::{Fill, Stroke as S};
    match name {
        "play" => &[("M8 5.5v13l10.5-6.5z", Fill)],
        "pause" => &[("M8 5.5v13M16 5.5v13", S(3.0))],
        "mute" => &[("M4 9.5h3.5L12 5.5v13l-4.5-4H4z", S(1.6)), ("M16 9.5l5 5M21 9.5l-5 5", S(1.6))],
        "full" => &[("M4 9V4h5M15 4h5v5M20 15v5h-5M9 20H4v-5", S(1.6))],
        "unfull" => &[("M9 4v5H4M20 9h-5V4M15 20v-5h5M4 15h5v5", S(1.6))],
        "clip" => &[("M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3", S(1.6)), ("M10 9.2v5.6l4.6-2.8z", S(1.6))],
        "spk" => &[("M4 9.5h3.5L12 5.5v13l-4.5-4H4z", S(1.6)), ("M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11", S(1.6))],
        "mic" => &[("R9 3 6 11 3", S(1.6)), ("M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21", S(1.6))],
        "micoff" => &[("M15 9.5V6a3 3 0 0 0-5.6-1.5M9 9v2a3 3 0 0 0 4.6 2.5M5.5 11a6.5 6.5 0 0 0 10.6 5M18.5 11a6.4 6.4 0 0 1-.5 2.5M12 17.5V21M4 4l16 16", S(1.6))],
        "folder" => &[("M3.5 7.5a1.5 1.5 0 0 1 1.5-1.5h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 18.5H5A1.5 1.5 0 0 1 3.5 17z", S(1.6))],
        "hdd" => &[("M4 14.5h16M4 14.5l2.5-8h11l2.5 8v4a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z", S(1.6)), ("M16 17h.1", S(1.6))],
        "disk" => &[("R3.5 6 17 12 1.5", S(1.6)), ("M3.5 13.5h17", S(1.6)), ("O16.5 16 .6", S(1.6))],
        "warn" => &[("M12 4 2.8 19.5h18.4z", S(1.6)), ("M12 10v4.5M12 17.2v.1", S(1.6))],
        "info" => &[("O12 12 8.5", S(1.6)), ("M12 11v5.5M12 7.8v.1", S(1.6))],
        "chev" => &[("M9.5 6l6 6-6 6", S(1.6))],
        "min" => &[("M6 12h12", S(1.6))],
        "x" => &[("M6.5 6.5l11 11M17.5 6.5l-11 11", S(1.6))],
        "arrow" => &[("M4 12h15M14 7l5 5-5 5", S(1.6))],
        "dl" => &[("M12 4v11M7.5 10.5 12 15l4.5-4.5M5 19.5h14", S(1.6))],
        "lock" => &[("R5 10.5 14 9.5 1.5", S(1.6)), ("M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5", S(1.6))],
        "screen" => &[("R3 4.5 18 12 1.5", S(1.6)), ("M9 20h6M12 16.5V20", S(1.6))],
        "res" => &[("R3 5 18 13 1.5", S(1.6)), ("M7 9V8h2M17 14v1h-2M9 21h6", S(1.6))],
        "trash" => &[("M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 12.5h9l1-12.5M10.5 11v5M13.5 11v5", S(1.6))],
        "pen" => &[("M4.5 19.5l1-4L15.5 5.5l3 3-10 10z", S(1.6)), ("M13.5 7.5l3 3", S(1.6))],
        "refresh" => &[("M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4v4h-4", S(1.6))],
        "chip" => &[("R6 6 12 12 1.5", S(1.6)), ("M9.5 9.5h5v5h-5zM9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3", S(1.6))],
        "check" => &[("M5 12.5l4.5 4.5L19 7.5", S(1.6))],
        "down" => &[("M6 9l6 6 6-6", S(3.0))],
        // Barre Alt+Z
        "b-clip" => &[("M4 7h3l2-3h6l2 3h3v12H4z", S(1.7)), ("O12 13 3.5", S(1.7))],
        "b-buf" => &[("O12 12 8.5", S(1.7)), ("M12 7.5V12l3 2", S(1.7))],
        "b-res" => &[("R3 5 18 12 2", S(1.7)), ("M8 20h8", S(1.7))],
        "b-fps" => &[("M4 18l5-6 4 3 7-9", S(1.7))],
        "b-aud" => &[("M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2", S(1.7))],
        "b-stats" => &[("R4 4 16 16 2", S(1.7)), ("M8 15v-3M12 15V9M16 15v-5", S(1.7))],
        "b-set" => &[("O12 12 3", S(1.7)), ("M12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8M16.6 16.6l1.8 1.8M5.6 18.4l1.8-1.8M16.6 7.4l1.8-1.8", S(1.7))],
        "spk1" => &[("M4 9.5h3.5L12 5.5v13l-4.5-4H4z", S(1.8)), ("M15.5 9a4.5 4.5 0 0 1 0 6", S(1.8))],
        "logo" => &[("M10 8V15A2 2 0 0 0 14 15V6A4 4 0 0 0 6 6V16A6 6 0 0 0 18 16V7", S(2.1))],
        _ => &[],
    }
}

/// Tracé SVG (`d`) -> chemin tiny-skia. Commandes M L H V C S Q A Z (et minuscules), plus R et O.
pub fn svg_path(d: &str) -> Option<Path> {
    let b = d.as_bytes();
    let mut i = 0;
    let mut pb = PathBuilder::new();
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut last_c: Option<(f32, f32)> = None;
    let mut cmd = b' ';
    fn num(b: &[u8], i: &mut usize) -> Option<f32> {
        while *i < b.len() && (b[*i] == b' ' || b[*i] == b',' || b[*i] == b'\n') {
            *i += 1;
        }
        let st = *i;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        while *i < b.len() && (b[*i].is_ascii_digit() || (b[*i] == b'.' && !dot)) {
            dot |= b[*i] == b'.';
            *i += 1;
        }
        std::str::from_utf8(&b[st..*i]).ok()?.parse().ok()
    }
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b',' || b[i] == b'\n') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
        } else if cmd == b'M' {
            cmd = b'L'; // paires après un M : des segments de droite
        } else if cmd == b'm' {
            cmd = b'l';
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let (x, y) = (num(b, &mut i)? + ox, num(b, &mut i)? + oy);
                pb.move_to(x, y);
                (cx, cy, sx, sy) = (x, y, x, y);
            }
            b'L' => {
                let (x, y) = (num(b, &mut i)? + ox, num(b, &mut i)? + oy);
                pb.line_to(x, y);
                (cx, cy) = (x, y);
            }
            b'H' => {
                cx = num(b, &mut i)? + ox;
                pb.line_to(cx, cy);
            }
            b'V' => {
                cy = num(b, &mut i)? + oy;
                pb.line_to(cx, cy);
            }
            b'C' => {
                let v: Vec<f32> = (0..6).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                pb.cubic_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy, v[4] + ox, v[5] + oy);
                last_c = Some((v[2] + ox, v[3] + oy));
                (cx, cy) = (v[4] + ox, v[5] + oy);
                continue;
            }
            b'S' => {
                let v: Vec<f32> = (0..4).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                let (x1, y1) = last_c.map_or((cx, cy), |(lx, ly)| (2.0 * cx - lx, 2.0 * cy - ly));
                pb.cubic_to(x1, y1, v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                last_c = Some((v[0] + ox, v[1] + oy));
                (cx, cy) = (v[2] + ox, v[3] + oy);
                continue;
            }
            b'Q' => {
                let v: Vec<f32> = (0..4).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                pb.quad_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                (cx, cy) = (v[2] + ox, v[3] + oy);
            }
            b'A' => {
                let v: Vec<f32> = (0..7).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                let (x, y) = (v[5] + ox, v[6] + oy);
                arc_to(&mut pb, (cx, cy), v[0], v[1], v[2], v[3] != 0.0, v[4] != 0.0, (x, y));
                (cx, cy) = (x, y);
            }
            b'Z' => {
                pb.close();
                (cx, cy) = (sx, sy);
            }
            b'R' => {
                let v: Vec<f32> = (0..5).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                push_rrect(&mut pb, r(v[0], v[1], v[2], v[3]), v[4]);
            }
            b'O' => {
                let v: Vec<f32> = (0..3).map(|_| num(b, &mut i)).collect::<Option<_>>()?;
                pb.push_circle(v[0], v[1], v[2]);
            }
            _ => return None,
        }
        last_c = None;
    }
    pb.finish()
}

/// Arc elliptique SVG (extrémités + rayons) converti en courbes de Bézier.
fn arc_to(pb: &mut PathBuilder, (x1, y1): (f32, f32), mut rx: f32, mut ry: f32, rot: f32, large: bool, sweep: bool, (x2, y2): (f32, f32)) {
    if rx == 0.0 || ry == 0.0 {
        return pb.line_to(x2, y2);
    }
    let phi = rot.to_radians();
    let (sp, cp) = phi.sin_cos();
    let (dx, dy) = ((x1 - x2) / 2.0, (y1 - y2) / 2.0);
    let (x1p, y1p) = (cp * dx + sp * dy, -sp * dx + cp * dy);
    let lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lam > 1.0 {
        rx *= lam.sqrt();
        ry *= lam.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let k = if den == 0.0 { 0.0 } else { (num / den).sqrt() * if large == sweep { -1.0 } else { 1.0 } };
    let (cxp, cyp) = (k * rx * y1p / ry, -k * ry * x1p / rx);
    let (cx, cy) = (cp * cxp - sp * cyp + (x1 + x2) / 2.0, sp * cxp + cp * cyp + (y1 + y2) / 2.0);
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry);
    if !sweep && dt > 0.0 {
        dt -= std::f32::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f32::consts::TAU;
    }
    let n = (dt.abs() / (std::f32::consts::FRAC_PI_2 + 0.001)).ceil().max(1.0) as usize;
    let d = dt / n as f32;
    let t = 4.0 / 3.0 * (d / 4.0).tan();
    let pt = |a: f32| -> (f32, f32, f32, f32) {
        let (s, c) = a.sin_cos();
        (cx + rx * c * cp - ry * s * sp, cy + rx * c * sp + ry * s * cp, -rx * s * cp - ry * c * sp, -rx * s * sp + ry * c * cp)
    };
    let mut a = t1;
    for _ in 0..n {
        let (px, py, dx1, dy1) = pt(a);
        let (qx, qy, dx2, dy2) = pt(a + d);
        pb.cubic_to(px + t * dx1, py + t * dy1, qx - t * dx2, qy - t * dy2, qx, qy);
        a += d;
    }
}

fn push_rrect(pb: &mut PathBuilder, q: R, rad: f32) {
    let rad = rad.min(q.w / 2.0).min(q.h / 2.0).max(0.0);
    if rad <= 0.01 {
        if let Some(rc) = tiny_skia::Rect::from_xywh(q.x, q.y, q.w, q.h) {
            pb.push_rect(rc);
        }
        return;
    }
    let k = rad * 0.5523;
    let (x0, y0, x1, y1) = (q.x, q.y, q.right(), q.bottom());
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - rad + k, y0, x1, y0 + rad - k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - rad + k, x1 - rad + k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + rad - k, y1, x0, y1 - rad + k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + rad - k, x0 + rad - k, y0, x0 + rad, y0);
    pb.close();
}

pub struct Canvas {
    pub pm: Pixmap,
    /// Mise à l'échelle de l'écran (1.0, 1.25, 1.5…).
    pub s: f32,
    /// Décalage du contenu (défilement).
    pub ox: f32,
    pub oy: f32,
    /// Découpe en cours, en pixels physiques : (x0, y0, x1, y1).
    clip: (i32, i32, i32, i32),
    mask: Mask,
    masked: bool,
}

impl Canvas {
    pub fn new(w: u32, h: u32, s: f32) -> Canvas {
        let (w, h) = (w.max(1), h.max(1));
        Canvas { pm: Pixmap::new(w, h).expect("tampon"), s, ox: 0.0, oy: 0.0, clip: (0, 0, w as i32, h as i32), mask: Mask::new(w, h).expect("masque"), masked: false }
    }

    /// Taille logique du tampon.
    pub fn size(&self) -> (f32, f32) {
        (self.pm.width() as f32 / self.s, self.pm.height() as f32 / self.s)
    }

    pub fn clear(&mut self, c: Col) {
        self.pm.fill(c.sk());
    }

    fn tf(&self) -> Transform {
        Transform::from_row(self.s, 0.0, 0.0, self.s, self.ox * self.s, self.oy * self.s)
    }

    /// Où tombe `q` par rapport à la découpe : 0 = dehors (rien à dessiner), 1 = entièrement dedans
    /// (dessin direct, sans masque : le chemin rapide de tiny-skia), 2 = à cheval (masque nécessaire).
    /// La plupart des éléments d'une zone défilante sont en 0 ou en 1 : c'est ce qui rend le dessin rapide.
    fn vis(&self, q: R) -> u8 {
        let s = self.s;
        let (x0, y0, x1, y1) = ((q.x + self.ox) * s - 1.0, (q.y + self.oy) * s - 1.0, (q.right() + self.ox) * s + 1.0, (q.bottom() + self.oy) * s + 1.0);
        let (cx0, cy0, cx1, cy1) = (self.clip.0 as f32, self.clip.1 as f32, self.clip.2 as f32, self.clip.3 as f32);
        if x1 <= cx0 || y1 <= cy0 || x0 >= cx1 || y0 >= cy1 {
            0
        } else if x0 >= cx0 && y0 >= cy0 && x1 <= cx1 && y1 <= cy1 {
            1
        } else {
            2
        }
    }

    /// Même question pour un tracé, une fois sa transformation appliquée (`pad` : demi-épaisseur du trait).
    fn vis_path(&self, path: &Path, extra: Transform, pad: f32) -> u8 {
        match path.bounds().transform(extra) {
            Some(b) => self.vis(r(b.x() - pad, b.y() - pad, b.width() + 2.0 * pad, b.height() + 2.0 * pad)),
            None => 2,
        }
    }

    fn m(&self) -> Option<&Mask> {
        self.masked.then_some(&self.mask)
    }

    /// Aligne un rectangle sur les pixels de l'écran (filets nets).
    fn snap(&self, q: R) -> R {
        let f = |v: f32| (v * self.s).round() / self.s;
        let (x, y) = (f(q.x + self.ox) - self.ox, f(q.y + self.oy) - self.oy);
        r(x, y, f(q.x + self.ox + q.w) - self.ox - x, f(q.y + self.oy + q.h) - self.oy - y)
    }

    /// Découpe rectangulaire (None = tout le tampon). Renvoie l'ancienne, à rendre à `restore_clip`.
    pub fn set_clip(&mut self, q: Option<R>) -> (i32, i32, i32, i32) {
        let old = self.clip;
        let (w, h) = (self.pm.width() as i32, self.pm.height() as i32);
        self.clip = match q {
            None => (0, 0, w, h),
            Some(q) => {
                let f = |v: f32| (v * self.s).round() as i32;
                (f(q.x + self.ox).clamp(0, w), f(q.y + self.oy).clamp(0, h), f(q.right() + self.ox).clamp(0, w), f(q.bottom() + self.oy).clamp(0, h))
            }
        };
        self.fill_mask();
        old
    }

    pub fn restore_clip(&mut self, c: (i32, i32, i32, i32)) {
        self.clip = c;
        self.fill_mask();
    }

    /// Découpe en cours, en pixels logiques (sans le décalage de défilement).
    pub fn clip_rect(&self) -> R {
        let (x0, y0, x1, y1) = self.clip;
        r(x0 as f32 / self.s - self.ox, y0 as f32 / self.s - self.oy, (x1 - x0) as f32 / self.s, (y1 - y0) as f32 / self.s)
    }

    fn fill_mask(&mut self) {
        let (w, h) = (self.pm.width() as i32, self.pm.height() as i32);
        let (x0, y0, x1, y1) = self.clip;
        self.masked = (x0, y0, x1, y1) != (0, 0, w, h);
        if !self.masked {
            return;
        }
        let d = self.mask.data_mut();
        d.fill(0);
        for y in y0..y1 {
            d[(y * w + x0) as usize..(y * w + x1) as usize].fill(255);
        }
    }

    fn paint(c: Col) -> Paint<'static> {
        let mut p = Paint::default();
        p.shader = Shader::SolidColor(c.sk());
        p.anti_alias = true;
        p
    }

    /// Rectangle plein, coins arrondis de rayon `rad`.
    pub fn rrect(&mut self, q: R, rad: f32, c: Col) {
        if c.3 == 0 || q.w <= 0.0 || q.h <= 0.0 {
            return;
        }
        let q = self.snap(q);
        let vis = self.vis(q);
        if vis == 0 {
            return;
        }
        let tf = self.tf();
        if rad <= 0.01 {
            if let Some(rc) = tiny_skia::Rect::from_xywh(q.x, q.y, q.w, q.h) {
                let mut p = Self::paint(c);
                p.anti_alias = false;
                let m = (vis == 2 && self.masked).then_some(&self.mask);
                self.pm.fill_rect(rc, &p, tf, m);
            }
            return;
        }
        let mut pb = PathBuilder::new();
        push_rrect(&mut pb, q, rad);
        if let Some(path) = pb.finish() {
            let m = (vis == 2 && self.masked).then_some(&self.mask);
            self.pm.fill_path(&path, &Self::paint(c), FillRule::Winding, tf, m);
        }
    }

    pub fn rect(&mut self, q: R, c: Col) {
        self.rrect(q, 0.0, c);
    }

    /// Bordure intérieure d'épaisseur `w` (comme `border` en CSS avec box-sizing: border-box).
    pub fn border(&mut self, q: R, rad: f32, w: f32, c: Col) {
        if c.3 == 0 || q.w <= 0.0 || q.h <= 0.0 {
            return;
        }
        let vis = self.vis(q);
        if vis == 0 {
            return;
        }
        let q = self.snap(q).inset(w / 2.0);
        let mut pb = PathBuilder::new();
        push_rrect(&mut pb, q, (rad - w / 2.0).max(0.0));
        if let Some(path) = pb.finish() {
            let st = Stroke { width: w, ..Default::default() };
            let (tf, m) = (self.tf(), (vis == 2 && self.masked).then_some(&self.mask));
            self.pm.stroke_path(&path, &Self::paint(c), &st, tf, m);
        }
    }

    /// Contour extérieur (comme `box-shadow: 0 0 0 w couleur`).
    pub fn outline(&mut self, q: R, rad: f32, w: f32, c: Col) {
        self.border(r(q.x - w, q.y - w, q.w + 2.0 * w, q.h + 2.0 * w), rad + w, w, c);
    }

    /// Dégradé linéaire dans un rectangle arrondi : `vertical` ou horizontal, arrêts (position 0 à 1, couleur).
    pub fn grad(&mut self, q: R, rad: f32, vertical: bool, stops: &[(f32, Col)]) {
        let q = self.snap(q);
        let vis = self.vis(q);
        if vis == 0 {
            return;
        }
        let (a, b) = if vertical { (Point::from_xy(q.x, q.y), Point::from_xy(q.x, q.bottom())) } else { (Point::from_xy(q.x, q.y), Point::from_xy(q.right(), q.y)) };
        let st = stops.iter().map(|(p, c)| GradientStop::new(*p, c.sk())).collect();
        let Some(sh) = LinearGradient::new(a, b, st, SpreadMode::Pad, Transform::identity()) else { return };
        let mut p = Paint::default();
        p.shader = sh;
        p.anti_alias = true;
        let mut pb = PathBuilder::new();
        push_rrect(&mut pb, q, rad);
        if let Some(path) = pb.finish() {
            let (tf, m) = (self.tf(), (vis == 2 && self.masked).then_some(&self.mask));
            self.pm.fill_path(&path, &p, FillRule::Winding, tf, m);
        }
    }

    /// Halo rond (centre opaque, bord transparent).
    pub fn glow(&mut self, cx: f32, cy: f32, rad: f32, c: Col) {
        let vis = self.vis(r(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad));
        if vis == 0 {
            return;
        }
        let st = vec![GradientStop::new(0.0, c.sk()), GradientStop::new(1.0, c.a(0.0).sk())];
        let Some(sh) = RadialGradient::new(Point::from_xy(cx, cy), 0.0, Point::from_xy(cx, cy), rad, st, SpreadMode::Pad, Transform::identity()) else { return };
        let mut p = Paint::default();
        p.shader = sh;
        p.anti_alias = true;
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, rad);
        if let Some(path) = pb.finish() {
            let (tf, m) = (self.tf(), (vis == 2 && self.masked).then_some(&self.mask));
            self.pm.fill_path(&path, &p, FillRule::Winding, tf, m);
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, rad: f32, c: Col) {
        let vis = self.vis(r(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad));
        if vis == 0 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, rad);
        if let Some(path) = pb.finish() {
            let (tf, m) = (self.tf(), (vis == 2 && self.masked).then_some(&self.mask));
            self.pm.fill_path(&path, &Self::paint(c), FillRule::Winding, tf, m);
        }
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: Col) {
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        pb.line_to(x1, y1);
        if let Some(path) = pb.finish() {
            self.stroke(&path, w, c, Transform::identity());
        }
    }

    /// Arc de cercle (angles en radians, sens horaire depuis l'axe x) : roue d'attente.
    pub fn arc(&mut self, cx: f32, cy: f32, rad: f32, a0: f32, a1: f32, w: f32, c: Col) {
        let mut pb = PathBuilder::new();
        let n = 24;
        for i in 0..=n {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            let (x, y) = (cx + rad * a.cos(), cy + rad * a.sin());
            if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
        }
        if let Some(path) = pb.finish() {
            self.stroke(&path, w, c, Transform::identity());
        }
    }

    /// Ombre portée approchée : couches translucides de plus en plus larges (pas de flou gaussien).
    /// Ombre portée sous `q`. Seul l'anneau hors de `q` est peint (trou pair-impair) : la carte posée
    /// dessus le cache, et peindre tout l'intérieur coûtait ~30 ms par image pour une grande carte.
    pub fn shadow(&mut self, q: R, rad: f32, dy: f32, blur: f32, c: Col) {
        let n = 6;
        let tf = self.tf();
        for i in 0..n {
            let g = blur * (i as f32 + 1.0) / n as f32;
            let o = self.snap(r(q.x - g, q.y - g + dy, q.w + 2.0 * g, q.h + 2.0 * g));
            let vis = self.vis(o);
            let col = c.a(0.5 / n as f32 * (1.0 + (n - i) as f32 / n as f32));
            if vis == 0 || col.3 == 0 {
                continue;
            }
            let mut pb = PathBuilder::new();
            push_rrect(&mut pb, o, rad + g);
            push_rrect(&mut pb, self.snap(q), rad);
            if let Some(path) = pb.finish() {
                let m = (vis == 2 && self.masked).then_some(&self.mask);
                self.pm.fill_path(&path, &Self::paint(col), FillRule::EvenOdd, tf, m);
            }
        }
    }

    pub fn fill(&mut self, path: &Path, c: Col, extra: Transform) {
        let vis = self.vis_path(path, extra, 0.0);
        if vis == 0 {
            return;
        }
        let (tf, m) = (self.tf().pre_concat(extra), (vis == 2 && self.masked).then_some(&self.mask));
        self.pm.fill_path(path, &Self::paint(c), FillRule::Winding, tf, m);
    }

    pub fn stroke(&mut self, path: &Path, w: f32, c: Col, extra: Transform) {
        let vis = self.vis_path(path, extra, w);
        if vis == 0 {
            return;
        }
        let st = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        let (tf, m) = (self.tf().pre_concat(extra), (vis == 2 && self.masked).then_some(&self.mask));
        self.pm.stroke_path(path, &Self::paint(c), &st, tf, m);
    }

    /// Trait en dégradé (logo).
    pub fn stroke_grad(&mut self, path: &Path, w: f32, a: (f32, f32), b: (f32, f32), stops: &[(f32, Col)], extra: Transform) {
        let vis = self.vis_path(path, extra, w);
        if vis == 0 {
            return;
        }
        let st = stops.iter().map(|(p, c)| GradientStop::new(*p, c.sk())).collect();
        let Some(sh) = LinearGradient::new(Point::from_xy(a.0, a.1), Point::from_xy(b.0, b.1), st, SpreadMode::Pad, Transform::identity()) else { return };
        let mut p = Paint::default();
        p.shader = sh;
        p.anti_alias = true;
        let stroke = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        let (tf, m) = (self.tf().pre_concat(extra), (vis == 2 && self.masked).then_some(&self.mask));
        self.pm.stroke_path(path, &p, &stroke, tf, m);
    }

    /// Icône de la table, dans un carré de côté `size` dont le coin haut-gauche est (x, y).
    pub fn icon(&mut self, name: &'static str, x: f32, y: f32, size: f32, c: Col) {
        self.icon_rot(name, x, y, size, c, 0.0);
    }

    /// Icône tournée de `deg` degrés autour de son centre (chevrons).
    pub fn icon_rot(&mut self, name: &'static str, x: f32, y: f32, size: f32, c: Col, deg: f32) {
        if self.vis(r(x, y, size, size)) == 0 {
            return;
        }
        let parts = ICONS.with(|ic| {
            ic.borrow_mut()
                .entry(name)
                .or_insert_with(|| std::rc::Rc::new(icon_src(name).iter().filter_map(|(d, k)| Some((svg_path(d)?, *k))).collect()))
                .clone()
        });
        let k = size / 24.0;
        let mut tf = Transform::from_row(k, 0.0, 0.0, k, x, y);
        if deg != 0.0 {
            tf = tf.pre_concat(Transform::from_rotate_at(deg, 12.0, 12.0));
        }
        for (path, kind) in parts.iter() {
            match kind {
                IconKind::Fill => self.fill(path, c, tf),
                IconKind::Stroke(w) => self.stroke(path, *w, c, tf),
            }
        }
    }

    /// Ligne de base d'un texte posé dans une boîte de ligne de hauteur `lh` commençant à `top`.
    fn baseline(top: f32, lh: f32, f: F) -> f32 {
        let m = fonts()[f.idx()].horizontal_line_metrics(f.px).map_or((f.px * 0.8, -f.px * 0.2), |m| (m.ascent, m.descent));
        top + (lh - (m.0 - m.1)) / 2.0 + m.0
    }

    /// Texte sur une ligne. `top` + `lh` : boîte de ligne (comme line-height en CSS). Renvoie la largeur.
    pub fn text(&mut self, x: f32, top: f32, lh: f32, s: &str, f: F, c: Col) -> f32 {
        if c.3 == 0 {
            return text_w(s, f);
        }
        let fi = f.idx();
        let sc = self.s;
        let base = ((Self::baseline(top, lh, f) + self.oy) * sc).round() as i32;
        let mut pen = (x + self.ox) * sc;
        let px = f.px * sc;
        let (cx0, cy0, cx1, cy1) = self.clip;
        if base + (px as i32) < cy0 || base - 2 * (px as i32) > cy1 {
            return text_w(s, f);
        }
        let pw = self.pm.width() as i32;
        // Couleur prémultipliée, ordre BGRA du tampon.
        let a = c.3 as u32;
        let (pb, pg, pr) = (c.2 as u32 * a / 255, c.1 as u32 * a / 255, c.0 as u32 * a / 255);
        let data = self.pm.data_mut();
        for ch in s.chars() {
            let g = glyph(fi, ch, px);
            let gx = pen.round() as i32 + g.xmin;
            let gy = base - g.ymin - g.h as i32;
            for row in 0..g.h as i32 {
                let y = gy + row;
                if y < cy0 || y >= cy1 {
                    continue;
                }
                for col in 0..g.w as i32 {
                    let xx = gx + col;
                    if xx < cx0 || xx >= cx1 {
                        continue;
                    }
                    let cov = g.cov[(row as usize) * g.w + col as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let i = ((y * pw + xx) * 4) as usize;
                    let sa = a * cov / 255;
                    let inv = 255 - sa;
                    data[i] = ((pb * cov + data[i] as u32 * inv) / 255) as u8;
                    data[i + 1] = ((pg * cov + data[i + 1] as u32 * inv) / 255) as u8;
                    data[i + 2] = ((pr * cov + data[i + 2] as u32 * inv) / 255) as u8;
                    data[i + 3] = (sa + data[i + 3] as u32 * inv / 255).min(255) as u8;
                }
            }
            pen += g.adv + f.ls * px;
        }
        (pen / sc) - self.ox - x
    }

    /// Texte aligné dans un rectangle : `align` -1 gauche, 0 centre, 1 droite ; centré en hauteur.
    pub fn text_in(&mut self, q: R, align: i8, s: &str, f: F, c: Col) {
        let w = text_w(s, f);
        let x = match align {
            0 => q.x + (q.w - w) / 2.0,
            1 => q.right() - w,
            _ => q.x,
        };
        self.text(x, q.y, q.h, s, f, c);
    }

    /// Texte coupé par « … » s'il dépasse `maxw`.
    pub fn text_cut(&mut self, x: f32, top: f32, lh: f32, maxw: f32, s: &str, f: F, c: Col) {
        if text_w(s, f) <= maxw {
            self.text(x, top, lh, s, f, c);
            return;
        }
        let mut cut = String::new();
        for ch in s.chars() {
            let mut t = cut.clone();
            t.push(ch);
            if text_w(&t, f) + text_w("…", f) > maxw {
                break;
            }
            cut = t;
        }
        cut.push('…');
        self.text(x, top, lh, &cut, f, c);
    }

    /// Paragraphe avec retour à la ligne automatique dans la largeur `w`. `draw` faux : mesure seule.
    /// Renvoie la hauteur occupée.
    pub fn para(&mut self, x: f32, y: f32, w: f32, lh: f32, spans: &[Span], draw: bool) -> f32 {
        // Découpe en mots (les espaces restent collés au mot précédent), « \n » force une ligne.
        let mut words: Vec<(String, F, Col, f32, bool)> = vec![];
        for sp in spans {
            let mut cur = String::new();
            for ch in sp.t.chars() {
                if ch == '\n' {
                    let wd = text_w(&cur, sp.f);
                    words.push((std::mem::take(&mut cur), sp.f, sp.c, wd, true));
                    continue;
                }
                cur.push(ch);
                if ch == ' ' || ch == '-' || ch == '\\' || ch == '/' {
                    let wd = text_w(&cur, sp.f);
                    words.push((std::mem::take(&mut cur), sp.f, sp.c, wd, false));
                }
            }
            if !cur.is_empty() {
                let wd = text_w(&cur, sp.f);
                words.push((cur, sp.f, sp.c, wd, false));
            }
        }
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for (t, f, c, wd, brk) in &words {
            let trimmed = wd - if t.ends_with(' ') { text_w(" ", *f) } else { 0.0 };
            if cx > 0.0 && cx + trimmed > w + 0.5 {
                cx = 0.0;
                cy += lh;
            }
            if draw {
                self.text(x + cx, y + cy, lh, t, *f, *c);
            }
            cx += wd;
            if *brk {
                cx = 0.0;
                cy += lh;
            }
        }
        cy + lh
    }

    /// Copie directe d'une image opaque déjà à la taille d'affichage (image vidéo, miniature préparée) :
    /// lignes recopiées telles quelles, sans filtrage ni masque. `x`, `y` : coin en pixels logiques.
    pub fn blit(&mut self, data: &[u8], w: u32, h: u32, x: f32, y: f32) {
        let (px, py) = (((x + self.ox) * self.s).round() as i32, ((y + self.oy) * self.s).round() as i32);
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, x1) = (px.max(cx0), (px + w as i32).min(cx1));
        if x1 <= x0 || data.len() < (w * h * 4) as usize {
            return;
        }
        let pw = self.pm.width() as i32;
        let dst = self.pm.data_mut();
        for row in 0..h as i32 {
            let yy = py + row;
            if yy < cy0 || yy >= cy1 {
                continue;
            }
            let s0 = ((row * w as i32 + (x0 - px)) * 4) as usize;
            let d0 = ((yy * pw + x0) * 4) as usize;
            let n = ((x1 - x0) * 4) as usize;
            dst[d0..d0 + n].copy_from_slice(&data[s0..s0 + n]);
        }
    }

    /// Copie agrandie ou réduite d'une image opaque (plus proche voisin) dans `dst` : quelques
    /// millisecondes même en plein écran, là où le filtrage de tiny-skia prenait plus de 150 ms par image.
    pub fn blit_scaled(&mut self, data: &[u8], w: u32, h: u32, dst: R) {
        if w == 0 || h == 0 || data.len() < (w * h * 4) as usize {
            return;
        }
        let x0f = (dst.x + self.ox) * self.s;
        let y0f = (dst.y + self.oy) * self.s;
        let (px, py) = (x0f.round() as i32, y0f.round() as i32);
        let (dw, dh) = ((dst.w * self.s).round() as i32, (dst.h * self.s).round() as i32);
        if dw <= 0 || dh <= 0 {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.clip;
        let (x0, x1) = (px.max(cx0), (px + dw).min(cx1));
        let (y0, y1) = (py.max(cy0), (py + dh).min(cy1));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // Colonne source de chaque colonne de destination, calculée une fois.
        let cols: Vec<usize> = (x0..x1).map(|x| (((x - px) as u64 * w as u64 / dw as u64).min(w as u64 - 1)) as usize).collect();
        let pw = self.pm.width() as i32;
        let dst_px = self.pm.data_mut();
        let mut last: Option<(u32, usize)> = None;
        for yy in y0..y1 {
            let sy = (((yy - py) as u64 * h as u64 / dh as u64).min(h as u64 - 1)) as u32;
            let d0 = ((yy * pw + x0) * 4) as usize;
            let n = ((x1 - x0) * 4) as usize;
            // Même ligne source que la précédente : simple recopie de la ligne déjà faite.
            if let Some((ly, ld)) = last.filter(|(ly, _)| *ly == sy) {
                dst_px.copy_within(ld..ld + n, d0);
                let _ = ly;
                continue;
            }
            let row = &data[(sy * w * 4) as usize..((sy + 1) * w * 4) as usize];
            for (i, &c) in cols.iter().enumerate() {
                dst_px[d0 + i * 4..d0 + i * 4 + 4].copy_from_slice(&row[c * 4..c * 4 + 4]);
            }
            last = Some((sy, d0));
        }
    }

    /// Coins arrondis après une copie directe : les quatre coins du rectangle sont repeints en `bg`.
    pub fn corners(&mut self, q: R, rad: f32, bg: Col) {
        let vis = self.vis(q);
        if vis == 0 || rad <= 0.0 {
            return;
        }
        let q = self.snap(q);
        let mut pb = PathBuilder::new();
        if let Some(rc) = tiny_skia::Rect::from_xywh(q.x - 0.5, q.y - 0.5, q.w + 1.0, q.h + 1.0) {
            pb.push_rect(rc);
        }
        push_rrect(&mut pb, q, rad);
        if let Some(path) = pb.finish() {
            let (tf, m) = (self.tf(), (vis == 2 && self.masked).then_some(&self.mask));
            self.pm.fill_path(&path, &Self::paint(bg), FillRule::EvenOdd, tf, m);
        }
    }

    /// Image (tampon BGRA prémultiplié) dans `dst`, coins arrondis ; `cover` : recadrée pour remplir.
    pub fn image(&mut self, img: PixmapRef, dst: R, rad: f32, cover: bool) {
        if self.vis(dst) == 0 {
            return;
        }
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        if iw < 1.0 || ih < 1.0 || dst.w < 1.0 || dst.h < 1.0 {
            return;
        }
        let k = if cover { (dst.w / iw).max(dst.h / ih) } else { (dst.w / iw).min(dst.h / ih) };
        let (dw, dh) = (iw * k, ih * k);
        let (dx, dy) = (dst.x + (dst.w - dw) / 2.0, dst.y + (dst.h - dh) / 2.0);
        // Masque = rectangle arrondi de destination, limité à la découpe en cours.
        let old = self.clip;
        let tf = self.tf();
        let (pw, ph) = (self.pm.width() as i32, self.pm.height() as i32);
        self.mask.data_mut().fill(0);
        let mut pb = PathBuilder::new();
        push_rrect(&mut pb, dst, rad);
        if let Some(path) = pb.finish() {
            self.mask.fill_path(&path, FillRule::Winding, true, tf);
        }
        let d = self.mask.data_mut();
        for y in 0..ph {
            if y < old.1 || y >= old.3 {
                d[(y * pw) as usize..((y + 1) * pw) as usize].fill(0);
            } else {
                d[(y * pw) as usize..(y * pw + old.0) as usize].fill(0);
                d[(y * pw + old.2) as usize..((y + 1) * pw) as usize].fill(0);
            }
        }
        let paint = PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() };
        let itf = Transform::from_row(k * self.s, 0.0, 0.0, k * self.s, (dx + self.ox) * self.s, (dy + self.oy) * self.s);
        self.pm.draw_pixmap(0, 0, img, &paint, itf, Some(&self.mask));
        self.restore_clip(old);
    }
}

/// Réduit `src` à exactement `w` x `h` pixels, recadrée pour remplir : fait une fois par miniature, qui
/// se recopie ensuite telle quelle à chaque dessin.
pub fn fit_cover(src: &Pixmap, w: u32, h: u32) -> Option<Pixmap> {
    let mut out = Pixmap::new(w.max(1), h.max(1))?;
    let k = (w as f32 / src.width() as f32).max(h as f32 / src.height() as f32);
    let (dx, dy) = ((w as f32 - src.width() as f32 * k) / 2.0, (h as f32 - src.height() as f32 * k) / 2.0);
    let paint = PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() };
    out.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, dx, dy), None);
    Some(out)
}

/// Trombone du logo, en dégradé orange (repère 24x24 mis à l'échelle `size`).
pub fn logo(c: &mut Canvas, x: f32, y: f32, size: f32, alpha: f32, extra: Transform) {
    let Some(p) = svg_path("M10 8V15A2 2 0 0 0 14 15V6A4 4 0 0 0 6 6V16A6 6 0 0 0 18 16V7") else { return };
    let k = size / 24.0;
    let tf = extra.pre_concat(Transform::from_row(k, 0.0, 0.0, k, x, y));
    c.stroke_grad(&p, 2.1, (6.0, 2.0), (18.0, 22.0), &[(0.0, hex(0xffb46a).a(alpha)), (0.55, hex(0xff7d2e).a(alpha)), (1.0, hex(0xc44a06).a(alpha))], tf);
}

/// Paysage de l'aperçu (ciel, soleil, quatre lignes de crêtes), repère 1600x1000 recadré dans `dst`.
pub fn scene(c: &mut Canvas, dst: R, rad: f32) {
    if c.vis(dst) == 0 {
        return;
    }
    let old = c.set_clip(Some(dst.inter(&c.clip_rect())));
    let k = (dst.w / 1600.0).max(dst.h / 1000.0);
    let (ox, oy) = (dst.x + (dst.w - 1600.0 * k) / 2.0, dst.y + (dst.h - 1000.0 * k) / 2.0);
    c.grad(r(ox, oy, 1600.0 * k, 1000.0 * k), 0.0, true, &[(0.0, hex(0x121c1d)), (0.55, hex(0x2a3330)), (0.78, hex(0x4b3d32)), (1.0, hex(0x4b3d32))]);
    c.glow(ox + 1120.0 * k, oy + 610.0 * k, 170.0 * k, hex(0xe2a36c).a(0.9));
    c.circle(ox + 1120.0 * k, oy + 610.0 * k, 34.0 * k, hex(0xe8b07a).a(0.8));
    let tf = Transform::from_row(k, 0.0, 0.0, k, ox, oy);
    for (d, col) in [
        ("M0 640 L180 560 L320 610 L470 520 L640 600 L790 540 L960 620 L1130 560 L1300 630 L1450 570 L1600 610 V1000 H0Z", 0x26302c),
        ("M0 700 L140 650 L300 690 L420 640 L600 700 L760 660 L900 720 L1080 670 L1240 720 L1420 680 L1600 710 V1000 H0Z", 0x1b2421),
        ("M0 790 L220 740 L380 770 L560 730 L760 800 L980 760 L1160 800 L1380 770 L1600 800 V1000 H0Z", 0x111816),
        ("M0 900 L300 860 L700 880 L1100 850 L1600 890 V1000 H0Z", 0x0b100e),
    ] {
        if let Some(p) = svg_path(d) {
            c.fill(&p, hex(col), tf);
        }
    }
    c.restore_clip(old);
    let _ = rad;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_path_parses() {
        for name in ["play", "pause", "mute", "full", "unfull", "clip", "spk", "mic", "micoff", "folder", "hdd", "disk", "warn", "info", "chev", "min", "x", "arrow", "dl", "lock", "screen", "res", "trash", "pen", "refresh", "chip", "check", "down", "b-clip", "b-buf", "b-res", "b-fps", "b-aud", "b-stats", "b-set", "spk1", "logo"] {
            let src = icon_src(name);
            assert!(!src.is_empty(), "{name} absente");
            for (d, _) in src {
                assert!(svg_path(d).is_some(), "{name} : tracé illisible {d}");
            }
        }
    }

    #[test]
    fn arc_ends_where_svg_says() {
        let p = svg_path("M10 8V15A2 2 0 0 0 14 15V6").unwrap();
        let b = p.bounds();
        assert!((b.bottom() - 17.0).abs() < 0.05, "bas de l'arc {}", b.bottom());
        assert!((b.right() - 14.0).abs() < 0.05);
    }

    #[test]
    fn text_is_drawn_and_wrapped() {
        let mut c = Canvas::new(200, 100, 1.0);
        c.clear(pal::BODY);
        let w = c.text(4.0, 4.0, 20.0, "Clipper 123", F::s(14.0, 500), pal::TEXT);
        assert!(w > 50.0 && w < 110.0, "largeur {w}");
        assert!(c.pm.data().chunks(4).any(|p| p[1] > 150), "aucun pixel clair : texte non dessiné");
        let spans = md("un **deux** trois quatre cinq six sept huit neuf dix onze douze", F::s(13.0, 400), pal::TEXT2, 600, pal::TEXT);
        assert_eq!(spans.len(), 3);
        let h = c.para(0.0, 0.0, 120.0, 18.0, &spans, false);
        assert!(h >= 54.0, "pas de retour à la ligne : {h}");
    }

    #[test]
    fn colors_are_stored_bgra() {
        let mut c = Canvas::new(4, 4, 1.0);
        c.rect(r(0.0, 0.0, 4.0, 4.0), hex(0xff7d2e));
        assert_eq!(&c.pm.data()[..4], &[0x2e, 0x7d, 0xff, 0xff]);
    }
}
