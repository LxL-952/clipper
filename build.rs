//! Logo : trombone orange brillant dessiné ici (pas de fichier image dans le dépôt), puis
//! - clipper.ico à côté de l'exe (pour l'installateur),
//! - ressource d'icône n° 1 liée dans l'exe (Explorateur, barre des tâches, zone de notification).
//! link.exe accepte un .res tel quel : pas besoin de rc.exe ni du SDK Windows.
use std::path::PathBuf;

// Pas de 256 px : +260 Ko pour un affichage que Windows obtient en agrandissant 64 px.
const SIZES: [usize; 7] = [16, 20, 24, 32, 40, 48, 64];
/// Orange de l'interface (--signal), du creux au reflet.
const DARK: [f32; 3] = [196.0, 74.0, 6.0];
const LIGHT: [f32; 3] = [255.0, 168.0, 88.0];
/// Demi-épaisseur du fil, repère 24x24.
const HW: f32 = 1.05;

/// Fil du trombone (repère 24x24, même tracé que le SVG de l'interface) :
/// M10 8 V15 A2 2 0 0 0 14 15 V6 A4 4 0 0 0 6 6 V16 A6 6 0 0 0 18 16 V7
enum Part {
    /// Branche verticale x, de y0 à y1.
    Leg(f32, f32, f32),
    /// Demi-cercle de centre (cx, cy) et rayon r, vers le bas (true) ou le haut.
    Bend(f32, f32, f32, bool),
}

const WIRE: [Part; 7] = [
    Part::Leg(10.0, 8.0, 15.0),
    Part::Bend(12.0, 15.0, 2.0, true),
    Part::Leg(14.0, 6.0, 15.0),
    Part::Bend(10.0, 6.0, 4.0, false),
    Part::Leg(6.0, 6.0, 16.0),
    Part::Bend(12.0, 16.0, 6.0, true),
    Part::Leg(18.0, 7.0, 16.0),
];

/// Point du fil le plus proche de (x, y).
fn nearest(x: f32, y: f32) -> (f32, f32) {
    let mut best = (f32::MAX, (0.0, 0.0));
    for p in &WIRE {
        let q = match *p {
            Part::Leg(lx, y0, y1) => (lx, y.clamp(y0, y1)),
            Part::Bend(cx, cy, r, down) if (y >= cy) == down => {
                let d = (x - cx).hypot(y - cy).max(1e-6);
                (cx + r * (x - cx) / d, cy + r * (y - cy) / d)
            }
            Part::Bend(cx, cy, r, _) => (if x < cx { cx - r } else { cx + r }, cy),
        };
        let d = (x - q.0).hypot(y - q.1);
        if d < best.0 {
            best = (d, q);
        }
    }
    best.1
}

/// Couleur d'un point : le fil est éclairé comme un tube de métal (lumière en haut à gauche).
fn shade(x: f32, y: f32) -> Option<[f32; 3]> {
    let (qx, qy) = nearest(x, y);
    let d = (x - qx).hypot(y - qy);
    if d > HW {
        return None;
    }
    // Normale du tube : composante dans le plan (vers le bord) et composante vers nous.
    let t = d / HW;
    let (nx, ny) = if d > 1e-6 { ((x - qx) / d * t, (y - qy) / d * t) } else { (0.0, 0.0) };
    let nz = (1.0 - t * t).sqrt();
    let lit = (nx * -0.45 + ny * -0.55 + nz * 0.70).max(0.0);
    let spec = lit.powi(20) * 0.85;
    Some([0, 1, 2].map(|i| {
        let c = DARK[i] + (LIGHT[i] - DARK[i]) * lit;
        c + (255.0 - c) * spec
    }))
}

/// RGBA non prémultiplié, anticrénelé par 4x4 sous-échantillons.
fn draw(n: usize) -> Vec<u8> {
    let mut px = Vec::with_capacity(n * n * 4);
    let s = 24.0 / n as f32;
    for py in 0..n {
        for pxi in 0..n {
            let (mut acc, mut cov) = ([0f32; 3], 0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (pxi as f32 + (sx as f32 + 0.5) / 4.0) * s;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * s;
                    if let Some(c) = shade(x, y) {
                        (0..3).for_each(|i| acc[i] += c[i]);
                        cov += 1.0;
                    }
                }
            }
            if cov == 0.0 {
                px.extend([0, 0, 0, 0]);
            } else {
                px.extend(acc.map(|v| (v / cov).round() as u8));
                px.push((cov / 16.0 * 255.0).round() as u8);
            }
        }
    }
    px
}

/// Image d'icône au format BMP (DIB 32 bits, de bas en haut, suivie du masque ET vide).
fn dib(n: usize, rgba: &[u8]) -> Vec<u8> {
    let mask_row = n.div_ceil(32) * 4;
    let mut d = Vec::new();
    for v in [40u32, n as u32, 2 * n as u32] {
        d.extend(v.to_le_bytes());
    }
    d.extend(1u16.to_le_bytes());
    d.extend(32u16.to_le_bytes());
    d.extend([0u8; 24]); // compression, taille, résolutions, couleurs : 0
    for y in (0..n).rev() {
        for p in rgba[y * n * 4..(y + 1) * n * 4].chunks(4) {
            d.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    d.extend(vec![0u8; mask_row * n]);
    d
}

/// Entrée de répertoire commune à .ico (offset u32) et RT_GROUP_ICON (id u16).
fn dir_entry(n: usize, len: usize) -> Vec<u8> {
    let b = if n >= 256 { 0 } else { n as u8 };
    let mut e = vec![b, b, 0, 0];
    e.extend(1u16.to_le_bytes());
    e.extend(32u16.to_le_bytes());
    e.extend((len as u32).to_le_bytes());
    e
}

/// Une ressource numérique dans un .res (en-tête de 32 octets, données alignées sur 4).
fn res_entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    for v in [0xFFFF, kind, 0xFFFF, id] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(0x0409u16.to_le_bytes());
    out.extend([0u8; 8]);
    out.extend(data);
    out.resize(out.len().div_ceil(4) * 4, 0);
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let images: Vec<(usize, Vec<u8>)> = SIZES.iter().map(|&n| (n, dib(n, &draw(n)))).collect();

    let mut ico = vec![0, 0, 1, 0];
    ico.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (n, img) in &images {
        ico.extend(dir_entry(*n, img.len()));
        ico.extend((offset as u32).to_le_bytes());
        offset += img.len();
    }
    images.iter().for_each(|(_, img)| ico.extend(img));

    let mut res = Vec::new();
    res_entry(&mut res, 0, 0, 0, &[]); // en-tête vide obligatoire
    let mut group = vec![0, 0, 1, 0];
    group.extend((images.len() as u16).to_le_bytes());
    for (i, (n, img)) in images.iter().enumerate() {
        res_entry(&mut res, 3, i as u16 + 1, 0x1010, img); // RT_ICON
        group.extend(dir_entry(*n, img.len()));
        group.extend((i as u16 + 1).to_le_bytes());
    }
    res_entry(&mut res, 14, 1, 0x1030, &group); // RT_GROUP_ICON n° 1 = icône de l'exe

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out.join("clipper.res"), res).unwrap();
    // OUT_DIR = <cible>/<profil>/build/clipper-xxx/out : l'icône va à côté de l'exe.
    if let Some(profile) = out.ancestors().nth(3) {
        let _ = std::fs::write(profile.join("clipper.ico"), &ico);
    }
    println!("cargo:rustc-link-arg-bins={}", out.join("clipper.res").display());
}
