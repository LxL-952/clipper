#![cfg_attr(not(test), windows_subsystem = "windows")]

mod audio;
mod barui;
mod capture;
mod config;
mod drivers;
mod elevate;
mod front;
mod front_a;
mod front_b;
mod gfx;
mod gpus;
mod overlay;
mod player;
#[cfg(test)]
mod shots;
mod sounds;
mod tools;
mod tray;
mod update;
mod ui;
mod widgets;

use config::Config;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tray_icon::menu::MenuEvent;
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

pub enum UserEvent {
    Hotkey(u32),
    Menu(tray_icon::menu::MenuId),
    TrayClick,
    /// Souris sur l'icône : températures du menu rafraîchies.
    TrayHover,
    Ipc(ui::Kind, String),
    /// Message à envoyer à toutes les fenêtres ouvertes (`window.__on`).
    Push(Value),
    /// Fin d'un clip / REC lancé dans un fil (toast + son).
    Saved(Result<PathBuf, String>, &'static str),
    /// Une fenêtre doit être redessinée (image du lecteur arrivée, fin de lecture).
    Redraw(ui::Kind),
    Quit,
}

/// Job Object « tuer à la fermeture » : si l'app se ferme ou plante, Windows arrête aussi
/// ffmpeg et WebView2 (aucun processus orphelin qui continuerait à capturer).
fn kill_children_with_us() {
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        // BREAKAWAY_OK : l'installateur de mise à jour sort du job (sinon Windows le tuait avec Clipper :
        // l'app se fermait et rien ne s'installait).
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as _, std::mem::size_of_val(&info) as u32);
        AssignProcessToJobObject(job, GetCurrentProcess());
        // Le handle reste ouvert jusqu'à la fin du processus : c'est sa fermeture qui déclenche l'arrêt.
    }
}

/// Priorité "sous la normale" : le jeu passe toujours avant nous.
fn lower_priority() {
    use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass};
    unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    eco(true);
}

/// EcoQoS : Windows place nos fils sur les cœurs économes, à basse fréquence. ffmpeg n'en hérite pas.
/// Coupé tant qu'une fenêtre de Clipper est affichée : l'interface est dessinée par le processeur, et ce
/// mode la rendait saccadée (lecteur à faible cadence, listes lentes). Remis dès que tout est fermé.
fn eco(on: bool) {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation};
    use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, SetPriorityClass};
    // Fenêtre affichée : priorité normale. Sous la normale, un jeu qui charge le processeur ne laissait à
    // l'interface que quelques images par seconde (impression d'une appli à 10 i/s).
    unsafe { SetPriorityClass(GetCurrentProcess(), if on { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS }) };
    let s = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED, StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 } };
    unsafe { SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, &s as *const _ as _, std::mem::size_of_val(&s) as u32) };
    // Fenêtre affichée : horloge de Windows à 1 ms. Sans ça, « redessiner dans 15 ms » attendait souvent
    // 31 ms (pas de 15,6 ms) : animations et défilement à ~30 i/s ou moins. Rendue dès que tout est fermé.
    static FINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if FINE.swap(!on, std::sync::atomic::Ordering::Relaxed) != !on {
        unsafe {
            if on {
                windows_sys::Win32::Media::timeEndPeriod(1);
            } else {
                windows_sys::Win32::Media::timeBeginPeriod(1);
            }
        }
    }
}

/// Rend au système la RAM froide de notre processus (pas celle des autres).
fn trim_self() {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe { windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet(GetCurrentProcess()) };
}

/// Logo (appareil photo) lié dans l'exe par build.rs, ressource n° 1.
fn tray_icon_image() -> Icon {
    Icon::from_resource(1, None).expect("icône dans l'exe")
}

/// Journal court dans %APPDATA%\Clipper\log.txt (l'app n'a pas de console).
fn log(msg: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(config::dir());
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(config::dir().join("log.txt"));
    // Journal inaccessible : l'erreur va dans %TEMP%\clipper-log.txt, sinon on ne saurait jamais pourquoi.
    let opened = opened.or_else(|e| {
        let alt = std::env::temp_dir().join("clipper-log.txt");
        let f = std::fs::OpenOptions::new().create(true).append(true).open(alt);
        if let Ok(mut f) = f.as_ref() {
            let _ = writeln!(f, "log.txt dans {} : {e}", config::dir().display());
        }
        f
    });
    if let Ok(mut f) = opened {
        // Heure locale : un bug signalé se retrouve dans le journal à la seconde près.
        let t = unsafe {
            let mut t = std::mem::zeroed();
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t);
            t
        };
        let _ = writeln!(f, "{:02}/{:02} {:02}:{:02}:{:02} {msg}", t.wDay, t.wMonth, t.wHour, t.wMinute, t.wSecond);
    }
}

/// Son de confirmation, sauf si l'utilisateur les a coupés (Avancé > Sons).
fn sound(cfg: &Config, s: sounds::Sound) {
    if cfg.sounds {
        sounds::play(s);
    }
}

fn disk_free(p: &Path) -> u64 {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    // Le dossier peut ne pas exister encore : on remonte au premier parent existant.
    let mut p = p.to_path_buf();
    while !p.exists() && p.pop() {}
    let w: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    free
}

/// explorer.exe lit sa ligne de commande lui-même : le chemin entre guillemets doit arriver tel quel.
/// Avec `.arg()`, Rust entoure tout de guillemets et explorer ouvrait « Documents » au lieu du dossier.
fn explorer(args: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(args).spawn();
}

/// Corbeille (récupérable). La suppression définitive est un réglage à part (delete_permanent).
fn to_recycle_bin(p: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
    unsafe { SHFileOperationW(&mut op) == 0 }
}

/// Nom du sous-dossier d'un clip : titre de la fenêtre du jeu (« VALORANT », « Battlefield 6 »), ou
/// « Bureau » pour le bureau, un navigateur, Discord, Clipper… Lit le titre et le nom de l'exe seulement.
fn game_folder(h: isize) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowTextW, GetWindowThreadProcessId};
    let h = h as windows_sys::Win32::Foundation::HWND;
    if h.is_null() || h == unsafe { GetShellWindow() } {
        return "Bureau".into();
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, &mut pid) };
    let exe = overlay::process_table().get(&pid).map(|e| e.1.to_ascii_lowercase()).unwrap_or_default();
    if exe.is_empty() || pid == std::process::id() || overlay::NOT_GAMES.contains(&exe.as_str()) {
        return "Bureau".into();
    }
    let mut buf = [0u16; 128];
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), 128) }.max(0) as usize;
    let title = String::from_utf16_lossy(&buf[..n]);
    folder_name(&title, &exe)
}

/// Titre nettoyé pour un nom de dossier Windows (sans ™ ® ni caractères interdits), sinon le nom de l'exe.
fn folder_name(title: &str, exe: &str) -> String {
    let clean: String = title.chars().filter(|c| !r#"\/:*?"<>|™®©"#.contains(*c) && !c.is_control()).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean: String = clean.chars().take(48).collect::<String>().trim_end_matches(['.', ' ']).to_string();
    if clean.is_empty() { exe.trim_end_matches(".exe").to_string() } else { clean }
}

/// Le dossier de sortie, ou un de ses sous-dossiers de jeu.
fn in_out_dir(cfg: &Config, path: &Path) -> bool {
    let p = path.parent();
    p == Some(cfg.out_dir.as_path()) || p.and_then(Path::parent) == Some(cfg.out_dir.as_path())
}

/// Vidéos du dossier de sortie, plus récentes d'abord. ffprobe en parallèle et en cache : liste immédiate.
/// Les clips récents qui ne se lisent pas tels quels ont leur copie de lecture préparée en tâche de fond.
fn list_videos(cfg: &Config, proxy: &EventLoopProxy<UserEvent>, limit: Option<usize>) -> Value {
    // Le dossier de sortie et ses sous-dossiers de jeu (un niveau).
    let top: Vec<std::fs::DirEntry> = std::fs::read_dir(&cfg.out_dir).into_iter().flatten().flatten().collect();
    let subs = top.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten());
    let all: Vec<std::fs::DirEntry> = subs.collect();
    let mut v: Vec<(std::time::SystemTime, PathBuf, u64)> = top
        .into_iter()
        .chain(all)
        .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| ["mp4", "mkv", "mov"].contains(&x.to_ascii_lowercase().as_str())))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path(), m.len()))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    if let Some(n) = limit {
        v.truncate(n);
    }
    let probes = par_probe(cfg, &v.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
    let items: Vec<Value> = v
        .into_iter()
        .zip(probes)
        .map(|((t, p, bytes), pr)| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let date = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            // `video` : le fichier a une piste d'images ; `audio` : pistes à faire entendre (mélangées par le lecteur).
            let (video, audio) = capture::playable(&pr);
            json!({
                "path": p, "name": name, "kind": if stem.starts_with("REC") { "REC" } else { "Clip" },
                "bytes": bytes, "secs": pr.secs, "date": date, "video": video, "audio": audio,
                "game": if p.parent() != Some(cfg.out_dir.as_path()) { p.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()) } else { None },
            })
        })
        .collect();
    let _ = proxy;
    json!({"type": if limit.is_some() { "recent" } else { "videos" }, "items": items})
}

