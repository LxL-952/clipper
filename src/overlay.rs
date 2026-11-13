//! Overlay de statistiques : fenêtre Win32 native (pas de WebView, ~2 Mo), transparente aux clics,
//! toujours au premier plan, exclue des captures. Rien n'est injecté dans le jeu.
//! FPS : événements ETW publics « Present » de DXGI (comme PresentMon). Demande les droits admin
//! ou le groupe « Utilisateurs du journal de performances » ; sinon la ligne FPS affiche « droits ».
use crate::config::Config;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Mutex;
use std::collections::VecDeque;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone)]
struct Look {
    items: Vec<String>,
    graphs: Vec<String>,
    corner: String,
    size: String,
    opacity: f32,
    /// Cartes graphiques affichées (noms complets) ; vide = la carte NVIDIA, sinon la première.
    gpus: Vec<String>,
}

static LOOK: Mutex<Option<Look>> = Mutex::new(None);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Overlay utilisable (mode raccourci ou toujours) : ses sondes sont préchargées.
static ENABLED: AtomicBool = AtomicBool::new(false);
static TEXT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Clé de chaque ligne de TEXT (« fps », « cpu »…), dans le même ordre : pour retrouver sa courbe.
static KEYS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Historique des valeurs par mesure, pour les courbes (NaN = pas de valeur à cet instant).
static HIST: Mutex<Vec<(String, VecDeque<f32>)>> = Mutex::new(Vec::new());
/// Points d'une courbe : 60 rafraîchissements (15 s à 0,25 s).
const HIST_LEN: usize = 60;

/// Premier nombre d'une valeur affichée : « 1,4/8,0 Go » -> 1.4, « 61 » -> 61, « — » -> NaN.
fn leading_number(s: &str) -> f32 {
    let n: String = s.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.').collect();
    n.replace(',', ".").parse().unwrap_or(f32::NAN)
}
/// PID du jeu au premier plan : seuls ses « Present » sont comptés.
static TARGET: AtomicU32 = AtomicU32::new(0);
/// Horodatages QPC des derniers « Present » du jeu (1 s max).
static PRESENTS: Mutex<VecDeque<i64>> = Mutex::new(VecDeque::new());
static ETW_OK: AtomicBool = AtomicBool::new(false);
/// Plus grande largeur affichée depuis le dernier apply() (le cadre ne rétrécit pas en cours de route).
static WIDTH: AtomicI32 = AtomicI32::new(0);
static ETW_LOGGED: AtomicBool = AtomicBool::new(false);
/// Diagnostic : « Present » de tous les processus depuis le début de la session ETW.
static ALL_PRESENTS: AtomicU32 = AtomicU32::new(0);

/// Capteur de température CPU : 0 pas encore lu, 1 présent, 2 absent.
static CPU_TEMP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Lit le capteur une fois, dans un fil : il demande deux lectures à 100 ms d'écart, ce qui retardait
/// l'ouverture de la fenêtre de 270 ms quand c'était fait au démarrage.
pub fn cpu_temp_probe() {
    std::thread::spawn(|| {
        let ok = Thermal::open().is_some_and(|t| {
            t.celsius();
            std::thread::sleep(std::time::Duration::from_millis(100));
            t.celsius().is_some()
        });
        CPU_TEMP.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    });
}

/// Capteur de température CPU présent (sinon l'UI grise l'option). Pas encore lu : supposé présent.
pub fn cpu_temp_available() -> bool {
    CPU_TEMP.load(Ordering::Relaxed) != 2
}
/// Fenêtre de l'overlay : apply() lui poste WM_APP pour la réveiller.
static OV: std::sync::atomic::AtomicPtr<core::ffi::c_void> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// Fils ETW endormis (park) tant que l'overlay est masqué : apply() les réveille.
static WAKE: Mutex<Vec<std::thread::Thread>> = Mutex::new(Vec::new());

/// Applique les réglages et l'état visible (mode "always", ou raccourci en mode "hotkey").
pub fn apply(cfg: &Config, shown: bool) {
    *LOOK.lock().unwrap() = Some(Look {
        items: cfg.overlay_items.clone(),
        graphs: cfg.overlay_graphs.clone(),
        corner: cfg.overlay_corner.clone(),
        size: cfg.overlay_size.clone(),
        opacity: cfg.overlay_opacity.clamp(0.2, 1.0),
        gpus: cfg.overlay_gpus.clone(),
    });
    REFRESH_MS.store(cfg.overlay_refresh_ms.clamp(100, 5000), Ordering::Relaxed);
    // Mesure : fichier « fps-log » présent dans %APPDATA%Clipper -> FPS écrits au journal toutes les 2 s.
    BENCH.store(crate::config::dir().join("fps-log").exists(), Ordering::Relaxed);
    ENABLED.store(cfg.overlay_mode == "always" || cfg.overlay_mode == "hotkey", Ordering::Relaxed);
    let v = cfg.overlay_mode == "always" || (cfg.overlay_mode == "hotkey" && shown);
    VISIBLE.store(v, Ordering::Relaxed);
    WIDTH.store(0, Ordering::Relaxed); // taille, éléments ou visibilité changés : largeur recalculée
    let h = OV.load(Ordering::Relaxed);
    if !h.is_null() {
        unsafe {
            // Overlay activé : sondes chargées tout de suite, en arrière-plan, pour un premier Alt+F instantané.
            if ENABLED.load(Ordering::Relaxed) {
                PostMessageW(h, WM_APP + 1, 0, 0);
            }
            PostMessageW(h, WM_APP, 0, 0);
        }
    }
    WAKE.lock().unwrap().iter().for_each(std::thread::Thread::unpark);
}

/// Lancé une fois : les deux fils dorment sans aucun réveil tant que l'overlay est masqué.
pub fn start() {
    std::thread::spawn(|| unsafe { window_thread() });
    std::thread::spawn(etw_thread);
}

// ---------- Statistiques ----------

struct Cpu {
    last: (u64, u64),
}

impl Cpu {
    fn sample(&mut self) -> f32 {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetSystemTimes;
        let (mut i, mut k, mut u): (FILETIME, FILETIME, FILETIME) = unsafe { std::mem::zeroed() };
        unsafe { GetSystemTimes(&mut i, &mut k, &mut u) };
        let f = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
        let (idle, total) = (f(i), f(k) + f(u)); // kernel inclut idle
        let (di, dt) = (idle - self.last.0, total - self.last.1);
        self.last = (idle, total);
        if dt == 0 { 0.0 } else { 100.0 * (1.0 - di as f32 / dt as f32) }
    }
}

/// NVML (nvml.dll fourni par le pilote NVIDIA), chargé dans NOTRE processus. Absent : lignes GPU vides.
struct Nvml {
    /// Cartes NVIDIA vues par le pilote ; `dev()` rend celle choisie dans les réglages.
    devs: Vec<*mut core::ffi::c_void>,
    names: Vec<String>,
    util: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u32; 2]) -> i32,
    temp: unsafe extern "C" fn(*mut core::ffi::c_void, u32, *mut u32) -> i32,
    mem: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u64; 3]) -> i32,
    /// Charge du bloc d'encodage NVENC (%, période d'échantillonnage en µs).
    enc: Option<unsafe extern "C" fn(*mut core::ffi::c_void, *mut u32, *mut u32) -> i32>,
}

/// Températures GPU (NVML) et CPU (capteur Windows) pour le menu de la zone de notification.
/// Appelé sur le fil principal, au survol de l'icône : sondes chargées au premier survol seulement.
pub fn temps() -> (Option<u32>, Option<f64>) {
    thread_local! {
        static PROBES: (Option<Nvml>, Option<Thermal>) = (Nvml::load(), Thermal::open());
    }
    PROBES.with(|(nv, th)| {
        let gpu = nv.as_ref().and_then(|n| {
            let mut t = 0u32;
            (unsafe { (n.temp)(n.dev(), 0, &mut t) } == 0).then_some(t)
        });
        (gpu, th.as_ref().and_then(Thermal::celsius))
    })
}

/// Charge de l'encodeur NVENC le plus occupé (%), lue par NVML : simple lecture de compteur du pilote,
/// rien d'ouvert dans le jeu. None sans carte NVIDIA. Sert au garde-fou de la capture (capture.rs).
pub fn encoder_load() -> Option<u32> {
    thread_local! {
        static NV: Option<Nvml> = Nvml::load();
    }
    NV.with(|nv| {
        let n = nv.as_ref()?;
        let f = n.enc?;
        n.devs.iter().filter_map(|&d| {
            let (mut u, mut p) = (0u32, 0u32);
            (unsafe { f(d, &mut u, &mut p) } == 0).then_some(u)
        }).max()
    })
}

/// Toutes les cartes graphiques (NVIDIA, AMD, Intel, puce intégrée comprise), pour le choix dans les réglages.
pub fn gpu_names() -> Vec<String> {
    crate::gpus::list().iter().map(|a| a.name.clone()).collect()
}

/// Cartes affichées : celles choisies qui existent encore ; aucune = la carte NVIDIA, sinon la première.
fn selected_gpus(chosen: &[String]) -> Vec<&'static crate::gpus::Adapter> {
    let all = crate::gpus::list();
    let sel: Vec<_> = all.iter().filter(|a| chosen.contains(&a.name)).collect();
    if !sel.is_empty() {
        return sel;
    }
    all.iter().find(|a| a.name.contains("NVIDIA")).or(all.first()).into_iter().collect()
}

impl Nvml {
    /// Première carte NVIDIA (menu de la zone de notification).
    fn dev(&self) -> *mut core::ffi::c_void {
        self.devs[0]
    }

    /// Carte NVIDIA de ce nom (DXGI et NVML donnent le même nom), pour sa température et ses chiffres exacts.
    fn by_name(&self, name: &str) -> Option<*mut core::ffi::c_void> {
        self.names.iter().position(|n| n == name).map(|i| self.devs[i])
    }

