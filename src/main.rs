#![cfg_attr(not(test), windows_subsystem = "windows")]

mod audio;
mod capture;
mod config;
mod drivers;
mod elevate;
mod overlay;
mod tools;
mod tray;
mod update;
mod ui;

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
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as _, std::mem::size_of_val(&info) as u32);
        AssignProcessToJobObject(job, GetCurrentProcess());
        // Le handle reste ouvert jusqu'à la fin du processus : c'est sa fermeture qui déclenche l'arrêt.
    }
}

/// Priorité "sous la normale" : le jeu passe toujours avant nous.
fn lower_priority() {
    use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass};
    unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    // EcoQoS : Windows place nos fils sur les cœurs économes, à basse fréquence. ffmpeg n'en hérite pas.
    use windows_sys::Win32::System::Threading::{PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation};
    let s = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED, StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED };
    unsafe { SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, &s as *const _ as _, std::mem::size_of_val(&s) as u32) };
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(config::dir().join("log.txt")) {
        // Heure locale : un bug signalé se retrouve dans le journal à la seconde près.
        let t = unsafe {
            let mut t = std::mem::zeroed();
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t);
            t
        };
        let _ = writeln!(f, "{:02}/{:02} {:02}:{:02}:{:02} {msg}", t.wDay, t.wMonth, t.wHour, t.wMinute, t.wSecond);
    }
}

fn beep(ok: bool) {
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONHAND, MB_OK};
    unsafe { MessageBeep(if ok { MB_OK } else { MB_ICONHAND }) };
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

/// Vidéos du dossier de sortie, plus récentes d'abord. ffprobe en parallèle et en cache : liste immédiate.
/// Les clips récents qui ne se lisent pas tels quels ont leur copie de lecture préparée en tâche de fond.
fn list_videos(cfg: &Config, proxy: &EventLoopProxy<UserEvent>) -> Value {
    let mut v: Vec<(std::time::SystemTime, PathBuf, u64)> = std::fs::read_dir(&cfg.out_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| ["mp4", "mkv", "mov"].contains(&x.to_ascii_lowercase().as_str())))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path(), m.len()))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    let probes = par_probe(cfg, &v.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
    let mut warm = vec![];
    let items: Vec<Value> = v
        .into_iter()
        .zip(probes)
        .enumerate()
        .map(|(i, ((t, p, bytes), pr))| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let date = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let play = capture::ready_url(&p, &pr);
            if play.is_none() && i < 8 {
                warm.push(p.clone());
            }
            json!({
                "path": p, "name": name, "kind": if stem.starts_with("REC") { "REC" } else { "Clip" },
                "bytes": bytes, "secs": pr.secs, "date": date, "play": play,
                "thumb": format!("/thumb/{}", ui::pct_encode(&format!("{stem}.jpg"))),
            })
        })
        .collect();
    prewarm(cfg, warm, proxy);
    json!({"type": "videos", "items": items})
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

/// Copies de lecture préparées d'avance ; chaque copie prête est signalée à l'UI (lecture instantanée).
fn prewarm(cfg: &Config, clips: Vec<PathBuf>, proxy: &EventLoopProxy<UserEvent>) {
    let p = proxy.clone();
    capture::prewarm(cfg, clips, move |path, url| {
        let _ = p.send_event(UserEvent::Push(json!({"type": "preview", "path": path, "url": url})));
    });
}

struct Keys {
    clip: Option<HotKey>,
    rec: Option<HotKey>,
    bar: Option<HotKey>,
    overlay: Option<HotKey>,
    /// Raccourcis demandés mais pris par une autre app (NVIDIA, AMD…).
    conflicts: Vec<String>,
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
    /// Fenêtre au premier plan (le jeu) quand la barre s'est ouverte : elle reprend le focus à la fermeture.
    bar_prev: isize,
    /// Numéro de la barre ouverte : un « close » tardif d'une ancienne barre ne ferme pas la nouvelle.
    bar_gen: u64,
    keys: Keys,
    gpu: Option<drivers::Gpu>,
    driver_url: Option<String>,
    proxy: EventLoopProxy<UserEvent>,
}

