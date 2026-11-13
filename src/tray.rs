//! Menu clic droit de la zone de notification, façon QuickCPU : une icône par ligne, températures
//! au survol, clip, REC, nettoyage de la RAM. Icônes dessinées ici (16 px), comme le logo.
use tray_icon::menu::{Icon, IconMenuItem, Menu, PredefinedMenuItem};

pub struct TrayMenu {
    pub menu: Menu,
    pub open: IconMenuItem,
    temps: IconMenuItem,
    pub clip: IconMenuItem,
    pub rec: IconMenuItem,
    pub ram: IconMenuItem,
    pub clipping: IconMenuItem,
    pub quit: IconMenuItem,
    rec_key: String,
}

const ORANGE: [u8; 3] = [255, 125, 46];
const RED: [u8; 3] = [255, 74, 61];
const GREY: [u8; 3] = [200, 205, 195];

/// Icône 16 px : `inside(x, y)` dit si un point (repère 16x16) est dans la forme ; anticrénelage 4x4.
fn glyph(color: [u8; 3], inside: impl Fn(f32, f32) -> bool) -> Option<Icon> {
    let mut px = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let mut cov = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    cov += inside(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) as u32;
                }
            }
            px.extend(color);
            px.push((cov * 255 / 16) as u8);
        }
    }
    Icon::from_rgba(px, 16, 16).ok()
}

fn dist(x: f32, y: f32, cx: f32, cy: f32) -> f32 {
    (x - cx).hypot(y - cy)
}

/// Distance d'un point au segment (ax, ay)-(bx, by) : pour dessiner des traits.
fn seg(x: f32, y: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    dist(x, y, ax + t * dx, ay + t * dy)
}

fn icons() -> [Option<Icon>; 6] {
    [
        // Ouvrir : fenêtre (cadre + barre de titre)
        glyph(GREY, |x, y| {
            let inr = (2.0..14.0).contains(&x) && (3.0..13.0).contains(&y);
            let hole = (3.3..12.7).contains(&x) && (6.0..11.7).contains(&y);
            inr && !hole
        }),
        // Températures : thermomètre
        glyph(ORANGE, |x, y| {
            let bulb = dist(x, y, 8.0, 11.5) < 3.0;
            let stem = (6.6..9.4).contains(&x) && (2.0..11.0).contains(&y) && !((7.4..8.6).contains(&x) && (3.0..8.0).contains(&y));
            bulb || stem || dist(x, y, 8.0, 2.4) < 1.4
        }),
        // Clip : trombone (même tracé que le logo, réduit)
        glyph(ORANGE, |x, y| {
            let (x, y) = (x * 1.5, y * 1.5);
            let w = 1.6;
            seg(x, y, (10.0, 8.0), (10.0, 15.0)) < w
                || (dist(x, y, 12.0, 15.0) - 2.0).abs() < w && y >= 15.0
                || seg(x, y, (14.0, 6.0), (14.0, 15.0)) < w
                || (dist(x, y, 10.0, 6.0) - 4.0).abs() < w && y <= 6.0
                || seg(x, y, (6.0, 6.0), (6.0, 16.0)) < w
                || (dist(x, y, 12.0, 16.0) - 6.0).abs() < w && y >= 16.0
                || seg(x, y, (18.0, 7.0), (18.0, 16.0)) < w
        }),
        // REC : pastille rouge cerclée
        glyph(RED, |x, y| {
            let d = dist(x, y, 8.0, 8.0);
            d < 3.6 || (d > 5.2 && d < 6.4)
        }),
        // RAM : barrette (corps + broches)
        glyph(GREY, |x, y| {
            let body = (1.5..14.5).contains(&x) && (4.0..10.5).contains(&y);
            let chips = [3.5f32, 7.0, 10.5].iter().any(|&c| x >= c && x < c + 2.0 && (5.5..9.0).contains(&y));
            let pins = (2.0..14.0).contains(&x) && (11.5..13.5).contains(&y) && (x.floor() as i32) % 2 == 0;
            (body && !chips) || pins
        }),
        // Quitter : croix
        glyph(GREY, |x, y| seg(x, y, (4.0, 4.0), (12.0, 12.0)) < 1.2 || seg(x, y, (12.0, 4.0), (4.0, 12.0)) < 1.2),
    ]
}

/// « Alt+KeyX » -> « Alt+X » pour l'affichage à droite du menu.
fn key_label(k: &str) -> String {
    k.replace("Key", "").replace("Digit", "").replace("Control", "Ctrl").replace("Shift", "Maj")
}