    fn load() -> Option<Self> {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let name: Vec<u16> = "nvml.dll".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let lib = LoadLibraryW(name.as_ptr());
            if lib.is_null() {
                return None;
            }
            let sym = |s: &[u8]| GetProcAddress(lib, s.as_ptr());
            let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(sym(b"nvmlInit_v2\0")?);
            let by_index: unsafe extern "C" fn(u32, *mut *mut core::ffi::c_void) -> i32 = std::mem::transmute(sym(b"nvmlDeviceGetHandleByIndex_v2\0")?);
            let count: unsafe extern "C" fn(*mut u32) -> i32 = std::mem::transmute(sym(b"nvmlDeviceGetCount_v2\0")?);
            let get_name: unsafe extern "C" fn(*mut core::ffi::c_void, *mut u8, u32) -> i32 = std::mem::transmute(sym(b"nvmlDeviceGetName\0")?);
            if init() != 0 {
                return None;
            }
            let mut n = 0u32;
            if count(&mut n) != 0 {
                n = 1;
            }
            let (mut devs, mut names) = (vec![], vec![]);
            for i in 0..n {
                let mut dev = std::ptr::null_mut();
                if by_index(i, &mut dev) == 0 {
                    let mut buf = [0u8; 96];
                    get_name(dev, buf.as_mut_ptr(), buf.len() as u32);
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    devs.push(dev);
                    names.push(String::from_utf8_lossy(&buf[..end]).into_owned());
                }
            }
            if devs.is_empty() {
                return None;
            }
            Some(Self {
                devs,
                names,
                util: std::mem::transmute(sym(b"nvmlDeviceGetUtilizationRates\0")?),
                temp: std::mem::transmute(sym(b"nvmlDeviceGetTemperature\0")?),
                mem: std::mem::transmute(sym(b"nvmlDeviceGetMemoryInfo\0")?),
                enc: sym(b"nvmlDeviceGetEncoderUtilization\0").map(|f| std::mem::transmute(f)),
            })
        }
    }
}

/// Température CPU : compteur Windows « Thermal Zone Information » (capteur ACPI de la carte mère).
/// Sans pilote ni droits admin. Sur certains PC il est absent ou approximatif : la ligne affiche alors « n/d ».
struct Thermal {
    query: *mut core::ffi::c_void,
    counter: *mut core::ffi::c_void,
}

impl Thermal {
    fn open() -> Option<Self> {
        use windows_sys::Win32::System::Performance::*;
        let path: Vec<u16> = r"\Thermal Zone Information(*)\Temperature".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let (mut q, mut c) = (std::ptr::null_mut(), std::ptr::null_mut());
            if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(q, path.as_ptr(), 0, &mut c) != 0 {
                PdhCloseQuery(q);
                return None;
            }
            Some(Self { query: q, counter: c })
        }
    }

    /// Zone la plus chaude, en °C.
    fn celsius(&self) -> Option<f64> {
        use windows_sys::Win32::System::Performance::*;
        unsafe {
            PdhCollectQueryData(self.query);
            let (mut size, mut n) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut n, std::ptr::null_mut());
            if size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut n, items) != 0 {
                return None;
            }
            // 373 K pile = valeur par défaut de certains BIOS (pas une mesure) : écartée, comme 0.
            let k = (0..n as usize).map(|i| (*items.add(i)).FmtValue.Anonymous.doubleValue).filter(|&k| k != 373.0 && k > 274.0).fold(f64::MIN, f64::max);
            let c = k - 273.15;
            (1.0..=110.0).contains(&c).then_some(c)
        }
    }
}

// ---------- PresentMon (Intel, outil séparé comme ffmpeg) ----------

/// PresentMon introuvable ou refusé : on reste sur l'ETW DXGI jusqu'au prochain lancement.
static PM_UNUSABLE: AtomicBool = AtomicBool::new(false);
/// Une capture PresentMon tourne et alimente PM_FRAMES.
static PM_ON: AtomicBool = AtomicBool::new(false);
static PM_STARTING: AtomicBool = AtomicBool::new(false);
/// Images de tous les processus : (arrivée, ms depuis l'image précédente, processus). 10 s gardées.
static PM_FRAMES: Mutex<VecDeque<(std::time::Instant, f64, u32)>> = Mutex::new(VecDeque::new());
/// Même chose, mesurée par nos « Present » DXGI (ETW), en parallèle de PresentMon.
static DX_FRAMES: Mutex<VecDeque<(std::time::Instant, f64, u32)>> = Mutex::new(VecDeque::new());
/// Horodatage (QPC) du dernier Present de chaque processus.
static DX_LAST: Mutex<Option<std::collections::HashMap<u32, i64>>> = Mutex::new(None);
/// Processus -> (parent, exe), tenu à jour par la lecture de PresentMon.
static PM_TABLE: Mutex<Option<std::collections::HashMap<u32, (u32, String)>>> = Mutex::new(None);
/// Processus du jeu détecté (celui qui affiche ses images) ; 0 = aucun.
static GAME: AtomicU32 = AtomicU32::new(0);

/// Programmes qui affichent des images sans être des jeux : jamais pris pour « le jeu en cours ».
pub const NOT_GAMES: [&str; 40] = [
    "explorer.exe", "dwm.exe", "clipper.exe", "msedgewebview2.exe", "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe",
    "vivaldi.exe", "discord.exe", "code.exe", "spotify.exe", "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "riotclientux.exe",
    "riotclientuxrender.exe", "riotclientservices.exe", "battle.net.exe", "taskmgr.exe", "obs64.exe", "vlc.exe", "slack.exe", "teams.exe",
    "ms-teams.exe", "whatsapp.exe", "telegram.exe", "claude.exe", "searchhost.exe", "startmenuexperiencehost.exe", "shellexperiencehost.exe",
    "applicationframehost.exe", "textinputhost.exe", "lockapp.exe", "nvidia overlay.exe", "nvidia app.exe", "widgets.exe", "notepad.exe",
    "windowsterminal.exe",
];

/// Choix du jeu parmi les processus qui affichent des images (`count` = images des 2 dernières secondes).
/// 1. L'appli au premier plan (ou un de ses enfants), si ce n'est ni Clipper ni un programme de bureau et
///    qu'elle affiche vraiment : plein écran, ou au moins 30 i/s si elle est fenêtrée.
/// 2. Sinon le jeu déjà détecté tant qu'il affiche encore des images : ouvrir la barre de Clipper, Discord ou
///    un autre écran ne fait plus tomber le compteur à 0.
fn pick_game(
    count: &std::collections::HashMap<u32, usize>,
    table: &std::collections::HashMap<u32, (u32, String)>,
    fg: u32,
    fullscreen: bool,
    me: u32,
    prev: u32,
) -> Option<u32> {
    let name = |p: u32| table.get(&p).map_or(String::new(), |e| e.1.to_ascii_lowercase());
    let desktop = |p: u32| p == 0 || belongs(p, me, table) || NOT_GAMES.contains(&name(p).as_str());
    // 0. Un jeu installé (Steam, Epic, Xbox…) qui affiche : c'est lui, au premier plan ou non.
    let known = |p: &u32| !desktop(*p) && is_installed_game(&name(*p));
    if prev != 0 && known(&prev) && count.get(&prev).is_some_and(|n| *n >= 2) {
        return Some(prev);
    }
    if let Some((&p, _)) = count.iter().filter(|(p, n)| **n >= 2 && known(p)).max_by_key(|(_, n)| **n) {
        return Some(p);
    }
    if !desktop(fg) {
        let best = count.iter().filter(|(p, _)| belongs(**p, fg, table) && !desktop(**p)).max_by_key(|(_, n)| **n);
        if let Some((&p, &n)) = best {
            if (fullscreen && n >= 2) || n >= 60 {
                return Some(p);
            }
        }
    }
    if prev != 0 && count.get(&prev).is_some_and(|n| *n >= 2) {
        return Some(prev);
    }
    // 3. Aucun jeu au premier plan (Discord, Spotify ou l'autre écran actif) : l'appli hors bureau qui
    //    affiche le plus, au moins 30 i/s. Marche pour tout jeu, sans liste de jeux connus.
    count.iter().filter(|(p, n)| **n >= 60 && !desktop(**p)).max_by_key(|(_, n)| **n).map(|(&p, _)| p)
}

/// Noms des .exe (minuscules) des jeux installés, lus une fois sur le disque, en arrière-plan.
static GAME_EXES: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();

fn is_installed_game(exe: &str) -> bool {
    GAME_EXES.get().is_some_and(|s| s.contains(exe))
}

/// Dossiers de jeux des lanceurs sur chaque disque, plus les bibliothèques Steam déclarées.
/// Lecture de fichiers seulement : aucun processus n'est ouvert.
pub fn scan_installed_games() {
    use std::path::{Path, PathBuf};
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        let mut roots: Vec<PathBuf> = vec![];
        for d in b'C'..=b'Z' {
            let drive = PathBuf::from(format!("{}:\\", d as char));
            if !drive.exists() {
                continue;
            }
            for sub in [
                r"Program Files (x86)\Steam\steamapps\common", r"Program Files\Steam\steamapps\common", r"SteamLibrary\steamapps\common",
                r"Steam\steamapps\common", r"Program Files\Epic Games", r"Epic Games", r"XboxGames", r"Riot Games",
                r"Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games", r"Program Files\EA Games", r"EA Games",
                r"Program Files (x86)\GOG Galaxy\Games", r"GOG Games", r"Games",
            ] {
                roots.push(drive.join(sub));
            }
        }
        // Bibliothèques Steam ajoutées ailleurs : lignes « "path" "D:\\Jeux" » de libraryfolders.vdf.
        if let Ok(vdf) = std::fs::read_to_string(r"C:\Program Files (x86)\Steam\steamapps\libraryfolders.vdf") {
            for l in vdf.lines().filter(|l| l.trim_start().starts_with("\"path\"")) {
                if let Some(p) = l.split('"').nth(3) {
                    roots.push(PathBuf::from(p.replace("\\\\", "\\")).join(r"steamapps\common"));
                }
            }
        }
        fn walk(dir: &Path, depth: u32, set: &mut std::collections::HashSet<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    if depth < 5 {
                        walk(&p, depth + 1, set);
                    }
                } else if let Some(n) = p.file_name().and_then(|n| n.to_str()).map(str::to_ascii_lowercase) {
                    // Outils livrés avec les jeux : jamais « le jeu ».
                    let helper = ["unins", "crash", "crs-", "setup", "redist", "vc_", "prereq", "launcher", "report", "helper", "install"];
                    if n.ends_with(".exe") && !helper.iter().any(|h| n.contains(h)) {
                        set.insert(n);
                    }
                }
            }
        }
        let mut set = std::collections::HashSet::new();
        roots.sort();
        roots.dedup();
        for r in &roots {
            walk(r, 0, &mut set);
        }
        crate::log(&format!("FPS : {} programmes de jeux installés repérés", set.len()));
        let _ = GAME_EXES.set(set);
    });
}