impl App {
    fn start_capture(cfg: &Config, native: (u32, u32)) -> (Arc<capture::Buffer>, Option<Arc<audio::Audio>>) {
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

    fn init_json(&self, page: &str, tab: Option<&str>) -> String {
        let devs = |d| audio::devices(d).into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>();
        let k = |k: &Option<HotKey>| k.as_ref().map(pretty);
        json!({
            "page": page, "tab": tab, "config": self.cfg, "encoders": self.encoders, "ffmpeg_ok": self.ffmpeg_ok, "update": self.update,
            "native": [self.native.0, self.native.1], "monitors": self.monitors,
            "outputs": devs(wasapi::Direction::Render), "inputs": devs(wasapi::Direction::Capture),
            "hotkeys": {"clip": k(&self.keys.clip), "rec": k(&self.keys.rec), "bar": k(&self.keys.bar), "overlay": k(&self.keys.overlay)},
            "hotkey_conflicts": self.keys.conflicts,
            "overlay_on": self.overlay_visible(),
            "bar_gen": self.bar_gen,
            "recording": self.recording.is_some(), "rec_secs": self.recording.map_or(0, |t| t.elapsed().as_secs()),
            "gpu": self.gpu.as_ref().map(|g| json!({"name": g.name, "driver": g.driver})),
            "elevated": elevate::is_elevated(),
            "gpus": self.gpus, "chip": capture::effective_chip(&self.cfg, &self.gpus),
            "cpu_temp": self.cpu_temp, "autostart": elevate::autostart(),
        })
        .to_string()
    }

    fn push(&self, msg: &Value) {
        let s = msg.to_string();
        for w in [&self.main, &self.bar].into_iter().flatten() {
            w.push(&s);
        }
    }

    fn open(&mut self, kind: ui::Kind, target: &EventLoopWindowTarget<UserEvent>, tab: Option<&str>) {
        let slot = if kind == ui::Kind::Main { &self.main } else { &self.bar };
        if let Some(w) = slot {
            if kind == ui::Kind::Main {
                w.window.set_minimized(false);
                w.window.set_focus();
                if let Some(t) = tab {
                    w.push(&json!({"type": "tab", "tab": t}).to_string());
                }
            }
            return;
        }
        let init = self.init_json(if kind == ui::Kind::Main { "index" } else { "bar" }, tab);
        let w = ui::open(kind, target, &init, self.cfg.out_dir.clone(), self.proxy.clone(), UserEvent::Ipc);
        if w.is_none() {
            log("WebView2 indisponible : impossible d'ouvrir la fenêtre");
        }
        if kind == ui::Kind::Main && w.is_some() {
            // Filet de sécurité : la page n'a pas dit "ready" (WebView2 lent, erreur JS) -> on montre quand même.
            let p = self.proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let _ = p.send_event(UserEvent::Ipc(ui::Kind::Main, r#"{"cmd":"ready"}"#.into()));
            });
        }
        match kind {
            ui::Kind::Main => self.main = w,
            ui::Kind::Bar => self.bar = w,
        }
    }

    /// Barre Alt+Z : mémorise le jeu, puis ouvre (la barre prend le focus : le jeu rend la souris).
    fn open_bar(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        self.bar_prev = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } as isize;
        self.bar_gen += 1;
        self.open(ui::Kind::Bar, target, None);
    }

