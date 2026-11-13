//! Fenêtre principale, dessinée nativement (plus de page web). L'état ci-dessous remplace les variables de
//! l'ancienne page ; les échanges avec le reste de Clipper gardent le même format : commandes JSON vers
//! `App::ipc` (file `out`) et messages JSON reçus par `on` (les anciens `window.__on`).
use crate::config::Config;
use crate::gfx::{Canvas, Col, F, R, hex, md, pal, r, text_w};
use crate::player::Player;
use crate::widgets::{Btn, Ui, ease, id, idn};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Clip,
    Audio,
    Videos,
    Drivers,
    Tools,
    /// Statistiques à l'écran (overlay en jeu).
    Stats,
    /// Démarrage, administrateur, puce d'encodage, mode étiré, limites connues.
    Advanced,
}

impl Tab {
    pub fn name(self) -> &'static str {
        match self {
            Tab::Clip => "clip",
            Tab::Audio => "audio",
            Tab::Videos => "videos",
            Tab::Drivers => "drivers",
            Tab::Tools => "tools",
            Tab::Stats => "stats",
            Tab::Advanced => "advanced",
        }
    }
    pub fn parse(s: &str) -> Option<Tab> {
        [Tab::Clip, Tab::Audio, Tab::Stats, Tab::Videos, Tab::Drivers, Tab::Tools, Tab::Advanced].into_iter().find(|t| t.name() == s)
    }
}

pub struct Vid {
    pub path: PathBuf,
    pub name: String,
    pub stem: String,
    pub rec: bool,
    pub bytes: u64,
    pub secs: f64,
    pub date: u64,
    pub audio: usize,
    pub video: bool,
}

pub struct Toast {
    pub kind: String,
    pub text: String,
    pub path: Option<PathBuf>,
    pub t0: Instant,
    pub out: Option<Instant>,
}

#[derive(Default)]
pub struct Drv {
    pub asked: bool,
    /// "", "ok", "update", "error"
    pub status: String,
    pub installed: String,
    pub latest: String,
    pub error: String,
    pub confirm: bool,
    /// "" (rien), "download", "verify", "install", "done"
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub prog_err: String,
}

pub struct CleanItem {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub files: u64,
    pub admin: bool,
    pub warn: String,
    pub on: bool,
}

#[derive(Default)]
pub struct Tools {
    pub scanning: bool,
    pub items: Option<Vec<CleanItem>>,
    pub confirm: bool,
    pub running: bool,
    pub done: Option<String>,
    pub ram_wait: bool,
    pub ram: Option<(u64, u64, u64)>,
}

use crate::config::LIVE;

pub struct Front {
    pub cfg: Config,
    pub tab: Tab,
    pub out: Vec<Value>,
    // données fixes de l'ouverture
    pub encoders: Vec<String>,
    pub native: (u32, u32),
    pub monitors: Vec<(u32, String)>,
    pub outputs: Vec<(String, String)>,
    pub inputs: Vec<(String, String)>,
    pub gpus: Vec<(String, String)>,
    pub ffmpeg_ok: bool,
    pub ff_installing: bool,
    pub cpu_temp: bool,
    /// Cartes NVIDIA mesurables par l'overlay (NVML).
    pub gpu_names: Vec<String>,
    pub gpu_scale: Option<bool>,
    pub elevated: bool,
    pub autostart: bool,
    pub admin_busy: bool,
    // réglages
    pending: Map<String, Value>,
    flush_at: Option<Instant>,
    save_msg: Option<(String, String, Option<Instant>)>,
    pub folds: HashMap<String, bool>,
    // raccourcis
    pub hk: [Option<String>; 5],
    pub conflicts: Vec<String>,
    pub hk_wait: Option<usize>,
    // barre de titre
    pub update: Option<String>,
    pub upd_text: Option<String>,
    pub recording: bool,
    pub rec_t0: Instant,
    // emplacements
    pub free: HashMap<PathBuf, u64>,
    // vidéos
    pub vids: Option<Vec<Vid>>,
    pub v_sel: Option<PathBuf>,
    pub v_pending: Option<PathBuf>,
    pub ren: Option<String>,
    pub del_box: bool,
    pub del_ask: bool,
    pub thumbs: HashMap<String, Option<tiny_skia::Pixmap>>,
    pub player: Player,
    pub p_idle: Instant,
    pub p_mouse: (f32, f32),
    /// Glissement en cours sur la barre de temps du lecteur.
    pub p_scrub: bool,
    /// Aperçu au survol de la barre de temps.
    pub preview: crate::preview::Preview,
    pub fullscreen: bool,
    /// Fenêtre agrandie (bouton carré, F11, double-clic sur la barre de titre), relu à chaque dessin.
    pub maximized: bool,
    /// Bouton survolé et début du survol (info-bulle après 0,5 s), et bulle à dessiner par-dessus tout.
    tip: Option<(u64, Instant)>,
    tip_box: Option<(R, String)>,
    pub ctx: Option<(PathBuf, (f32, f32))>,
    // pilotes, outils
    pub gpu: Option<(String, String)>,
    pub drv: Drv,
    pub tools: Tools,
    // audio
    pub levels: (f32, f32),
    levels_at: Instant,
    // calques
    pub toasts: Vec<Toast>,
    pub intro: Option<Instant>,
    pub closing: Option<Instant>,
    /// Délai avant le prochain dessin utile (compteur REC, notification qui part, clignotement…).
    pub next: Option<Duration>,
    /// Réglages en JSON, faits une fois par image (avant : une conversion complète par contrôle affiché).
    cfg_json: Option<Value>,
    /// Miniatures encore décodables pendant cette image ; le reste attend l'image suivante (défilement fluide).
    thumb_left: u8,
    thumb_more: bool,
    pub t_start: Instant,
}

pub const HK: [(&str, &str, &str); 5] = [
    ("clip", "key_clip", "Enregistrer un clip"), ("rec", "key_rec", "Démarrer / arrêter le REC"), ("bar", "key_bar", "Ouvrir la barre"),
    ("overlay", "key_overlay", "Afficher les statistiques"), ("clipping", "key_clipping", "Activer / désactiver le clipping"),
];

/// « Alt+KeyX » -> « Alt+X ».
pub fn pretty(k: &str) -> String {
    let mut s = String::new();
    for part in k.split('+') {
        if !s.is_empty() {
            s.push('+');
        }
        let p = part.strip_prefix("Key").filter(|x| x.len() == 1).or_else(|| part.strip_prefix("Digit").filter(|x| x.len() == 1)).unwrap_or(part);
        s.push_str(match p {
            "Shift" => "Maj",
            "Control" => "Ctrl",
            x => x,
        });
    }
    s
}

pub fn fr(v: f64, d: usize) -> String {
    format!("{v:.d$}").replace('.', ",")
}

/// 12345678 -> « 12 345 678 » (espaces insécables fines, comme toLocaleString('fr-FR')).
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(ch);
    }
    out
}