/// Écran de la plus grande fenêtre visible du jeu (ou d'un de ses parents) : l'overlay s'y affiche, même
/// quand une autre appli a le premier plan. Lit seulement la liste des fenêtres, rien dans le jeu.
fn game_monitor(game: u32) -> Option<RECT> {
    use windows_sys::Win32::Foundation::LPARAM;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, IsWindowVisible};
    if game == 0 {
        return None;
    }
    let table = PM_TABLE.lock().unwrap().clone().unwrap_or_default();
    struct Seek {
        game: u32,
        table: std::collections::HashMap<u32, (u32, String)>,
        best: (i64, HWND),
    }
    unsafe extern "system" fn each(w: HWND, l: LPARAM) -> i32 {
        unsafe {
            let s = &mut *(l as *mut Seek);
            let mut pid = 0;
            GetWindowThreadProcessId(w, &mut pid);
            let mut r: RECT = std::mem::zeroed();
            if IsWindowVisible(w) != 0 && (pid == s.game || belongs(s.game, pid, &s.table)) && GetWindowRect(w, &mut r) != 0 {
                let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if area > s.best.0 {
                    s.best = (area, w);
                }
            }
        }
        1
    }
    let mut s = Seek { game, table, best: (0, std::ptr::null_mut()) };
    unsafe {
        EnumWindows(Some(each), &mut s as *mut Seek as LPARAM);
        if s.best.1.is_null() {
            return None;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(MonitorFromWindow(s.best.1, MONITOR_DEFAULTTONEAREST), &mut mi) != 0).then_some(mi.rcMonitor)
    }
}
/// Mode mesure (scripts de banc) : FPS, temps d'image et 1 % low du jeu écrits au journal toutes les 2 s.
static BENCH: AtomicBool = AtomicBool::new(false);

/// Fréquence de mise à jour de l'overlay (réglage « Mise à jour »), en ms.
static REFRESH_MS: AtomicU32 = AtomicU32::new(500);
/// Dernière appli pour laquelle des FPS ont été affichés (journal, une ligne par appli).
static PM_SHOWN: AtomicU32 = AtomicU32::new(0);

/// Table processus -> (parent, nom de l'exe), lue par un instantané Toolhelp (lecture seule, aucun handle ouvert).
pub fn process_table() -> std::collections::HashMap<u32, (u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    let mut m = std::collections::HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return m;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            m.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    m
}

/// `pid` est `target` ou un de ses descendants (4 niveaux) : Chromium, Electron et WebView2 affichent
/// leurs images depuis un processus GPU enfant, pas depuis celui qui possède la fenêtre.
fn belongs(pid: u32, target: u32, table: &std::collections::HashMap<u32, (u32, String)>) -> bool {
    let mut p = pid;
    for _ in 0..5 {
        if p == target {
            return true;
        }
        match table.get(&p) {
            Some(&(parent, _)) if parent != 0 && parent != p => p = parent,
            _ => return false,
        }
    }
    false
}

/// Colonnes utiles d'une ligne d'en-tête CSV de PresentMon (noms 1.x ou 2.x, casse ignorée).
/// Avec la génération d'images (DLSS, FSR, XeSS frame gen), chaque image générée est une ligne à
/// 0 ms depuis la précédente (PresentMon, ticket #388) : elle compte comme une image, voir `expand`.
/// Renvoie (processus, temps entre deux images affichées, temps entre deux images envoyées). L'affiché est
/// ce que montre le compteur NVIDIA : il compte les images générées par le pilote, que le jeu n'envoie pas.
fn pm_columns(header: &str) -> Option<(usize, Option<usize>, usize)> {
    let cols: Vec<String> = header.split(',').map(|c| c.trim().to_ascii_lowercase()).collect();
    let find = |names: &[&str]| names.iter().find_map(|n| cols.iter().position(|c| c == n));
    let pid = find(&["processid"])?;
    let shown = find(&["displayedtime", "msbetweendisplaychange"]);
    let sent = find(&["msbetweenpresents", "frametime"])?;
    Some((pid, shown, sent))
}

/// (processus, ms entre deux images) d'une ligne de données ; 0 = image générée. « NA » : ignorée.
fn pm_row(line: &str, pid: usize, ms: usize) -> Option<(u32, f64)> {
    let f: Vec<&str> = line.split(',').collect();
    let v: f64 = f.get(ms)?.trim().parse().ok()?;
    (v >= 0.0).then_some(())?;
    Some((f.get(pid)?.trim().parse().ok()?, v))
}

/// Temps de chaque image affichée : une image du jeu suivie de ses images générées (0 ms) se partage
/// son temps avec elles. 16,6 puis 0, 0 (frame gen x3) = trois images de 5,5 ms, comme le compteur NVIDIA.
fn expand(ms: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut out: Vec<f64> = vec![];
    let mut group = 0usize; // images du groupe en cours (l'image du jeu + ses générées)
    for v in ms {
        if v > 0.0 || out.is_empty() {
            out.push(v);
            group = 1;
        } else {
            group += 1;
            let start = out.len() + 1 - group;
            let t = out[start] * (group - 1) as f64 / group as f64;
            out[start..].iter_mut().for_each(|x| *x = t);
            out.push(t);
        }
    }
    out
}

/// Capture tant que l'overlay est visible. Faux si PresentMon est absent ou s'arrête aussitôt (droits).
fn run_presentmon() -> bool {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;
    // Celui livré par l'installateur, à côté de clipper.exe ; sinon celui du PATH (winget).
    let bundled = std::env::current_exe().ok().map(|e| e.with_file_name("presentmon.exe")).filter(|p| p.is_file());
    let mut cmd = std::process::Command::new(bundled.as_deref().unwrap_or("presentmon".as_ref()));
    cmd.creation_flags(0x0800_0000 | 0x0000_4000) // sans fenêtre, priorité sous la normale
        // Métriques 2.x (pas --v1_metrics) : une ligne par image affichée, images générées comprises.
        .args(["--output_stdout", "--no_console_stats", "--session_name", "ClipperPresentMon", "--stop_existing_session"])
        // Suivi jusqu'à l'affichage indispensable : en plein écran (flip indépendant), une image n'est « terminée »
        // qu'une fois affichée ; sans ce suivi, PresentMon ne sortait aucune ligne pour les jeux.
        .args(["--no_track_input"])
        // Mesures 2.x : une ligne par image affichée, images générées (DLSS / FSR frame gen) comprises, avec
        // leur type. Copie entre deux GPU (PC portable) suivie, sinon les images du jeu manquaient.
        .args(["--v2_metrics", "--track_frame_type", "--track_hybrid_present"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        crate::log("FPS : PresentMon introuvable, mesure par ETW DXGI");
        return false;
    };
    let Some(out) = child.stdout.take() else { return false };
    PM_STARTING.store(true, Ordering::Relaxed);
    let child = std::sync::Arc::new(Mutex::new(child));
    // Overlay masqué : on arrête PresentMon, la lecture ci-dessous se termine.
    let (c, me) = (child.clone(), std::thread::current());
    let watcher = std::thread::spawn(move || {
        let w = std::thread::current();
        WAKE.lock().unwrap().push(w.clone());
        // PresentMon tourne seulement overlay affiché avec des mesures d'images (voir `fps_wanted`) : masqué,
        // il est arrêté et ne coûte plus rien. Alt+F le relance (« — » pendant ~1 s, choix assumé).
        while matches!(c.lock().unwrap().try_wait(), Ok(None)) && fps_wanted() {
            std::thread::park_timeout(std::time::Duration::from_secs(1));
        }
        WAKE.lock().unwrap().retain(|t| t.id() != w.id());
        let _ = c.lock().unwrap().kill();
        stop_session("ClipperPresentMon"); // tué, PresentMon ne ferme pas sa session ETW : on la ferme
        me.unpark();
    });
    let started = std::time::Instant::now();
    let (mut cols, mut rows) = (None, 0u64);
    let (mut seen, mut shown_ok) = (0u32, false);
    let (mut table, mut table_at) = (process_table(), std::time::Instant::now());
    *PM_TABLE.lock().unwrap() = Some(table.clone());
    for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
        let Some(cc) = cols else {
            cols = pm_columns(&line);
            if cols.is_some() {
                PM_ON.store(true, Ordering::Relaxed);
                crate::log("FPS : PresentMon actif");
            }
            continue;
        };
        // Colonne « affichée » d'abord ; restée vide sur 500 lignes (PC portable dont l'écran passe par la puce
        // intégrée, par exemple) : on revient aux images envoyées par le jeu.
        let col = match cc.1 {
            Some(s) if shown_ok || seen < 500 => s,
            _ => cc.2,
        };
        seen += 1;
        let Some((pid, ms)) = pm_row(&line, cc.0, col) else { continue };
        if col != cc.2 && !shown_ok {
            shown_ok = true;
            crate::log("FPS : images comptées à l'affichage (frame gen compris)");
        }
        if seen == 500 && !shown_ok {
            crate::log("FPS : temps d'affichage absent, images comptées à l'envoi");
        }
        rows += 1;
        // Processus inconnu (nouveau jeu, nouvel enfant) : instantané rafraîchi, au plus une fois par seconde.
        if !table.contains_key(&pid) && table_at.elapsed().as_secs() >= 1 {
            (table, table_at) = (process_table(), std::time::Instant::now());
            *PM_TABLE.lock().unwrap() = Some(table.clone());
        }
        let mut q = PM_FRAMES.lock().unwrap();
        q.push_back((std::time::Instant::now(), ms, pid));
        if q.len() > 60_000 {
            q.pop_front();
        }
    }
    let _ = child.lock().unwrap().kill();
    let _ = watcher.join();
    PM_ON.store(false, Ordering::Relaxed);
    PM_STARTING.store(false, Ordering::Relaxed);
    PM_FRAMES.lock().unwrap().clear();
    GAME.store(0, Ordering::Relaxed);
    // Arrêt immédiat sans aucune ligne : pas les droits ou version incompatible.
    if rows == 0 && cols.is_none() && started.elapsed() < std::time::Duration::from_secs(3) && VISIBLE.load(Ordering::Relaxed) {
        crate::log(&format!("FPS : PresentMon s'est arrêté aussitôt (admin = {}), mesure par ETW DXGI", crate::elevate::is_elevated()));
        return false;
    }
    true
}

