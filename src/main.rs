#![cfg_attr(not(test), windows_subsystem = "windows")]

mod audio;
mod capture;
mod config;
mod drivers;
mod overlay;
mod tools;
mod ui;

use config::Config;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

pub enum UserEvent {
    Hotkey(u32),
    Menu(tray_icon::menu::MenuId),
    TrayClick,
    Ipc(ui::Kind, String),
    /// Message à envoyer à toutes les fenêtres ouvertes (`window.__on`).
    Push(Value),
    /// Fin d'un clip / REC lancé dans un fil (toast + son).
    Saved(Result<PathBuf, String>, &'static str),
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
        let _ = writeln!(f, "{msg}");
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

/// Corbeille (récupérable), jamais une suppression définitive.
fn to_recycle_bin(p: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW};
    let from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
    unsafe { SHFileOperationW(&mut op) == 0 }
}

/// Vidéos du dossier de sortie, plus récentes d'abord. Durée lue par ffprobe.
fn list_videos(cfg: &Config) -> Value {
    let mut v: Vec<(std::time::SystemTime, PathBuf, u64)> = std::fs::read_dir(&cfg.out_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()).is_some_and(|x| ["mp4", "mkv", "mov"].contains(&x)))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path(), m.len()))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    let items: Vec<Value> = v
        .into_iter()
        .map(|(t, p, bytes)| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let secs = capture::duration(cfg, &p);
            let date = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            json!({
                "path": p, "name": name, "kind": if stem.starts_with("REC") { "REC" } else { "Clip" },
                "bytes": bytes, "secs": secs, "date": date,
                "thumb": format!("/thumb/{}", ui::pct_encode(&format!("{stem}.jpg"))),
                "url": format!("/video?p={}", ui::pct_encode(&p.to_string_lossy())),
            })
        })
        .collect();
    json!({"type": "videos", "items": items})
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
    native: (u32, u32),
    monitors: Vec<Value>,
    buffer: Arc<capture::Buffer>,
    audio: Option<Arc<audio::Audio>>,
    recording: Option<Instant>,
    /// Overlay affiché par le raccourci (mode "hotkey").
    overlay_shown: bool,
    main: Option<ui::Win>,
    bar: Option<ui::Win>,
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

    fn restart_capture(&mut self) {
        if self.recording.is_some() {
            return; // jamais pendant un REC : on appliquera à l'arrêt
        }
        self.buffer.stop();
        if let Some(a) = &self.audio {
            a.stop();
        }
        self.cfg.encoder = capture::resolve_encoder(&self.cfg, &self.encoders);
        self.native = capture::native_size(&self.cfg).unwrap_or(self.native);
        (self.buffer, self.audio) = Self::start_capture(&self.cfg, self.native);
    }

    fn init_json(&self, page: &str, tab: Option<&str>) -> String {
        let devs = |d| audio::devices(d).into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>();
        let k = |k: &Option<HotKey>| k.as_ref().map(pretty);
        json!({
            "page": page, "tab": tab, "config": self.cfg, "encoders": self.encoders,
            "native": [self.native.0, self.native.1], "monitors": self.monitors,
            "outputs": devs(wasapi::Direction::Render), "inputs": devs(wasapi::Direction::Capture),
            "hotkeys": {"clip": k(&self.keys.clip), "rec": k(&self.keys.rec), "bar": k(&self.keys.bar), "overlay": k(&self.keys.overlay)},
            "hotkey_conflicts": self.keys.conflicts,
            "recording": self.recording.is_some(), "rec_secs": self.recording.map_or(0, |t| t.elapsed().as_secs()),
            "gpu": self.gpu.as_ref().map(|g| json!({"name": g.name, "driver": g.driver})),
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
        match kind {
            ui::Kind::Main => self.main = w,
            ui::Kind::Bar => self.bar = w,
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
            "clip" => self.clip(),
            "rec" => self.toggle_rec(),
            "open_settings" => self.open(ui::Kind::Main, target, m.get("tab").and_then(Value::as_str)),
            "close" => match kind {
                ui::Kind::Main => self.main = None,
                ui::Kind::Bar => self.bar = None,
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
                self.bg(move |_| list_videos(&c));
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
                    match r {
                        Ok(()) => json!({"type": "driver_progress", "phase": "done", "done": 0, "total": 0}),
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
        let mut v = serde_json::to_value(&self.cfg).unwrap_or_default();
        let mut slot = &mut v;
        for part in key.split('.') {
            match slot.get_mut(part) {
                Some(s) => slot = s,
                None => return log(&format!("réglage inconnu : {key}")),
            }
        }
        *slot = value;
        let Ok(new) = serde_json::from_value::<Config>(v) else { return log(&format!("valeur refusée pour {key}")) };
        self.cfg = new;
        if let Err(e) = self.cfg.save() {
            log(&format!("config non sauvegardée : {e}"));
        }
        // Réglages sans effet sur ffmpeg : pas de relance (pas de trou dans le buffer).
        let live = ["driver_branch", "out_dir", "container", "overlay_mode", "overlay_items", "overlay_corner", "overlay_size", "overlay_opacity", "key_clip", "key_rec", "key_bar", "key_overlay"];
        if !live.contains(&key.split('.').next().unwrap_or("")) {
            self.restart_capture();
        }
        overlay::apply(&self.cfg, self.overlay_shown);
        self.push(&json!({"type": "config", "config": self.cfg}));
    }

    fn video_action(&mut self, action: &str, path: &Path, name: &str) {
        // Seulement des fichiers directement dans le dossier de sortie.
        if path.parent() != Some(self.cfg.out_dir.as_path()) {
            return log("action vidéo refusée hors du dossier de sortie");
        }
        let thumb = |p: &Path| config::dir().join("thumbs").join(format!("{}.jpg", p.file_stem().unwrap_or_default().to_string_lossy()));
        match action {
            "open_folder" => {
                let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
            }
            "delete" => {
                if to_recycle_bin(path) {
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
        self.bg(move |_| list_videos(&c));
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
        _ => {}
    }
    // Une seule instance : un 2e double-clic réveille la première (fenêtre réglages) puis quitte.
    use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};
    let name: Vec<u16> = "Local\\ClipperOpenSettings".encode_utf16().chain(Some(0)).collect();
    let ev = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
        unsafe { SetEvent(ev) };
        return;
    }
    kill_children_with_us();
    let ev = ev as usize;
    lower_priority();
    let mut cfg = Config::load();
    let encoders = capture::probe_encoders(&cfg);
    cfg.encoder = capture::resolve_encoder(&cfg, &encoders);
    let native = capture::native_size(&cfg).unwrap_or((1920, 1080));
    let monitors = capture::monitors(&cfg);
    log(&format!("encodeurs {encoders:?}, choisi {}, écran {native:?}", cfg.encoder));
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
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::DoubleClick { .. } = e {
            let _ = proxy.send_event(UserEvent::TrayClick);
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
        native,
        monitors,
        buffer,
        audio,
        recording: None,
        overlay_shown: false,
        main: None,
        bar: None,
        keys,
        gpu: drivers::installed(),
        driver_url: None,
        proxy: event_loop.create_proxy(),
    };

    let open = MenuItem::new("Ouvrir", true, None);
    let quit = MenuItem::new("Quitter", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &quit]).expect("menu");
    let mut tray = None;

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                tray = TrayIconBuilder::new()
                    .with_tooltip("Clipper")
                    .with_icon(tray_icon_image())
                    .with_menu(Box::new(menu.clone()))
                    .build()
                    .ok();
                if !args.iter().any(|a| a == "--tray") {
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
                    if app.bar.take().is_none() {
                        app.open(ui::Kind::Bar, target, None);
                    }
                } else if is(&app.keys.overlay) {
                    app.overlay_shown = !app.overlay_shown;
                    overlay::apply(&app.cfg, app.overlay_shown);
                }
            }
            Event::UserEvent(UserEvent::Ipc(kind, body)) => app.ipc(kind, &body, target),
            Event::UserEvent(UserEvent::Push(v)) => {
                if v["type"] == "driver_url" {
                    app.driver_url = v["url"].as_str().map(String::from);
                } else {
                    app.push(&v);
                }
            }
            Event::UserEvent(UserEvent::Saved(r, kind)) => {
                beep(r.is_ok());
                log(&format!("{kind} : {r:?}"));
                let msg = match &r {
                    Ok(p) => json!({"type": "toast", "kind": kind, "path": p,
                        "text": if kind == "clip" { "Clip enregistré" } else { "REC enregistré" }}),
                    Err(e) => json!({"type": "toast", "kind": "error", "text": e}),
                };
                app.push(&msg);
                if kind == "rec" && app.recording.is_none() {
                    // Réglages changés pendant le REC : appliqués maintenant.
                    app.restart_capture();
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
            Event::UserEvent(UserEvent::Menu(m)) => {
                if m == quit.id() {
                    app.buffer.stop();
                    if let Some(a) = &app.audio {
                        a.stop();
                    }
                    (app.main, app.bar) = (None, None);
                    tray.take();
                    *flow = ControlFlow::Exit;
                } else if m == open.id() {
                    app.open(ui::Kind::Main, target, None);
                }
            }
            _ => {}
        }
    });
}