pub fn bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1_073_741_824.0 {
        format!("{} Go", fr(b / 1_073_741_824.0, if b >= 1e11 { 0 } else { 1 }))
    } else if b >= 1_048_576.0 {
        format!("{} Mo", thousands((b / 1_048_576.0).round() as u64))
    } else {
        format!("{} ko", ((b / 1024.0).round() as u64).max(1))
    }
}

/// 75 -> « 1:15 », 3700 -> « 1:01:40 ».
pub fn dur(s: f64) -> String {
    let s = s.round().max(0.0) as u64;
    let (h, m, x) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{x:02}") } else { format!("{m}:{x:02}") }
}

/// Date d'un fichier : « 8 nov., 19:12 » (heure locale).
pub fn when(unix: u64) -> String {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = (unix + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut loc): (SYSTEMTIME, SYSTEMTIME) = unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc);
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut loc);
    }
    const M: [&str; 12] = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."];
    format!("{} {}, {:02}:{:02}", loc.wDay, M[(loc.wMonth as usize).clamp(1, 12) - 1], loc.wHour, loc.wMinute)
}

/// Écrit `val` au chemin « a.b » d'un objet JSON. Faux si le chemin n'existe pas.
fn set_path(v: &mut Value, key: &str, val: Value) -> bool {
    let mut slot = v;
    for part in key.split('.') {
        match slot.get_mut(part) {
            Some(s) => slot = s,
            None => return false,
        }
    }
    *slot = val;
    true
}

fn pairs(v: &Value, a: &str, b: &str) -> Vec<(String, String)> {
    v.as_array().into_iter().flatten().map(|x| (x[a].as_str().unwrap_or("").to_string(), x[b].as_str().unwrap_or("").to_string())).collect()
}