/// Valeurs affichées : FPS moyens, temps d'image moyen, 1 % low.
struct FpsText {
    fps: String,
    ft: String,
    low: String,
}

impl FpsText {
    fn same(v: &str, w: &str) -> Self {
        Self { fps: v.into(), ft: w.into(), low: w.into() }
    }
}

/// Historique gardé pour le 1 % low (assez d'images pour qu'un 1 % ait un sens).
const LOW_WINDOW_SECS: f64 = 10.0;

/// 1 % low en FPS : moyenne des 1 % d'images les plus lentes (méthode de CapFrameX et d'Afterburner).
/// Moins de 20 images : pas de valeur (un seul à-coup ferait tout le chiffre).
fn one_percent_low(frametimes_ms: &[f64]) -> Option<f64> {
    if frametimes_ms.len() < 20 {
        return None;
    }
    let mut v = frametimes_ms.to_vec();
    v.sort_by(|a, b| b.total_cmp(a));
    let k = v.len().div_ceil(100);
    let worst = v[..k].iter().sum::<f64>() / k as f64;
    (worst > 0.0).then(|| 1000.0 / worst)
}

/// FPS et temps d'image moyens sur la période de rafraîchissement (0,5 s minimum : PresentMon envoie
/// ses lignes par paquets), 1 % low sur les 10 dernières secondes.
fn pm_text() -> FpsText {
    let now = std::time::Instant::now();
    let mut q = PM_FRAMES.lock().unwrap();
    while q.front().is_some_and(|(t, _, _)| now.duration_since(*t).as_secs_f64() > LOW_WINDOW_SECS) {
        q.pop_front();
    }
    // PresentMon livre ses lignes par paquets (jusqu'à ~1 s d'écart) : on compte les images reçues
    // dans les 2 dernières secondes, et on moyenne les dernières sur la période de rafraîchissement.
    const STALE_SECS: f64 = 2.0;
    let live = |f: &&(std::time::Instant, f64, u32)| now.duration_since(f.0).as_secs_f64() <= STALE_SECS;
    let mut count = std::collections::HashMap::<u32, usize>::new();
    q.iter().filter(live).for_each(|f| *count.entry(f.2).or_default() += 1);
    // Nos Present DXGI : par processus, la source qui voit le plus d'images l'emporte.
    let mut dx = DX_FRAMES.lock().unwrap();
    while dx.front().is_some_and(|(t, _, _)| now.duration_since(*t).as_secs_f64() > LOW_WINDOW_SECS) {
        dx.pop_front();
    }
    let mut dx_count = std::collections::HashMap::<u32, usize>::new();
    dx.iter().filter(live).for_each(|f| *dx_count.entry(f.2).or_default() += 1);
    for (p, n) in &dx_count {
        let c = count.entry(*p).or_default();
        *c = (*c).max(*n);
    }
    // Processus vu seulement par DXGI : la table des noms est rafraîchie (au plus une fois par seconde).
    {
        static AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let mut at = AT.lock().unwrap();
        let mut table = PM_TABLE.lock().unwrap();
        let missing = dx_count.keys().any(|p| !table.as_ref().is_some_and(|t| t.contains_key(p)));
        if missing && !at.is_some_and(|t| t.elapsed().as_secs() < 1) {
            *table = Some(process_table());
            *at = Some(now);
        }
    }
    let prev = GAME.load(Ordering::Relaxed);
    let game = {
        let table = PM_TABLE.lock().unwrap();
        let empty = std::collections::HashMap::new();
        let table = table.as_ref().unwrap_or(&empty);
        let g = pick_game(&count, table, TARGET.load(Ordering::Relaxed), crate::capture::fullscreen_foreground(), std::process::id(), prev);
        if g != Some(prev) {
            match g {
                Some(p) => {
                    crate::log(&format!("FPS : jeu détecté : {} ({p})", table.get(&p).map_or("?", |e| e.1.as_str())));
                }
                None if prev != 0 => crate::log("FPS : plus de jeu en cours"),
                None => {}
            }
        }
        g
    };
    GAME.store(game.unwrap_or(0), Ordering::Relaxed);
    let Some(src) = game else {
        pm_diag(&count);
        return FpsText::same("—", "—");
    };
    let span_ms = (REFRESH_MS.load(Ordering::Relaxed) as f64).max(500.0);
    // Images du jeu sur 10 s, images générées comprises (frame gen) : FPS, temps d'image et 1 % low.
    let pm_n = q.iter().filter(live).filter(|f| f.2 == src).count();
    let dx_n = dx_count.get(&src).copied().unwrap_or(0);
    // Source gardée tant que l'autre ne voit pas nettement plus d'images (10 %) : pas de va-et-vient.
    static LAST_SRC: AtomicU32 = AtomicU32::new(2);
    let was_dx = LAST_SRC.load(Ordering::Relaxed) == 1;
    let from_dx = if was_dx { pm_n <= dx_n + dx_n / 10 } else { dx_n > pm_n + pm_n / 10 };
    {
        // Changement de source journalisé au plus toutes les 30 s (avant : une ligne toutes les ~2 s quand
        // les deux sources se suivaient de près).
        static SRC_LOGGED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        if LAST_SRC.swap(from_dx as u32, Ordering::Relaxed) != from_dx as u32 && SRC_LOGGED.lock().unwrap().replace(now).is_none_or(|t| now.duration_since(t).as_secs() >= 30) {
            crate::log(&format!("FPS : source {} (2 s : PresentMon {pm_n} images, DXGI {dx_n})", if from_dx { "DXGI" } else { "PresentMon" }));
        }
    }
    let q = if from_dx { &*dx } else { &*q };
    let all = expand(q.iter().filter(|f| f.2 == src).map(|f| f.1));
    let (mut sum, mut n) = (0.0, 0usize);
    for v in all.iter().rev() {
        sum += v;
        n += 1;
        if sum >= span_ms {
            break;
        }
    }
    if n < 2 || sum <= 0.0 {
        return FpsText::same("0", "—");
    }
    let ft = sum / n as f64;
    let fps = 1000.0 / ft;
    let low = one_percent_low(&all);
    if PM_SHOWN.swap(src, Ordering::Relaxed) != src {
        let generated = q.iter().filter(|f| f.2 == src && f.1 == 0.0).count();
        crate::log(&format!("FPS : overlay affiche {fps:.0} FPS{} / {ft:.1} ms (processus {src}, {generated} images générées sur {} vues)", dlss_tag(src), all.len()));
    }
    if BENCH.load(Ordering::Relaxed) {
        static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
        let mut l = LAST.lock().unwrap();
        if l.is_none_or(|t| t.elapsed().as_secs_f64() >= 2.0) {
            *l = Some(now);
            // Sur 2 s : moyenne des temps d'image ; 1 % low sur les 10 s gardées.
            let recent: Vec<f64> = all.iter().rev().scan(0.0, |s, v| { *s += v; (*s <= 2000.0).then_some(*v) }).collect();
            let avg = recent.iter().sum::<f64>() / recent.len().max(1) as f64;
            crate::log(&format!("BENCH fps {:.1} ft {avg:.2} low1 {} n {} capture {}", 1000.0 / avg.max(0.001), low.map_or("-".into(), |l| format!("{l:.1}")), recent.len(), !crate::capture::is_idle()));
        }
    }
    FpsText {
        // « 114 DLSS FG » : comme le compteur NVIDIA, quand le jeu a chargé le DLSS.
        fps: format!("{fps:.0}{}", dlss_tag(src)),
        ft: format!("{ft:.1} ms").replace('.', ","),
        low: low.map_or("—".into(), |l| format!("{l:.0}")),
    }
}

