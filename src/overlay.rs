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

/// Capteur de température CPU présent (sinon l'UI grise l'option).
pub fn cpu_temp_available() -> bool {
    Thermal::open().is_some_and(|t| {
        t.celsius();
        std::thread::sleep(std::time::Duration::from_millis(100));
        t.celsius().is_some()
    })
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
    });
    REFRESH_MS.store(cfg.overlay_refresh_ms.clamp(100, 5000), Ordering::Relaxed);
    ENABLED.store(cfg.overlay_mode == "always" || cfg.overlay_mode == "hotkey", Ordering::Relaxed);
    let v = cfg.overlay_mode == "always" || (cfg.overlay_mode == "hotkey" && shown);
    VISIBLE.store(v, Ordering::Relaxed);
    WIDTH.store(0, Ordering::Relaxed); // taille, éléments ou visibilité changés : largeur recalculée
    let h = OV.load(Ordering::Relaxed);
    if !h.is_null() {
        unsafe { PostMessageW(h, WM_APP, 0, 0) };
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
    dev: *mut core::ffi::c_void,
    util: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u32; 2]) -> i32,
    temp: unsafe extern "C" fn(*mut core::ffi::c_void, u32, *mut u32) -> i32,
    mem: unsafe extern "C" fn(*mut core::ffi::c_void, *mut [u64; 3]) -> i32,
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
            (unsafe { (n.temp)(n.dev, 0, &mut t) } == 0).then_some(t)
        });
        (gpu, th.as_ref().and_then(Thermal::celsius))
    })
}