impl Front {
    /// `init` : le même objet que l'ancien `window.__INIT__`.
    pub fn new(init: &Value, wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Front {
        let cfg: Config = serde_json::from_value(init["config"].clone()).unwrap_or_default();
        let hk = |k: &str| init["hotkeys"][k].as_str().map(String::from);
        let folds = std::fs::read_to_string(crate::config::dir().join("ui.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let now = Instant::now();
        let mut player = Player::new(&cfg.ffmpeg, wake);
        // Volume du lecteur gardé d'une ouverture à l'autre.
        if let Some(v) = std::fs::read_to_string(crate::config::dir().join("volume.txt")).ok().and_then(|s| s.trim().parse::<f32>().ok()) {
            player.set_volume(v, v == 0.0);
        }
        Front {
            player,
            tab: init["tab"].as_str().and_then(Tab::parse).unwrap_or(Tab::Clip),
            out: vec![],
            encoders: init["encoders"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect(),
            native: (init["native"][0].as_u64().unwrap_or(1920) as u32, init["native"][1].as_u64().unwrap_or(1080) as u32),
            monitors: init["monitors"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| (m["idx"].as_u64().unwrap_or(0) as u32, format!("{} · {}×{}", m["name"].as_str().unwrap_or(""), m["w"], m["h"])))
                .collect(),
            outputs: pairs(&init["outputs"], "id", "name"),
            inputs: pairs(&init["inputs"], "id", "name"),
            gpus: pairs(&init["gpus"], "id", "name"),
            ffmpeg_ok: init["ffmpeg_ok"].as_bool() != Some(false),
            ff_installing: false,
            cpu_temp: init["cpu_temp"].as_bool() != Some(false),
            gpu_names: init["gpu_names"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default(),
            gpu_scale: init["gpu_scale"].as_bool(),
            elevated: init["elevated"].as_bool().unwrap_or(false),
            autostart: init["autostart"].as_bool().unwrap_or(false),
            admin_busy: false,
            pending: Map::new(),
            flush_at: None,
            save_msg: None,
            folds,
            hk: [hk("clip"), hk("rec"), hk("bar"), hk("overlay"), hk("clipping")],
            conflicts: init["hotkey_conflicts"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(pretty)).collect(),
            hk_wait: None,
            update: init["update"].as_str().map(String::from),
            upd_text: None,
            recording: init["recording"].as_bool().unwrap_or(false),
            rec_t0: now - Duration::from_secs(init["rec_secs"].as_u64().unwrap_or(0)),
            free: HashMap::new(),
            vids: None,
            v_sel: None,
            v_pending: None,
            ren: None,
            del_box: false,
            del_ask: false,
            thumbs: HashMap::new(),
            p_idle: now,
            p_mouse: (-1.0, -1.0),
            p_scrub: false,
            preview: Default::default(),
            fullscreen: false,
            maximized: false,
            tip: None,
            tip_box: None,
            ctx: None,
            gpu: init["gpu"]["name"].as_str().map(|n| (n.to_string(), init["gpu"]["driver"].as_str().unwrap_or("").to_string())),
            drv: Drv::default(),
            tools: Tools::default(),
            levels: (0.0, 0.0),
            levels_at: now,
            toasts: vec![],
            intro: cfg.intro.then_some(now),
            closing: None,
            next: None,
            cfg_json: None,
            thumb_left: 2,
            thumb_more: false,
            t_start: now,
            cfg,
        }
    }

    pub fn send(&mut self, v: Value) {
        self.out.push(v);
    }

    /// Change un réglage : affiché tout de suite, envoyé tout de suite s'il ne relance pas la capture,
    /// sinon regroupé pendant 0,7 s (plusieurs clics de suite = une seule relance).
    pub fn set(&mut self, key: &str, value: Value) {
        let mut v = serde_json::to_value(&self.cfg).unwrap_or_default();
        let mut slot = &mut v;
        for part in key.split('.') {
            match slot.get_mut(part) {
                Some(s) => slot = s,
                None => return,
            }
        }
        *slot = value.clone();
        if let Ok(mut c) = serde_json::from_value::<Config>(v) {
            c.rec = c.clip;
            self.cfg = c;
        }
        self.cfg_json = None;
        self.pending.insert(key.to_string(), value);
        if self.pending.keys().all(|k| LIVE.contains(&k.split('.').next().unwrap_or(""))) {
            self.flush();
        } else {
            self.flush_at = Some(Instant::now() + Duration::from_millis(700));
            self.save_msg = Some(("Enregistrement…".into(), "La capture redémarre avec ces réglages.".into(), None));
        }
    }

    pub fn flush(&mut self) {
        self.flush_at = None;
        if self.pending.is_empty() {
            return;
        }
        let restart = self.pending.keys().any(|k| !LIVE.contains(&k.split('.').next().unwrap_or("")));
        let values = Value::Object(std::mem::take(&mut self.pending));
        self.send(json!({"cmd": "set_many", "values": values}));
        self.save_msg = restart.then(|| ("Réglages enregistrés".into(), "Capture relancée.".into(), Some(Instant::now() + Duration::from_millis(1800))));
    }

    pub fn select_tab(&mut self, t: Tab) {
        if self.tab == Tab::Videos && t != Tab::Videos {
            self.player.pause();
        }
        self.tab = t;
        match t {
            Tab::Videos => self.send(json!({"cmd": "videos"})),
            Tab::Drivers if !self.drv.asked => self.check_driver(),
            _ => {}
        }
    }

    pub fn check_driver(&mut self) {
        self.drv = Drv { asked: true, ..Default::default() };
        let b = self.cfg.driver_branch.clone();
        self.send(json!({"cmd": "driver_check", "branch": b}));
    }

    pub fn toast(&mut self, kind: &str, text: &str, path: Option<PathBuf>) {
        self.toasts.push(Toast { kind: kind.into(), text: text.into(), path, t0: Instant::now(), out: None });
    }

    pub fn cur(&self) -> Option<&Vid> {
        let sel = self.v_sel.as_ref()?;
        self.vids.as_ref()?.iter().find(|v| &v.path == sel)
    }

    /// Choisit une vidéo : le lecteur l'ouvre à l'arrêt, sur sa première image.
    pub fn pick(&mut self, path: Option<PathBuf>) {
        self.v_sel = path;
        (self.ren, self.del_box) = (None, false);
        match self.cur().map(|v| (v.path.clone(), v.secs, v.audio, v.video)) {
            Some((p, secs, audio, true)) => self.player.open(&p, secs, audio),
            _ => self.player.close(),
        }
    }

    fn save_folds(&self) {
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(crate::config::dir().join("ui.json"), serde_json::to_vec(&self.folds).unwrap_or_default());
    }

    pub fn fold(&mut self, key: &str) -> bool {
        self.folds.get(key).copied().unwrap_or(false)
    }

    pub fn toggle_fold(&mut self, key: &str) {
        let v = !self.fold(key);
        self.folds.insert(key.to_string(), v);
        self.save_folds();
    }

    /// Message venu du reste de Clipper (mêmes types que les anciens `window.__on`).
    pub fn on(&mut self, m: &Value) {
        let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
        match m["type"].as_str().unwrap_or("") {
            "update" => self.update = m["version"].as_str().map(String::from),
            "config" => {
                if let Ok(mut c) = serde_json::from_value::<Config>(m["config"].clone()) {
                    // Les changements pas encore envoyés restent affichés.
                    if !self.pending.is_empty() {
                        let mut v = serde_json::to_value(&c).unwrap_or_default();
                        for (k, val) in &self.pending {
                            set_path(&mut v, k, val.clone());
                        }
                        c = serde_json::from_value(v).unwrap_or(c);
                    }
                    self.cfg = c;
                }
                self.cfg_json = None;
                for p in [self.cfg.out_dir.clone(), self.cfg.buffer_dir.clone()] {
                    if !self.free.contains_key(&p) {
                        self.send(json!({"cmd": "disk_free", "path": p}));
                    }
                }
            }
            "hotkeys" => {
                let hk = |k: &str| m["hotkeys"][k].as_str().map(String::from);
                self.hk = [hk("clip"), hk("rec"), hk("bar"), hk("overlay"), hk("clipping")];
                self.conflicts = m["conflicts"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(pretty)).collect();
            }
            "update_state" => {
                if m["ok"] == true {
                    self.upd_text = Some(s("text"));
                } else {
                    self.upd_text = None;
                    self.toast("error", &s("text"), None);
                }
            }
            "gpu" => {
                self.gpu = Some((s("name"), s("driver")));
            }
            "state" => {
                self.recording = m["recording"].as_bool().unwrap_or(false);
                self.rec_t0 = Instant::now() - Duration::from_secs(m["rec_secs"].as_u64().unwrap_or(0));
            }
            "toast" => {
                let kind = s("kind");
                self.toast(&kind, &s("text"), m["path"].as_str().map(PathBuf::from));
                if self.tab == Tab::Videos && kind != "error" {
                    self.send(json!({"cmd": "videos"}));
                }
            }
            "folder" => {
                let p = PathBuf::from(s("path"));
                self.free.insert(p.clone(), m["free_bytes"].as_u64().unwrap_or(0));
                match s("key").as_str() {
                    "buffer_dir" => self.cfg.buffer_dir = p,
                    _ => self.cfg.out_dir = p,
                }
            }
            "disk" => {
                self.free.insert(PathBuf::from(s("path")), m["free_bytes"].as_u64().unwrap_or(0));
            }
            "videos" => {
                let vids: Vec<Vid> = m["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| {
                        let path = PathBuf::from(v["path"].as_str().unwrap_or(""));
                        Vid {
                            stem: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                            path,
                            name: v["name"].as_str().unwrap_or("").into(),
                            rec: v["kind"] == "REC",
                            bytes: v["bytes"].as_u64().unwrap_or(0),
                            secs: v["secs"].as_f64().unwrap_or(0.0),
                            date: v["date"].as_u64().unwrap_or(0),
                            audio: v["audio"].as_u64().unwrap_or(0) as usize,
                            video: v["video"].as_bool().unwrap_or(true),
                        }
                    })
                    .collect();
                // Miniatures des vidéos disparues : mémoire rendue.
                self.thumbs.retain(|k, _| vids.iter().any(|v| v.stem == k.trim_end_matches('#')));
                let had = self.v_sel.clone();
                self.vids = Some(vids);
                let has = |p: &PathBuf| self.vids.as_ref().is_some_and(|l| l.iter().any(|v| &v.path == p));
                if let Some(p) = self.v_pending.take().filter(|p| has(p)) {
                    self.pick(Some(p));
                } else if had.as_ref().is_some_and(|p| !has(p)) {
                    self.pick(None);
                } else if had.is_none() {
                    let first = self.vids.as_ref().and_then(|l| l.first()).map(|v| v.path.clone());
                    if first.is_some() {
                        self.pick(first);
                    }
                }
            }
            "driver" => {
                let st = s("status");
                if st == "nogpu" {
                    self.gpu = None;
                    return;
                }
                let d = &mut self.drv;
                (d.status, d.error, d.latest) = (st, s("error"), s("latest"));
                if !s("installed").is_empty() {
                    d.installed = s("installed");
                    if let Some(g) = &mut self.gpu {
                        g.1 = d.installed.clone();
                    }
                }
                (d.confirm, d.phase) = (false, String::new());
            }
            "driver_progress" => {
                let d = &mut self.drv;
                let ph = s("phase");
                if ph == "error" {
                    (d.phase, d.prog_err) = (String::new(), s("error"));
                    d.status = "error".into();
                    d.error = format!("Installation interrompue : {}.", if d.prog_err.is_empty() { "erreur inconnue" } else { &d.prog_err });
                } else {
                    (d.phase, d.done, d.total) = (ph, m["done"].as_u64().unwrap_or(0), m["total"].as_u64().unwrap_or(0));
                    if d.phase == "done" {
                        let b = self.cfg.driver_branch.clone();
                        self.send(json!({"cmd": "driver_check", "branch": b})); // version installée relue
                    }
                }
            }
            "clean" => {
                self.tools.scanning = false;
                self.tools.confirm = false;
                self.tools.items = Some(
                    m["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| CleanItem {
                            id: c["id"].as_str().unwrap_or("").into(),
                            name: c["name"].as_str().unwrap_or("").into(),
                            bytes: c["bytes"].as_u64().unwrap_or(0),
                            files: c["files"].as_u64().unwrap_or(0),
                            admin: c["admin"].as_bool().unwrap_or(false),
                            warn: c["warn"].as_str().unwrap_or("").into(),
                            on: c["default"].as_bool().unwrap_or(false) && c["bytes"].as_u64().unwrap_or(0) > 0,
                        })
                        .collect(),
                );
            }
            "clean_done" => {
                self.tools.running = false;
                let sk = m["skipped"].as_u64().unwrap_or(0);
                self.tools.done = Some(format!(
                    "**{} libérés.** {}",
                    bytes(m["freed_bytes"].as_u64().unwrap_or(0)),
                    if sk > 0 { format!("{} fichiers en cours d'utilisation ont été ignorés.", thousands(sk)) } else { String::new() }
                ));
                self.send(json!({"cmd": "clean_scan"}));
            }
            "ram" => {
                self.tools.ram_wait = false;
                self.tools.ram = Some((m["free_before"].as_u64().unwrap_or(0), m["free_after"].as_u64().unwrap_or(0), m["total"].as_u64().unwrap_or(0)));
            }
            "levels" => self.levels = (m["game"].as_f64().unwrap_or(0.0) as f32, m["mic"].as_f64().unwrap_or(0.0) as f32),
            "autostart" => self.autostart = m["on"].as_bool().unwrap_or(false),
            "admin" => {
                self.admin_busy = false;
                if let Some(e) = m["elevated"].as_bool() {
                    self.elevated = e;
                }
                if m["ok"] != true {
                    self.toast("error", "Windows a refusé l'autorisation administrateur.", None);
                }
            }
            "tab" => {
                if let Some(t) = m["tab"].as_str().and_then(Tab::parse) {
                    self.select_tab(t);
                }
            }
            "ffmpeg" => {
                self.ff_installing = false;
            }
            _ => {}
        }
    }

    /// Dessine toute la fenêtre et traite les entrées de cette image.
    pub fn frame(&mut self, ui: &mut Ui) {
        let now = ui.now;
        self.next = None;
        self.cfg_json = None;
        (self.thumb_left, self.thumb_more) = (2, false);
        ui.begin();
        ui.c.clear(pal::BODY);
        let (w, h) = ui.c.size();
        self.keys(ui);
        if self.fullscreen && self.tab == Tab::Videos {
            // Vidéo en grand écran : le lecteur occupe toute la fenêtre.
            self.player_view(ui, r(0.0, 0.0, w, h), 0.0);
        } else {
            self.titlebar(ui, w);
            let body = r(1.0, 45.0, w - 2.0, h - 46.0);
            match self.tab {
                Tab::Clip => self.tab_clip(ui, body),
                Tab::Audio => self.tab_audio(ui, body),
                Tab::Videos => self.tab_videos(ui, body),
                Tab::Drivers => self.tab_drivers(ui, body),
                Tab::Tools => self.tab_tools(ui, body),
                Tab::Stats => self.tab_stats(ui, body),
                Tab::Advanced => self.tab_advanced(ui, body),
            }
            self.overlays(ui, w, h);
            ui.popups();
            ui.c.border(r(0.0, 0.0, w, h), 0.0, 1.0, hex(0x333d30));
        }
        self.intro_layer(ui, w, h);
        if let Some(t) = self.closing {
            // Fondu de fermeture : le contenu s'efface vers la couleur de fond, puis la fenêtre est fermée.
            let k = (now.duration_since(t).as_secs_f32() / 0.16).min(1.0);
            ui.c.rect(r(0.0, 0.0, w, h), pal::BODY.a(k));
            ui.s.animating = true;
            if k >= 1.0 {
                self.closing = None;
                self.send(json!({"cmd": "close"}));
            }
        }
        ui.end();
        // Envoi différé des réglages qui relancent la capture.
        if let Some(t) = self.flush_at {
            if now >= t {
                self.flush();
            } else {
                self.wake_in(t - now);
            }
        }
        if let Some((_, _, Some(t))) = &self.save_msg {
            if now >= *t {
                self.save_msg = None;
            } else {
                let d = *t - now;
                self.wake_in(d);
            }
        }
        if self.recording {
            self.wake_in(Duration::from_millis(400));
        }
        if self.tab == Tab::Audio && !self.cfg.audio_tracks.is_empty() {
            // Niveaux en direct : demandés 15 fois par seconde, seulement quand l'onglet est affiché.
            if now.duration_since(self.levels_at) >= Duration::from_millis(66) {
                self.levels_at = now;
                self.send(json!({"cmd": "levels"}));
            }
            self.wake_in(Duration::from_millis(66));
        }
        if self.player.tick() {
            self.wake_in(Duration::from_millis(if self.player.playing { 100 } else { 60 }));
        }
        if self.thumb_more {
            self.wake_in(Duration::from_millis(16)); // miniatures restantes : image suivante
        }
    }

    /// Valeur d'un réglage (premier niveau) lue dans le JSON de l'image en cours.
    pub fn cfg_get(&mut self, key: &str) -> Option<Value> {
        if self.cfg_json.is_none() {
            self.cfg_json = serde_json::to_value(&self.cfg).ok();
        }
        self.cfg_json.as_ref().and_then(|v| v.get(key).cloned())
    }

    pub fn wake_in(&mut self, d: Duration) {
        self.next = Some(self.next.map_or(d, |n| n.min(d)));
    }

    fn keys(&mut self, ui: &mut Ui) {
        // Saisie d'un raccourci : la prochaine combinaison avec Ctrl, Alt ou Maj devient le raccourci.
        if let Some(i) = self.hk_wait {
            for k in &ui.i.keys {
                if k == "Escape" {
                    self.hk_wait = None;
                    return;
                }
                let modifier = ["Control", "Alt", "Shift", "Super", "Meta"].iter().any(|m| k.starts_with(m));
                if modifier || !(ui.i.ctrl || ui.i.alt || ui.i.shift) || k.starts_with("Unidentified") {
                    continue;
                }
                let mut parts = vec![];
                if ui.i.ctrl {
                    parts.push("Ctrl");
                }
                if ui.i.alt {
                    parts.push("Alt");
                }
                if ui.i.shift {
                    parts.push("Shift");
                }
                parts.push(k.as_str());
                let combo = parts.join("+");
                self.hk[i] = Some(pretty(&combo));
                self.hk_wait = None;
                self.set(HK[i].1, json!(combo));
                return;
            }
            return;
        }
        if ui.i.key("F11") {
            self.toggle_max();
        }
        if ui.i.key("Escape") {
            // Échap ferme d'abord ce qui est ouvert (menu, liste, champ, renommage, suppression), puis la
            // vidéo en grand écran, puis restaure la fenêtre agrandie.
            let busy = self.ctx.is_some() || ui.s.popup.is_some() || ui.s.focus != 0 || self.ren.is_some() || self.del_box;
            self.ctx = None;
            if self.fullscreen {
                self.set_fullscreen(false);
            } else if self.maximized && !busy {
                self.toggle_max();
            }
        }
        if ui.i.ctrl && ui.i.key("KeyS") {
            self.flush();
        }
    }

    pub fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.send(json!({"cmd": "fullscreen", "on": on}));
    }

    /// Agrandit la fenêtre, ou la remet à sa taille et sa place d'avant (gardées par Windows).
    pub fn toggle_max(&mut self) {
        self.maximized = !self.maximized; // icône à jour tout de suite, confirmée au dessin suivant
        self.send(json!({"cmd": "maximize", "on": self.maximized}));
    }

    /// Info-bulle sous un bouton survolé plus de 0,5 s (un seul calque, rien tant que la souris bouge ailleurs).
    fn tooltip(&mut self, ui: &mut Ui, wid: u64, q: R, text: &str) {
        let hot = ui.hot(q) && ui.s.active == 0;
        match self.tip {
            Some((i, _)) if i == wid && !hot => self.tip = None,
            Some((i, _)) if i == wid => {}
            _ if hot => self.tip = Some((wid, ui.now)),
            _ => {}
        }
        let Some((_, t0)) = self.tip.filter(|(i, _)| *i == wid) else { return };
        let wait = Duration::from_millis(500).saturating_sub(ui.now.duration_since(t0));
        if !wait.is_zero() {
            return self.wake_in(wait);
        }
        let tw = text_w(text, F::s(12.0, 500)) + 20.0;
        let (sw, _) = ui.c.size();
        // Dessinée après le contenu de l'onglet (overlays), sinon il la recouvrait.
        self.tip_box = Some((r((q.cx() - tw / 2.0).min(sw - 8.0 - tw).max(8.0), q.bottom() + 6.0, tw, 26.0), text.to_string()));
    }

    fn titlebar(&mut self, ui: &mut Ui, w: f32) {
        let c = &mut *ui.c;
        c.rect(r(1.0, 44.0, w - 2.0, 1.0), pal::LINE);
        crate::gfx::logo(c, 15.0, 13.0, 20.0, 1.0, tiny_skia::Transform::identity());
        c.text(44.0, 1.0, 44.0, "Clipper", F::s(14.0, 600).ls(-0.01), pal::TEXT);
        let mut x = 44.0 + text_w("Clipper", F::s(14.0, 600).ls(-0.01)) + 14.0 + 6.0;
        // onglets
        let tabs: [(Tab, &str); 7] = [
            (Tab::Clip, "Clipper"), (Tab::Audio, "Audio"), (Tab::Stats, "Statistiques"), (Tab::Videos, "Vidéos"), (Tab::Drivers, "Pilotes"), (Tab::Tools, "Outils"),
            (Tab::Advanced, "Avancé"),
        ];
        let f = F::s(13.0, 500);
        let badge = (self.drv.status == "update").then(|| self.drv.latest.clone());
        let widths: Vec<f32> = tabs
            .iter()
            .map(|(t, l)| {
                22.0 + text_w(l, f) + if *t == Tab::Clip { 14.0 } else { 0.0 } + if *t == Tab::Drivers { badge.as_ref().map_or(0.0, |b| 8.0 + text_w(b, F::m(11.0, 400))) } else { 0.0 }
            })
            .collect();
        // Écarts de 2 px entre onglets : un de moins que le nombre d'onglets (avant : 4 en dur, prévu pour 5
        // onglets ; avec 7, « Avancé » dépassait du cadre).
        let total = widths.iter().sum::<f32>() + 8.0 + 2.0 * (tabs.len() as f32 - 1.0);
        let bx = r(x, 4.0, total, 38.0);
        ui.c.rrect(bx, 7.0, pal::WELL);
        ui.c.border(bx, 7.0, 1.0, pal::LINE);
        x += 4.0;
        let mut pick = None;
        for (i, (t, label)) in tabs.iter().enumerate() {
            let cell = r(x, 8.0, widths[i], 30.0);
            let on = self.tab == *t;
            let hot = ui.hot(cell);
            if ui.click(idn("tab", i), cell) {
                pick = Some(*t);
            }
            if on {
                ui.c.rrect(r(cell.x, cell.y + 1.0, cell.w, cell.h), 5.0, pal::BLACK.a(0.4));
                ui.c.rrect(cell, 5.0, pal::RAISE);
                ui.c.rect(r(cell.x + 2.0, cell.y, cell.w - 4.0, 1.0), pal::WHITE.a(0.05));
            }
            let mut tx = cell.x + 11.0;
            if *t == Tab::Clip {
                ui.led(tx + 3.0, cell.cy(), 6.0, pal::OK);
                tx += 14.0;
            }
            tx += ui.c.text(tx, cell.y, cell.h, label, f, if on || hot { pal::TEXT } else { pal::TEXT3 });
            if let (Tab::Drivers, Some(b)) = (*t, &badge) {
                ui.c.text(tx + 8.0, cell.y, cell.h, b, F::m(11.0, 400), pal::WARN);
            }
            x += widths[i] + 2.0;
        }
        if let Some(t) = pick {
            self.select_tab(t);
        }
        let tabs_end = bx.right() + 12.0;
        // côté droit, de droite à gauche
        let mut rx = w - 1.0 - 6.0 - 36.0;
        let q = r(rx, 6.0, 36.0, 32.0);
        if ui.wbtn(id("wClose"), q, "x", true) && self.closing.is_none() {
            self.flush();
            self.player.pause();
            self.closing = Some(ui.now);
        }
        self.tooltip(ui, id("wClose"), q, "Fermer");
        // Agrandir / restaurer : la fenêtre occupe l'écran où elle se trouve, barre des tâches comprise
        // (tao s'arrête à la zone de travail), à n'importe quelle mise à l'échelle.
        rx -= 4.0 + 36.0;
        let q = r(rx, 6.0, 36.0, 32.0);
        if ui.wbtn(id("wMax"), q, if self.maximized { "restore" } else { "max" }, false) {
            self.toggle_max();
        }
        self.tooltip(ui, id("wMax"), q, if self.maximized { "Restaurer (F11)" } else { "Agrandir (F11)" });
        rx -= 4.0 + 36.0;
        let q = r(rx, 6.0, 36.0, 32.0);
        if ui.wbtn(id("wMin"), q, "min", false) {
            self.send(json!({"cmd": "minimize"}));
        }
        self.tooltip(ui, id("wMin"), q, "Réduire");
        if self.recording {
            let secs = self.rec_t0.elapsed().as_secs();
            let t = format!("{:02}:{:02}", secs / 60, secs % 60);
            let tw = text_w("REC ", F::s(12.0, 400)) + text_w(&t, F::m(12.0, 400));
            rx -= 14.0 + tw + 14.0;
            let blink = (self.t_start.elapsed().as_millis() % 1600) < 800;
            ui.led(rx + 3.5, 23.0, 7.0, pal::REC.a(if blink { 1.0 } else { 0.35 }));
            let x2 = rx + 14.0 + ui.c.text(rx + 14.0, 1.0, 44.0, "REC ", F::s(12.0, 400), hex(0xffd9d5));
            ui.c.text(x2, 1.0, 44.0, &t, F::m(12.0, 400), hex(0xffd9d5));
        }
        if let Some(v) = self.update.clone() {
            // Bandeau de mise à jour : un clic télécharge, vérifie l'empreinte et installe.
            let busy = self.upd_text.is_some();
            let f = F::s(12.0, 600);
            // Libellé selon la place laissée par les onglets : long, court, puis icône seule (jamais par-dessus).
            let room = rx - 14.0 - 6.0 - tabs_end;
            let bw_of = |l: &str| if l.is_empty() { 40.0 } else { 22.0 + 18.0 + 7.0 + text_w(l, f) + 2.0 };
            let label = match self.upd_text.clone() {
                Some(t) if bw_of(&t) <= room => t,
                Some(_) => "…".to_string(),
                None => [format!("Nouvelle version {v} · Installer"), format!("Installer {v}")].into_iter().find(|l| bw_of(l) <= room).unwrap_or_default(),
            };
            let bw = bw_of(&label);
            rx -= 14.0 + 6.0 + bw;
            let q = r(rx, 8.0, bw, 28.0);
            let hot = !busy && ui.hot(q);
            if !busy && ui.click(id("upd"), q) {
                self.upd_text = Some("Téléchargement et vérification…".into());
                self.send(json!({"cmd": "update_install"}));
            }
            let o = if busy { 0.6 } else { 1.0 };
            ui.c.rrect(q, 6.0, if hot { pal::SIGNAL } else { pal::SIGNAL_SOFT }.a(o));
            ui.c.border(q, 6.0, 1.0, pal::SIGNAL.a(o));
            let fg = if hot { pal::SIGNAL_INK } else { pal::TEXT }.a(o);
            ui.c.icon("dl", q.x + 11.0, q.y + 5.0, 18.0, fg);
            ui.c.text(q.x + 36.0, q.y, q.h, &label, f, fg);
        }
        // Glisser la fenêtre : appui dans la barre de titre, hors de tout contrôle. Double-clic : agrandir /
        // restaurer, comme une fenêtre de Windows.
        if ui.i.pressed && ui.s.active == 0 && ui.i.mouse.1 < 44.0 && ui.i.mouse.1 >= 0.0 && ui.s.popup.is_none() && self.intro.is_none() {
            if ui.i.dbl {
                self.toggle_max();
            } else {
                self.send(json!({"cmd": "drag"}));
            }
        }
    }

    /// Notifications, menu du clic droit, bandeau « Enregistrement… ».
    fn overlays(&mut self, ui: &mut Ui, w: f32, h: f32) {
        let now = ui.now;
        if let Some((b, text)) = self.tip_box.take() {
            ui.overlay(|ui| {
                ui.c.shadow(b, 6.0, 4.0, 10.0, pal::BLACK.a(0.4));
                ui.c.rrect(b, 6.0, pal::RAISE);
                ui.c.border(b, 6.0, 1.0, pal::LINE2);
                ui.c.text_in(b, 0, &text, F::s(12.0, 500), pal::TEXT);
            });
        }
        // bandeau des réglages
        let mut bottom = h - 16.0;
        if let Some((t, hint, _)) = self.save_msg.clone() {
            let bw = (28.0 + text_w(&t, F::s(13.0, 600)).max(text_w(&hint, F::s(12.0, 400)))).ceil();
            let q = r(w - 16.0 - bw, h - 16.0 - 54.0, bw, 54.0);
            ui.overlay(|ui| {
                ui.c.shadow(q, 10.0, 8.0, 14.0, pal::BLACK.a(0.45));
                ui.c.rrect(q, 10.0, pal::RAISE);
                ui.c.border(q, 10.0, 1.0, pal::LINE);
                ui.c.text(q.x + 14.0, q.y + 9.0, 18.0, &t, F::s(13.0, 600), pal::TEXT);
                ui.c.text(q.x + 14.0, q.y + 29.0, 17.0, &hint, F::s(12.0, 400), pal::TEXT3);
            });
            bottom = q.y - 8.0;
        }
        // notifications
        let mut i = 0;
        let mut action: Option<(usize, bool)> = None;
        while i < self.toasts.len() {
            let t = &self.toasts[i];
            let life = Duration::from_millis(if t.kind == "error" { 9000 } else { 6000 });
            let age = now.duration_since(t.t0);
            let (mut alpha, mut dy) = (ease(age.as_secs_f32() / 0.45), 10.0 * (1.0 - ease(age.as_secs_f32() / 0.45)));
            let out = t.out.or((age >= life).then(|| t.t0 + life));
            if let Some(o) = out {
                let k = now.saturating_duration_since(o).as_secs_f32() / 0.25;
                if k >= 1.0 {
                    self.toasts.remove(i);
                    continue;
                }
                (alpha, dy) = (1.0 - k, 6.0 * k);
            }
            let t = &self.toasts[i];
            let title = match t.kind.as_str() {
                "clip" => "Clip enregistré",
                "rec" => "REC enregistré",
                "error" => "Erreur",
                _ => "Clipper",
            };
            let sp = md(&t.text, F::s(12.0, 400), pal::TEXT2, 500, pal::TEXT2);
            let th = ui.c.para(0.0, 0.0, 340.0 - 14.0 - 7.0 - 12.0 - 12.0 - 28.0 - 10.0, 17.4, &sp, false);
            let see = t.kind != "error" && t.path.is_some();
            let hh = 12.0 + 20.0 + 4.0 + th + if see { 8.0 + 32.0 } else { 0.0 } + 12.0;
            let q = r(w - 16.0 - 340.0, bottom - hh + dy, 340.0, hh);
            let err = t.kind == "error";
            let (close, view) = ui.overlay(|ui| {
                ui.c.shadow(q, 8.0, 8.0, 14.0, pal::BLACK.a(0.5 * alpha));
                ui.c.rrect(q, 8.0, hex(0x151a14).a(0.97 * alpha));
                ui.c.border(q, 8.0, 1.0, hex(0x3a4537).a(alpha));
                ui.led(q.x + 17.5, q.y + 22.0, 7.0, if err { pal::REC } else { pal::OK }.a(alpha));
                ui.c.text(q.x + 33.0, q.y + 12.0, 20.0, title, F::s(14.0, 600), pal::TEXT.a(alpha));
                let spa: Vec<_> = sp.iter().map(|s| crate::gfx::Span { c: s.c.a(alpha), ..s.clone() }).collect();
                ui.c.para(q.x + 33.0, q.y + 36.0, q.w - 33.0 - 50.0, 17.4, &spa, true);
                let close = ui.wbtn(idn("toast-x", i), r(q.right() - 38.0, q.y + 12.0, 28.0, 28.0), "x", false);
                let view = see && ui.btn(idn("toast-see", i), r(q.x + 33.0, q.bottom() - 44.0, Ui::btn_w("Voir", false, Btn::N), 32.0), "Voir", None, Btn::N);
                (close, view)
            });
            if close || view {
                action = Some((i, view));
            }
            bottom = q.y - dy - 8.0;
            if alpha < 1.0 || out.is_some() {
                ui.s.animating = true;
            } else {
                let rest = life.saturating_sub(age);
                self.wake_in(rest);
            }
            i += 1;
        }
        if let Some((i, view)) = action {
            if view {
                self.v_pending = self.toasts[i].path.clone();
                self.select_tab(Tab::Videos);
            }
            self.toasts[i].out = Some(now);
        }
        // menu du clic droit sur un clip
        if let Some((path, (mx, my))) = self.ctx.clone() {
            let items: [(&str, &'static str, &str, bool); 5] = [
                ("play", "clip", "Lire ici", false),
                ("open", "arrow", "Ouvrir avec le lecteur", false),
                ("open_folder", "folder", "Ouvrir le dossier", false),
                ("rename", "pen", "Renommer", false),
                ("delete", "trash", "Supprimer", true),
            ];
            let mh = 8.0 + 5.0 * 32.0 + 9.0;
            let q = r(mx.min(w - 218.0), my.min(h - mh - 8.0), 210.0, mh);
            ui.s.block = Some(q);
            let mut chosen = None;
            ui.overlay(|ui| {
                ui.c.shadow(q, 8.0, 8.0, 14.0, pal::BLACK.a(0.5));
                ui.c.rrect(q, 8.0, pal::RAISE);
                ui.c.border(q, 8.0, 1.0, pal::LINE);
                let mut y = q.y + 4.0;
                for (n, (act, icon, label, danger)) in items.iter().enumerate() {
                    if *danger {
                        ui.c.rect(r(q.x + 6.0, y + 4.0, q.w - 12.0, 1.0), pal::LINE);
                        y += 9.0;
                    }
                    let cell = r(q.x + 4.0, y, q.w - 8.0, 32.0);
                    if ui.hot(cell) {
                        ui.c.rrect(cell, 5.0, pal::RAISE_H);
                    }
                    if ui.click(idn("ctx", n), cell) {
                        chosen = Some(*act);
                    }
                    let fg = if *danger { hex(0xff8f7d) } else { pal::TEXT };
                    ui.c.icon(icon, cell.x + 10.0, cell.y + 8.5, 15.0, if *danger { fg } else { pal::TEXT3 });
                    ui.c.text(cell.x + 35.0, cell.y, 32.0, label, F::s(13.0, 400), fg);
                    y += 32.0;
                }
            });
            if let Some(act) = chosen {
                self.ctx = None;
                ui.s.block = None;
                match act {
                    "open" | "open_folder" => self.send(json!({"cmd": "video_action", "action": act, "path": path})),
                    "play" => {
                        self.pick(Some(path));
                        self.player.toggle();
                    }
                    "rename" => {
                        self.pick(Some(path));
                        self.ren = self.cur().map(|v| v.name.rsplit_once('.').map_or(v.name.clone(), |(a, _)| a.to_string()));
                        ui.s.focus = id("renIn");
                    }
                    _ => {
                        self.pick(Some(path));
                        self.ask_delete();
                    }
                }
            } else if (ui.i.pressed || ui.i.rpressed) && !q.has(ui.i.mouse) {
                self.ctx = None;
                ui.s.block = None;
            }
        }
    }

    pub fn ask_delete(&mut self) {
        if self.cfg.delete_confirm {
            (self.del_box, self.del_ask, self.ren) = (true, false, None);
        } else {
            self.do_delete();
        }
    }

    pub fn do_delete(&mut self) {
        if let Some(p) = self.v_sel.clone() {
            self.player.close();
            self.send(json!({"cmd": "video_action", "action": "delete", "path": p}));
        }
        self.del_box = false;
    }

    /// Intro : le trombone tombe et se clipse sur la page (2,7 s, un clic la passe).
    fn intro_layer(&mut self, ui: &mut Ui, w: f32, h: f32) {
        let Some(t0) = self.intro else { return };
        let t = ui.now.duration_since(t0).as_secs_f32();
        if t >= 2.7 || ui.i.pressed {
            self.intro = None;
            ui.s.animating = true;
            return;
        }
        ui.s.animating = true;
        let fade = if t > 2.2 { 1.0 - ((t - 2.2) / 0.5).min(1.0) } else { 1.0 };
        let c = &mut *ui.c;
        c.rect(r(0.0, 0.0, w, h), pal::GROUND.a(fade));
        let (sx, sy) = ((w - 150.0) / 2.0, (h - 190.0) / 2.0);
        // feuille
        let k = ease(t / 0.8);
        let press = if (1.09..1.44).contains(&t) { 1.0 - 0.015 * (std::f32::consts::PI * (t - 1.09) / 0.35).sin() } else { 1.0 };
        let sh = r(sx, sy + 34.0 + 16.0 * (1.0 - k), 150.0, 156.0 * press);
        c.shadow(sh, 10.0, 18.0, 22.0, pal::BLACK.a(0.45 * k * fade));
        c.rrect(sh, 10.0, pal::PANEL.a(k * fade));
        c.border(sh, 10.0, 1.0, pal::LINE2.a(k * fade));
        for (i, wd) in [114.0, 66.0, 82.0].iter().enumerate() {
            c.rrect(r(sh.x + 18.0, sh.y + 18.0 + i as f32 * 16.0, *wd, 6.0), 3.0, pal::RAISE.a(k * fade));
        }
        // trombone : chute avec rebond, de 0,25 s à 1,45 s
        let ct = ((t - 0.25) / 1.2).clamp(0.0, 1.0);
        if t >= 0.25 {
            let fall = if ct < 0.7 { 1.0 - (1.0 - ct / 0.7).powi(3) } else { 1.0 + 0.06 * (std::f32::consts::PI * (ct - 0.7) / 0.3).sin() * (1.0 - ct) };
            let (dy, rot) = (-120.0 * (1.0 - fall), -22.0 * (1.0 - fall).max(0.0));
            let tf = tiny_skia::Transform::from_rotate_at(rot, w / 2.0, sy + 38.0 + dy).pre_translate(0.0, dy);
            crate::gfx::logo(c, w / 2.0 - 32.0, sy, 64.0, (ct / 0.35).min(1.0) * fade, tf);
        }
        // éclat au moment où il se clipse
        if (1.09..1.79).contains(&t) {
            let ft = (t - 1.09) / 0.7;
            let rad = 45.0 * (0.3 + 1.1 * ft);
            c.arc(w / 2.0, sy + 34.0, rad, 0.0, std::f32::consts::TAU, 2.0, pal::SIGNAL.a(0.9 * (1.0 - ft) * fade));
        }
        if t >= 1.3 {
            let wt = ease((t - 1.3) / 0.6);
            let f = F::s(20.0, 700).ls(-0.01);
            c.text((w - text_w("Clipper", f)) / 2.0, sy + 190.0 + 16.0 + 6.0 * (1.0 - wt), 28.0, "Clipper", f, pal::TEXT.a(wt * fade));
        }
    }

    // ---------- briques de mise en page communes aux onglets ----------

    /// En-tête repliable d'une section (.grp). Renvoie (ouverte, y du contenu).
    pub fn grp(&mut self, ui: &mut Ui, key: &str, title: &str, x: f32, y: f32, w: f32) -> (bool, f32) {
        let open = self.fold(key);
        // Titre lisible d'un coup d'œil (avant : petites capitales de 11 px, on se perdait entre sections).
        let head = r(x, y + 16.0, w, 34.0);
        let hot = ui.hot(head);
        if ui.click(id(key), head) {
            self.toggle_fold(key);
        }
        ui.c.text(x, head.y, 34.0, title, F::s(16.0, 600).ls(-0.01), if hot { pal::WHITE } else { pal::TEXT });
        let rot = ui.anim(id(key) ^ 7, if open { 90.0 } else { 0.0 }, 0.2);
        ui.c.icon_rot("chev", x + w - 16.0, head.y + 9.0, 16.0, if hot { pal::TEXT } else { pal::TEXT3 }, rot);
        (open, y + 16.0 + 34.0 + if open { 14.0 } else { 0.0 })
    }

    /// Fin de section : marge basse et filet. Renvoie le y de la section suivante.
    pub fn grp_end(&mut self, ui: &mut Ui, open: bool, x: f32, y: f32, w: f32, last: bool) -> f32 {
        let y = y + if open { 20.0 } else { 18.0 };
        if !last {
            ui.c.rect(r(x, y, w, 1.0), pal::LINE);
        }
        y + 1.0
    }

    /// Libellé de champ (.f > .lab), colonne de 118 px ; `small` : seconde ligne grise.
    pub fn lab(&self, c: &mut Canvas, x: f32, y: f32, h: f32, label: &str, small: Option<&str>) {
        match small {
            Some(s) => {
                let top = y + (h - 36.0) / 2.0;
                c.text(x, top, 19.0, label, F::s(13.0, 400), pal::TEXT2);
                c.text(x, top + 19.0, 17.0, s, F::s(12.0, 400), pal::TEXT3);
            }
            None => {
                c.text(x, y, h, label, F::s(13.0, 400), pal::TEXT2);
            }
        }
    }

    /// Rangée à interrupteur (.sw-row) : titre, description, interrupteur à droite. Renvoie (clic, hauteur).
    pub fn sw_row(&mut self, ui: &mut Ui, wid: u64, x: f32, y: f32, w: f32, title: &str, hint: &str, on: bool, disabled: bool) -> (bool, f32) {
        ui.c.text(x, y, 19.0, title, F::s(13.0, 500), pal::TEXT);
        let hh = ui.hint(x, y + 21.0, w - 44.0 - 24.0, hint);
        let clicked = ui.switch(wid, x + w - 44.0, y + 1.0, on, disabled);
        (clicked, (21.0 + hh).max(26.0))
    }

    /// Filet entre deux rangées à interrupteur. Renvoie le y de la rangée suivante.
    pub fn sw_sep(&self, c: &mut Canvas, x: f32, y: f32, w: f32) -> f32 {
        c.rect(r(x, y + 14.0, w, 1.0), pal::LINE);
        y + 29.0
    }

    /// Miniature du clip ouvert, à la taille du lecteur, dessinée dans `q` (attente de la 1re image et fondu).
    /// Rangée à part de celle de la grille (clé « nom# ») : sinon les deux tailles se remplaçaient à chaque
    /// dessin. Faux : pas de miniature.
    pub fn player_thumb(&mut self, ui: &mut crate::widgets::Ui, q: R) -> bool {
        let Some(stem) = self.cur().map(|v| v.stem.clone()) else { return false };
        let (w, h) = ((q.w * ui.c.s).round() as u32, (q.h * ui.c.s).round() as u32);
        let key = format!("{stem}#");
        if self.thumbs.get(&key).is_none_or(|t| t.as_ref().is_some_and(|p| (p.width(), p.height()) != (w, h))) {
            self.thumbs.insert(key.clone(), load_thumb(&stem, w, h));
        }
        let Some(Some(img)) = self.thumbs.get(&key) else { return false };
        ui.c.blit(img.data(), img.width(), img.height(), q.x, q.y);
        true
    }

    /// Miniature d'un clip, déjà réduite à `w` x `h` pixels d'écran : décodée et réduite une fois, puis
    /// recopiée telle quelle à chaque dessin (refaite seulement si la taille des cartes change).
    pub fn thumb(&mut self, stem: &str, w: u32, h: u32) -> Option<&tiny_skia::Pixmap> {
        let stale = self.thumbs.get(stem).is_none_or(|t| t.as_ref().is_some_and(|p| (p.width(), p.height()) != (w, h)));
        if stale {
            // Lecture disque + décodage JPEG : au plus 2 par image, sinon le défilement saccadait.
            if self.thumb_left == 0 {
                self.thumb_more = true;
                return None;
            }
            self.thumb_left -= 1;
            self.thumbs.insert(stem.to_string(), load_thumb(stem, w, h));
        }
        self.thumbs.get(stem).and_then(|t| t.as_ref())
    }
}

pub fn load_thumb(stem: &str, w: u32, h: u32) -> Option<tiny_skia::Pixmap> {
    let img = std::fs::read(crate::config::dir().join("thumbs").join(format!("{stem}.jpg"))).ok().and_then(|b| decode_jpeg(&b))?;
    crate::gfx::fit_cover(&img, w, h)
}

/// JPEG -> tampon BGRA opaque.
pub fn decode_jpeg(b: &[u8]) -> Option<tiny_skia::Pixmap> {
    let mut d = zune_jpeg::JpegDecoder::new(zune_jpeg::zune_core::bytestream::ZCursor::new(b));
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    let n = (w * h) as usize;
    if px.len() < n * 3 {
        return None;
    }
    for (dst, src) in pm.data_mut().chunks_exact_mut(4).zip(px.chunks_exact(3)) {
        dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
    }
    Some(pm)
}

/// Couleur d'une pastille d'état.
pub fn led_col(kind: &str) -> Col {
    match kind {
        "ok" => pal::OK,
        "update" | "warn" => pal::WARN,
        "error" | "rec" => pal::REC,
        _ => pal::LINE2,
    }
}