/// Journal, toutes les 10 s au plus : une appli plein écran au premier plan mais aucun jeu compté. Dit quels
/// processus envoient des images, pour comprendre un compteur vide (jeu non détecté, images non remontées).
fn pm_diag(count: &std::collections::HashMap<u32, usize>) {
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let fg = TARGET.load(Ordering::Relaxed);
    if fg == 0 || !crate::capture::fullscreen_foreground() {
        return;
    }
    let mut last = LAST.lock().unwrap();
    if last.is_some_and(|t| t.elapsed().as_secs() < 10) {
        return;
    }
    *last = Some(std::time::Instant::now());
    let table = PM_TABLE.lock().unwrap();
    let name = |p: u32| table.as_ref().and_then(|t| t.get(&p)).map_or("?".to_string(), |e| e.1.clone());
    let mut top: Vec<(u32, usize)> = count.iter().map(|(p, n)| (*p, *n)).collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let top: Vec<String> = top.iter().take(3).map(|(p, n)| format!("{} ({p}) {n}", name(*p))).collect();
    crate::log(&format!("FPS : plein écran {} ({fg}) sans jeu compté ; images des 2 dernières s : {}", name(fg), if top.is_empty() { "aucune".into() } else { top.join(", ") }));
}

fn fps_text() -> FpsText {
    if PM_ON.load(Ordering::Relaxed) {
        return pm_text();
    }
    // PresentMon lancé mais pas encore de sortie (il écrit par blocs de 4 Ko : rien tant qu'aucun jeu n'affiche d'images).
    if PM_STARTING.load(Ordering::Relaxed) {
        return FpsText::same("0", "—");
    }
    if !ETW_OK.load(Ordering::Relaxed) {
        return FpsText::same("admin", "admin");
    }
    let mut freq = 0i64;
    let mut now = 0i64;
    unsafe {
        windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut freq);
        windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut now);
    }
    let mut q = PRESENTS.lock().unwrap();
    // L'ETW temps réel livre ses événements par paquets (~1 s) : la demi-seconde de calcul se mesure
    // depuis la dernière image reçue, pas depuis maintenant (sinon « 0 » entre deux paquets).
    // « 0 » seulement après 2 s sans image ; 1 % low sur ces 2 s.
    while q.front().is_some_and(|&t| now - t > freq * 2) {
        q.pop_front();
    }
    let n = q.len();
    if n < 2 {
        return FpsText::same("0", "—");
    }
    let last = q[n - 1];
    let first = q.iter().position(|&t| last - t <= freq / 2).unwrap_or(0).min(n - 2);
    let ft = (last - q[first]) as f64 * 1000.0 / freq as f64 / (n - 1 - first) as f64;
    let gaps: Vec<f64> = q.iter().zip(q.iter().skip(1)).map(|(a, b)| (b - a) as f64 * 1000.0 / freq as f64).collect();
    FpsText {
        fps: format!("{:.0}", 1000.0 / ft),
        ft: format!("{ft:.1} ms").replace('.', ","),
        low: one_percent_low(&gaps).map_or("—".into(), |l| format!("{l:.0}")),
    }
}

fn foreground_pid() -> u32 {
    let mut pid = 0;
    // Propriétaire de la fenêtre au premier plan : simple lecture, aucun handle sur le jeu.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid
}

/// Chiffres d'une carte : utilisation %, température °C, (mémoire utilisée, totale) en octets.
struct GpuStat {
    util: Option<u32>,
    temp: Option<u32>,
    mem: Option<(u64, u64)>,
}

/// NVIDIA : NVML (mêmes chiffres que le pilote, température comprise). Autres cartes (AMD, Intel, puce
/// intégrée) : compteurs Windows ; leur température n'est pas lisible sans le logiciel du fabricant.
fn gpu_stat(a: &crate::gpus::Adapter, nv: &Option<Nvml>, pdh: &std::collections::HashMap<(i32, u32), crate::gpus::Sample>) -> GpuStat {
    if let Some((n, d)) = nv.as_ref().and_then(|n| n.by_name(&a.name).map(|d| (n, d))) {
        unsafe {
            let (mut u, mut t, mut m) = ([0u32; 2], 0u32, [0u64; 3]);
            return GpuStat {
                util: ((n.util)(d, &mut u) == 0).then_some(u[0]),
                temp: ((n.temp)(d, 0, &mut t) == 0).then_some(t),
                mem: ((n.mem)(d, &mut m) == 0).then_some((m[2], m[0])),
            };
        }
    }
    let s = pdh.get(&a.luid()).copied().unwrap_or_default();
    GpuStat { util: s.util.map(|v| v.round() as u32), temp: None, mem: s.mem }
}

fn refresh(cpu: &mut Cpu, nv: &Option<Nvml>, th: &Option<Thermal>, gc: &Option<crate::gpus::Counters>) {
    let (items, chosen) = LOOK.lock().unwrap().as_ref().map(|l| (l.items.clone(), l.gpus.clone())).unwrap_or_default();
    TARGET.store(foreground_pid(), Ordering::Relaxed);
    let c = cpu.sample();
    let wants_gpu = items.iter().any(|k| k == "gpu" || k == "gpu_temp" || k == "vram");
    let sel = if wants_gpu { selected_gpus(&chosen) } else { vec![] };
    // Compteurs Windows lus seulement pour une carte sans NVML (AMD, Intel) : « GPU Engine » compte des
    // centaines d'entrées, NVML ne coûte presque rien.
    let need_pdh = sel.iter().any(|a| nv.as_ref().and_then(|n| n.by_name(&a.name)).is_none());
    let pdh = match gc {
        Some(g) if need_pdh => g.sample(),
        _ => Default::default(),
    };
    let gpus: Vec<(&crate::gpus::Adapter, GpuStat)> = sel.into_iter().map(|a| (a, gpu_stat(a, nv, &pdh))).collect();
    // Une seule carte : « GPU » ; plusieurs : « GPU 4060 », « GPU 780M ».
    let tag = |base: &str, a: &crate::gpus::Adapter| if gpus.len() > 1 { format!("{base} {}", a.short) } else { base.to_string() };
    let (free, total) = crate::tools::ram();
    // Même unité que le Gestionnaire des tâches (1 Go = 1024³ octets).
    let go = |b: u64| format!("{:.1}", b as f64 / 1_073_741_824.0).replace('.', ",");
    // Bureau ou appli fenêtrée : elles ne redessinent qu'au moindre changement, leurs « FPS » sautent
    // de 0 à 144 sans rien dire. Chiffres d'images seulement pour une appli plein écran (un jeu).
    // Avec PresentMon, le jeu est détecté parmi tous les processus (voir pick_game), fenêtré ou non.
    // Sans lui (ETW DXGI) : seulement l'appli plein écran au premier plan.
    let f = if PM_ON.load(Ordering::Relaxed) || PM_STARTING.load(Ordering::Relaxed) || crate::capture::fullscreen_foreground() { fps_text() } else { FpsText::same("—", "—") };
    let rows = items
        .iter()
        .flat_map(|k| {
            // Mesures GPU : une ligne par carte choisie, clé « gpu@780M » pour sa courbe.
            let per_gpu = |label: &str, val: &dyn Fn(&GpuStat) -> String| -> Vec<(String, (String, String))> {
                gpus.iter().enumerate().map(|(i, (a, s))| (if i == 0 { k.clone() } else { format!("{k}@{}", a.short) }, (tag(label, a), val(s)))).collect()
            };
            match k.as_str() {
                "gpu" => return per_gpu("GPU", &|s| s.util.map_or("—".into(), |v| format!("{v} %"))),
                "gpu_temp" => return per_gpu("GPU °C", &|s| s.temp.map_or("n/d".into(), |v| v.to_string())),
                "vram" => return per_gpu("VRAM", &|s| s.mem.map_or("—".into(), |(u, t)| format!("{}/{} Go", go(u), go(t)))),
                _ => {}
            }
            Some((k.clone(), match k.as_str() {
                "fps" => ("FPS".into(), f.fps.clone()),
                "low1" => ("1% LOW".into(), f.low.clone()),
                // Temps moyen d'une image (1000 / FPS) : plus il est bas et régulier, plus le jeu est fluide.
                "frametime" => ("MS/IMG".into(), f.ft.clone()),
                "cpu" => ("CPU".into(), format!("{c:.0} %")),
                "cpu_temp" => ("CPU °C".into(), th.as_ref().and_then(Thermal::celsius).map_or("n/d".into(), |t| format!("{t:.0}"))),
                "ram" => ("RAM".into(), format!("{}/{} Go", go(total - free), go(total))),
                _ => return vec![],
            }))
            .into_iter()
            .collect()
        })
        .collect::<Vec<(String, (String, String))>>();
    // Courbes : un point par rafraîchissement, pour chaque mesure affichée.
    let mut hist = HIST.lock().unwrap();
    hist.retain(|(k, _)| rows.iter().any(|(r, _)| r == k));
    for (k, (_, v)) in &rows {
        let i = hist.iter().position(|(h, _)| h == k).unwrap_or_else(|| {
            hist.push((k.clone(), VecDeque::with_capacity(HIST_LEN)));
            hist.len() - 1
        });
        let q = &mut hist[i].1;
        if q.len() == HIST_LEN {
            q.pop_front();
        }
        q.push_back(leading_number(v));
    }
    drop(hist);
    *KEYS.lock().unwrap() = rows.iter().map(|(k, _)| k.clone()).collect();
    *TEXT.lock().unwrap() = rows.into_iter().map(|(_, r)| r).collect();
}

// ---------- Fenêtre ----------

/// Mise à l'échelle Windows de l'écran du jeu, en % (100, 125, 150…), relue par `place`.
static SCALE: AtomicU32 = AtomicU32::new(100);

/// Taille du texte en pixels réels : 13, 16 ou 20 px à 100 %, multipliée par la mise à l'échelle de l'écran
/// du jeu (avant : pixels fixes, l'overlay était minuscule à 125 % et 150 %).
fn font_px(size: &str) -> i32 {
    let base = match size {
        "s" => 13,
        "l" => 20,
        _ => 16,
    };
    (base * SCALE.load(Ordering::Relaxed) as i32 + 50) / 100
}