    /// Ferme la barre et rend le focus au jeu (simple changement de fenêtre, comme Alt+Tab).
    fn close_bar(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, SetForegroundWindow};
        self.bar = None;
        let h = std::mem::take(&mut self.bar_prev) as windows_sys::Win32::Foundation::HWND;
        if !h.is_null() && unsafe { IsWindow(h) } != 0 {
            unsafe { SetForegroundWindow(h) };
        }
    }

    fn clip(&self) {
        let (c, b, p) = (self.cfg.clone(), self.buffer.clone(), self.proxy.clone());
        std::thread::spawn(move || {
            let _ = p.send_event(UserEvent::Saved(capture::save_clip(&c, &b.dir), "clip"));
        });
    }

    fn toggle_rec(&mut self) {
        if self.recording.take().is_none() {
            if let Err(e) = capture::rec_start(&self.cfg, &self.buffer) {
                beep(false);
                log(&e);
                self.push(&json!({"type": "toast", "kind": "error", "text": e}));
                return;
            }
            self.recording = Some(Instant::now());
            beep(true);
        } else {
            let (c, b, p) = (self.cfg.clone(), self.buffer.clone(), self.proxy.clone());
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
            // 1re image peinte : la fenêtre apparaît directement sur l'intro, sans écran blanc.
            "ready" => {
                if let Some(w) = self.main.as_ref().filter(|w| !w.window.is_visible()) {
                    w.window.set_visible(true);
                    w.window.set_focus();
                    // L'intro attend ce signal : sinon elle tournait déjà, fenêtre encore cachée.
                    w.push(&json!({"type": "shown"}).to_string());
                }
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
            "reload" => {
                if self.main.take().is_some() {
                    self.open(ui::Kind::Main, target, m.get("tab").and_then(Value::as_str));
                }
            }
            "update_found" => {
                self.update = m["version"].as_str().map(String::from);
                self.push(&json!({"type": "update", "version": self.update}));
            }
            // Bouton « Installer » du bandeau : téléchargement vérifié puis installateur silencieux,
            // qui ferme Clipper et le relance à jour.
            "update_install" => self.bg(|_| {
                let r = update::FOUND.lock().unwrap().take();
                let res = r.ok_or("Aucune mise à jour en attente.".to_string()).and_then(|r| update::download(&r)).and_then(|p| update::install(&p));
                match res {
                    Ok(()) => json!({"type": "toast", "kind": "ok", "text": "Installation de la mise à jour… Clipper va redémarrer."}),
                    Err(e) => json!({"type": "toast", "kind": "error", "text": format!("Mise à jour impossible : {e}")}),
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
                if self.main.take().is_some() {
                    self.open(ui::Kind::Main, target, None);
                }
            }
            "clip" => self.clip(),
            "rec" => self.toggle_rec(),
            "open_settings" => self.open(ui::Kind::Main, target, m.get("tab").and_then(Value::as_str)),
            "close" => match kind {
                ui::Kind::Main => self.main = None,
                // Échap : la barre demande sa fermeture 220 ms plus tard (animation). Si Alt+Z a entre-temps
                // fermé puis rouvert une barre, ce message vise l'ancienne : on l'ignore.
                ui::Kind::Bar if m["gen"].as_u64().is_none_or(|g| g == self.bar_gen) => self.close_bar(),
                ui::Kind::Bar => {}
            },
            "minimize" => {
                if let Some(w) = &self.main {
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
                self.bg(move |p| list_videos(&c, p));
            }
            "video_action" => self.video_action(&s("action"), Path::new(&s("path")), &s("name")),
            "video_preview" => {
                let p = PathBuf::from(s("path"));
                if p.parent() != Some(self.cfg.out_dir.as_path()) {
                    return log("lecture refusée hors du dossier de sortie");
                }
                let (c, force) = (self.cfg.clone(), m["force"].as_bool().unwrap_or(false));
                self.bg(move |_| match capture::preview(&c, &p, force) {
                    Ok(url) => json!({"type": "preview", "path": p, "url": url}),
                    Err(e) => json!({"type": "preview", "path": p, "error": e}),
                });
            }
            "levels" => {
                let peak = |src: &audio::Source| f32::from_bits(src.peak.swap(0, std::sync::atomic::Ordering::Relaxed));
                let (g, mi) = self.audio.as_ref().map_or((0.0, 0.0), |a| (peak(&a.game), peak(&a.mic)));
                self.push(&json!({"type": "levels", "game": g, "mic": mi}));
            }
            "driver_check" => {
                let studio = s("branch") == "studio";
                self.set("driver_branch", json!(if studio { "studio" } else { "game_ready" }));
                // Version installée relue à chaque vérification (nvidia-smi) : après une installation,
                // l'onglet ne propose plus de réinstaller le même pilote.
                self.gpu = drivers::installed().or(self.gpu.take());
                let Some(g) = &self.gpu else {
                    self.push(&json!({"type": "driver", "status": "error", "error": "Aucun GPU NVIDIA détecté."}));
                    return;
                };
                let (name, inst) = (g.name.clone(), g.driver.clone());
                let branch = s("branch");
                self.bg(move |p| match drivers::latest(&name, studio) {
                    Ok(l) => {
                        let _ = p.send_event(UserEvent::Push(json!({"type": "driver_url", "url": l.url})));
                        json!({"type": "driver", "installed": inst, "latest": l.version, "branch": branch, "url": l.url,
                               "status": if drivers::is_newer(&l.version, &inst) { "update" } else { "ok" }})
                    }
                    Err(e) => json!({"type": "driver", "installed": inst, "branch": branch, "status": "error", "error": e}),
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
                    let r = drivers::download(&url, &dest, prog).and_then(|exe| {
                        let _ = p.send_event(UserEvent::Push(json!({"type": "driver_progress", "phase": "install", "done": 0, "total": 0})));
                        let r = drivers::install(&exe);
                        let _ = std::fs::remove_file(&exe);
                        r
                    });
                    // L'installateur a fini : on vérifie que la version a vraiment changé avant de dire « installé ».
                    let after = drivers::installed().map(|g| g.driver).unwrap_or_default();
                    log(&format!("pilote : {before} -> {after} ({r:?})"));
                    match r {
                        Ok(()) if after != before => json!({"type": "driver_progress", "phase": "done", "done": 0, "total": 0, "installed": after}),
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
            "relaunch_admin" => {
                if self.recording.is_some() {
                    return self.push(&json!({"type": "toast", "kind": "error", "text": "Arrêtez le REC avant de relancer."}));
                }
                if elevate::run_task(false) {
                    let _ = self.proxy.send_event(UserEvent::Quit);
                } else {
                    self.push(&json!({"type": "toast", "kind": "error", "text": "Relance en administrateur impossible."}));
                }
            }
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
        let live = ["driver_branch", "out_dir", "container", "overlay_mode", "overlay_items", "overlay_corner", "overlay_size", "overlay_opacity", "overlay_refresh_ms", "overlay_graphs", "intro", "update_check", "delete_permanent", "delete_confirm", "key_clip", "key_rec", "key_bar", "key_overlay", "admin_mode"];
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
        self.cfg = new;
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
        // Seulement des fichiers directement dans le dossier de sortie.
        if path.parent() != Some(self.cfg.out_dir.as_path()) {
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
        self.bg(move |p| list_videos(&c, p));
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
    // Une seule instance : un 2e double-clic réveille la première (fenêtre réglages) puis quitte.
    if !from_task && elevate::signal_existing() {
        return;
    }
    // Mode administrateur : on passe par la tâche planifiée (aucune demande UAC). Échec : lancement normal.
    if !from_task && Config::load().admin_mode && !elevate::is_elevated() && elevate::run_task(tray_start) {
        return;
    }
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};
    let Some(ev) = elevate::own_event(from_task) else { return };
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

    // RegisterHotKey sous le capot : pas de hook clavier, rien d'injecté.
    let hotkeys = GlobalHotKeyManager::new().expect("gestionnaire de raccourcis");
    let mut conflicts = vec![];
    // Raccourci déjà pris (overlay NVIDIA Alt+Z / Alt+R, Radeon Software…) : repli Alt+Maj, puis Ctrl+Alt.
    let mut register = |s: &str| -> Option<HotKey> {
        for prefix in ["", "Shift+", "Control+"] {
            let k = format!("{prefix}{s}").parse::<HotKey>().ok()?;
            if hotkeys.register(k).is_ok() {
                if !prefix.is_empty() {
                    conflicts.push(s.replace("Key", ""));
                    log(&format!("raccourci {s} déjà pris par une autre app, remplacé par {prefix}{s}"));
                }
                return Some(k);
            }
        }
        conflicts.push(s.replace("Key", ""));
        log(&format!("raccourci {s} indisponible, même avec Maj ou Ctrl"));
        None
    };
    let (clip, rec, bar, overlay) = (register(&cfg.key_clip), register(&cfg.key_rec), register(&cfg.key_bar), register(&cfg.key_overlay));
    let keys = Keys { clip, rec, bar, overlay, conflicts };

    overlay::apply(&cfg, false);
    overlay::start();
    // Une fois les sondes de démarrage finies, leur RAM ne sert plus.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(10));
        trim_self();
    });
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
        bar_prev: 0,
        bar_gen: 0,
        keys,
        gpu: drivers::installed(),
        driver_url: None,
        proxy: event_loop.create_proxy(),
    };

    let label = |k: &Option<HotKey>| k.as_ref().map(pretty).unwrap_or_default();
    let tm = tray::TrayMenu::new(&label(&app.keys.clip), &label(&app.keys.rec));
    let mut shown_rec = false;
    let mut tray = None;

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
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
            }
            Event::UserEvent(UserEvent::Hotkey(h)) => {
                let is = |k: &Option<HotKey>| k.is_some_and(|k| k.id() == h);
                if is(&app.keys.clip) {
                    app.clip();
                } else if is(&app.keys.rec) {
                    app.toggle_rec();
                } else if is(&app.keys.bar) {
                    if app.bar.is_some() {
                        app.close_bar();
                    } else {
                        app.open_bar(target);
                    }
                } else if is(&app.keys.overlay) {
                    app.toggle_overlay();
                }
            }
            Event::UserEvent(UserEvent::Ipc(kind, body)) => app.ipc(kind, &body, target),
            Event::UserEvent(UserEvent::Push(v)) => {
                if v["type"] == "driver_url" {
                    app.driver_url = v["url"].as_str().map(String::from);
                } else if v["type"] == "admin" {
                    if v["ok"] == true {
                        app.set("admin_mode", v["on"].clone());
                    }
                    app.push(&json!({"type": "admin", "ok": v["ok"], "on": v["on"], "elevated": elevate::is_elevated()}));
                } else {
                    app.push(&v);
                }
            }
            Event::UserEvent(UserEvent::Saved(r, kind)) => {
                beep(r.is_ok());
                log(&format!("{kind} : {r:?}"));
                if let Ok(p) = &r {
                    prewarm(&app.cfg, vec![p.clone()], &app.proxy); // lisible dans l'app dès l'ouverture
                }
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
                // Fenêtre détruite = processus WebView2 libérés.
                if app.main.as_ref().is_some_and(|w| w.window.id() == window_id) {
                    app.main = None;
                    trim_self(); // la fenêtre et son WebView2 sont partis
                } else if app.bar.as_ref().is_some_and(|w| w.window.id() == window_id) {
                    app.bar = None;
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
                (app.main, app.bar) = (None, None);
                tray.take();
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
        if app.recording.is_some() != shown_rec {
            shown_rec = app.recording.is_some();
            tm.set_recording(shown_rec);
        }
    });
}