impl TrayMenu {
    pub fn new(clip_key: &str, rec_key: &str) -> Self {
        let [i_open, i_temp, i_clip, i_rec, i_ram, i_quit] = icons();
        let rec_key = key_label(rec_key);
        let m = Self {
            menu: Menu::new(),
            open: IconMenuItem::new("Ouvrir Clipper", true, i_open, None),
            // Ligne d'information (grisée), mise à jour au survol de l'icône.
            // Texte de départ aussi long que la ligne finale : le menu prend sa largeur à l'ouverture, une
            // valeur arrivée ensuite était coupée (la température du CPU disparaissait).
            temps: IconMenuItem::new(temps_text(), false, i_temp, None),
            clip: IconMenuItem::new(format!("Enregistrer un clip\t{}", key_label(clip_key)), true, i_clip, None),
            rec: IconMenuItem::new(format!("Démarrer le REC\t{rec_key}"), true, i_rec, None),
            ram: IconMenuItem::new("Nettoyer la RAM", true, i_ram, None),
            clipping: IconMenuItem::new("Désactiver le clipping", true, None, None),
            quit: IconMenuItem::new("Quitter Clipper", true, i_quit, None),
            rec_key,
        };
        let sep = || PredefinedMenuItem::separator();
        let _ = m.menu.append_items(&[&m.open, &m.temps, &sep(), &m.clip, &m.rec, &m.clipping, &m.ram, &sep(), &m.quit]);
        m
    }

    /// Au survol de l'icône : dernières températures connues, sans aucune lecture de capteur (instantané).
    pub fn refresh_temps(&self) {
        // Le fil des capteurs se réveille et lit pendant 30 s (menu peut-être ouvert).
        *HOVER.lock().unwrap() = Some(std::time::Instant::now());
        if let Some(t) = SENSOR.get() {
            t.unpark();
        }
        self.temps.set_text(temps_text());
        redraw_open_menu();
    }

    /// Texte de l'interrupteur selon l'état du clipping.
    pub fn set_clipping(&self, on: bool) {
        self.clipping.set_text(if on { "Désactiver le clipping" } else { "Activer le clipping" });
    }

    pub fn set_recording(&self, on: bool) {
        self.rec.set_text(format!("{} le REC\t{}", if on { "Arrêter" } else { "Démarrer" }, self.rec_key));
    }
}

/// Menu déjà affiché : Windows ne repeint pas un texte changé tout seul. On repeint la fenêtre du
/// menu (classe système « #32768 ») pour que les températures bougent sous les yeux.
fn redraw_open_menu() {
    use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let class: Vec<u16> = "#32768".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = FindWindowW(class.as_ptr(), std::ptr::null());
        if !h.is_null() {
            InvalidateRect(h, std::ptr::null(), 1);
        }
    }
}

/// Dernières températures lues (GPU, CPU) : gardées si une lecture échoue, « n/d » seulement si jamais lues.
static TEMPS: std::sync::Mutex<(Option<u32>, Option<f64>)> = std::sync::Mutex::new((None, None));

fn temps_text() -> String {
    let (gpu, cpu) = *TEMPS.lock().unwrap();
    let c = cpu.map_or("   n/d".into(), |t| format!("{t:>3.0} °C"));
    let g = gpu.map_or("   n/d".into(), |t| format!("{t:>3} °C"));
    format!("CPU {c}   ·   GPU {g}")
}

/// Dernier survol de l'icône : les capteurs ne sont lus que dans les 30 s qui suivent.
static HOVER: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
static SENSOR: std::sync::OnceLock<std::thread::Thread> = std::sync::OnceLock::new();

/// Lecture des capteurs en tâche de fond, hors du fil de l'interface (le capteur ACPI peut mettre du temps à
/// répondre) : une fois au lancement, puis toutes les 5 s seulement pendant 30 s après un survol de l'icône
/// (avant : toutes les 5 s en permanence, pour un menu rarement ouvert). Jeu plein écran : aucune lecture.
pub fn start_temp_cache() {
    let h = std::thread::spawn(|| loop {
        let never = TEMPS.lock().unwrap().1.is_none() && TEMPS.lock().unwrap().0.is_none();
        let recent = HOVER.lock().unwrap().is_some_and(|t| t.elapsed().as_secs() < 30);
        if never || (recent && !crate::capture::fullscreen_foreground()) {
            let (gpu, cpu) = crate::overlay::temps();
            let mut t = TEMPS.lock().unwrap();
            t.0 = gpu.or(t.0);
            t.1 = cpu.or(t.1);
        }
        if recent {
            std::thread::sleep(std::time::Duration::from_secs(5));
        } else {
            std::thread::park(); // réveillé par un survol (refresh_temps)
        }
    });
    let _ = SENSOR.set(h.thread().clone());
}