/// Mise à l'échelle (%) de l'écran qui contient `r`.
fn monitor_scale(r: &RECT) -> u32 {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let c = POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 };
    let (mut dx, mut dy) = (96u32, 96u32);
    unsafe { GetDpiForMonitor(MonitorFromPoint(c, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    (dx.max(96) * 100 / 96).clamp(100, 400)
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(h, &mut ps);
            let mut r: RECT = std::mem::zeroed();
            GetClientRect(h, &mut r);
            let bg = CreateSolidBrush(0x0013_1914); // fond vert-de-gris (DA E), en BGR
            FillRect(dc, &r, bg);
            DeleteObject(bg as _);
            let px = font_px(&LOOK.lock().unwrap().as_ref().map(|l| l.size.clone()).unwrap_or_default());
            let font = make_font(px);
            let old = SelectObject(dc, font as _);
            SetBkMode(dc, TRANSPARENT as i32);
            let pad = px / 2;
            let row = px + 6 * SCALE.load(Ordering::Relaxed) as i32 / 100;
            let graphs = LOOK.lock().unwrap().as_ref().map(|l| l.graphs.clone()).unwrap_or_default();
            let keys = KEYS.lock().unwrap().clone();
            let gw = graph_width(px, &keys, &graphs);
            let hist = HIST.lock().unwrap();
            for (i, (k, v)) in TEXT.lock().unwrap().iter().enumerate() {
                let y = pad + i as i32 * row;
                let draw = |s: &str, x: i32, right: bool, color: u32| {
                    let t: Vec<u16> = s.encode_utf16().collect();
                    SetTextColor(dc, color);
                    let mut rr = RECT { left: x, top: y, right: r.right - pad - gw, bottom: y + row };
                    DrawTextW(dc, t.as_ptr(), t.len() as i32, &mut rr, if right { DT_RIGHT } else { DT_LEFT } | DT_SINGLELINE | DT_NOCLIP);
                };
                draw(k, pad, false, 0x0086_978D); // gris-vert
                draw(v, pad, true, if i == 0 && k == "FPS" { 0x002E_7DFF } else { 0x00E2_ECE9 }); // orange signal / texte
                let key = keys.get(i).map(String::as_str).unwrap_or("");
                if gw > 0 && graphs.iter().any(|g| g == key.split('@').next().unwrap_or(key)) {
                    if let Some((_, q)) = hist.iter().find(|(h, _)| h == key) {
                        let (x1, top) = (r.right - pad, y + 3);
                        draw_graph(dc, RECT { left: x1 - (gw - px / 2), top, right: x1, bottom: top + row - 7 }, q);
                    }
                }
            }
            drop(hist);
            SelectObject(dc, old);
            EndPaint(h, &ps);
            return 0;
        }
        DefWindowProcW(h, msg, w, l)
    }
}

/// Place réservée aux courbes (écart compris) : 0 si aucune mesure affichée n'en a.
fn graph_width(px: i32, keys: &[String], graphs: &[String]) -> i32 {
    if keys.iter().any(|k| graphs.iter().any(|g| g == k.split('@').next().unwrap_or(k))) { px * 5 + px / 2 } else { 0 }
}

/// Courbe façon Afterburner : ligne de base discrète, tracé orange, échelle 0 -> max récent.
unsafe fn draw_graph(dc: HDC, r: RECT, q: &VecDeque<f32>) {
    unsafe {
        let base = CreatePen(PS_SOLID, 1, 0x002E_3A2B);
        let old = SelectObject(dc, base as _);
        MoveToEx(dc, r.left, r.bottom, std::ptr::null_mut());
        LineTo(dc, r.right, r.bottom);
        let max = q.iter().copied().filter(|v| v.is_finite()).fold(0f32, f32::max).max(1.0) * 1.1;
        let (w, h) = ((r.right - r.left) as f32, (r.bottom - r.top) as f32);
        // Les derniers points à droite : la courbe défile vers la gauche.
        let x0 = r.right as f32 - w * (q.len().saturating_sub(1)) as f32 / (HIST_LEN - 1) as f32;
        let pts: Vec<(usize, windows_sys::Win32::Foundation::POINT)> = q
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .map(|(i, v)| (i, windows_sys::Win32::Foundation::POINT { x: (x0 + w * i as f32 / (HIST_LEN - 1) as f32) as i32, y: (r.bottom as f32 - v / max * h) as i32 }))
            .collect();
        let line = CreatePen(PS_SOLID, 1, 0x002E_7DFF);
        SelectObject(dc, line as _);
        // Un trou (valeur absente) coupe la ligne au lieu de relier deux points éloignés.
        for seg in pts.chunk_by(|a, b| b.0 == a.0 + 1) {
            let p: Vec<_> = seg.iter().map(|(_, p)| *p).collect();
            if p.len() > 1 {
                Polyline(dc, p.as_ptr(), p.len() as i32);
            }
        }
        SelectObject(dc, old);
        DeleteObject(base as _);
        DeleteObject(line as _);
    }
}

/// Police à chiffres fixes : les colonnes restent alignées quand les valeurs changent. Gardée tant que la
/// taille ne change pas (fil de la fenêtre de l'overlay seulement) ; l'appelant ne la détruit pas.
unsafe fn make_font(px: i32) -> HFONT {
    thread_local! {
        static FONT: std::cell::Cell<(i32, usize)> = const { std::cell::Cell::new((0, 0)) };
    }
    FONT.with(|f| {
        let (cur, h) = f.get();
        if cur == px && h != 0 {
            return h as HFONT;
        }
        let face: Vec<u16> = "Consolas".encode_utf16().chain(Some(0)).collect();
        let new = unsafe { CreateFontW(-px, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, CLEARTYPE_QUALITY as u32, 0, face.as_ptr()) };
        if h != 0 {
            unsafe { DeleteObject(h as _) };
        }
        f.set((px, new as usize));
        new
    })
}

/// Largeur = libellé le plus long + écart + valeur la plus longue, mesurés avec la vraie police.
fn measure(h: HWND, px: i32) -> i32 {
    unsafe {
        let dc = GetDC(h);
        let font = make_font(px);
        let old = SelectObject(dc, font as _);
        let w = |s: &str| {
            let t: Vec<u16> = s.encode_utf16().collect();
            let mut sz = std::mem::zeroed();
            GetTextExtentPoint32W(dc, t.as_ptr(), t.len() as i32, &mut sz);
            sz.cx
        };
        let rows = TEXT.lock().unwrap();
        let (kl, vl) = rows.iter().fold((0, 0), |(a, b), (k, v)| (a.max(w(k)), b.max(w(v))));
        drop(rows);
        SelectObject(dc, old);
        ReleaseDC(h, dc);
        let graphs = LOOK.lock().unwrap().as_ref().map(|l| l.graphs.clone()).unwrap_or_default();
        px / 2 * 2 + kl + px + vl + graph_width(px, &KEYS.lock().unwrap(), &graphs)
    }
}

/// Coin choisi de l'écran du jeu.
fn place(h: HWND, rows: usize, look: &Look) {
    unsafe {
        // Écran du jeu : celui de l'appli plein écran au premier plan. Sans jeu plein écran (clic sur
        // l'autre écran, fenêtre de Clipper…), l'overlay reste où il était au lieu de sauter d'écran.
        static SCREEN: Mutex<Option<RECT>> = Mutex::new(None);
        // Écran du jeu relu toutes les 2 s au plus (liste des fenêtres parcourue) : pas à chaque rafraîchissement.
        static SEEN: Mutex<Option<(u32, std::time::Instant)>> = Mutex::new(None);
        let mut screen = SCREEN.lock().unwrap();
        let game = GAME.load(Ordering::Relaxed);
        let fresh = SEEN.lock().unwrap().is_some_and(|(g, t)| g == game && t.elapsed().as_secs() < 2);
        if fresh && screen.is_some() {
        } else if let Some(r) = game_monitor(game) {
            *screen = Some(r);
            *SEEN.lock().unwrap() = Some((game, std::time::Instant::now()));
        } else if crate::capture::fullscreen_foreground() || screen.is_none() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi);
            *screen = Some(mi.rcMonitor);
        }
        let m = screen.unwrap();
        drop(screen);
        // Échelle de cet écran : texte, interligne et marge suivent (100 %, 125 %, 150 %…). Changement
        // d'échelle (jeu passé sur l'autre écran) : largeur remesurée.
        let sc = monitor_scale(&m);
        if SCALE.swap(sc, Ordering::Relaxed) != sc {
            WIDTH.store(0, Ordering::Relaxed);
        }
        let px = font_px(&look.size);
        // Largeur qui ne rétrécit jamais tant que l'overlay reste affiché avec les mêmes réglages :
        // « 3 » puis « 144 » ne fait plus sauter le cadre à chaque rafraîchissement.
        let mw = measure(h, px);
        let w = WIDTH.fetch_max(mw, Ordering::Relaxed).max(mw);
        let hgt = px / 2 * 2 + rows as i32 * (px + 6 * sc as i32 / 100);
        let margin = 12 * sc as i32 / 100;
        // « tl », « tc », « tr », « bl », « bc », « br » : haut ou bas, puis gauche, milieu ou droite.
        let x = match look.corner.chars().nth(1) {
            Some('r') => m.right - w - margin,
            Some('c') => m.left + (m.right - m.left - w) / 2,
            _ => m.left + margin,
        };
        let y = if look.corner.starts_with('b') { m.bottom - hgt - margin } else { m.top + margin };
        // Fenêtre déplacée ou réglée seulement quand quelque chose change (et remise au premier plan toutes
        // les 2 s) : la reposer à chaque rafraîchissement forçait Windows à recomposer l'image du jeu.
        static LAST: Mutex<Option<((i32, i32, i32, i32, u8), std::time::Instant)>> = Mutex::new(None);
        let alpha = (look.opacity * 255.0) as u8;
        let key = (x, y, w, hgt, alpha);
        let mut last = LAST.lock().unwrap();
        let same = last.is_some_and(|(k, t)| k == key && t.elapsed().as_secs() < 2) && IsWindowVisible(h) != 0;
        if !same {
            SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            if last.is_none_or(|(k, _)| k.4 != alpha) {
                SetLayeredWindowAttributes(h, 0, alpha, LWA_ALPHA);
            }
            *last = Some((key, std::time::Instant::now()));
        }
    }
}