impl Nvml {
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
            if init() != 0 {
                return None;
            }
            let mut dev = std::ptr::null_mut();
            if by_index(0, &mut dev) != 0 {
                return None;
            }
            Some(Self {
                dev,
                util: std::mem::transmute(sym(b"nvmlDeviceGetUtilizationRates\0")?),
                temp: std::mem::transmute(sym(b"nvmlDeviceGetTemperature\0")?),
                mem: std::mem::transmute(sym(b"nvmlDeviceGetMemoryInfo\0")?),
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
/// Images de l'appli au premier plan ou de ses processus enfants : (arrivée, ms depuis l'image précédente, processus).
static PM_FRAMES: Mutex<VecDeque<(std::time::Instant, f64, u32)>> = Mutex::new(VecDeque::new());
/// Appli à laquelle appartiennent les images de PM_FRAMES.
static PM_FOR: AtomicU32 = AtomicU32::new(0);
/// Fréquence de mise à jour de l'overlay (réglage « Mise à jour »), en ms.
static REFRESH_MS: AtomicU32 = AtomicU32::new(500);
/// Dernière appli pour laquelle des FPS ont été affichés (journal, une ligne par appli).
static PM_SHOWN: AtomicU32 = AtomicU32::new(0);

/// Table processus -> (parent, nom de l'exe), lue par un instantané Toolhelp (lecture seule, aucun handle ouvert).
fn process_table() -> std::collections::HashMap<u32, (u32, String)> {
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
fn pm_columns(header: &str) -> Option<(usize, usize)> {
    let cols: Vec<String> = header.split(',').map(|c| c.trim().to_ascii_lowercase()).collect();
    let pid = cols.iter().position(|c| c == "processid")?;
    let ms = cols.iter().position(|c| c == "msbetweenpresents").or_else(|| cols.iter().position(|c| c == "frametime"))?;
    Some((pid, ms))
}

/// (processus, ms entre deux images) d'une ligne de données.
fn pm_row(line: &str, (pid, ms): (usize, usize)) -> Option<(u32, f64)> {
    let f: Vec<&str> = line.split(',').collect();
    Some((f.get(pid)?.trim().parse().ok()?, f.get(ms)?.trim().parse().ok()?))
}

/// Capture tant que l'overlay est visible. Faux si PresentMon est absent ou s'arrête aussitôt (droits).
fn run_presentmon() -> bool {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;
    // Celui livré par l'installateur, à côté de clipper.exe ; sinon celui du PATH (winget).
    let bundled = std::env::current_exe().ok().map(|e| e.with_file_name("presentmon.exe")).filter(|p| p.is_file());
    let mut cmd = std::process::Command::new(bundled.as_deref().unwrap_or("presentmon".as_ref()));
    cmd.creation_flags(0x0800_0000 | 0x0000_4000) // sans fenêtre, priorité sous la normale
        .args(["--output_stdout", "--no_console_stats", "--v1_metrics", "--session_name", "ClipperPresentMon", "--stop_existing_session"])
        // Suivi jusqu'à l'affichage indispensable : en plein écran (flip indépendant), une image n'est « terminée »
        // qu'une fois affichée ; sans ce suivi, PresentMon ne sortait aucune ligne pour les jeux.
        .args(["--no_track_input"])
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
        // Masqué : PresentMon reste 30 s de plus. Un Alt+F rapide réaffiche les FPS tout de suite au lieu
        // d'attendre ~1 s qu'il redémarre (coût : ~1 % de CPU pendant ces 30 s, puis plus rien).
        let mut hidden_since: Option<std::time::Instant> = None;
        while matches!(c.lock().unwrap().try_wait(), Ok(None)) {
            if VISIBLE.load(Ordering::Relaxed) {
                hidden_since = None;
            } else if hidden_since.get_or_insert_with(std::time::Instant::now).elapsed().as_secs() >= 30 {
                break;
            }
            std::thread::park_timeout(std::time::Duration::from_secs(1));
        }
        WAKE.lock().unwrap().retain(|t| t.id() != w.id());
        let _ = c.lock().unwrap().kill();
        me.unpark();
    });
    let started = std::time::Instant::now();
    let (mut cols, mut rows, mut target, mut logged) = (None, 0u64, 0u32, 0u32);
    let (mut table, mut table_at) = (process_table(), std::time::Instant::now());
    let mut target_since = std::time::Instant::now();
    let mut warned = 0u32;
    for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
        let Some(cc) = cols else {
            cols = pm_columns(&line);
            if cols.is_some() {
                PM_ON.store(true, Ordering::Relaxed);
                crate::log("FPS : PresentMon actif");
            }
            continue;
        };
        let Some((pid, ms)) = pm_row(&line, cc) else { continue };
        rows += 1;
        let t = TARGET.load(Ordering::Relaxed);
        // Processus inconnu (nouveau jeu, nouvel enfant) : instantané rafraîchi, au plus une fois par seconde.
        if (!table.contains_key(&pid) || !table.contains_key(&t)) && table_at.elapsed().as_secs() >= 1 {
            (table, table_at) = (process_table(), std::time::Instant::now());
        }
        let name = |p: u32| table.get(&p).map_or("?".to_string(), |e| e.1.clone());
        let mut q = PM_FRAMES.lock().unwrap();
        PM_FOR.store(t, Ordering::Relaxed);
        if t != target {
            q.clear(); // autre appli au premier plan
            target = t;
            target_since = std::time::Instant::now();
        }
        // L'Explorateur (bureau, barre des tâches) est l'ancêtre de presque tout : ses « enfants » ne comptent pas,
        // sinon un jeu ouvert en arrière-plan donnait ses FPS au bureau.
        let shell = table.get(&t).is_some_and(|e| e.1.eq_ignore_ascii_case("explorer.exe"));
        let mine = if shell { pid == t } else { belongs(pid, t, &table) };
        if !mine && logged != t && warned != t && target_since.elapsed().as_secs() >= 5 {
            warned = t;
            crate::log(&format!("FPS : aucune image de {} ({t}) en 5 s ; d'autres processus en affichent ({})", name(t), name(pid)));
        }
        if mine {
            if logged != t {
                logged = t;
                crate::log(&format!("FPS : images de {} ({t}) reçues via {} ({pid}), {ms:.1} ms", name(t), name(pid)));
            }
            q.push_back((std::time::Instant::now(), ms, pid));
            if q.len() > 30_000 {
                q.pop_front();
            }
        }
    }
    let _ = child.lock().unwrap().kill();
    let _ = watcher.join();
    PM_ON.store(false, Ordering::Relaxed);
    PM_STARTING.store(false, Ordering::Relaxed);
    PM_FRAMES.lock().unwrap().clear();
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
    // Images d'une autre appli (fenêtre changée depuis) : jamais affichées pour la nouvelle.
    if PM_FOR.load(Ordering::Relaxed) != TARGET.load(Ordering::Relaxed) {
        return FpsText::same("0", "—");
    }
    // PresentMon livre ses lignes par paquets (jusqu'à ~1 s d'écart) : une fenêtre « reçues dans les
    // dernières 0,5 s » était vide entre deux paquets, d'où « 180, 0, 180, 0 ». On moyenne donc les
    // dernières images reçues, sur 0,5 s de durée d'image cumulée ; « 0 » seulement après 2 s sans image.
    const STALE_SECS: f64 = 2.0;
    let live = |f: &&(std::time::Instant, f64, u32)| now.duration_since(f.0).as_secs_f64() <= STALE_SECS;
    // Plusieurs processus (appli + enfant GPU) : celui qui affiche le plus d'images.
    let mut count = std::collections::HashMap::<u32, usize>::new();
    q.iter().filter(live).for_each(|f| *count.entry(f.2).or_default() += 1);
    let Some((&src, _)) = count.iter().max_by_key(|(_, n)| **n) else { return FpsText::same("0", "—") };
    let span_ms = (REFRESH_MS.load(Ordering::Relaxed) as f64).max(500.0);
    let (mut sum, mut n) = (0.0, 0usize);
    for f in q.iter().rev().filter(|f| f.2 == src) {
        sum += f.1;
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
    let all: Vec<f64> = q.iter().filter(|f| f.2 == src).map(|f| f.1).collect();
    let low = one_percent_low(&all);
    let t = TARGET.load(Ordering::Relaxed);
    if PM_SHOWN.swap(t, Ordering::Relaxed) != t {
        crate::log(&format!("FPS : overlay affiche {fps:.0} FPS / {ft:.1} ms (processus {t})"));
    }
    FpsText {
        fps: format!("{fps:.0}"),
        ft: format!("{ft:.1} ms").replace('.', ","),
        low: low.map_or("—".into(), |l| format!("{l:.0}")),
    }
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

fn refresh(cpu: &mut Cpu, nv: &Option<Nvml>, th: &Option<Thermal>) {
    let items = LOOK.lock().unwrap().as_ref().map(|l| l.items.clone()).unwrap_or_default();
    TARGET.store(foreground_pid(), Ordering::Relaxed);
    let c = cpu.sample();
    let (mut gu, mut gt, mut gm) = (None, None, None);
    if let Some(n) = nv {
        unsafe {
            let mut u = [0u32; 2];
            if (n.util)(n.dev, &mut u) == 0 {
                gu = Some(u[0]);
            }
            let mut t = 0u32;
            if (n.temp)(n.dev, 0, &mut t) == 0 {
                gt = Some(t);
            }
            let mut m = [0u64; 3];
            if (n.mem)(n.dev, &mut m) == 0 {
                gm = Some((m[2], m[0]));
            }
        }
    }
    let (free, total) = crate::tools::ram();
    // Même unité que le Gestionnaire des tâches (1 Go = 1024³ octets).
    let go = |b: u64| format!("{:.1}", b as f64 / 1_073_741_824.0).replace('.', ",");
    // Bureau ou appli fenêtrée : elles ne redessinent qu'au moindre changement, leurs « FPS » sautent
    // de 0 à 144 sans rien dire. Chiffres d'images seulement pour une appli plein écran (un jeu).
    let f = if crate::capture::fullscreen_foreground() { fps_text() } else { FpsText::same("—", "—") };
    let rows = items
        .iter()
        .filter_map(|k| {
            Some((k.clone(), match k.as_str() {
                "fps" => ("FPS".into(), f.fps.clone()),
                "low1" => ("1% LOW".into(), f.low.clone()),
                "frametime" => ("IMAGE".into(), f.ft.clone()),
                "cpu" => ("CPU".into(), format!("{c:.0} %")),
                "gpu" => ("GPU".into(), gu.map_or("—".into(), |v| format!("{v} %"))),
                "gpu_temp" => ("GPU °C".into(), gt.map_or("—".into(), |v| v.to_string())),
                "cpu_temp" => ("CPU °C".into(), th.as_ref().and_then(Thermal::celsius).map_or("n/d".into(), |t| format!("{t:.0}"))),
                "vram" => ("VRAM".into(), gm.map_or("—".into(), |(u, t)| format!("{}/{} Go", go(u), go(t)))),
                "ram" => ("RAM".into(), format!("{}/{} Go", go(total - free), go(total))),
                _ => return None,
            }))
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

fn font_px(size: &str) -> i32 {
    match size {
        "s" => 13,
        "l" => 20,
        _ => 16,
    }
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
            let row = px + 6;
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
                if gw > 0 && graphs.iter().any(|g| g == key) {
                    if let Some((_, q)) = hist.iter().find(|(h, _)| h == key) {
                        let (x1, top) = (r.right - pad, y + 3);
                        draw_graph(dc, RECT { left: x1 - (gw - px / 2), top, right: x1, bottom: top + row - 7 }, q);
                    }
                }
            }
            drop(hist);
            SelectObject(dc, old);
            DeleteObject(font as _);
            EndPaint(h, &ps);
            return 0;
        }
        DefWindowProcW(h, msg, w, l)
    }
}

/// Place réservée aux courbes (écart compris) : 0 si aucune mesure affichée n'en a.
fn graph_width(px: i32, keys: &[String], graphs: &[String]) -> i32 {
    if keys.iter().any(|k| graphs.contains(k)) { px * 5 + px / 2 } else { 0 }
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

/// Police à chiffres fixes : les colonnes restent alignées quand les valeurs changent.
unsafe fn make_font(px: i32) -> HFONT {
    let face: Vec<u16> = "Consolas".encode_utf16().chain(Some(0)).collect();
    unsafe { CreateFontW(-px, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, CLEARTYPE_QUALITY as u32, 0, face.as_ptr()) }
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
        DeleteObject(font as _);
        ReleaseDC(h, dc);
        let graphs = LOOK.lock().unwrap().as_ref().map(|l| l.graphs.clone()).unwrap_or_default();
        px / 2 * 2 + kl + px + vl + graph_width(px, &KEYS.lock().unwrap(), &graphs)
    }
}

/// Coin choisi de l'écran du jeu.
fn place(h: HWND, rows: usize, look: &Look) {
    unsafe {
        let px = font_px(&look.size);
        // Largeur qui ne rétrécit jamais tant que l'overlay reste affiché avec les mêmes réglages :
        // « 3 » puis « 144 » ne fait plus sauter le cadre à chaque rafraîchissement.
        let m = measure(h, px);
        let w = WIDTH.fetch_max(m, Ordering::Relaxed).max(m);
        let hgt = px / 2 * 2 + rows as i32 * (px + 6);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY), &mut mi);
        let m = mi.rcMonitor;
        let margin = 12;
        let x = if look.corner.ends_with('r') { m.right - w - margin } else { m.left + margin };
        let y = if look.corner.starts_with('b') { m.bottom - hgt - margin } else { m.top + margin };
        SetWindowPos(h, HWND_TOPMOST, x, y, w, hgt, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetLayeredWindowAttributes(h, 0, (look.opacity * 255.0) as u8, LWA_ALPHA);
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
            (c, Nvml::load(), Thermal::open())
        };
        // Overlay activé (raccourci ou toujours) : sondes chargées dès le lancement, l'overlay s'affiche
        // sans les ~0,4 s de chargement. Mode « masqué » : jamais chargées (nvml.dll et PDH hors mémoire).
        let mut probes: Option<(Cpu, Option<Nvml>, Option<Thermal>)> = ENABLED.load(Ordering::Relaxed).then(load);
        let tick = |probes: &mut Option<(Cpu, Option<Nvml>, Option<Thermal>)>| {
            let (cpu, nv, th) = probes.get_or_insert_with(load);
            refresh(cpu, nv, th);
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

unsafe extern "system" fn on_event(r: *mut windows_sys::Win32::System::Diagnostics::Etw::EVENT_RECORD) {
    let h = unsafe { &(*r).EventHeader };
    if h.EventDescriptor.Id == 42 {
        ALL_PRESENTS.fetch_add(1, Ordering::Relaxed);
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
fn etw_thread() {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    WAKE.lock().unwrap().push(std::thread::current());
    let name: Vec<u16> = "ClipperFps".encode_utf16().chain(Some(0)).collect();
    loop {
        let want = VISIBLE.load(Ordering::Relaxed)
            && LOOK.lock().unwrap().as_ref().is_some_and(|l| l.items.iter().any(|i| i == "fps" || i == "frametime" || i == "low1"));
        if !want {
            std::thread::park(); // réveillé par apply()
            continue;
        }
        // PresentMon (Intel) d'abord : couvre DirectX, Vulkan et OpenGL. Absent ou refusé : ETW DXGI ci-dessous.
        if !PM_UNUSABLE.load(Ordering::Relaxed) {
            if run_presentmon() {
                continue;
            }
            PM_UNUSABLE.store(true, Ordering::Relaxed);
        }
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
                continue;
            }
            // Tous les mots-clés : avec 0, certains fournisseurs à manifeste n'envoient pas Present_Start.
            EnableTraceEx2(handle, &DXGI, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_INFORMATION as u8, u64::MAX, 0, 0, std::ptr::null());
            let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
            log.LoggerName = name.as_ptr() as *mut u16;
            log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
            log.Anonymous2.EventRecordCallback = Some(on_event);
            let th = OpenTraceW(&mut log);
            if th.Value == u64::MAX {
                crate::log("FPS : OpenTraceW a échoué");
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            ETW_OK.store(true, Ordering::Relaxed);
            ALL_PRESENTS.store(0, Ordering::Relaxed);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(5));
                crate::log(&format!("FPS : ETW actif, {} images vues en 5 s (tous processus)", ALL_PRESENTS.load(Ordering::Relaxed)));
            });
            // Arrêt de la session quand l'overlay est masqué : ProcessTrace rend alors la main.
            let stop_name = name.clone();
            let watcher = std::thread::spawn(move || {
                let me = std::thread::current();
                WAKE.lock().unwrap().push(me.clone());
                while VISIBLE.load(Ordering::Relaxed) {
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
        assert_eq!(super::pm_row("game.exe,4242,0x1,DXGI,0,0,0,1.5,0.2,6.944", c), Some((4242, 6.944)));
        let v2 = "Application,ProcessID,SwapChainAddress,PresentRuntime,CPUStartTime,FrameTime,CPUBusy";
        let c = super::pm_columns(v2).unwrap();
        assert_eq!(super::pm_row("game.exe,7,0x1,DXGI,12.0,16.6,3.0", c), Some((7, 16.6)));
        assert!(super::pm_columns("rien,à,voir").is_none());
        assert_eq!(super::pm_row("ligne,courte", c), None);
    }
}

#[cfg(test)]
mod live {
    #[test]
    #[ignore = "lit le GPU"]
    fn stats_without_window() {
        let mut cfg = crate::config::Config::default();
        cfg.overlay_items = ["cpu", "gpu", "gpu_temp", "vram", "ram", "cpu_temp"].map(String::from).to_vec();
        super::apply(&cfg, false);
        let (mut cpu, nv, th) = (super::Cpu { last: (0, 0) }, super::Nvml::load(), super::Thermal::open());
        cpu.sample();
        std::thread::sleep(std::time::Duration::from_millis(500));
        super::refresh(&mut cpu, &nv, &th);
        println!("NVML chargé : {}", nv.is_some());
        for (k, v) in super::TEXT.lock().unwrap().iter() {
            println!("{k:<7} {v}");
        }
    }
}