/// ffprobe sur 6 fils au plus (les fichiers déjà vus sortent du cache sans rien lancer).
fn par_probe(cfg: &Config, paths: &[PathBuf]) -> Vec<capture::Probe> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<capture::Probe>> = paths.iter().map(|_| Default::default()).collect();
    std::thread::scope(|s| {
        for _ in 0..paths.len().min(6) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(p) = paths.get(i) else { break };
                *out[i].lock().unwrap() = capture::probe(cfg, p);
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

struct Keys {
    clip: Option<HotKey>,
    rec: Option<HotKey>,
    bar: Option<HotKey>,
    overlay: Option<HotKey>,
    clipping: Option<HotKey>,
    /// Raccourcis demandés mais pris par une autre app (NVIDIA, AMD…).
    conflicts: Vec<String>,
}

/// Enregistre les 4 raccourcis (RegisterHotKey : pas de hook clavier, rien d'injecté).
/// Raccourci déjà pris (overlay NVIDIA Alt+Z / Alt+R, Radeon Software…) : repli Maj+, puis Ctrl+.
fn register_keys(mgr: &GlobalHotKeyManager, cfg: &Config) -> Keys {
    let mut conflicts = vec![];
    let mut taken: Vec<HotKey> = vec![];
    let mut register = |s: &str| -> Option<HotKey> {
        for prefix in ["", "Shift+", "Control+"] {
            let Ok(k) = format!("{prefix}{s}").parse::<HotKey>() else { continue };
            // Deux actions sur la même touche : la seconde prend le repli au lieu d'échouer en silence.
            if !taken.contains(&k) && mgr.register(k).is_ok() {
                if !prefix.is_empty() {
                    conflicts.push(s.replace("Key", ""));
                    log(&format!("raccourci {s} déjà pris, remplacé par {prefix}{s}"));
                }
                taken.push(k);
                return Some(k);
            }
        }
        conflicts.push(s.replace("Key", ""));
        log(&format!("raccourci {s} indisponible, même avec Maj ou Ctrl"));
        None
    };
    let (clip, rec, bar, overlay) = (register(&cfg.key_clip), register(&cfg.key_rec), register(&cfg.key_bar), register(&cfg.key_overlay));
    let clipping = register(&cfg.key_clipping);
    Keys { clip, rec, bar, overlay, clipping, conflicts }
}

/// Fonction donnée au lecteur vidéo : demande un nouveau dessin de la fenêtre depuis n'importe quel fil.
fn redraw_fn(proxy: &EventLoopProxy<UserEvent>, kind: ui::Kind) -> Arc<dyn Fn() + Send + Sync> {
    let p = std::sync::Mutex::new(proxy.clone());
    Arc::new(move || {
        let _ = p.lock().unwrap().send_event(UserEvent::Redraw(kind));
    })
}

fn pretty(k: &HotKey) -> String {
    let s = format!("{k}");
    s.replace("shift", "Maj").replace("control", "Ctrl").replace("alt", "Alt").replace("Key", "").replace("super", "Win")
}

struct App {
    cfg: Config,
    encoders: Vec<&'static str>,
    /// FFmpeg lancé avec succès au démarrage (sinon : aucun encodeur, aucun écran, message dans l'UI).
    ffmpeg_ok: bool,
    /// Version plus récente trouvée sur GitHub (bandeau « Nouvelle version »).
    update: Option<String>,
    gpus: Vec<capture::Gpu>,
    /// Capteur de température CPU lisible par Windows (souvent absent sur PC de bureau).
    cpu_temp: bool,
    native: (u32, u32),
    monitors: Vec<Value>,
    buffer: Arc<capture::Buffer>,
    audio: Option<Arc<audio::Audio>>,
    recording: Option<Instant>,
    /// Réglages changés pendant un REC : capture relancée à l'arrêt du REC.
    restart_pending: bool,
    reprobe_pending: bool,
    /// Overlay affiché par le raccourci (mode "hotkey").
    overlay_shown: bool,
    main: Option<ui::Win>,
    bar: Option<ui::Win>,
    /// Contenu de la fenêtre principale (None : fenêtre fermée) et de la barre.
    front: std::cell::RefCell<Option<front::Front>>,
    barui: std::cell::RefCell<barui::BarUi>,
    /// Prochain dessin programmé de chaque fenêtre (animation, compteur, notification) : [principale, barre].
    wake: [Option<Instant>; 2],
    /// Dernier dessin de chaque fenêtre (plafond de cadence, voir `draw`).
    last_draw: [Option<Instant>; 2],
    /// Diagnostic : (dessins, temps cumulé en ms, début de la période).
    perf: (u32, f32, Instant),
    /// Fenêtre au premier plan (le jeu) quand la barre s'est ouverte : elle reprend le focus à la fermeture.
    bar_prev: isize,
    /// Dossier (du jeu) où le REC en cours sera enregistré.
    rec_dir: PathBuf,
    /// Numéro de la barre ouverte : un « close » tardif d'une ancienne barre ne ferme pas la nouvelle.
    bar_gen: u64,
    keys: Keys,
    hotkeys: GlobalHotKeyManager,
    /// Barre affichée (la fenêtre, elle, reste créée et cachée : réouverture immédiate).
    bar_open: bool,
    gpu: Option<drivers::Gpu>,
    driver_url: Option<String>,
    proxy: EventLoopProxy<UserEvent>,
}

impl App {
    fn start_capture(cfg: &Config, native: (u32, u32)) -> (Arc<capture::Buffer>, Option<Arc<audio::Audio>>) {
        // Clipping désactivé : ni ffmpeg, ni capture du son, ni fil de surveillance ; segments effacés.
        if !cfg.clipping {
            return (Arc::new(capture::Buffer::idle(cfg.buffer_dir.clone())), None);
        }
        let audio = (audio::pipe_count(cfg) > 0).then(|| audio::Audio::start(cfg));
        let buffer = Arc::new(capture::Buffer::start(cfg.clone(), native, cfg.buffer_dir.clone(), audio.clone()));
        (buffer, audio)
    }

    /// `reprobe` : relire la taille de l'écran (ffprobe, ~1 s) seulement si l'écran capturé a changé.
    fn restart_capture(&mut self, reprobe: bool) {
        if self.recording.is_some() {
            self.restart_pending = true; // jamais pendant un REC : on appliquera à l'arrêt
            self.reprobe_pending |= reprobe;
            return;
        }
        let reprobe = reprobe || std::mem::take(&mut self.reprobe_pending);
        self.restart_pending = false;
        self.buffer.stop();
        if let Some(a) = &self.audio {
            a.stop();
        }
        self.cfg.encoder = capture::resolve_encoder(&self.cfg, &self.encoders, &self.gpus);
        if reprobe {
            self.native = capture::native_size(&self.cfg).unwrap_or(self.native);
        }
        (self.buffer, self.audio) = Self::start_capture(&self.cfg, self.native);
    }

    fn hotkeys_json(&self) -> Value {
        let k = |k: &Option<HotKey>| k.as_ref().map(pretty);
        json!({"clip": k(&self.keys.clip), "rec": k(&self.keys.rec), "bar": k(&self.keys.bar), "overlay": k(&self.keys.overlay), "clipping": k(&self.keys.clipping)})
    }

    /// Nouveaux raccourcis actifs tout de suite : les anciens sont libérés, les nouveaux enregistrés.
    fn rebind_keys(&mut self) {
        let old: Vec<HotKey> = [self.keys.clip, self.keys.rec, self.keys.bar, self.keys.overlay, self.keys.clipping].into_iter().flatten().collect();
        if let Err(e) = self.hotkeys.unregister_all(&old) {
            log(&format!("raccourcis : libération impossible ({e})"));
        }
        self.keys = register_keys(&self.hotkeys, &self.cfg);
        self.push(&json!({"type": "hotkeys", "hotkeys": self.hotkeys_json(), "conflicts": self.keys.conflicts}));
    }

    fn init_json(&self, page: &str, tab: Option<&str>) -> Value {
        let devs = |d| audio::devices(d).into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>();
        json!({
            "page": page, "tab": tab, "config": self.cfg, "encoders": self.encoders, "ffmpeg_ok": self.ffmpeg_ok, "update": self.update,
            "native": [self.native.0, self.native.1], "monitors": self.monitors,
            "outputs": devs(wasapi::Direction::Render), "inputs": devs(wasapi::Direction::Capture),
            "hotkeys": self.hotkeys_json(),
            "hotkey_conflicts": self.keys.conflicts,
            "overlay_on": self.overlay_visible(),
            "bar_gen": self.bar_gen,
            "recording": self.recording.is_some(), "rec_secs": self.recording.map_or(0, |t| t.elapsed().as_secs()),
            "gpu": self.gpu.as_ref().map(|g| json!({"name": g.name, "driver": g.driver})),
            "elevated": elevate::is_elevated(),
            "gpu_scale": capture::gpu_scaling(),
            "gpus": self.gpus, "chip": capture::effective_chip(&self.cfg, &self.gpus),
            "cpu_temp": self.cpu_temp, "gpu_names": overlay::gpu_names(), "autostart": elevate::autostart(),
        })
    }

    /// Message pour l'interface : la fenêtre principale et la barre mettent leur état à jour, puis se redessinent.
    fn push(&self, msg: &Value) {
        if let Some(f) = self.front.borrow_mut().as_mut() {
            f.on(msg);
        }
        self.barui.borrow_mut().on(msg);
        for w in [&self.main, &self.bar].into_iter().flatten() {
            w.window.request_redraw();
        }
    }

    fn open(&mut self, kind: ui::Kind, target: &EventLoopWindowTarget<UserEvent>, tab: Option<&str>) {
        let slot = if kind == ui::Kind::Main { &self.main } else { &self.bar };
        if let Some(w) = slot {
            if kind == ui::Kind::Main {
                w.window.set_minimized(false);
                w.window.set_focus();
                if let (Some(t), Some(f)) = (tab, self.front.borrow_mut().as_mut()) {
                    f.on(&json!({"type": "tab", "tab": t}));
                }
                w.window.request_redraw();
            }
            return;
        }
        let w = ui::open(kind, target);
        if w.is_none() {
            log("impossible de créer la fenêtre");
        }
        match kind {
            ui::Kind::Main => {
                self.main = w;
                if self.main.is_some() {
                    let init = self.init_json("index", tab);
                    let mut f = front::Front::new(&init, redraw_fn(&self.proxy, ui::Kind::Main));
                    if f.intro.is_some() {
                        sounds::play(sounds::Sound::Intro);
                    }
                    if let Some(t) = tab.and_then(front::Tab::parse) {
                        f.select_tab(t);
                    }
                    for p in [self.cfg.out_dir.clone(), self.cfg.buffer_dir.clone()] {
                        f.free.insert(p.clone(), disk_free(&p));
                    }
                    *self.front.borrow_mut() = Some(f);
                    eco(false);
                    // Première image dessinée avant d'afficher la fenêtre : jamais de cadre vide.
                    self.draw(ui::Kind::Main, target);
                    if let Some(w) = &self.main {
                        w.window.set_visible(true);
                        w.window.set_focus();
                    }
                }
            }
            ui::Kind::Bar => self.bar = w,
        }
    }

    /// Dessine une fenêtre : l'interface traite les entrées reçues, peint son tampon, Windows l'affiche.
    /// Les commandes émises par l'interface sont ensuite exécutées comme avant (`ipc`).
    fn draw(&mut self, kind: ui::Kind, target: &EventLoopWindowTarget<UserEvent>) {
        let now = Instant::now();
        let slot = if kind == ui::Kind::Main { &mut self.main } else { &mut self.bar };
        let Some(win) = slot.as_mut() else { return };
        if kind == ui::Kind::Bar && !self.bar_open && !win.window.is_visible() {
            return;
        }
        // Au plus un dessin toutes les 14 ms par fenêtre (~70 i/s) : chaque image du lecteur et chaque mouvement
        // de souris en demandaient un (321 dessins en 2 s mesurés). Le suivant est programmé, rien n'est perdu.
        let i = (kind == ui::Kind::Bar) as usize;
        const GAP: std::time::Duration = std::time::Duration::from_millis(14);
        if let Some(t) = self.last_draw[i].filter(|t| now.duration_since(*t) < GAP) {
            let due = t + GAP;
            self.wake[i] = Some(self.wake[i].map_or(due, |w| w.min(due)));
            return;
        }
        self.last_draw[i] = Some(now);
        win.fit();
        let t_frame = Instant::now();
        let (cmds, next) = {
            let mut u = widgets::Ui { c: &mut win.canvas, i: &win.input, s: &mut win.st, now };
            match kind {
                ui::Kind::Main => {
                    let mut f = self.front.borrow_mut();
                    let Some(f) = f.as_mut() else { return };
                    f.frame(&mut u);
                    (std::mem::take(&mut f.out), f.next)
                }
                ui::Kind::Bar => {
                    let mut b = self.barui.borrow_mut();
                    b.frame(&mut u);
                    (std::mem::take(&mut b.out), b.next)
                }
            }
        };
        front_b::PROF.lock().unwrap()[3] = t_frame.elapsed().as_secs_f32() * 1000.0;
        let (hand, animating) = (win.st.hand, win.st.animating);
        // Un clic, une touche ou la molette changent l'état pendant le dessin (section dépliée, liste
        // ouverte…) : l'image suivante montre le résultat tout de suite, sans attendre un mouvement de souris.
        let acted = win.input.pressed || win.input.released || win.input.rpressed || win.input.wheel != 0.0 || !win.input.keys.is_empty();
        win.input.end_frame();
        win.set_hand(hand);
        win.present();
        // Diagnostic du lecteur : images reçues de ffmpeg et dessins réellement faits, toutes les 2 s.
        self.perf.0 += 1;
        self.perf.1 += now.elapsed().as_secs_f32() * 1000.0;
        if self.perf.2.elapsed().as_secs_f32() >= 2.0 {
            let got = player::FRAMES.swap(0, std::sync::atomic::Ordering::Relaxed);
            // Plus écrit dans le journal (une ligne toutes les 2 s pendant la lecture) : gardé pour le débogage.
            let _ = got;
            self.perf = (0, 0.0, Instant::now());
        }
        self.wake[i] = if animating || acted || !cmds.is_empty() { Some(now + std::time::Duration::from_millis(15)) } else { next.map(|d| now + d) };
        for c in cmds {
            self.ipc(kind, &c.to_string(), target);
        }
    }

    /// Barre Alt+Z : mémorise le jeu, puis affiche la fenêtre déjà créée (aucune attente de WebView2).
    /// La barre prend le focus : le jeu rend la souris.
    fn open_bar(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        self.bar_prev = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as isize;
        self.bar_gen += 1;
        self.open(ui::Kind::Bar, target, None); // créée si elle ne l'est pas encore
        self.bar_open = self.bar.is_some();
        // État du moment, puis animation d'entrée.
        let show = json!({"type": "show", "gen": self.bar_gen, "config": self.cfg, "hotkeys": self.hotkeys_json(),
            "native": [self.native.0, self.native.1], "overlay_on": self.overlay_visible(),
            "recording": self.recording.is_some(), "rec_secs": self.recording.map_or(0, |t| t.elapsed().as_secs())});
        self.barui.borrow_mut().on(&show);
        eco(false);
        if let Some(w) = &self.bar {
            ui::show_bar(&w.window);
        }
        // Première image peinte fenêtre encore cachée, puis affichage : jamais de cadre vide.
        self.draw(ui::Kind::Bar, target);
        if let Some(w) = &self.bar {
            ui::reveal_bar(&w.window);
        }
    }

    /// Ferme la barre : animation de sortie côté page (qui arrête aussi la lecture), fenêtre cachée 200 ms
    /// plus tard. `refocus` : rend le focus au jeu (Alt+Z, Échap) ; faux si l'utilisateur a cliqué ailleurs.
    fn hide_bar(&mut self, refocus: bool) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};
        if !std::mem::take(&mut self.bar_open) {
            return;
        }
        self.barui.borrow_mut().on(&json!({"type": "hide"}));
        if let Some(w) = &self.bar {
            w.window.request_redraw();
        }
        let (p, g) = (self.proxy.clone(), self.bar_gen);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = p.send_event(UserEvent::Ipc(ui::Kind::Bar, json!({"cmd": "bar_hidden", "gen": g}).to_string()));
        });
        let h = std::mem::take(&mut self.bar_prev) as windows_sys::Win32::Foundation::HWND;
        if refocus && !h.is_null() && unsafe { IsWindow(h) } != 0 {
            unsafe { SetForegroundWindow(h) };
        }
    }

    /// Fenêtre du jeu : celle d'avant la barre si elle est ouverte, sinon celle au premier plan.
    fn game_window(&self) -> isize {
        if self.bar_prev != 0 { self.bar_prev } else { unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() as isize } }
    }

    /// Réglages avec le dossier de sortie du jeu en cours (« Clipper\Valorant », « Clipper\Bureau »…).
    fn cfg_for_game(&self) -> Config {
        let mut c = self.cfg.clone();
        c.out_dir = c.out_dir.join(game_folder(self.game_window()));
        c
    }

    /// Raccourci et menu de l'icône : coupe ou relance toute la capture, tout de suite, sans redémarrer.
    fn toggle_clipping(&mut self) {
        if self.recording.is_some() {
            return self.push(&json!({"type": "toast", "kind": "error", "text": "Arrêtez le REC avant de désactiver le clipping."}));
        }
        let on = !self.cfg.clipping;
        self.set("clipping", json!(on));
        log(if on { "clipping activé" } else { "clipping désactivé : capture arrêtée" });
        sound(&self.cfg, if on { sounds::Sound::RecStart } else { sounds::Sound::RecStop });
    }

    fn clip(&self) {
        if !self.cfg.clipping {
            sound(&self.cfg, sounds::Sound::Error);
            return self.push(&json!({"type": "toast", "kind": "error", "text": "Clipping désactivé : rien n'est enregistré."}));
        }
        let (c, b, p) = (self.cfg_for_game(), self.buffer.clone(), self.proxy.clone());
        std::thread::spawn(move || {
            let _ = p.send_event(UserEvent::Saved(capture::save_clip(&c, &b.dir), "clip"));
        });
    }

    fn toggle_rec(&mut self) {
        if self.recording.is_none() && !self.cfg.clipping {
            sound(&self.cfg, sounds::Sound::Error);
            return self.push(&json!({"type": "toast", "kind": "error", "text": "Clipping désactivé : activez-le pour enregistrer."}));
        }
        if self.recording.take().is_none() {
            if let Err(e) = capture::rec_start(&self.cfg, &self.buffer) {
                sound(&self.cfg, sounds::Sound::Error);
                log(&e);
                self.push(&json!({"type": "toast", "kind": "error", "text": e}));
                return;
            }
            self.recording = Some(Instant::now());
            // Dossier du jeu au début du REC (à l'arrêt, le jeu peut être fermé ou la barre ouverte).
            self.rec_dir = self.cfg_for_game().out_dir;
            sound(&self.cfg, sounds::Sound::RecStart);
        } else {
            // Tout de suite à l'appui : l'assemblage du fichier prend ensuite 2 à 3 s.
            sound(&self.cfg, sounds::Sound::RecStop);
            let mut c = self.cfg.clone();
            c.out_dir = std::mem::take(&mut self.rec_dir);
            if c.out_dir.as_os_str().is_empty() {
                c.out_dir = self.cfg.out_dir.clone();
            }
            let (c, b, p) = (c, self.buffer.clone(), self.proxy.clone());
            std::thread::spawn(move || {
                let _ = p.send_event(UserEvent::Saved(capture::rec_stop(&c, &b), "rec"));
            });
        }
        self.push(&json!({"type": "state", "recording": self.recording.is_some(), "rec_secs": 0}));
    }

    /// Exécute `f` dans un fil et envoie son résultat aux fenêtres.
    fn bg(&self, f: impl FnOnce(&EventLoopProxy<UserEvent>) -> Value + Send + 'static) {
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            let v = f(&p);
            let _ = p.send_event(UserEvent::Push(v));
        });
    }

    fn ipc(&mut self, kind: ui::Kind, body: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let Ok(m) = serde_json::from_str::<Value>(body) else { return };
        let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match m["cmd"].as_str().unwrap_or("") {
            "set" => self.set(&s("key"), m["value"].clone()),
            "set_many" => {
                let items: Vec<(String, Value)> = m["values"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.set_many(&items);
            }
            // Vidéo en grand écran : la fenêtre suit (sinon la vidéo ne remplit que la fenêtre).
            "fullscreen" => {
                if let Some(w) = &self.main {
                    let on = m["on"].as_bool().unwrap_or(false);
                    w.window.set_fullscreen(on.then_some(tao::window::Fullscreen::Borderless(None)));
                }
            }
            "overlay_toggle" => self.toggle_overlay(),
            // Outils > « Ouvrir » : les dossiers d'une catégorie de nettoyage dans l'Explorateur (lecture seule).
            "clean_open" => {
                let id = s("id");
                if id == "recycle" {
                    return explorer("shell:RecycleBinFolder");
                }
                // Au plus 4 fenêtres (les navigateurs ont un dossier par profil).
                for d in tools::roots(&id).into_iter().filter(|d| d.is_dir()).take(4) {
                    explorer(&format!("\"{}\"", d.display()));
                }
            }
            // Ctrl+R / F5 dans la fenêtre : rouverte avec les réglages actuels (sinon la page rechargée
            // reprend ceux de l'ouverture, ex. l'intro désactivée revenait).
            // Fin de l'animation de sortie : la fenêtre est cachée (sauf si la barre a été rouverte entre-temps).
            "bar_hidden" => {
                if !self.bar_open && m["gen"].as_u64() == Some(self.bar_gen) {
                    if let Some(w) = &mut self.bar {
                        ui::conceal_bar(&w.window);
                        w.release(); // tampon rendu : la barre cachée ne garde presque rien en mémoire
                    }
                    self.barui.borrow_mut().player.close();
                    eco(self.main.is_none());
                }
            }
            // Galerie de la barre : les derniers clips.
            "recent" => {
                let c = self.cfg.clone();
                self.bg(move |p| list_videos(&c, p, Some(4)));
            }
            "update_found" => {
                self.update = m["version"].as_str().map(String::from);
                self.push(&json!({"type": "update", "version": self.update}));
            }
            // Bouton « Installer » du bandeau : téléchargement vérifié puis installateur silencieux,
            // qui ferme Clipper et le relance à jour.
            "update_install" => self.bg(|_| {
                // La release reste en attente : un échec (réseau) n'empêche pas de réessayer.
                let r = update::FOUND.lock().unwrap().clone();
                let res = r.ok_or("Aucune mise à jour en attente.".to_string()).and_then(|r| update::download(&r)).and_then(|p| update::install(&p));
                log(&format!("mise à jour : {res:?}"));
                match res {
                    Ok(()) => json!({"type": "update_state", "ok": true, "text": "Installation… Clipper va se fermer puis se relancer à jour."}),
                    Err(e) => json!({"type": "update_state", "ok": false, "text": format!("Mise à jour impossible : {e}")}),
                }
            }),
            // Bouton « Installer FFmpeg » : winget dans une fenêtre visible, puis Clipper se met à jour seul.
            "install_ffmpeg" => {
                let p = self.proxy.clone();
                std::thread::spawn(move || {
                    let ok = std::process::Command::new("winget")
                        .args(["install", "-e", "--id", "Gyan.FFmpeg", "--scope", "user", "--accept-package-agreements", "--accept-source-agreements"])
                        .status()
                        .is_ok_and(|s| s.success());
                    let _ = p.send_event(UserEvent::Ipc(ui::Kind::Main, json!({"cmd": "ffmpeg_installed", "ok": ok}).to_string()));
                });
            }
            "ffmpeg_installed" => {
                self.ffmpeg_ok = capture::locate_ffmpeg(&self.cfg);
                if self.ffmpeg_ok {
                    self.encoders = capture::probe_encoders(&self.cfg);
                    self.cfg.encoder = capture::resolve_encoder(&self.cfg, &self.encoders, &self.gpus);
                    self.native = capture::native_size(&self.cfg).unwrap_or((1920, 1080));
                    self.monitors = capture::monitors(&self.cfg);
                    self.restart_capture(false);
                    self.push(&json!({"type": "toast", "kind": "ok", "text": "FFmpeg installé : Clipper enregistre."}));
                } else {
                    self.push(&json!({"type": "toast", "kind": "error", "text": "FFmpeg n'a pas pu être installé (winget)."}));
                }
                // Fenêtre rouverte : elle reçoit les nouveaux encodeurs et écrans.
                if self.main.is_some() {
                    self.close_main();
                    self.open(ui::Kind::Main, target, None);
                }
            }
            "clip" => self.clip(),
            "rec" => self.toggle_rec(),
            "open_settings" => {
                self.hide_bar(false);
                self.open(ui::Kind::Main, target, m.get("tab").and_then(Value::as_str));
            }
            "close" => match kind {
                // La page a déjà joué son fondu : fenêtre cachée d'abord, puis détruite (pas d'écran blanc).
                ui::Kind::Main => self.close_main(),
                // Échap ou clic dans le vide autour de la barre.
                ui::Kind::Bar => self.hide_bar(m["refocus"].as_bool().unwrap_or(true)),
            },
            "minimize" => {
                // Option « Réduire dans la zone de notification » : la fenêtre disparaît (mémoire de l'interface
                // libérée) et Clipper reste près de l'horloge ; sinon réduction classique dans la barre des tâches.
                if self.cfg.minimize_to_tray {
                    self.close_main();
                } else if let Some(w) = &self.main {
                    w.window.set_minimized(true);
                }
            }
            "drag" => {
                if let Some(w) = &self.main {
                    let _ = w.window.drag_window();
                }
            }
            "pick_folder" => {
                let key = s("key");
                let cur = if key == "buffer_dir" { &self.cfg.buffer_dir } else { &self.cfg.out_dir };
                if let Some(dir) = rfd::FileDialog::new().set_directory(cur).pick_folder() {
                    let dir = if key == "buffer_dir" { dir.join("Clipper-buffer") } else { dir };
                    self.set(&key, json!(dir));
                    self.push(&json!({"type": "folder", "key": key, "path": dir, "free_bytes": disk_free(&dir)}));
                }
            }
            "disk_free" => {
                let p = PathBuf::from(s("path"));
                self.push(&json!({"type": "disk", "path": p, "free_bytes": disk_free(&p)}));
            }
            "videos" => {
                let c = self.cfg.clone();
                self.bg(move |p| list_videos(&c, p, None));
            }
            "video_action" => self.video_action(&s("action"), Path::new(&s("path")), &s("name")),
            "levels" => {
                let peak = |src: &audio::Source| f32::from_bits(src.peak.swap(0, std::sync::atomic::Ordering::Relaxed));
                let (g, mi) = self.audio.as_ref().map_or((0.0, 0.0), |a| (peak(&a.game), peak(&a.mic)));
                self.push(&json!({"type": "levels", "game": g, "mic": mi}));
            }
            "driver_check" => {
                let studio = s("branch") == "studio";
                self.set("driver_branch", json!(if studio { "studio" } else { "game_ready" }));
                // Carte et version installée relues à chaque vérification, hors du fil de l'interface : après une
                // mise à jour du pilote (par Clipper ou non), l'onglet montre la nouvelle version. Détection
                // ratée au lancement (pilote en cours d'installation) : rattrapée ici.
                let known = self.gpu.as_ref().map(|g| (g.name.clone(), g.driver.clone()));
                let branch = s("branch");
                self.bg(move |p| {
                    let Some((name, inst)) = drivers::installed().map(|g| (g.name, g.driver)).or(known) else {
                        return json!({"type": "driver", "status": "nogpu"});
                    };
                    let _ = p.send_event(UserEvent::Push(json!({"type": "gpu", "name": name, "driver": inst})));
                    match drivers::latest(&name, studio) {
                        Ok(l) => {
                            let _ = p.send_event(UserEvent::Push(json!({"type": "driver_url", "url": l.url})));
                            json!({"type": "driver", "installed": inst, "latest": l.version, "branch": branch, "url": l.url,
                                   "status": if drivers::is_newer(&l.version, &inst) { "update" } else { "ok" }})
                        }
                        Err(e) => json!({"type": "driver", "installed": inst, "branch": branch, "status": "error", "error": e}),
                    }
                });
            }
            "driver_install" => {
                let Some(url) = self.driver_url.clone() else { return };
                let before = self.gpu.as_ref().map(|g| g.driver.clone()).unwrap_or_default();
                self.bg(move |p| {
                    let name = url.rsplit('/').next().unwrap_or("nvidia-driver.exe").to_string();
                    let dest = std::env::temp_dir().join("Clipper").join("driver").join(name);
                    let prog = |done: u64, total: u64| {
                        let _ = p.send_event(UserEvent::Push(json!({"type": "driver_progress", "done": done, "total": total, "phase": "download"})));
                    };
                    let mut ran = false; // installateur lancé (sinon : échec du téléchargement, rien à attendre)
                    let r = drivers::download(&url, &dest, prog).and_then(|exe| {
                        ran = true;
                        let _ = p.send_event(UserEvent::Push(json!({"type": "driver_progress", "phase": "install", "done": 0, "total": 0})));
                        let r = drivers::install(&exe);
                        let _ = std::fs::remove_file(&exe);
                        r
                    });
                    // L'installateur a fini : on vérifie que la version a vraiment changé avant de dire « installé ».
                    // Le pilote se recharge pendant quelques secondes : on relit jusqu'à 20 s.
                    let mut after = String::new();
                    for _ in 0..10 {
                        after = drivers::installed().map(|g| g.driver).unwrap_or_default();
                        // Même en cas d'erreur de l'installateur : il renvoie parfois un code d'échec (0x80070005,
                        // étape annexe refusée) alors que le pilote est bien installé. La version fait foi.
                        if !ran || (!after.is_empty() && after != before) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    if let Some(g) = drivers::installed() {
                        let _ = p.send_event(UserEvent::Push(json!({"type": "gpu", "name": g.name, "driver": g.driver})));
                    }
                    log(&format!("pilote : {before} -> {after} ({r:?})"));
                    match r {
                        _ if !after.is_empty() && after != before => json!({"type": "driver_progress", "phase": "done", "done": 0, "total": 0, "installed": after}),
                        Ok(()) => json!({"type": "driver_progress", "phase": "error", "done": 0, "total": 0,
                                         "error": format!("L'installateur NVIDIA s'est terminé mais le pilote est toujours en {before}. Redémarrez le PC puis vérifiez à nouveau ; sinon relancez l'installation.")}),
                        Err(e) => json!({"type": "driver_progress", "phase": "error", "error": e, "done": 0, "total": 0}),
                    }
                });
            }
            "clean_scan" => {
                let keep = vec![self.cfg.buffer_dir.clone()];
                self.bg(move |_| {
                    let items: Vec<Value> = tools::CATEGORIES
                        .iter()
                        .map(|c| {
                            let (b, n) = tools::scan(c.id, &keep);
                            json!({"id": c.id, "name": c.name, "bytes": b, "files": n, "admin": c.admin, "default": c.default, "warn": c.warn})
                        })
                        .collect();
                    json!({"type": "clean", "items": items})
                });
            }
            "clean_run" => {
                let ids: Vec<String> = m["ids"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
                let keep = vec![self.cfg.buffer_dir.clone()];
                self.bg(move |_| {
                    let known = |id: &str| tools::CATEGORIES.iter().find(|c| c.id == id);
                    let (admin, user): (Vec<_>, Vec<_>) = ids.into_iter().filter(|i| known(i).is_some()).partition(|i| known(i).unwrap().admin);
                    let (mut freed, mut skipped) = (0, 0);
                    for id in &user {
                        let (f, s) = tools::clean(id, &keep);
                        (freed, skipped) = (freed + f, skipped + s);
                    }
                    if !admin.is_empty() {
                        let before: u64 = admin.iter().map(|i| tools::scan(i, &keep).0).sum();
                        if tools::run_elevated(&format!("--clean {}", admin.join(" "))).is_ok() {
                            let after: u64 = admin.iter().map(|i| tools::scan(i, &keep).0).sum();
                            freed += before.saturating_sub(after);
                        } else {
                            skipped += 1;
                        }
                    }
                    json!({"type": "clean_done", "freed_bytes": freed, "skipped": skipped})
                });
            }
            "admin_mode" => {
                let on = m["value"].as_bool().unwrap_or(false);
                self.bg(move |_| {
                    let ok = tools::run_elevated(&format!("--admin-task {}", if on { "on" } else { "off" })).is_ok_and(|c| c == 0);
                    json!({"type": "admin", "ok": ok, "on": on})
                });
            }
            "autostart" => {
                let on = m["value"].as_bool().unwrap_or(false);
                if !elevate::set_autostart(on) {
                    self.push(&json!({"type": "toast", "kind": "error", "text": "Impossible de modifier le démarrage avec Windows."}));
                }
                self.push(&json!({"type": "autostart", "on": elevate::autostart()}));
            }
            "relaunch_admin" => self.relaunch(),
            "ram_clean" => self.bg(|_| {
                let (before, total) = tools::ram();
                let game = capture::fullscreen_foreground();
                let ok = tools::run_elevated(if game { "--clean-ram --standby-only" } else { "--clean-ram" }).is_ok();
                std::thread::sleep(std::time::Duration::from_millis(500));
                let (after, _) = tools::ram();
                json!({"type": "ram", "free_before": before, "free_after": if ok { after } else { before }, "total": total, "ok": ok})
            }),
            other => log(&format!("commande UI inconnue : {other}")),
        }
    }

    /// Change un champ (chemin "clip.height" accepté), valide via serde, sauvegarde, relance si besoin.
    fn set(&mut self, key: &str, value: Value) {
        self.set_many(&[(key.to_string(), value)]);
    }

    /// Plusieurs réglages d'un coup (bouton « Enregistrer ») : une seule relance de ffmpeg au plus.
    fn set_many(&mut self, items: &[(String, Value)]) {
        // Réglages sans effet sur ffmpeg : pas de relance (pas de trou dans le buffer).
        let live = ["driver_branch", "out_dir", "container", "overlay_mode", "overlay_items", "overlay_corner", "overlay_size", "overlay_opacity", "overlay_refresh_ms", "overlay_gpus", "overlay_graphs", "intro", "sounds", "update_check", "delete_permanent", "delete_confirm", "key_clip", "key_rec", "key_bar", "key_overlay", "admin_mode", "minimize_to_tray", "key_clipping"];
        let mut v = serde_json::to_value(&self.cfg).unwrap_or_default();
        let mut restart = false;
        let reprobe = items.iter().any(|(k, _)| k == "monitor");
        'items: for (key, value) in items {
            let mut slot = &mut v;
            for part in key.split('.') {
                match slot.get_mut(part) {
                    Some(s) => slot = s,
                    None => {
                        log(&format!("réglage inconnu : {key}"));
                        continue 'items;
                    }
                }
            }
            *slot = value.clone();
            restart |= !live.contains(&key.split('.').next().unwrap_or(""));
        }
        let Ok(mut new) = serde_json::from_value::<Config>(v) else { return log("valeurs refusées") };
        new.rec = new.clip; // une seule qualité : le REC réutilise toujours le buffer
        let keys_changed = (&new.key_clip, &new.key_rec, &new.key_bar, &new.key_overlay, &new.key_clipping) != (&self.cfg.key_clip, &self.cfg.key_rec, &self.cfg.key_bar, &self.cfg.key_overlay, &self.cfg.key_clipping);
        self.cfg = new;
        if keys_changed {
            self.rebind_keys();
        }
        if let Err(e) = self.cfg.save() {
            log(&format!("config non sauvegardée : {e}"));
        }
        if restart {
            self.restart_capture(reprobe);
        }
        overlay::apply(&self.cfg, self.overlay_shown);
        self.push(&json!({"type": "config", "config": self.cfg}));
        self.push(&json!({"type": "overlay", "on": self.overlay_visible()}));
    }

    /// Relance Clipper dans le mode demandé par le réglage : par la tâche (administrateur, sans demande UAC)
    /// ou normalement (par l'Explorateur, qui lance toujours sans élévation).
    fn relaunch(&mut self) {
        if self.recording.is_some() {
            return self.push(&json!({"type": "toast", "kind": "error", "text": "Arrêtez le REC : Clipper se relancera ensuite dans le bon mode."}));
        }
        let ok = if self.cfg.admin_mode { elevate::run_task(false) } else { elevate::relaunch_normal() };
        if ok {
            let _ = self.proxy.send_event(UserEvent::Quit);
        } else {
            self.push(&json!({"type": "toast", "kind": "error", "text": "Relance impossible : fermez puis rouvrez Clipper."}));
        }
    }

    /// Ferme la fenêtre principale : cachée, détruite, son contenu (et le lecteur) libérés.
    fn close_main(&mut self) {
        if let Some(w) = self.main.take() {
            w.window.set_visible(false);
        }
        *self.front.borrow_mut() = None;
        self.wake[0] = None;
        eco(!self.bar_open);
        trim_self();
    }

    fn overlay_visible(&self) -> bool {
        self.cfg.overlay_mode == "always" || (self.cfg.overlay_mode == "hotkey" && self.overlay_shown)
    }

    /// Alt+F et le bouton Stats de la barre : afficher / masquer l'overlay, quel que soit le mode.
    /// Mode « toujours » ou « masqué » : on repasse en mode raccourci, sinon le raccourci ne ferait plus rien.
    fn toggle_overlay(&mut self) {
        let on = !self.overlay_visible();
        self.overlay_shown = on;
        if self.cfg.overlay_mode == "hotkey" {
            overlay::apply(&self.cfg, on);
            self.push(&json!({"type": "overlay", "on": on}));
        } else {
            self.set("overlay_mode", json!("hotkey")); // applique et prévient les fenêtres
        }
    }

    fn video_action(&mut self, action: &str, path: &Path, name: &str) {
        // Dossier vide de l'onglet Vidéos : ouvre le dossier de « Emplacements ».
        if action == "open_folder" && path == self.cfg.out_dir.as_path() {
            let _ = std::fs::create_dir_all(path);
            return explorer(&format!("\"{}\"", path.display()));
        }
        // Seulement des fichiers du dossier de sortie ou d'un de ses dossiers de jeu.
        if !in_out_dir(&self.cfg, path) {
            return log("action vidéo refusée hors du dossier de sortie");
        }
        let thumb = |p: &Path| config::dir().join("thumbs").join(format!("{}.jpg", p.file_stem().unwrap_or_default().to_string_lossy()));
        match action {
            "open_folder" => return explorer(&format!("/select,\"{}\"", path.display())),
            // Lecteur par défaut de Windows.
            "open" => return explorer(&format!("\"{}\"", path.display())),
            "delete" => {
                // Réglage « Supprimer définitivement » : le fichier est effacé, sans passer par la Corbeille.
                let gone = if self.cfg.delete_permanent { std::fs::remove_file(path).is_ok() } else { to_recycle_bin(path) };
                if gone {
                    let _ = std::fs::remove_file(thumb(path));
                }
            }
            "rename" => {
                let clean: String = name.trim().chars().filter(|c| !r#"\/:*?"<>|"#.contains(*c)).collect();
                if clean.is_empty() {
                    return;
                }
                let ext = path.extension().unwrap_or_default().to_string_lossy();
                let to = path.with_file_name(format!("{}.{ext}", clean.trim_end_matches(&format!(".{ext}"))));
                if !to.exists() && std::fs::rename(path, &to).is_ok() {
                    let _ = std::fs::rename(thumb(path), thumb(&to));
                }
            }
            _ => {}
        }
        let c = self.cfg.clone();
        self.bg(move |p| list_videos(&c, p, None));
    }
}

fn main() {
    // Sans console : tout panic finit dans le journal, avec l'endroit exact.
    std::panic::set_hook(Box::new(|i| log(&format!("PANIC : {i}"))));
    // Sous-commandes lancées en mode administrateur (UAC) par l'app elle-même.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--clean-ram") => std::process::exit(tools::clean_ram_elevated(args.iter().any(|a| a == "--standby-only"))),
        Some("--clean") => std::process::exit(tools::clean_elevated(&args[1..], &[Config::load().buffer_dir])),
        Some("--admin-task") => std::process::exit(elevate::set_task(args.get(1).is_some_and(|a| a == "on"))),
        Some("--quit") => std::process::exit(if elevate::signal_quit() { 0 } else { 1 }),
        _ => {}
    }
    let from_task = args.iter().any(|a| a == "--from-task");
    let tray_start = args.iter().any(|a| a == "--tray") || (from_task && elevate::take_tray_flag());
    // Relance demandée par l'instance précédente (changement de mode) : elle est en train de se fermer.
    let relaunched = from_task || elevate::take_relaunch_flag();
    // Une seule instance : un 2e double-clic réveille la première (fenêtre réglages) puis quitte.
    if !relaunched && elevate::signal_existing() {
        return;
    }
    // Mode administrateur : on passe par la tâche planifiée (aucune demande UAC). La tâche doit lancer CE
    // programme : après une installation ailleurs (ou une vieille tâche), elle est recréée, avec une seule
    // demande UAC. Échec ou refus : lancement normal.
    if !from_task && Config::load().admin_mode && !elevate::is_elevated() {
        if !elevate::task_ok() {
            log("mode administrateur : tâche absente ou liée à un autre programme, recréée");
            let _ = tools::run_elevated("--admin-task on");
        }
        if elevate::task_ok() && elevate::run_task(tray_start) {
            return;
        }
        log("mode administrateur : tâche indisponible, lancement normal");
    }
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    let Some(ev) = elevate::own_event(relaunched) else { return };
    kill_children_with_us();
    let ev = ev as usize;
    lower_priority();
    let mut cfg = Config::load();
    let ffmpeg_ok = capture::locate_ffmpeg(&cfg);
    let encoders = capture::probe_encoders(&cfg);
    let gpus = capture::gpus();
    cfg.encoder = capture::resolve_encoder(&cfg, &encoders, &gpus);
    let native = capture::native_size(&cfg).unwrap_or((1920, 1080));
    let monitors = capture::monitors(&cfg);
    log(&format!("encodeurs {encoders:?}, choisi {}, écran {native:?}", cfg.encoder));
    overlay::scan_installed_games();
    // Une seule fois au lancement : jamais pendant qu'un clip s'assemble.
    capture::clean_leftovers(&cfg.out_dir);
    let (buffer, audio) = App::start_capture(&cfg, native);

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            let _ = proxy.send_event(UserEvent::Hotkey(e.id));
        }
    }));
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = proxy.send_event(UserEvent::Menu(e.id));
    }));
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || loop {
        unsafe { WaitForSingleObject(ev as _, INFINITE) };
        let _ = proxy.send_event(UserEvent::TrayClick);
    });
    if let Some(q) = elevate::quit_event() {
        let (q, proxy) = (q as usize, event_loop.create_proxy());
        std::thread::spawn(move || {
            unsafe { WaitForSingleObject(q as _, INFINITE) };
            let _ = proxy.send_event(UserEvent::Quit);
        });
    }
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        match e {
            TrayIconEvent::DoubleClick { .. } => drop(proxy.send_event(UserEvent::TrayClick)),
            TrayIconEvent::Enter { .. } => {
                let _ = proxy.send_event(UserEvent::TrayHover);
                // Menu peut-être ouvert : températures relues chaque seconde pendant 20 s, puis plus rien.
                static TICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !TICKING.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    let p = proxy.clone();
                    std::thread::spawn(move || {
                        for _ in 0..20 {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            let _ = p.send_event(UserEvent::TrayHover);
                        }
                        TICKING.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }
            }
            _ => {}
        }
    }));

    let hotkeys = GlobalHotKeyManager::new().expect("gestionnaire de raccourcis");
    let keys = register_keys(&hotkeys, &cfg);

    overlay::apply(&cfg, false);
    overlay::start();
    // Une fois les sondes de démarrage finies, leur RAM ne sert plus.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        trim_self();
    });
    let bar_ui = barui::BarUi::new(&cfg, redraw_fn(&event_loop.create_proxy(), ui::Kind::Bar));
    let mut app = App {
        cfg,
        encoders,
        ffmpeg_ok,
        update: None,
        gpus,
        cpu_temp: overlay::cpu_temp_available(),
        native,
        monitors,
        buffer,
        audio,
        recording: None,
        restart_pending: false,
        reprobe_pending: false,
        overlay_shown: false,
        main: None,
        bar: None,
        front: std::cell::RefCell::new(None),
        barui: std::cell::RefCell::new(bar_ui),
        wake: [None, None],
        last_draw: [None, None],
        perf: (0, 0.0, Instant::now()),
        bar_prev: 0,
        rec_dir: PathBuf::new(),
        bar_gen: 0,
        keys,
        hotkeys,
        bar_open: false,
        gpu: drivers::installed(),
        driver_url: None,
        proxy: event_loop.create_proxy(),
    };

    let label = |k: &Option<HotKey>| k.as_ref().map(pretty).unwrap_or_default();
    tray::start_temp_cache();
    let tm = tray::TrayMenu::new(&label(&app.keys.clip), &label(&app.keys.rec));
    let mut shown_rec = false;
    // Inverse de l'état réel : le texte de l'interrupteur du menu est posé au premier tour.
    let mut shown_clipping = !app.cfg.clipping;
    let mut tray = None;

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        // Souris, clavier, taille : traduits pour l'interface de la fenêtre concernée, qui se redessine.
        if let Event::WindowEvent { window_id, event: ref we, .. } = event {
            for win in [&mut app.main, &mut app.bar].into_iter().flatten() {
                if win.window.id() == window_id && win.event(we) {
                    win.window.request_redraw();
                }
            }
        }
        match event {
            Event::NewEvents(StartCause::Init) => {
                // Mises à jour : une requête GitHub, 10 s après le lancement (rien pendant le démarrage).
                if app.cfg.update_check {
                    let p = app.proxy.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        match update::check() {
                            Ok(Some(r)) => {
                                let v = r.version.clone();
                                *update::FOUND.lock().unwrap() = Some(r);
                                let _ = p.send_event(UserEvent::Ipc(ui::Kind::Main, json!({"cmd": "update_found", "version": v}).to_string()));
                            }
                            Ok(None) => {}
                            Err(e) => log(&format!("mise à jour : {e}")),
                        }
                    });
                }
                tray = TrayIconBuilder::new()
                    .with_tooltip("Clipper")
                    .with_icon(tray_icon_image())
                    .with_menu(Box::new(tm.menu.clone()))
                    .build()
                    .ok();
                if !tray_start {
                    app.open(ui::Kind::Main, target, None);
                }
                // Barre créée d'avance, cachée : Alt+Z l'affiche sans attendre WebView2.
                app.open(ui::Kind::Bar, target, None);
            }
            Event::UserEvent(UserEvent::Hotkey(h)) => {
                let is = |k: &Option<HotKey>| k.is_some_and(|k| k.id() == h);
                if is(&app.keys.clip) {
                    app.clip();
                } else if is(&app.keys.rec) {
                    app.toggle_rec();
                } else if is(&app.keys.bar) {
                    if app.bar_open {
                        app.hide_bar(true);
                    } else {
                        app.open_bar(target);
                    }
                } else if is(&app.keys.overlay) {
                    app.toggle_overlay();
                } else if is(&app.keys.clipping) {
                    app.toggle_clipping();
                }
            }
            Event::UserEvent(UserEvent::Ipc(kind, body)) => app.ipc(kind, &body, target),
            Event::UserEvent(UserEvent::Push(v)) => {
                if v["type"] == "driver_url" {
                    app.driver_url = v["url"].as_str().map(String::from);
                } else if v["type"] == "gpu" {
                    app.gpu = Some(drivers::Gpu { name: v["name"].as_str().unwrap_or("").into(), driver: v["driver"].as_str().unwrap_or("").into() });
                    app.push(&v);
                } else if v["type"] == "admin" {
                    if v["ok"] == true {
                        app.set("admin_mode", v["on"].clone());
                    }
                    app.push(&json!({"type": "admin", "ok": v["ok"], "on": v["on"], "elevated": elevate::is_elevated()}));
                    // Le mode demandé prend effet tout de suite : relance dans le bon mode (sauf pendant un REC).
                    if v["ok"] == true && v["on"].as_bool() != Some(elevate::is_elevated()) && app.recording.is_none() {
                        app.relaunch();
                    }
                } else {
                    app.push(&v);
                }
            }
            Event::UserEvent(UserEvent::Saved(r, kind)) => {
                match (&r, kind) {
                    (Err(_), _) => sound(&app.cfg, sounds::Sound::Error),
                    (Ok(_), "clip") => sound(&app.cfg, sounds::Sound::Clip),
                    _ => {} // fin de REC : son déjà joué à l'appui
                }
                log(&format!("{kind} : {r:?}"));
                let msg = match &r {
                    Ok(p) => {
                        // Durée réelle du fichier (pas celle du réglage) : un clip trop court se voit tout de suite.
                        let secs = capture::probe(&app.cfg, p).secs.round() as u64;
                        let len = if secs >= 60 { format!("{} min {:02} s", secs / 60, secs % 60) } else { format!("{secs} s") };
                        log(&format!("{kind} : {len}"));
                        json!({"type": "toast", "kind": kind, "path": p,
                            "text": format!("{} enregistré · {len}", if kind == "clip" { "Clip" } else { "REC" })})
                    }
                    Err(e) => json!({"type": "toast", "kind": "error", "text": e}),
                };
                app.push(&msg);
                if kind == "rec" && app.recording.is_none() && app.restart_pending {
                    // Réglages changés pendant le REC : appliqués maintenant.
                    app.restart_capture(false);
                }
            }
            Event::WindowEvent { window_id, event: tao::event::WindowEvent::CloseRequested, .. } => {
                if app.main.as_ref().is_some_and(|w| w.window.id() == window_id) {
                    app.close_main();
                } else if app.bar.as_ref().is_some_and(|w| w.window.id() == window_id) {
                    app.hide_bar(true);
                }
            }
            // Clic ailleurs sur l'écran : la barre perd le focus et se ferme (avec son animation).
            Event::WindowEvent { window_id, event: tao::event::WindowEvent::Focused(false), .. } => {
                if app.bar.as_ref().is_some_and(|w| w.window.id() == window_id) {
                    // Clic dans le jeu (ou nulle part) : le focus lui est rendu comme avec Alt+Z, sinon certains
                    // jeux (The Last of Us) ne reprenaient plus la souris dans leurs menus. Clic sur une autre
                    // appli (Discord…) : elle garde le focus.
                    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
                    let pid = |h: isize| {
                        let mut p = 0u32;
                        unsafe { GetWindowThreadProcessId(h as _, &mut p) };
                        p
                    };
                    // Premier plan pas encore changé (la barre elle-même) : traité comme « nulle part ».
                    let fg = unsafe { GetForegroundWindow() } as isize;
                    let bar = app.bar.as_ref().map_or(0, |w| tao::platform::windows::WindowExtWindows::hwnd(&w.window) as isize);
                    let game = app.bar_prev != 0 && (fg == 0 || fg == bar || pid(fg) == pid(app.bar_prev));
                    app.hide_bar(game);
                }
            }
            Event::RedrawRequested(window_id) => {
                let kind = if app.bar.as_ref().is_some_and(|w| w.window.id() == window_id) { ui::Kind::Bar } else { ui::Kind::Main };
                app.draw(kind, target);
            }
            Event::UserEvent(UserEvent::Redraw(kind)) => {
                if let Some(w) = if kind == ui::Kind::Main { &app.main } else { &app.bar } {
                    w.window.request_redraw();
                }
            }
            Event::UserEvent(UserEvent::TrayClick) => app.open(ui::Kind::Main, target, None),
            Event::UserEvent(UserEvent::TrayHover) => tm.refresh_temps(),
            Event::UserEvent(UserEvent::Menu(m)) => {
                if m == *tm.quit.id() {
                    let _ = app.proxy.send_event(UserEvent::Quit);
                } else if m == *tm.open.id() {
                    app.open(ui::Kind::Main, target, None);
                } else if m == *tm.clip.id() {
                    app.clip();
                } else if m == *tm.rec.id() {
                    app.toggle_rec();
                } else if m == *tm.clipping.id() {
                    app.toggle_clipping();
                } else if m == *tm.ram.id() {
                    // Même chemin que le bouton de l'onglet Outils (autorisation administrateur demandée).
                    let _ = app.proxy.send_event(UserEvent::Ipc(ui::Kind::Main, r#"{"cmd":"ram_clean"}"#.into()));
                }
            }
            Event::UserEvent(UserEvent::Quit) => {
                overlay::shutdown();
                app.buffer.stop();
                if let Some(a) = &app.audio {
                    a.stop();
                }
                for w in [&app.main, &app.bar].into_iter().flatten() {
                    w.window.set_visible(false); // cachées avant d'être détruites : pas d'écran blanc
                }
                *app.front.borrow_mut() = None; // arrête aussi le lecteur
                app.barui.borrow_mut().player.close();
                (app.main, app.bar) = (None, None);
                tray.take();
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
        // Dessins programmés (animation en cours, compteur REC, notification qui s'efface) : la boucle dort
        // jusqu'au prochain ; sans rien de programmé, elle ne se réveille que sur un événement.
        let now = Instant::now();
        // Dessin direct, sans request_redraw : tao perdait parfois ces demandes (intro figée jusqu'au
        // premier mouvement de souris).
        for (i, kind) in [ui::Kind::Main, ui::Kind::Bar].into_iter().enumerate() {
            if *flow != ControlFlow::Exit && app.wake[i].is_some_and(|t| t <= now) {
                app.wake[i] = None;
                app.draw(kind, target);
            }
        }
        if *flow != ControlFlow::Exit {
            if let Some(t) = app.wake.iter().flatten().min() {
                *flow = ControlFlow::WaitUntil(*t);
            }
        }
        if app.cfg.clipping != shown_clipping {
            shown_clipping = app.cfg.clipping;
            tm.set_clipping(shown_clipping);
        }
        if app.recording.is_some() != shown_rec {
            shown_rec = app.recording.is_some();
            tm.set_recording(shown_rec);
        }
    });
}