unsafe fn window_thread() {
    unsafe {
        let cls: Vec<u16> = "ClipperStats".encode_utf16().chain(Some(0)).collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.lpszClassName = cls.as_ptr();
        if RegisterClassW(&wc) == 0 {
            crate::log(&format!("overlay : RegisterClassW a échoué ({})", windows_sys::Win32::Foundation::GetLastError()));
        }
        // LAYERED + TRANSPARENT : les clics passent au jeu. NOACTIVATE : jamais le focus.
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let h = CreateWindowExW(ex, cls.as_ptr(), cls.as_ptr(), WS_POPUP, 0, 0, 160, 80, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
        if h.is_null() {
            crate::log(&format!("overlay : CreateWindowExW a échoué ({})", windows_sys::Win32::Foundation::GetLastError()));
            return;
        }
        // Jamais dans les clips ni les REC (ni dans les captures d'écran).
        SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE);
        OV.store(h, Ordering::Relaxed);
        PostMessageW(h, WM_APP, 0, 0); // apply() a pu passer avant la création de la fenêtre
        let load = || {
            let mut c = Cpu { last: (0, 0) };
            c.sample();
            (c, Nvml::load(), Thermal::open(), crate::gpus::Counters::open())
        };
        // Overlay activé (raccourci ou toujours) : sondes chargées dès le lancement, l'overlay s'affiche
        // sans les ~0,4 s de chargement. Mode « masqué » : jamais chargées (nvml.dll et PDH hors mémoire).
        let mut probes: Option<(Cpu, Option<Nvml>, Option<Thermal>, Option<crate::gpus::Counters>)> = ENABLED.load(Ordering::Relaxed).then(load);
        let tick = |probes: &mut Option<(Cpu, Option<Nvml>, Option<Thermal>, Option<crate::gpus::Counters>)>| {
            let (cpu, nv, th, gc) = probes.get_or_insert_with(load);
            refresh(cpu, nv, th, gc);
            let look = LOOK.lock().unwrap().clone();
            if let Some(look) = look {
                let rows = TEXT.lock().unwrap().len();
                place(h, rows, &look);
            }
            InvalidateRect(h, std::ptr::null(), 1);
        };
        // Masqué : GetMessageW dort, aucun réveil. Visible : WM_TIMER toutes les 500 ms.
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_APP if VISIBLE.load(Ordering::Relaxed) => {
                    SetTimer(h, 1, REFRESH_MS.load(Ordering::Relaxed), None);
                    tick(&mut probes);
                }
                WM_APP => {
                    KillTimer(h, 1);
                    ShowWindow(h, SW_HIDE);
                }
                WM_TIMER => tick(&mut probes),
                m if m == WM_APP + 1 => {
                    if probes.is_none() {
                        probes = Some(load());
                    }
                }
                _ => {
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

// ---------- ETW : événements Present de DXGI ----------

/// Microsoft-Windows-DXGI. Événement 42 = Present_Start (un par image affichée par le jeu).
const DXGI: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);

/// Microsoft-Windows-Kernel-Process : son événement 5 (ImageLoad) dit quelle DLL un processus charge.
const KERNEL_PROCESS: windows_sys::core::GUID = windows_sys::core::GUID::from_u128(0x22FB2CD6_0E7B_422B_A0C7_2FAD1FD0E716);
/// Processus -> 1 = DLSS (super résolution), 2 = DLSS frame gen. Rempli par les chargements de DLL.
static DLSS: Mutex<Option<std::collections::HashMap<u32, u8>>> = Mutex::new(None);

/// « DLSS FG », « DLSS » ou rien, pour le jeu mesuré.
fn dlss_tag(pid: u32) -> &'static str {
    match DLSS.lock().unwrap().as_ref().and_then(|m| m.get(&pid)) {
        Some(2) => " DLSS FG",
        Some(_) => " DLSS",
        None => "",
    }
}

/// Nom de DLL (UTF-16 dans les données de l'événement) : niveau DLSS qu'elle indique.
fn dlss_level(data: &[u8]) -> u8 {
    let s: String = char::decode_utf16(data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]))).map(|c| c.unwrap_or(' ').to_ascii_lowercase()).collect();
    if s.contains("nvngx_dlssg") || s.contains("sl.dlss_g") {
        2
    } else if s.contains("nvngx_dlss.dll") || s.contains("sl.dlss.dll") {
        1
    } else {
        0
    }
}

unsafe extern "system" fn on_event(r: *mut windows_sys::Win32::System::Diagnostics::Etw::EVENT_RECORD) {
    let h = unsafe { &(*r).EventHeader };
    if h.ProviderId.data1 == KERNEL_PROCESS.data1 {
        if h.EventDescriptor.Id == 5 {
            let (p, n) = unsafe { ((*r).UserData as *const u8, (*r).UserDataLength as usize) };
            // Les données sont alignées sur 2 octets ou non : on essaie les deux décalages.
            let data = if p.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(p, n) } };
            let lvl = dlss_level(data).max(dlss_level(data.get(1..).unwrap_or(&[])));
            if lvl > 0 {
                let mut m = DLSS.lock().unwrap();
                let e = m.get_or_insert_with(Default::default).entry(h.ProcessId).or_default();
                *e = (*e).max(lvl);
            }
        }
        return;
    }
    if h.EventDescriptor.Id == 42 {
        ALL_PRESENTS.fetch_add(1, Ordering::Relaxed);
        // Même forme que PM_FRAMES : (arrivée, ms depuis le Present précédent du processus, processus).
        static FREQ: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
        let freq = *FREQ.get_or_init(|| {
            let mut f = 0i64;
            unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut f) };
            f.max(1)
        });
        let mut last = DX_LAST.lock().unwrap();
        let last = last.get_or_insert_with(Default::default);
        let ms = last.insert(h.ProcessId, h.TimeStamp).map_or(0.0, |t| (h.TimeStamp - t) as f64 * 1000.0 / freq as f64);
        if ms > 0.0 {
            let mut q = DX_FRAMES.lock().unwrap();
            q.push_back((std::time::Instant::now(), ms, h.ProcessId));
            if q.len() > 60_000 {
                q.pop_front();
            }
        }
    }
    if h.EventDescriptor.Id == 42 && h.ProcessId == TARGET.load(Ordering::Relaxed) {
        let mut q = PRESENTS.lock().unwrap();
        q.push_back(h.TimeStamp);
        if q.len() > 1000 {
            q.pop_front();
        }
    }
}

/// La session ETW ne tourne que quand l'overlay est visible ET que FPS / temps d'image sont choisis.
/// Ferme une session ETW par son nom (PresentMon tué, ou Clipper qui quitte) : sinon Windows la garde
/// ouverte, avec ses tampons, jusqu'au prochain lancement.
fn stop_session(name: &str) {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    let n: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + n.len() * 2;
    let mut b = vec![0u8; size];
    let p = b.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
    unsafe {
        (*p).Wnode.BufferSize = size as u32;
        (*p).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, n.as_ptr(), p, EVENT_TRACE_CONTROL_STOP);
    }
}

/// À la fermeture de Clipper : sessions de mesure des FPS fermées.
pub fn shutdown() {
    stop_session("ClipperPresentMon");
    stop_session("ClipperFps");
    stop_session("ClipperDxgi");
}

/// Mesure des images active : overlay activé (raccourci ou toujours) avec FPS, temps d'image ou 1 % low.
/// PresentMon et la session ETW ne tournent que pendant que l'overlay est affiché : masqué, il ne coûte rien
/// (avant : ils restaient actifs pour un Alt+F instantané ; le prix est ~1 s de « — » à l'ouverture).
fn fps_wanted() -> bool {
    VISIBLE.load(Ordering::Relaxed) && ENABLED.load(Ordering::Relaxed) && LOOK.lock().unwrap().as_ref().is_some_and(|l| l.items.iter().any(|i| i == "fps" || i == "frametime" || i == "low1"))
}

fn etw_thread() {
    WAKE.lock().unwrap().push(std::thread::current());
    let name: Vec<u16> = "ClipperFps".encode_utf16().chain(Some(0)).collect();
    loop {
        if !fps_wanted() {
            std::thread::park(); // réveillé par apply()
            continue;
        }
        // PresentMon (Intel) d'abord : couvre DirectX, Vulkan et OpenGL. Absent ou refusé : ETW DXGI ci-dessous.
        if !PM_UNUSABLE.load(Ordering::Relaxed) {
            // En parallèle, nos propres « Present » DXGI : PresentMon ne sort parfois qu'une fraction des
            // images (suivi jusqu'à l'écran qui échoue, DLSS frame gen sur PC portable). On garde la source
            // qui voit le plus d'images du jeu (voir pm_text).
            static DX_RUNNING: AtomicBool = AtomicBool::new(false);
            if !DX_RUNNING.swap(true, Ordering::Relaxed) {
                std::thread::spawn(|| {
                    let n: Vec<u16> = "ClipperDxgi".encode_utf16().chain(Some(0)).collect();
                    while fps_wanted() && !PM_UNUSABLE.load(Ordering::Relaxed) {
                        dxgi_session(&n);
                    }
                    DX_FRAMES.lock().unwrap().clear();
                    DX_RUNNING.store(false, Ordering::Relaxed);
                });
            }
            if run_presentmon() {
                continue;
            }
            PM_UNUSABLE.store(true, Ordering::Relaxed);
        }
        dxgi_session(&name);
    }
}

/// Session ETW DXGI jusqu'à ce que la mesure ne soit plus voulue. Échec : pause de 5 s, puis retour.
fn dxgi_session(name: &[u16]) {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    {
        unsafe {
            // Propriétés + nom de session à la suite, comme l'exige l'API.
            let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
            let mut buf = vec![0u8; size];
            let props = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
            (*props).Wnode.BufferSize = size as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).Wnode.ClientContext = 1; // horodatage QPC
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
            // Session restée d'un ancien lancement : on l'arrête d'abord.
            ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).Wnode.BufferSize = size as u32;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            let code = StartTraceW(&mut handle, name.as_ptr(), props);
            if code != 0 {
                if ETW_OK.swap(false, Ordering::Relaxed) || !ETW_LOGGED.swap(true, Ordering::Relaxed) {
                    crate::log(&format!("FPS : session ETW refusée (code {code}, admin = {})", crate::elevate::is_elevated()));
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
                return;
            }
            // Tous les mots-clés : avec 0, certains fournisseurs à manifeste n'envoient pas Present_Start.
            EnableTraceEx2(handle, &DXGI, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_INFORMATION as u8, u64::MAX, 0, 0, std::ptr::null());
            // Chargements de DLL (mot-clé « image ») : DLSS repéré sans rien ouvrir dans le jeu.
            EnableTraceEx2(handle, &KERNEL_PROCESS, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_INFORMATION as u8, 0x40, 0, 0, std::ptr::null());
            let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            log.LoggerName = name.as_ptr() as *mut u16;
            log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            log.Anonymous2.EventRecordCallback = Some(on_event);
            let th = OpenTraceW(&mut log);
            if th.Value == u64::MAX {
                crate::log("FPS : OpenTraceW a échoué");
                std::thread::sleep(std::time::Duration::from_secs(5));
                return;
            }
            ETW_OK.store(true, Ordering::Relaxed);
            ALL_PRESENTS.store(0, Ordering::Relaxed);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(5));
                crate::log(&format!("FPS : ETW actif, {} images vues en 5 s (tous processus)", ALL_PRESENTS.load(Ordering::Relaxed)));
            });
            // Arrêt de la session quand l'overlay est masqué : ProcessTrace rend alors la main.
            let stop_name = name.to_vec();
            let watcher = std::thread::spawn(move || {
                let me = std::thread::current();
                WAKE.lock().unwrap().push(me.clone());
                while fps_wanted() {
                    std::thread::park();
                }
                WAKE.lock().unwrap().retain(|t| t.id() != me.id());
                let mut b = vec![0u8; size];
                let p = b.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
                (*p).Wnode.BufferSize = size as u32;
                (*p).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
                ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, stop_name.as_ptr(), p, EVENT_TRACE_CONTROL_STOP);
            });
            ProcessTrace(&th, 1, std::ptr::null(), std::ptr::null());
            CloseTrace(th);
            let _ = watcher.join();
            PRESENTS.lock().unwrap().clear();
        }
    }
}

#[cfg(test)]
mod presentmon_csv {
    #[test]
    fn graph_reads_first_number_of_each_value() {
        assert_eq!(super::leading_number("1,4/8,0 Go"), 1.4);
        assert_eq!(super::leading_number("61"), 61.0);
        assert_eq!(super::leading_number("6,9 ms"), 6.9);
        assert!(super::leading_number("—").is_nan());
        assert!(super::leading_number("n/d").is_nan());
    }

    #[test]
    fn one_percent_low_is_mean_of_slowest_frames() {
        // 99 images à 10 ms (100 FPS) et 1 à-coup de 50 ms : 1 % low = 20 FPS.
        let mut v = vec![10.0; 99];
        v.push(50.0);
        assert_eq!(super::one_percent_low(&v).map(|x| x.round()), Some(20.0));
        // 200 images : les 2 plus lentes (40 et 60 ms) donnent 20 FPS.
        let mut v = vec![8.0; 198];
        v.extend([40.0, 60.0]);
        assert_eq!(super::one_percent_low(&v).map(|x| x.round()), Some(20.0));
        assert_eq!(super::one_percent_low(&[10.0; 5]), None);
    }

    #[test]
    fn generated_frames_count_as_frames() {
        // En-tête PresentMon 2 (extrait) ; une image générée a 0 ms depuis la précédente.
        let h = "Application,ProcessID,SwapChainAddress,FrameType,MsBetweenPresents,MsUntilDisplayed,MsBetweenDisplayChange";
        let c = super::pm_columns(h).unwrap();
        assert_eq!(c, (1, Some(6), 4));
        assert_eq!(super::pm_row("tlou-ii.exe,8440,0x1,Application,16.6,20.1,8.3", c.0, c.2), Some((8440, 16.6)));
        assert_eq!(super::pm_row("tlou-ii.exe,8440,0x1,NVIDIA DLSS-FG,0.0,NA,NA", c.0, c.2), Some((8440, 0.0)));
        assert_eq!(super::pm_row("tlou-ii.exe,8440,0x1,Application,NA,NA,NA", c.0, c.2), None);
        // Frame gen x3 à 60 i/s : 180 images/s de 5,5 ms ; sans frame gen, rien ne change.
        let e = super::expand([16.5, 0.0, 0.0, 16.5, 0.0, 0.0].into_iter());
        assert_eq!(e, vec![5.5; 6]);
        assert_eq!(super::expand([10.0, 12.0].into_iter()), vec![10.0, 12.0]);
        let x2 = super::expand([20.0, 0.0, 10.0].into_iter());
        assert_eq!(x2, vec![10.0, 10.0, 10.0]);
    }

    #[test]
    fn game_is_detected_and_kept() {
        // Jeu (40), Clipper (10) et son WebView2 (20), Chrome (50), bureau (60).
        let t: std::collections::HashMap<u32, (u32, String)> =
            [(10, (1, "clipper.exe")), (20, (10, "msedgewebview2.exe")), (40, (1, "VALORANT-Win64-Shipping.exe")), (50, (1, "chrome.exe")), (60, (1, "explorer.exe"))]
                .into_iter()
                .map(|(p, (pp, n))| (p, (pp, n.to_string())))
                .collect();
        let c = |v: &[(u32, usize)]| v.iter().copied().collect::<std::collections::HashMap<u32, usize>>();
        // Jeu plein écran au premier plan : détecté.
        assert_eq!(super::pick_game(&c(&[(40, 300), (20, 90)]), &t, 40, true, 10, 0), Some(40));
        // La barre de Clipper prend le focus : le jeu reste le jeu (pas les images de la barre).
        assert_eq!(super::pick_game(&c(&[(40, 300), (20, 90)]), &t, 10, false, 10, 40), Some(40));
        // Alt+Tab vers Chrome ou le bureau : toujours le jeu tant qu'il affiche.
        assert_eq!(super::pick_game(&c(&[(40, 300), (50, 120)]), &t, 50, true, 10, 40), Some(40));
        assert_eq!(super::pick_game(&c(&[(40, 300)]), &t, 60, false, 10, 40), Some(40));
        // Jeu fermé : plus rien, même avec Chrome en plein écran.
        assert_eq!(super::pick_game(&c(&[(50, 120)]), &t, 50, true, 10, 40), None);
        // Jeu fenêtré : détecté s'il affiche au moins 30 i/s, pas une appli qui redessine parfois.
        assert_eq!(super::pick_game(&c(&[(40, 240)]), &t, 40, false, 10, 0), Some(40));
        assert_eq!(super::pick_game(&c(&[(40, 12)]), &t, 40, false, 10, 0), None);
    }

    #[test]
    fn child_gpu_process_counts_for_the_window_owner() {
        // clipper (10) -> msedgewebview2 (20) -> processus GPU (30) ; jeu sans rapport (40).
        let t: std::collections::HashMap<u32, (u32, String)> =
            [(10, (1, "clipper.exe")), (20, (10, "msedgewebview2.exe")), (30, (20, "msedgewebview2.exe")), (40, (1, "jeu.exe"))]
                .into_iter()
                .map(|(p, (pp, n))| (p, (pp, n.to_string())))
                .collect();
        assert!(super::belongs(30, 10, &t));
        assert!(super::belongs(10, 10, &t));
        assert!(!super::belongs(40, 10, &t));
        assert!(!super::belongs(99, 10, &t));
    }

    #[test]
    fn reads_columns_by_name() {
        let v1 = "Application,ProcessID,SwapChainAddress,Runtime,SyncInterval,PresentFlags,Dropped,TimeInSeconds,msInPresentAPI,msBetweenPresents";
        let c = super::pm_columns(v1).unwrap();
        assert_eq!(super::pm_row("game.exe,4242,0x1,DXGI,0,0,0,1.5,0.2,6.944", c.0, c.2), Some((4242, 6.944)));
        let v2 = "Application,ProcessID,SwapChainAddress,PresentRuntime,CPUStartTime,FrameTime,CPUBusy";
        let c = super::pm_columns(v2).unwrap();
        assert_eq!(super::pm_row("game.exe,7,0x1,DXGI,12.0,16.6,3.0", c.0, c.2), Some((7, 16.6)));
        assert!(super::pm_columns("rien,à,voir").is_none());
        assert_eq!(super::pm_row("ligne,courte", c.0, c.2), None);
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "lit le GPU"]
    fn stats_without_window() {
        let mut cfg = crate::config::Config::default();
        cfg.overlay_items = ["cpu", "gpu", "gpu_temp", "vram", "ram", "cpu_temp"].map(String::from).to_vec();
        cfg.overlay_gpus = super::gpu_names(); // toutes les cartes
        super::apply(&cfg, false);
        let (mut cpu, nv, th, gc) = (super::Cpu { last: (0, 0) }, super::Nvml::load(), super::Thermal::open(), crate::gpus::Counters::open());
        cpu.sample();
        std::thread::sleep(std::time::Duration::from_millis(500));
        super::refresh(&mut cpu, &nv, &th, &gc);
        println!("NVML chargé : {}", nv.is_some());
        for (k, v) in super::TEXT.lock().unwrap().iter() {
            println!("{k:<7} {v}");
        }
    }
}
