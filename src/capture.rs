//! Buffer ffmpeg : capture -> encodeur matériel -> segments de 2 s (.ts, ou .mkv pour l'AV1).
use crate::config::Config;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL: u32 = 0x0000_4000;
pub const SEG_SECS: u32 = 2;

fn ffmpeg(cfg: &Config) -> Command {
    let mut c = Command::new(&cfg.ffmpeg);
    c.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// ffmpeg de l'enregistrement continu et du REC : priorité normale. Sous la normale, un jeu qui prend tout
/// le processeur (compilation des shaders, chargement) l'affamait : plus aucun segment pendant 13 s, relance,
/// et un trou dans le clip. L'encodage se fait sur la carte graphique : peu de processeur pris au jeu.
fn ffmpeg_capture(cfg: &Config) -> Command {
    const NORMAL: u32 = 0x0000_0020;
    let mut c = ffmpeg(cfg);
    c.creation_flags(CREATE_NO_WINDOW | NORMAL);
    c
}

/// Refuse le mode « efficacité » (EcoQoS) pour un processus lancé : sans réglage explicite, Windows
/// traitait les ffmpeg sans fenêtre comme du travail de fond (assemblage d'un clip en 7 s au lieu de 2 s).
fn no_eco(ch: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Threading::{PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation};
    let s = PROCESS_POWER_THROTTLING_STATE { Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION, ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED, StateMask: 0 };
    unsafe { SetProcessInformation(ch.as_raw_handle() as _, ProcessPowerThrottling, &s as *const _ as _, std::mem::size_of_val(&s) as u32) };
}

/// Priorité « sous la normale » sur la carte graphique pour notre ffmpeg de capture : le planificateur GPU
/// de Windows sert d'abord le jeu (copie de l'écran, conversion des couleurs et encodage passent après ses
/// images). Réglé sur notre propre processus, rien n'est ouvert ni modifié dans le jeu.
fn gpu_low(ch: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    let name: Vec<u16> = "gdi32.dll".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let lib = LoadLibraryW(name.as_ptr());
        if lib.is_null() {
            return;
        }
        let Some(f) = GetProcAddress(lib, b"D3DKMTSetProcessSchedulingPriorityClass\0".as_ptr()) else { return };
        let set: unsafe extern "system" fn(*mut core::ffi::c_void, i32) -> i32 = std::mem::transmute(f);
        // Refusé (0xC000000D) tant que le processus démarre : réessayé en tâche de fond pendant 5 s.
        let h = ch.as_raw_handle() as usize;
        std::thread::spawn(move || {
            let mut code = 0;
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(500));
                // 1 = D3DKMT_SCHEDULINGPRIORITYCLASS_BELOW_NORMAL.
                code = set(h as _, 1);
                if code == 0 {
                    return;
                }
            }
            crate::log(&format!("capture : priorité GPU basse refusée (0x{code:08x})"));
        });
    }
}

/// Lance, sort du mode efficacité, attend la fin : pour les étapes d'un clip qu'on attend.
fn run_now(mut c: Command) -> bool {
    let Ok(mut ch) = c.spawn() else { return false };
    no_eco(&ch);
    ch.wait().is_ok_and(|s| s.success())
}

/// Encodeurs qui encodent vraiment une image sur cette machine (pas seulement compilés).
/// FFmpeg utilisable ? S'il manque au PATH de Clipper (cas typique : installé par winget juste avant
/// le lancement, PATH du processus pas encore relu), on le cherche dans le PATH à jour du registre et
/// dans les dossiers de WinGet, puis on ajoute son dossier au PATH du processus. Avant tout autre fil.
pub fn locate_ffmpeg(cfg: &Config) -> bool {
    let works = || {
        Command::new(&cfg.ffmpeg)
            .creation_flags(CREATE_NO_WINDOW)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if works() || cfg.ffmpeg != "ffmpeg" {
        return works(); // déjà trouvé, ou chemin choisi par l'utilisateur : on n'y touche pas
    }
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = registry_path().split(';').filter(|d| !d.is_empty()).map(PathBuf::from).collect();
    dirs.push(PathBuf::from(&local).join(r"Microsoft\WinGet\Links"));
    // Paquets WinGet « …FFmpeg… » : le bin est un ou deux niveaux plus bas.
    for pkg in std::fs::read_dir(PathBuf::from(&local).join(r"Microsoft\WinGet\Packages")).into_iter().flatten().flatten() {
        if pkg.file_name().to_string_lossy().to_ascii_lowercase().contains("ffmpeg") {
            for sub in std::fs::read_dir(pkg.path()).into_iter().flatten().flatten() {
                dirs.push(sub.path().join("bin"));
            }
            dirs.push(pkg.path().join("bin"));
        }
    }
    dirs.extend([r"C:\ffmpeg\bin", r"C:\Program Files\ffmpeg\bin"].map(PathBuf::from));
    let Some(found) = dirs.into_iter().find(|d| d.join("ffmpeg.exe").is_file() && d.join("ffprobe.exe").is_file()) else {
        crate::log("FFmpeg introuvable (PATH, WinGet, C:\\ffmpeg)");
        return false;
    };
    let path = format!("{};{}", found.display(), std::env::var("PATH").unwrap_or_default());
    // Sûr : appelé au démarrage, avant le lancement des autres fils.
    unsafe { std::env::set_var("PATH", path) };
    crate::log(&format!("FFmpeg trouvé hors du PATH : {}", found.display()));
    works()
}

/// PATH utilisateur + machine tel qu'il est dans le registre maintenant (variables développées).
fn registry_path() -> String {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    let read = |root, key: &str| -> String {
        let (k, v): (Vec<u16>, Vec<u16>) = (key.encode_utf16().chain(Some(0)).collect(), "Path".encode_utf16().chain(Some(0)).collect());
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe { RegGetValueW(root, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) } == 0;
        if ok { String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]) } else { String::new() }
    };
    format!("{};{}", read(HKEY_CURRENT_USER, "Environment"), read(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"))
}

// AV1 : segments MKV (voir seg_ext), le MPEG-TS de ffmpeg le transformait en flux de données illisible.
const ENCODERS: [&str; 8] = ["h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "av1_amf", "h264_qsv", "libx264"];

pub fn probe_encoders(cfg: &Config) -> Vec<&'static str> {
    // Les 8 essais lancés ensemble (avant : l'un après l'autre, 1,24 s au démarrage).
    let kids: Vec<_> = ENCODERS
        .into_iter()
        .map(|e| (e, ffmpeg(cfg).args(["-loglevel", "quiet", "-f", "lavfi", "-i", "color=black:s=256x256:d=0.1", "-frames:v", "1", "-c:v", e, "-f", "null", "-"]).spawn()))
        .collect();
    kids.into_iter().filter_map(|(e, ch)| ch.ok()?.wait().ok()?.success().then_some(e)).collect()
}

/// Ce que le démarrage doit savoir : ffmpeg utilisable, encodeurs qui marchent, écrans vus par ddagrab.
/// Relu dans probes.json quand rien n'a changé depuis le lancement précédent (même Clipper, même ffmpeg,
/// mêmes cartes et pilotes, mêmes écrans à la même résolution) : la fenêtre s'ouvre sans attendre les
/// sondes (2,1 s avant). Sinon elles tournent, en parallèle, et le fichier est réécrit.
/// `fresh` : sondes refaites dans tous les cas (FFmpeg installé depuis l'app).
pub fn startup_probes(cfg: &Config, fresh: bool) -> (bool, Vec<&'static str>, Vec<serde_json::Value>) {
    let file = crate::config::dir().join("probes.json");
    if let Some(key) = hw_key(cfg).filter(|_| !fresh) {
        let cached: Option<serde_json::Value> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(c) = cached.filter(|c| c["key"] == key.as_str()) {
            let has = |e: &str| c["encoders"].as_array().is_some_and(|a| a.iter().any(|x| x == e));
            let encoders = ENCODERS.into_iter().filter(|e| has(e)).collect();
            let monitors = c["monitors"].as_array().cloned().unwrap_or_default();
            *MONITOR_GDI.lock().unwrap() = monitors.iter().map(|m| m["gdi"].as_str().map(String::from)).collect();
            return (true, encoders, monitors);
        }
    }
    if !locate_ffmpeg(cfg) {
        return (false, vec![], vec![]);
    }
    let (encoders, monitors) = std::thread::scope(|s| {
        let e = s.spawn(|| probe_encoders(cfg));
        let m = monitors(cfg);
        (e.join().unwrap_or_default(), m)
    });
    // Rien n'est gardé si une sonde n'a rien trouvé (carte occupée, écran en veille) : refait au prochain lancement.
    if let (Some(key), false, false) = (hw_key(cfg), encoders.is_empty(), monitors.is_empty()) {
        let _ = std::fs::write(&file, serde_json::json!({"key": key, "encoders": encoders, "monitors": monitors}).to_string());
    }
    (true, encoders, monitors)
}

/// Taille de l'écran capturé, prise dans la liste des écrans (déjà sondée) au lieu d'un ffprobe de plus.
pub fn native_of(monitors: &[serde_json::Value], idx: u32) -> Option<(u32, u32)> {
    let m = monitors.iter().find(|m| m["idx"] == idx)?;
    Some((m["w"].as_u64()? as u32, m["h"].as_u64()? as u32))
}

/// Empreinte de ce dont dépendent les sondes. None : ffmpeg introuvable dans le PATH (il faut le chercher).
fn hw_key(cfg: &Config) -> Option<String> {
    let exe = ffmpeg_file(cfg)?;
    let m = std::fs::metadata(&exe).ok()?;
    let date = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut k = format!("{}|{}|{}|{date}", env!("CARGO_PKG_VERSION"), exe.display(), m.len());
    for (name, ver) in display_drivers() {
        k += &format!("|{name}={ver}");
    }
    for (gdi, name) in monitor_names() {
        let (w, h) = gdi_size(&gdi).unwrap_or((0, 0));
        k += &format!("|{gdi}:{name}:{w}x{h}");
    }
    Some(k)
}

/// ffmpeg.exe réellement lancé : le chemin choisi, sinon le premier trouvé dans le PATH.
fn ffmpeg_file(cfg: &Config) -> Option<PathBuf> {
    let p = PathBuf::from(&cfg.ffmpeg);
    if p.components().count() > 1 {
        return p.is_file().then_some(p);
    }
    let name = if p.extension().is_some() { p } else { p.with_extension("exe") };
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(&name)).find(|f| f.is_file())
}

/// (carte, version du pilote) de chaque carte graphique, lus dans le registre (instantané).
fn display_drivers() -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let read = |key: &str, value: &str| -> Option<String> {
        let mut buf = vec![0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, w(key).as_ptr(), w(value).as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) } == 0;
        ok.then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    };
    (0..16)
        .filter_map(|i| {
            let key = format!(r"SYSTEM\CurrentControlSet\Control\Class\{{4d36e968-e325-11ce-bfc1-08002be10318}}\{i:04}");
            Some((read(&key, "DriverDesc")?, read(&key, "DriverVersion").unwrap_or_default()))
        })
        .collect()
}

/// Taille de sortie pour une classe (1080/1440/2160) sur un écran natif, ratio conservé.
/// `None` = au-dessus du natif (option grisée). `Some(native)` = zéro copie GPU.
pub fn output_size(class: u32, native: (u32, u32)) -> Option<(u32, u32)> {
    let (nw, nh) = native;
    // Une classe couvre la hauteur native jusqu'à +12 % (1080p sur 1200, 1440p sur 1600…).
    if class <= nh && nh <= class * 112 / 100 {
        return Some(native);
    }
    if class > nh {
        return None;
    }
    Some(((class * nw / nh) & !1, class & !1))
}

/// Débit vidéo en kb/s : base H.264 à 60 fps, ajusté codec, fps et curseur qualité.
pub fn bitrate_kbps(h: u32, fps: u32, encoder: &str, quality: f32) -> u32 {
    let base = match h {
        0..=1200 => 28_000.0,
        1201..=1600 => 45_000.0,
        _ => 80_000.0,
    };
    let fps_k = if fps <= 30 { 0.65 } else { 1.0 };
    let codec_k = if encoder.starts_with("hevc") || encoder.starts_with("av1") { 0.7 } else { 1.0 };
    (base * fps_k * codec_k * quality.clamp(0.5, 2.0)) as u32
}

/// NVENC et x264 : qualité constante plafonnée (le débit calculé est un maximum, pas une dépense fixe).
pub fn constant_quality(encoder: &str) -> bool {
    encoder.ends_with("_nvenc") || encoder == "libx264"
}

/// Preset le plus léger compatible avec la qualité visée.
/// Profil d'encodage : multiplicateur de débit appliqué au curseur qualité.
/// « perf » (défaut) : le moins de travail GPU ; « balanced » : l'ancien réglage ; « quality » : plus de détails.
pub fn profile_k(profile: &str) -> f32 {
    match profile {
        "quality" => 1.2,
        "balanced" => 1.0,
        _ => 0.85,
    }
}

fn preset_args(encoder: &str, quality: f32, profile: &str) -> Vec<&'static str> {
    if encoder.ends_with("_nvenc") && profile == "perf" {
        // Compétitif : préréglage le plus léger. AQ spatiale gardée : mesuré en 1080p60 temps réel (RTX 4060),
        // p1 = 8,3 % de NVENC, p1 + AQ = 7,4 % (aucun surcoût mesurable) pour +2,7 points de VMAF ; p4 + AQ =
        // 12,4 % sans mieux que p2 (scripts/quality-bench.ps1). Qualité constante : le débit suit la scène.
        let cq = if encoder.starts_with("h264") { "22" } else if encoder.starts_with("hevc") { "23" } else { "29" };
        let _ = quality;
        vec!["-preset", "p1", "-tune", "hq", "-rc", "vbr", "-cq", cq, "-spatial-aq", "1"]
    } else if encoder.ends_with("_nvenc") {
        // Mesuré à 1200p60 : p1 = 7 % NVENC, p3 = 14 %. Au-delà de x1, p2 (un peu plus de GPU) ; jamais plus.
        let p = if quality <= 1.0 { "p1" } else { "p2" };
        // Qualité constante : mesuré sur 60 s de jeu, CBR 14 Mb/s HEVC = 94 Mo VMAF 97,4 ; CQ 24 = 34 Mo
        // VMAF 97,0 (H.264 : 143 Mo -> 38 Mo, CQ 23). Même travail GPU, l'action garde le débit max.
        // Clips trouvés flous en jeu détaillé (The Last of Us) : 3 crans de qualité en plus, réglage « hq » (la
        // faible latence de « ll » ne sert à rien, le clip est tamponné) et AQ spatiale (détails fins, textes).
        let cq = if encoder.starts_with("h264") { "20" } else if encoder.starts_with("hevc") { "21" } else { "27" };
        vec!["-preset", p, "-tune", "hq", "-rc", "vbr", "-cq", cq, "-spatial-aq", "1"]
    } else if encoder.ends_with("_amf") {
        // AMF reste en CBR : son mode qualité (QVBR 23) tombait à VMAF 85, visible.
        vec!["-quality", "speed", "-rc", "cbr"]
    } else if encoder.ends_with("_qsv") {
        vec!["-preset", "veryfast"]
    } else {
        vec!["-preset", "ultrafast", "-tune", "zerolatency"]
    }
}

/// Taille finale : classe de résolution, puis étirement éventuel (jeu en 4:3 étiré).
pub fn final_size(cfg: &Config, q: crate::config::Quality, native: (u32, u32)) -> (u32, u32) {
    let (w, h) = output_size(q.height, native).unwrap_or(native);
    let ratio = match cfg.stretch.as_str() {
        "16:9" => Some(16.0 / 9.0),
        "16:10" => Some(16.0 / 10.0),
        _ => None,
    };
    match ratio {
        Some(r) if (w as f32 / h as f32 - r).abs() > 0.01 => (((h as f32 * r) as u32) & !1, h),
        _ => (w, h),
    }
}

#[derive(Clone, serde::Serialize)]
pub struct Gpu {
    /// "nvidia", "amd" ou "intel".
    pub id: String,
    pub name: String,
}

/// Processeurs graphiques du PC (noms Windows), un par fabricant.
pub fn gpus() -> Vec<Gpu> {
    use windows_sys::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW};
    let mut v: Vec<Gpu> = vec![];
    for i in 0.. {
        let mut d: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        d.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut d, 0) } == 0 {
            break;
        }
        let n = d.DeviceString.iter().position(|&c| c == 0).unwrap_or(d.DeviceString.len());
        let name = String::from_utf16_lossy(&d.DeviceString[..n]).trim().to_string();
        let up = name.to_ascii_uppercase();
        let id = if up.contains("NVIDIA") {
            "nvidia"
        } else if up.contains("AMD") || up.contains("RADEON") {
            "amd"
        } else if up.contains("INTEL") {
            "intel"
        } else {
            continue; // Microsoft Basic Display, bureau à distance…
        };
        if !v.iter().any(|g| g.id == id) {
            v.push(Gpu { id: id.into(), name });
        }
    }
    v
}

/// Fabricant dont la puce exécute l'encodeur (None = processeur, toujours possible).
pub fn encoder_vendor(e: &str) -> Option<&'static str> {
    if e.ends_with("_nvenc") {
        Some("nvidia")
    } else if e.ends_with("_amf") {
        Some("amd")
    } else if e.ends_with("_qsv") {
        Some("intel")
    } else {
        None
    }
}

/// Puce réellement choisie. Anciennes valeurs : « dgpu » = NVIDIA, « igpu » = AMD/Intel ; « auto » = NVIDIA sinon la première.
pub fn effective_chip(cfg: &Config, gpus: &[Gpu]) -> Option<String> {
    let has = |id: &str| gpus.iter().any(|g| g.id == id);
    let pick = match cfg.encode_chip.as_str() {
        c if has(c) => Some(c),
        "igpu" => ["amd", "intel"].into_iter().find(|id| has(id)),
        _ => None,
    };
    pick.map(String::from)
        .or_else(|| has("nvidia").then(|| "nvidia".into()))
        .or_else(|| gpus.first().map(|g| g.id.clone()))
}

/// Encodeur réel : celui choisi s'il tourne sur la puce choisie, sinon le H.264 de cette puce.
pub fn resolve_encoder(cfg: &Config, ok: &[&'static str], gpus: &[Gpu]) -> String {
    let chip = effective_chip(cfg, gpus);
    let allowed = |e: &&str| ok.contains(e) && encoder_vendor(e).is_none_or(|v| chip.as_deref().is_none_or(|c| c == v));
    std::iter::once(cfg.encoder.as_str())
        .chain(["h264_nvenc", "h264_amf", "h264_qsv", "hevc_nvenc", "hevc_amf", "libx264"])
        .find(|e| allowed(e))
        .map_or("libx264".into(), String::from)
}

/// (\\.\DISPLAYn, nom du moniteur) des écrans actifs, nom lu dans l'EDID par Windows.
fn monitor_names() -> Vec<(String, String)> {
    use windows_sys::Win32::Devices::Display::*;
    let wstr = |s: &[u16]| String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]);
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
            return vec![];
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![std::mem::zeroed(); np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![std::mem::zeroed(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) != 0 {
            return vec![];
        }
        paths.truncate(np as usize);
        paths
            .iter()
            .filter_map(|p| {
                let mut src: DISPLAYCONFIG_SOURCE_DEVICE_NAME = std::mem::zeroed();
                src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                src.header.adapterId = p.sourceInfo.adapterId;
                src.header.id = p.sourceInfo.id;
                let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
                tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                tgt.header.adapterId = p.targetInfo.adapterId;
                tgt.header.id = p.targetInfo.id;
                if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                    return None;
                }
                DisplayConfigGetDeviceInfo(&mut tgt.header);
                Some((wstr(&src.viewGdiDeviceName), wstr(&tgt.monitorFriendlyDeviceName)))
            })
            .collect()
    }
}

/// Mode actuel d'un écran GDI : (largeur, hauteur, fréquence en Hz).
fn gdi_mode(gdi: &str) -> Option<(u32, u32, u32)> {
    use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    let name: Vec<u16> = gdi.encode_utf16().chain(Some(0)).collect();
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    (unsafe { EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) } != 0).then_some((dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency))
}

/// Résolution actuelle d'un écran GDI.
fn gdi_size(gdi: &str) -> Option<(u32, u32)> {
    gdi_mode(gdi).map(|(w, h, _)| (w, h))
}

/// (\\.\DISPLAYn, nom) de chaque sortie ddagrab : écran de même résolution, dans l'ordre DISPLAY1, DISPLAY2…
fn match_names(sizes: &[(u32, u32)], mut screens: Vec<(String, (u32, u32), String)>) -> Vec<Option<(String, String)>> {
    screens.sort_by_key(|(gdi, _, _)| gdi.trim_start_matches(r"\\.\DISPLAY").parse::<u32>().unwrap_or(99));
    let mut used = vec![false; screens.len()];
    sizes
        .iter()
        .map(|s| {
            let i = (0..screens.len()).find(|&i| !used[i] && screens[i].1 == *s)?;
            used[i] = true;
            Some((screens[i].0.clone(), screens[i].2.clone()))
        })
        .collect()
}

/// Taille de l'écran telle que ddagrab la voit (même numérotation des sorties).
pub fn native_size(cfg: &Config) -> Option<(u32, u32)> {
    parse_size(&ddagrab_size(cfg, cfg.monitor).output().ok()?.stdout)
}

/// ffprobe qui lit la taille de la sortie `idx` de ddagrab (« 1920,1080 »).
fn ddagrab_size(cfg: &Config, idx: u32) -> Command {
    let mut c = Command::new(ffprobe(cfg));
    c.creation_flags(CREATE_NO_WINDOW)
        .args(["-v", "error", "-f", "lavfi", "-i", &format!("ddagrab=output_idx={idx}")])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    c
}

fn parse_size(out: &[u8]) -> Option<(u32, u32)> {
    let s = String::from_utf8_lossy(out);
    let (w, h) = s.trim().split_once(',')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Écrans vus par ddagrab (index 0..3), pour le réglage « Écran capturé ».
pub fn monitors(cfg: &Config) -> Vec<serde_json::Value> {
    // Les 4 sorties sondées ensemble (avant : l'une après l'autre, 0,65 s au démarrage).
    let kids: Vec<_> = (0..4).map(|i| ddagrab_size(cfg, i).spawn()).collect();
    let sizes: Vec<(u32, u32)> = kids.into_iter().map(|k| k.ok().and_then(|k| k.wait_with_output().ok()).and_then(|o| parse_size(&o.stdout))).map_while(|s| s).collect();
    let screens = monitor_names().into_iter().filter_map(|(gdi, name)| Some((gdi.clone(), gdi_size(&gdi)?, name))).collect();
    let names = match_names(&sizes, screens);
    *MONITOR_GDI.lock().unwrap() = names.iter().map(|n| n.as_ref().map(|(gdi, _)| gdi.clone())).collect();
    sizes
        .iter()
        .zip(names)
        .enumerate()
        .map(|(i, (&(w, h), name))| {
            let name = name.map(|(_, n)| n).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Écran {}", i + 1));
            serde_json::json!({"idx": i, "w": w, "h": h, "name": name, "gdi": MONITOR_GDI.lock().unwrap().get(i).cloned().flatten()})
        })
        .collect()
}

/// \\.\DISPLAYn de chaque sortie ddagrab (rempli par `monitors`) : retrouve le même écran pour gfxcapture.
static MONITOR_GDI: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());

/// HMONITOR de l'écran GDI `gdi` (\\.\DISPLAYn).
fn hmonitor(gdi: &str) -> Option<usize> {
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows_sys::core::BOOL {
        let (want, found) = unsafe { &mut *(data as *mut (String, Option<usize>)) };
        let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(m, &mut mi as *mut _ as _) } != 0 {
            let n = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(mi.szDevice.len());
            if String::from_utf16_lossy(&mi.szDevice[..n]).eq_ignore_ascii_case(want) {
                *found = Some(m as usize);
                return 0;
            }
        }
        1
    }
    let mut data: (String, Option<usize>) = (gdi.to_string(), None);
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), &mut data as *mut _ as LPARAM) };
    data.1
}

/// ffprobe.exe à côté de ffmpeg.exe : seul le nom du fichier change (avant, tout le chemin était réécrit et un
/// dossier « ffmpeg-7.1-full » devenait « ffprobe-7.1-full », introuvable).
fn ffprobe(cfg: &Config) -> String {
    let p = Path::new(&cfg.ffmpeg);
    match p.file_name().and_then(|n| n.to_str()) {
        Some(n) => p.with_file_name(n.replace("ffmpeg", "ffprobe")).to_string_lossy().into_owned(),
        None => "ffprobe".into(),
    }
}

/// HMONITOR de l'écran capturé (réglage « Écran capturé ») : sert à savoir quel jeu le clip montre.
pub fn captured_hmonitor(cfg: &Config) -> Option<usize> {
    let gdi = MONITOR_GDI.lock().unwrap().get(cfg.monitor as usize).cloned().flatten()?;
    hmonitor(&gdi)
}

/// Source de la capture : gfxcapture (Windows Graphics Capture, WGC). Avant : Desktop Duplication (ddagrab),
/// qui supporte mal un second logiciel qui capture l'écran en même temps (partage d'écran Discord saccadé tant
/// que Clipper tournait). WGC est fait pour plusieurs captures à la fois (Discord, OBS et Teams l'utilisent).
/// Mesuré (NVENC p1, 1080p60) : aucune image perdue (Desktop Duplication en perdait ~1 %), même processeur
/// (~5 % d'un cœur). Prix : un peu plus de temps de carte, en jeu 17,6 % du moteur 3D au lieu de 13,5 %
/// (moyenne de 3 passages de 25 s pendant Geometry Dash).
/// Pas de cadre jaune : `display_border` est à 0 (Windows 11, et Windows 10 récent).
/// Taille réduite (720p…) : le redimensionnement se fait sur la carte graphique, sans copie de l'écran entier
/// vers le processeur (1440p -> 720p : 6 % d'un cœur au lieu de 30 à 87 %).
fn gfx_source(cfg: &Config, (w, h): (u32, u32), native: (u32, u32), fps: u32) -> String {
    let gdi = MONITOR_GDI.lock().unwrap().get(cfg.monitor as usize).cloned().flatten();
    let screen = gdi.as_deref().and_then(hmonitor).map_or(format!("monitor_idx={}", cfg.monitor), |m| format!("hmonitor={m}"));
    let hz = gdi.as_deref().and_then(gdi_mode).map_or(0, |(_, _, hz)| hz);
    let size = if (w, h) == native { String::new() } else { format!(":width={w}:height={h}:scale_mode=bicubic") };
    format!("gfxcapture={screen}{size}:resize_mode=scale:max_framerate={}:capture_cursor=1:display_border=0,fps={fps}", wgc_max_rate(fps, hz))
}

/// Plafond d'images demandé à WGC. Windows jette toute image arrivée trop tôt après la précédente : avec un
/// plafond à 30 sur un écran 144 Hz, il n'en gardait qu'une sur 5 (~29 i/s irréguliers, clips saccadés).
/// Écran plus rapide que le clip : plafond au double (sans dépasser l'écran), `fps` choisit des images
/// régulières. Sinon le plafond du clip suffit (l'écran ne peut pas en montrer plus) : moins d'images à
/// recevoir, moins de travail pour la carte (mesuré : 3D 27,7 % au lieu de 29,4 %). `hz` = 0 : inconnu.
fn wgc_max_rate(fps: u32, hz: u32) -> u32 {
    match hz {
        0 => fps * 2,
        hz if hz <= fps => fps,
        hz => (fps * 2).min(hz),
    }
}

/// Résultat du dernier essai de la capture WGC (None = pas encore essayé).
static GPU_SCALE: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// La capture WGC marche-t-elle sur ce PC avec cet encodeur ? Essai réel de 3 images (8 s max), gardé en
/// cache. Échec (FFmpeg antérieur à la 8.0, Windows trop ancien, portable AMD + NVIDIA…) : retour à Desktop
/// Duplication, redimensionnement par le processeur.
fn gpu_scale_ok(cfg: &Config, src: &str, off_gpu: bool) -> bool {
    let key = format!("{src}|{}", cfg.encoder);
    if let Some((k, ok)) = GPU_SCALE.lock().unwrap().as_ref() {
        if *k == key {
            return *ok;
        }
    }
    let mut vf = src.to_string();
    if off_gpu {
        vf += ",hwdownload,format=bgra,format=nv12";
    }
    // Écran immobile (lancement avec Windows, bureau inactif) : sans le point de maintien, WGC n'envoyait pas
    // ses 3 images, l'essai échouait et toute la session restait en Desktop Duplication.
    let probe = src.starts_with("gfxcapture");
    if probe {
        crate::keepalive::set(crate::keepalive::Owner::Probe, captured_hmonitor(cfg));
    }
    let ok = ffmpeg(cfg)
        .args(["-loglevel", "quiet", "-f", "lavfi", "-i", &vf, "-frames:v", "3", "-c:v", &cfg.encoder, "-f", "null", "-"])
        .spawn()
        .is_ok_and(|mut ch| {
            let t = std::time::Instant::now();
            loop {
                match ch.try_wait() {
                    Ok(Some(s)) => break s.success(),
                    Ok(None) if t.elapsed() < Duration::from_secs(8) => std::thread::sleep(Duration::from_millis(100)),
                    _ => {
                        let _ = ch.kill();
                        break false;
                    }
                }
            }
        });
    if probe {
        crate::keepalive::set(crate::keepalive::Owner::Probe, None);
    }
    crate::log(&format!("capture Windows Graphics Capture : {}", if ok { "oui" } else { "non, retour à Desktop Duplication" }));
    *GPU_SCALE.lock().unwrap() = Some((key, ok));
    ok
}

/// Pour l'UI : la capture WGC (et donc le redimensionnement sur la carte graphique) marche-t-elle ?
/// (None = pas encore essayé.)
pub fn gpu_scaling() -> Option<bool> {
    GPU_SCALE.lock().unwrap().as_ref().map(|(_, ok)| *ok)
}

/// Ce que l'onglet Vidéos doit savoir d'un fichier (un seul ffprobe, gardé en cache).
#[derive(Clone, Default)]
pub struct Probe {
    pub secs: f64,
    video: String,
    audio: usize,
    first_title: String,
}

type ProbeKey = (PathBuf, std::time::SystemTime, u64);
static PROBES: Mutex<Option<std::collections::HashMap<ProbeKey, Probe>>> = Mutex::new(None);

/// Durée, codec et pistes. Résultat gardé tant que le fichier ne change pas (date + taille) :
/// rouvrir l'onglet Vidéos ne relance aucun ffprobe.
pub fn probe(cfg: &Config, p: &Path) -> Probe {
    let Ok(m) = std::fs::metadata(p) else { return Probe::default() };
    let key = (p.to_path_buf(), m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len());
    if let Some(pr) = PROBES.lock().unwrap().as_ref().and_then(|c| c.get(&key)) {
        return pr.clone();
    }
    let out = Command::new(ffprobe(cfg))
        .creation_flags(CREATE_NO_WINDOW | 0x20) // priorité normale : sert au clip qu'on attend
        .args(["-v", "error", "-show_entries", "format=duration:stream=codec_type,codec_name:stream_tags=title", "-of", "json"])
        .arg(p)
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let j: serde_json::Value = serde_json::from_slice(&out).unwrap_or_default();
    let mut pr = Probe { secs: j["format"]["duration"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0), ..Default::default() };
    for s in j["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if pr.video.is_empty() => pr.video = s["codec_name"].as_str().unwrap_or("").into(),
            Some("audio") => {
                if pr.audio == 0 {
                    pr.first_title = s["tags"]["title"].as_str().unwrap_or("").into();
                }
                pr.audio += 1;
            }
            _ => {}
        }
    }
    PROBES.lock().unwrap().get_or_insert_with(Default::default).insert(key, pr.clone());
    pr
}

/// Pistes audio à faire entendre : la 1re seule si c'est « Tout » (le mélange existe déjà).
fn useful_audio(pr: &Probe) -> usize {
    if pr.first_title == crate::audio::Track::Mix.title() { pr.audio.min(1) } else { pr.audio }
}

/// Pour le lecteur : (le fichier a une piste d'images, nombre de pistes audio à faire entendre).
pub fn playable(pr: &Probe) -> (bool, usize) {
    (!pr.video.is_empty(), useful_audio(pr))
}

/// Extension des segments du buffer. ffmpeg écrit l'AV1 dans un MPEG-TS comme un flux de données
/// (« bin_data ») : clips sans vidéo. En MKV il reste de l'AV1 (vérifié : 360 images sur 360, durée exacte).
pub fn seg_ext(encoder: &str) -> &'static str {
    if encoder.starts_with("av1") { "mkv" } else { "ts" }
}

pub fn buffer_args(cfg: &Config, native: (u32, u32), dir: &Path, start: u64, pipes: &[String]) -> Vec<String> {
    let mut a = capture_args(cfg, cfg.clip, native, pipes);
    let ext = seg_ext(&cfg.encoder);
    let format = if ext == "mkv" { "matroska" } else { "mpegts" };
    a.extend(["-f".into(), "segment".into(), "-segment_time".into(), SEG_SECS.to_string(), "-segment_format".into(), format.into()]);
    if ext == "mkv" {
        // Chaque segment MKV repart de 0 : sinon l'assemblage additionnait les horodatages (8 s au lieu de 6).
        a.extend(["-reset_timestamps".into(), "1".into()]);
    } else {
        // Chaque paquet écrit tout de suite dans le fichier du segment (cache de Windows, aucun accès disque
        // forcé). Sans ça, ffmpeg écrit par blocs de 256 Ko : la fin du segment en cours manquait au clip
        // (jusqu'à 1 s à faible débit).
        a.extend(["-segment_format_options".into(), "flush_packets=1".into()]);
    }
    for s in ["-segment_start_number".into(), start.to_string(), dir.join(format!("%010d.{ext}")).to_string_lossy().into_owned()] {
        a.push(s);
    }
    a
}

/// Entrées (écran + audio) et encodage pour la qualité `q`, sans la sortie.
fn capture_args(cfg: &Config, q: crate::config::Quality, native: (u32, u32), pipes: &[String]) -> Vec<String> {
    // Vrais pixels : seulement la classe de résolution (720p…). L'étirement (4:3 -> 16:9) n'est plus un
    // agrandissement CPU (flou, coûteux) : le fichier déclare un format d'affichage 16:9 et les lecteurs
    // étirent eux-mêmes. Vérifié : 1920x1200 + -aspect 16:9 -> MP4 « 16:9 » après segments et assemblage.
    let (w, h) = output_size(q.height, native).unwrap_or(native);
    let shown = final_size(cfg, q, native);
    // Les images capturées vivent sur la carte NVIDIA : AMF/QSV/x264 doivent passer par la mémoire.
    let off_gpu = !cfg.encoder.ends_with("_nvenc");
    let gfx = gfx_source(cfg, (w, h), native, q.fps);
    let wgc = gpu_scale_ok(cfg, &gfx, off_gpu);
    // Repli si WGC ne marche pas : Desktop Duplication.
    let mut vf = if wgc { gfx } else { format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", cfg.monitor, q.fps) };
    if wgc && off_gpu {
        // Réduction éventuelle déjà faite sur la carte : seule l'image finale quitte le GPU.
        vf += ",hwdownload,format=bgra,format=nv12";
    } else if !wgc && ((w, h) != native || off_gpu) {
        // Image hors du GPU : changement de résolution (720p…) ou encodeur non NVIDIA. scale_d3d11 (sur
        // la carte) échoue sur les portables AMD + NVIDIA : redimensionnement processeur, filtre bilinéaire.
        vf += ",hwdownload,format=bgra";
        if (w, h) != native {
            vf += &format!(",scale={w}:{h}:flags=bilinear");
        }
        vf += &format!(",format=nv12,fps={}", q.fps);
    }
    // Garde-fou : encodeur saturé (voir le chien de garde du buffer) -> préréglage le plus léger, débit -30 %.
    let light = LIGHT.load(Ordering::Relaxed);
    let profile = if light { "perf" } else { cfg.profile.as_str() };
    let kb = bitrate_kbps(h, q.fps, &cfg.encoder, cfg.quality * profile_k(profile) * if light { 0.7 } else { 1.0 });
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &vf].iter().map(|s| s.to_string()).collect();
    a.extend(crate::audio::input_args(pipes));
    let kinds = crate::audio::tracks(cfg);
    a.extend(crate::audio::output_args(if kinds.len() == pipes.len() { &kinds } else { &[] }));
    a.extend(["-c:v".into(), cfg.encoder.clone()]);
    a.extend(preset_args(&cfg.encoder, if profile == "quality" { cfg.quality.max(1.01) } else { cfg.quality }, profile).iter().map(|s| s.to_string()));
    // Couleurs déclarées BT.709 plage limitée (norme HD) : sans ces balises, certains lecteurs supposaient du
    // BT.601 et les teintes viraient légèrement.
    a.extend(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"].map(String::from));
    if shown != (w, h) {
        a.extend(["-aspect".into(), format!("{}:{}", shown.0, shown.1)]);
    }
    let target = if constant_quality(&cfg.encoder) { "0".to_string() } else { format!("{kb}k") };
    if cfg.encoder == "libx264" {
        a.extend(["-crf".into(), "22".into()]);
    }
    for s in [
        "-b:v".into(), target, "-maxrate".into(), format!("{kb}k"), "-bufsize".into(), format!("{}k", kb * 2),
        "-g".into(), (q.fps * SEG_SECS).to_string(), "-forced-idr".into(), "1".into(),
    ] {
        a.push(s);
    }
    a
}

/// Segments du dossier, du plus ancien au plus récent (le nom = numéro croissant).
pub fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        // Seulement les segments numérotés (pas le fichier rec-….ts de la 2e capture).
        .filter(|p| p.extension().is_some_and(|x| x == "ts" || x == "mkv") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
        .collect();
    v.sort();
    v
}

fn seg_number(p: &Path) -> u64 {
    p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Résolution actuelle de l'écran capturé, lue dans Windows (aucun ffprobe).
fn screen_size(cfg: &Config) -> Option<(u32, u32)> {
    let gdi = MONITOR_GDI.lock().unwrap().get(cfg.monitor as usize).cloned().flatten()?;
    gdi_size(&gdi)
}

/// ffmpeg vient-il d'écrire qu'il a perdu l'écran ? Son journal n'est relu que s'il a grossi.
fn video_lost(log: &Path, seen: &mut u64) -> bool {
    let len = std::fs::metadata(log).map_or(0, |m| m.len());
    if len == *seen {
        return false;
    }
    *seen = len;
    std::fs::read(log).is_ok_and(|b| {
        let t = String::from_utf8_lossy(&b);
        t.contains("AcquireNextFrame failed") || t.contains("Error during demuxing")
    })
}

/// Fin utilisable d'un segment MPEG-TS coupé en pleine écriture : le début du dernier paquet qui ouvre une
/// image (flux vidéo = PID 0x100 chez ffmpeg, indicateur de début d'unité). Tout ce qui précède est entier ;
/// l'image qui commence là ne l'est peut-être pas. 0 : pas une seule image entière, c'est-à-dire moins de deux
/// débuts d'image. Avant : un segment tout juste ouvert (en-têtes PAT/PMT puis une image commencée) donnait sa
/// position, et la copie du segment en cours ne contenait aucune image : clip non recoupé, 62 s au lieu de 60.
fn ts_cut(data: &[u8]) -> usize {
    let mut starts = (0..data.len() / 188).rev().filter(|i| {
        let p = &data[i * 188..];
        p[0] == 0x47 && p[1] & 0x40 != 0 && (u16::from(p[1] & 0x1f) << 8 | u16::from(p[2])) == 0x100
    });
    match (starts.next(), starts.next()) {
        (Some(last), Some(_)) => last * 188,
        _ => 0,
    }
}

/// PTS (s) du premier et du dernier début d'image vidéo d'un morceau de MPEG-TS (min et max : l'ordre de
/// décodage n'est pas celui d'affichage). Lu directement dans les en-têtes PES, sans ffprobe.
fn ts_pts_range(data: &[u8]) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in data.chunks_exact(188) {
        if p[0] != 0x47 || p[1] & 0x40 == 0 || (u16::from(p[1] & 0x1f) << 8 | u16::from(p[2])) != 0x100 {
            continue;
        }
        // Champ d'adaptation éventuel, puis en-tête PES : 00 00 01, id, longueur, 2 octets d'options, taille, PTS.
        let o = if p[3] & 0x20 != 0 { 5 + usize::from(p[4]) } else { 4 };
        let Some(h) = p.get(o..o + 14) else { continue };
        if h[..3] != [0, 0, 1] || h[7] & 0x80 == 0 {
            continue;
        }
        let b = |i: usize| u64::from(h[9 + i]);
        let pts = ((b(0) >> 1 & 7) << 30 | b(1) << 22 | (b(2) >> 1) << 15 | b(3) << 7 | b(4) >> 1) as f64 / 90_000.0;
        (lo, hi) = (lo.min(pts), hi.max(pts));
    }
    (lo <= hi).then_some((lo, hi))
}

/// Durée exacte d'une suite de segments .ts d'une même capture (horodatages continus) : du premier PTS au
/// dernier, plus une image. None si un fichier n'est pas lisible ainsi.
fn ts_span(paths: &[PathBuf], fps: u32) -> Option<f64> {
    use std::io::Read;
    let (first, last) = (paths.first()?, paths.last()?);
    // Début du premier fichier (256 Ko suffisent d'ordinaire), sinon le fichier entier.
    let mut head = vec![0u8; 256 * 1024];
    let n = std::fs::File::open(first).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let lo = ts_pts_range(&head[..n]).or_else(|| ts_pts_range(&std::fs::read(first).ok()?)).map(|r| r.0);
    let hi = std::fs::read(last).ok().and_then(|d| ts_pts_range(&d)).map(|r| r.1);
    match (lo, hi) {
        (Some(lo), Some(hi)) if hi >= lo => Some(hi - lo + 1.0 / f64::from(fps.max(1))),
        _ => {
            // Sans durée exacte, le clip n'est pas recoupé (il dépasse la durée du buffer) : dire pourquoi.
            let size = |p: &Path| std::fs::metadata(p).map_or(-1, |m| m.len() as i64);
            crate::log(&format!("clip : durée exacte illisible ({} : {} octets, début {lo:?} ; {} : {} octets, fin {hi:?})", first.display(), size(first), last.display(), size(last)));
            None
        }
    }
}

/// Dernier segment d'une capture arrêtée net : raccourci à sa dernière image entière.
fn trim_ts(seg: &Path) {
    if seg.extension().is_none_or(|e| e != "ts") {
        return;
    }
    let Ok(data) = std::fs::read(seg) else { return };
    let cut = ts_cut(&data);
    if cut > 0 && cut < data.len() {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(seg) {
            let _ = f.set_len(cut as u64);
        }
    }
}

fn trim(ch: &Child) {
    use std::os::windows::io::AsRawHandle;
    unsafe { windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet(ch.as_raw_handle() as _) };
}

pub struct Buffer {
    pub dir: PathBuf,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Premier segment à garder pour un REC en cours (None = pas de REC).
    pub rec_from: Arc<Mutex<Option<u64>>>,
}

impl Buffer {
    /// Lance le superviseur : relance ffmpeg si ddagrab perd l'écran (887a0026), nettoie les vieux segments.
    pub fn start(cfg: Config, native: (u32, u32), dir: PathBuf, audio: Option<Arc<crate::audio::Audio>>) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        purge_if_format_changed(&cfg, native, &dir, audio.as_ref().map_or(String::new(), |_| format!("{:?}", crate::audio::tracks(&cfg))));
        // Première capture depuis le lancement de Clipper : les segments d'une session précédente ne sont pas
        // datés sur l'horloge du buffer (timeline.rs). Effacés : jamais dans un clip.
        static FIRST: AtomicBool = AtomicBool::new(true);
        if FIRST.swap(false, Ordering::Relaxed) {
            segments(&dir).iter().for_each(|p| drop(std::fs::remove_file(p)));
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("tail-")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None::<Child>));
        let rec_from = Arc::new(Mutex::new(None::<u64>));
        let (s, c, r, d) = (stop.clone(), child.clone(), rec_from.clone(), dir.clone());
        // Superviseur précédent (réglage changé, clipping recoupé) : on attend sa fin avant de commencer. Il
        // met jusqu'à ~1 s à voir son arrêt, puis ferme sa capture sur l'horloge du buffer, retire son point de
        // maintien et baisse CAPTURING. Avant, ces écritures pouvaient tomber après le début du nouveau : la
        // nouvelle capture était close aussitôt et ses segments jamais datés (clips vides jusqu'à la relance).
        // Attente dans le nouveau fil, jamais dans celui de la fenêtre.
        let prev = PREV.lock().unwrap().take();
        let h = std::thread::spawn(move || {
            if let Some(p) = prev {
                let _ = p.join();
            }
            let keep = (cfg.buffer_secs / SEG_SECS + 3) as usize;
            let mut spawns = 0u64;
            // Diagnostic (saccades signalées sur un seul PC) : source de capture écrite à chaque changement, et
            // bilan toutes les 10 min : relances de ffmpeg, secondes sans image (point de maintien forcé).
            let mut last_src = String::new();
            let (mut nudged, mut bilan, mut spawns_at_bilan) = (0u32, std::time::Instant::now(), 1u64); // 1er lancement : pas une relance
            let mut away = 0u32;
            // ffmpeg mort juste après son lancement (disque plein, écran indisponible…) : attente croissante
            // avant la relance (2, 4, 8, 16, 30 s). Avant : relancé toutes les 1,3 s sans fin, une ligne de
            // journal à chaque fois.
            let mut fails = 0u32;
            let mut native = native;
            // Écran verrouillé : capture en pause (relancer ffmpeg ne servirait à rien), reprise au retour.
            let mut locked = false;
            while !s.load(Ordering::Relaxed) {
                if crate::foreground::secure_desktop() {
                    if !locked {
                        crate::log("capture : écran verrouillé, en pause jusqu'au retour (le buffer garde ce qu'il a)");
                        locked = true;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if locked {
                    crate::log("capture : écran déverrouillé, reprise");
                    locked = false;
                }
                // « Capture du bureau » désactivée : rien ne tourne tant qu'aucun jeu plein écran n'est au premier plan.
                if !cfg.desktop_capture && !crate::foreground::fullscreen_foreground() {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                // Le ffmpeg précédent a été arrêté net (réglage changé, écran perdu) : son dernier segment est
                // tronqué, parfois vide. Vide : retiré. Sinon coupé juste avant sa dernière image, peut-être
                // incomplète (avant : un ffprobe à chaque relance, et le segment entier jeté s'il était illisible).
                if let Some(last) = segments(&d).last() {
                    if std::fs::metadata(last).is_ok_and(|m| m.len() == 0) {
                        let _ = std::fs::remove_file(last);
                    } else {
                        trim_ts(last);
                    }
                }
                // Résolution de l'écran relue à chaque lancement (lecture Windows, instantanée) : un jeu en plein
                // écran exclusif ou une résolution étirée la changent en cours de route. Avant, celle du démarrage
                // de Clipper servait toujours, et le buffer mélangeait deux tailles d'image.
                if let Some(sz) = screen_size(&cfg).filter(|sz| *sz != native) {
                    crate::log(&format!("capture : écran passé de {}x{} à {}x{}", native.0, native.1, sz.0, sz.1));
                    native = sz;
                }
                // Format des segments de ce lancement : un clip ne mélange jamais deux formats.
                let sig = format!("{} {:?} {:?} {} {:?}", cfg.encoder, output_size(cfg.clip.height, native).unwrap_or(native), final_size(&cfg, cfg.clip, native), cfg.clip.fps, crate::audio::tracks(&cfg));
                let next = segments(&d).last().map_or(0, |p| seg_number(p) + 1);
                let mut cmd = ffmpeg_capture(&cfg);
                let _ = std::fs::rename(d.join("ffmpeg.log"), d.join("ffmpeg.prev.log"));
                spawns += 1;
                if let Ok(f) = std::fs::File::create(d.join("ffmpeg.log")) {
                    cmd.stderr(f); // dernière erreur gardée pour le diagnostic
                }
                let pipes = audio.as_ref().map_or(vec![], |a| a.open_pipes(&crate::audio::tracks(&cfg), spawns));
                let args = buffer_args(&cfg, native, &d, next, &pipes);
                let src = args.iter().position(|a| a == "-i").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
                if src != last_src {
                    crate::log(&format!("capture : source {src}, encodeur {}", cfg.encoder));
                    last_src = src;
                }
                let _ = std::fs::write(d.join("ffmpeg-args.txt"), format!("pipes={pipes:?} audio={}
{}", audio.is_some(), args.join(" ")));
                let spawned_at = std::time::Instant::now();
                // Capture WGC : point de maintien sur l'écran capturé (voir keepalive.rs).
                let wgc = args.iter().any(|a| a.starts_with("gfxcapture"));
                match cmd.args(args).spawn() {
                    Ok(mut ch) if s.load(Ordering::Relaxed) => {
                        let _ = ch.kill();
                        break;
                    }
                    Ok(ch) => {
                        CAPTURING.store(true, Ordering::Relaxed);
                        no_eco(&ch); // enregistrement continu : jamais ralenti par Windows
                        gpu_low(&ch); // mais servi après le jeu sur la carte graphique
                        *c.lock().unwrap() = Some(ch);
                        REGISTRY.lock().unwrap().begin(crate::timeline::now(), &sig);
                        crate::keepalive::set(crate::keepalive::Owner::Buffer, if wgc { captured_hmonitor(&cfg) } else { None });
                    }
                    Err(_) => {
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
                let mut tick = 0u32;
                // Garde-fou : secondes d'encodeur saturé (> 90 %) et de calme (< 50 %) d'affilée.
                let (mut hot, mut calm) = (0u32, 0u32);
                // Chien de garde : ffmpeg vivant mais plus rien d'écrit (capture figée) -> relancé. Suivi sur le
                // numéro ET la taille du dernier segment (écrit image par image, flush_packets=1). À 2 s, le point
                // de maintien WGC se montre (écran immobile) ; toujours rien à 3 s : session figée (vu au lancement de
                // CS2, quand il passe en plein écran), relance. Avant : sur le numéro seul, 13 s de trou avant la relance.
                let (mut last_seg, mut still) = (None::<(u64, u64)>, 0u32);
                // Jeu qui vient d'entrer en plein écran ou d'en sortir (Alt+Tab) : la session WGC peut s'y figer.
                // Secondes depuis la dernière bascule (u32::MAX : aucune).
                let (mut full, mut since_flip) = (crate::foreground::fullscreen_foreground(), u32::MAX);
                // Longueur du journal de ffmpeg déjà lue (voir `video_lost`).
                let mut log_len = 0u64;
                // Dernier relevé sur l'horloge du buffer : un pas de 250 ms qui dure plus de 5 s = sortie de veille.
                let mut last = crate::timeline::now();
                loop {
                    // Quatre pas de 250 ms par seconde : les nouveaux segments sont datés au quart de seconde sur
                    // l'horloge du buffer, et une capture qui perd l'écran est relancée tout de suite.
                    let (mut lost, mut slept) = (false, false);
                    for _ in 0..4 {
                        std::thread::sleep(Duration::from_millis(250));
                        let t = crate::timeline::now();
                        if crate::watchdog::woke_from_sleep(last, t) {
                            slept = true;
                            break;
                        }
                        last = t;
                        let numbers: Vec<u64> = segments(&d).iter().map(|p| seg_number(p)).collect();
                        REGISTRY.lock().unwrap().observe(t, &numbers);
                        lost = video_lost(&d.join("ffmpeg.log"), &mut log_len);
                        if lost || s.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    // Sortie de veille : capture close au dernier instant connu d'avant la veille, puis relancée. Sans
                    // ça, le dernier segment d'avant semblait durer toute la veille et entrait dans le premier clip
                    // pris au réveil (2 s d'images d'avant la mise en veille).
                    if slept {
                        crate::log("capture : sortie de veille, relance");
                        REGISTRY.lock().unwrap().end(last);
                        stop_child(&c);
                        break;
                    }
                    // Verrouillage pendant la capture : arrêt tout de suite, le tour suivant attend le retour.
                    if !lost && crate::foreground::secure_desktop() {
                        stop_child(&c);
                        break;
                    }
                    if lost {
                        // ddagrab a perdu l'écran (alt-tab d'un jeu en plein écran exclusif, changement de
                        // résolution, écran de verrouillage) : ffmpeg n'envoie plus d'images mais reste en vie sur
                        // les tubes du son. Avant, le chien de garde mettait 13 s à le voir : 13 s de trou dans le
                        // buffer et un segment de 15 s à l'image figée. Relancé tout de suite.
                        crate::log("capture : écran perdu (changement de mode d'affichage), relance immédiate");
                        stop_child(&c);
                        break;
                    }
                    tick += 1;
                    if tick == 10 {
                        fails = 0; // a tourné normalement
                    }
                    // Notre propre ffmpeg (jamais un autre processus) : rend la RAM froide du pilote.
                    // Mesuré : 100 Mo -> ~14 Mo de RAM physique, sans perte d'images.
                    if tick == 5 || tick % 60 == 0 {
                        if let Some(ch) = c.lock().unwrap().as_ref() {
                            trim(ch);
                        }
                    }
                    // Garde-fou, toutes les 2 s (une lecture NVML) : NVENC saturé 10 s -> mode allégé ; calme
                    // 2 min en mode allégé -> retour au profil choisi. Chaque bascule relance ffmpeg (~1 s de trou).
                    if tick % 2 == 0 {
                        if let Some(u) = crate::overlay::encoder_load() {
                            let light = LIGHT.load(Ordering::Relaxed);
                            let flip;
                            (hot, calm, flip) = crate::watchdog::encoder_guard(u, hot, calm, light);
                            if flip && r.lock().unwrap().is_none() {
                                LIGHT.store(!light, Ordering::Relaxed);
                                crate::log(&format!("capture : encodeur à {u} %, mode {}", if light { "normal rétabli" } else { "allégé (préréglage p1, débit -30 %)" }));
                                stop_child(&c);
                                break;
                            }
                        }
                    }
                    // Résolution de l'écran changée (jeu en plein écran exclusif, résolution étirée) : WGC continue
                    // en remettant l'image à l'ancienne taille, sans s'arrêter (Desktop Duplication, lui, s'arrêtait).
                    // Relancé pour enregistrer à la nouvelle taille. Une lecture Windows toutes les 2 s.
                    if tick % 2 == 1 && screen_size(&cfg).is_some_and(|sz| sz != native) {
                        stop_child(&c);
                        break;
                    }
                    let alive = c.lock().unwrap().as_mut().is_some_and(|ch| matches!(ch.try_wait(), Ok(None)));
                    let segs = segments(&d);
                    let newest = segs.last().map(|p| (seg_number(p), std::fs::metadata(p).map_or(0, |m| m.len())));
                    still = if newest == last_seg { still + 1 } else { 0 };
                    last_seg = newest;
                    // WGC sans image depuis 2 s : point de maintien montré, même par-dessus un jeu plein écran.
                    crate::keepalive::force(crate::watchdog::nudge(still));
                    nudged += u32::from(crate::watchdog::nudge(still));
                    if bilan.elapsed() >= Duration::from_secs(600) {
                        crate::log(&format!("capture : bilan 10 min : {} relance(s) de ffmpeg, {nudged} s sans image, mode {}", spawns - spawns_at_bilan, if LIGHT.load(Ordering::Relaxed) { "allégé" } else { "normal" }));
                        (nudged, bilan, spawns_at_bilan) = (0, std::time::Instant::now(), spawns);
                    }
                    let now_full = crate::foreground::fullscreen_foreground();
                    since_flip = if now_full != full { 0 } else { since_flip.saturating_add(1) };
                    full = now_full;
                    // 1 s sans image juste après une bascule plein écran : session figée, relancée tout de suite.
                    if crate::watchdog::frozen(still, since_flip, alive) {
                        crate::log(&format!("buffer : ffmpeg ne produit plus de segments depuis {still} s, relance"));
                        stop_child(&c);
                        break;
                    }
                    if !alive && !s.load(Ordering::Relaxed) {
                        if spawned_at.elapsed() < Duration::from_secs(6) {
                            fails += 1;
                        }
                        let err = std::fs::read_to_string(d.join("ffmpeg.log")).unwrap_or_default();
                        // Une ligne aux 1er, 2e, 4e, 8e… échecs d'affilée : le journal ne gonfle plus.
                        if fails <= 1 || fails.is_power_of_two() {
                            crate::log(&format!("buffer : ffmpeg s'est arrêté, relance dans {} s ({})", crate::watchdog::backoff(fails).as_secs_f32(), err.lines().last().unwrap_or("aucun message")));
                        }
                    }
                    // Jamais un segment d'un REC en cours ni d'un clip en cours d'assemblage.
                    let floor = r.lock().unwrap().unwrap_or(u64::MAX).min(REGISTRY.lock().unwrap().floor().unwrap_or(u64::MAX));
                    for p in segs.iter().take(segs.len().saturating_sub(keep)) {
                        if seg_number(p) < floor {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    // Retour au bureau plus de 5 s : on coupe ffmpeg (0 % GPU), les segments déjà écrits restent.
                    away = if !cfg.desktop_capture && !crate::foreground::fullscreen_foreground() { away + 1 } else { 0 };
                    if away > 5 {
                        if let Some(mut ch) = c.lock().unwrap().take() {
                            let _ = ch.kill();
                        }
                        away = 0;
                        break;
                    }
                    if !alive || s.load(Ordering::Relaxed) {
                        break;
                    }
                }
                REGISTRY.lock().unwrap().end(crate::timeline::now());
                crate::keepalive::set(crate::keepalive::Owner::Buffer, None);
                // Attente par tranches de 300 ms : un arrêt (réglage changé, clipping coupé) reste immédiat.
                let until = std::time::Instant::now() + crate::watchdog::backoff(fails);
                while std::time::Instant::now() < until && !s.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
            CAPTURING.store(false, Ordering::Relaxed);
            crate::keepalive::set(crate::keepalive::Owner::Buffer, None);
            // stop() peut passer pendant un spawn : sans ça, le ffmpeg lancé juste après continuait seul.
            stop_child(&c);
        });
        *PREV.lock().unwrap() = Some(h);
        Self { dir, stop, child, rec_from }
    }

    /// Buffer à l'arrêt (clipping désactivé) : aucun fil, aucun ffmpeg, et les segments sont effacés
    /// (disque libéré, pas de vieux morceaux recollés à la réactivation).
    pub fn idle(dir: PathBuf) -> Self {
        segments(&dir).iter().for_each(|p| drop(std::fs::remove_file(p)));
        let none = || Arc::new(Mutex::new(None::<Child>));
        Self { dir, stop: Arc::new(AtomicBool::new(true)), child: none(), rec_from: Arc::new(Mutex::new(None)) }
    }

    /// ffmpeg attendu jusqu'à sa fin (quelques ms après kill) : sinon il pouvait encore écrire un segment après
    /// l'effacement du buffer (clipping coupé), et ce morceau d'avant la coupure entrait dans le clip suivant.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        stop_child(&self.child);
    }
}

/// Arrête le ffmpeg de la capture (s'il tourne) et attend sa fin : le fichier du segment est alors fermé.
fn stop_child(c: &Mutex<Option<Child>>) {
    if let Some(mut ch) = c.lock().unwrap().take() {
        let _ = ch.kill();
        let _ = ch.wait();
    }
}

/// Segments d'un autre codec, d'une autre taille ou d'un autre nombre de pistes : impossibles
/// à assembler avec `-c copy` (« ffmpeg n'a pas pu assembler le clip »). On repart de zéro.
fn purge_if_format_changed(cfg: &Config, native: (u32, u32), dir: &Path, pipes: String) {
    let (w, h) = final_size(cfg, cfg.clip, native);
    let sig = format!("{} {w}x{h} {} audio{pipes}", cfg.encoder, cfg.clip.fps);
    let file = dir.join("format.txt");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(sig.as_str()) {
        segments(dir).iter().for_each(|p| drop(std::fs::remove_file(p)));
        let _ = std::fs::write(&file, sig);
    }
}

/// Assemble des segments sans réencodage (`-c copy`).
/// Restes d'un clip interrompu (Clipper fermé pendant l'assemblage) : « Clip … .mp4.part » sans index,
/// illisible, et la liste « Clip … .txt ». Au démarrage aucun assemblage ne tourne : on les supprime.
pub fn clean_leftovers(out_dir: &Path) {
    let ours = |n: &str| n.starts_with("Clip ") || n.starts_with("REC ");
    // Le dossier de sortie et ses dossiers de jeu (un niveau) : depuis la 1.1, les clips vont dans « ClipperJeu ».
    let subs = std::fs::read_dir(out_dir).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path());
    for dir in std::iter::once(out_dir.to_path_buf()).chain(subs) {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if ours(&name) && (name.ends_with(".part") || name.ends_with(".txt")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

pub fn concat(cfg: &Config, parts: &[Vec<PathBuf>], out: &Path, trim: f64) -> Result<(), String> {
    if parts.is_empty() {
        return Err("Le buffer est encore vide.".into());
    }
    let list = out.with_extension("txt");
    // Écrit sous un nom .part puis renommé : l'onglet Vidéos ne voit jamais un clip à moitié écrit.
    let part = out.with_extension(format!("{}.part", cfg.container));
    let format = match cfg.container.as_str() {
        "mkv" => "matroska",
        "mov" => "mov",
        _ => "mp4",
    };
    let mut c = ffmpeg_capture(cfg);
    c.args(["-loglevel", "error"]);
    // Choix de l'entrée : voir clipplan.rs. Noms courts, dossier courant : la ligne de commande reste courte
    // même pour un long REC. Plusieurs captures : avant, le flux unique donnait un fichier cassé.
    match crate::clipplan::input(parts) {
        crate::clipplan::Input::Stream { dir, names } => {
            c.current_dir(dir).arg("-i").arg(format!("concat:{}", names.join("|")));
        }
        crate::clipplan::Input::List(body) => {
            std::fs::write(&list, body).map_err(|e| e.to_string())?;
            c.args(["-protocol_whitelist", "file,concat,crypto,data", "-f", "concat", "-safe", "0", "-i"]).arg(&list);
        }
    }
    // Pas de « +faststart » : il réécrivait tout le fichier une 2e fois (double écriture disque). Disque
    // occupé par un autre programme, un clip de 54 s passait de 2 s à 26 s. Les lecteurs, navigateurs et
    // Discord lisent très bien l'index en fin de fichier.
    c.args(["-map", "0", "-c", "copy"]);
    if trim > 0.0 {
        // Les `trim` premières secondes passent avant zéro : le lecteur les décode sans les montrer.
        c.args(["-output_ts_offset", &format!("-{trim:.3}"), "-avoid_negative_ts", "disabled"]);
    }
    c.args(["-f", format, "-y"]).arg(&part);
    let ok = run_now(c) && std::fs::rename(&part, out).is_ok();
    let _ = std::fs::remove_file(&list);
    if ok {
        Ok(())
    } else {
        let _ = std::fs::remove_file(&part);
        Err("ffmpeg n'a pas pu assembler le clip.".into())
    }
}

/// "2026-11-07 21-14-03" en heure locale.
fn stamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Assemble le fichier ; contrôle, miniature et planche d'aperçus suivent en tâche de fond.
fn save(cfg: &Config, snap: &Snapshot, kind: &str, target: f64) -> Result<(PathBuf, f64), String> {
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| format!("Dossier de sortie inaccessible : {e}"))?;
    let mut out = cfg.out_dir.join(format!("{kind} {}.{}", stamp(), cfg.container));
    // Deux enregistrements dans la même seconde : le second ne remplace pas le premier.
    let mut n = 2;
    while out.exists() {
        out = cfg.out_dir.join(format!("{kind} {} ({n}).{}", stamp(), cfg.container));
        n += 1;
    }
    // Clip : pile la durée du buffer. L'excédent du début est gardé dans le fichier mais marqué « à ne pas
    // montrer » (liste d'édition MP4/MOV) : sans réencodage, durée affichée exacte (voir clipplan.rs).
    // `target` : durée du buffer, ou moins si un clip récent a déjà emporté le début (clipplan::window).
    let trim = crate::clipplan::trim(snap.media, target, kind == "Clip" && cfg.container != "mkv");
    concat(cfg, &snap.parts, &out, trim)?;
    let want = crate::clipplan::expected(snap.media, snap.secs, trim, target);
    let (cfg, file) = (cfg.clone(), out.clone());
    // Après l'annonce du clip, à priorité « inactive ». Avant, la miniature était faite avant d'annoncer le
    // clip (~0,2 s de plus à chaque fois).
    std::thread::spawn(move || {
        // Miniature d'abord, annoncée à la fenêtre : le clip apparaît avec son image ~0,2 s après l'appui. Avant,
        // elle attendait la vérification (ffprobe sur tout le fichier) : ~1 s de cadre vide.
        // Aussi pendant un jeu plein écran (un seul fil, priorité « inactive », une image) : avant, elle
        // attendait le retour au bureau et la barre ouverte en jeu montrait des clips sans image.
        if make_thumb(&cfg, &file) {
            thumbs_ready();
        }
        verify(&cfg, &file, want);
        while crate::foreground::fullscreen_foreground() {
            std::thread::sleep(Duration::from_secs(5));
        }
        // Planche d'aperçus de la barre de temps.
        make_strip(&cfg, &file);
    });
    Ok((out, want))
}

/// Ce que dit un clip sur lui-même (paquets vidéo lus par ffprobe).
#[derive(Default, Debug, PartialEq)]
struct Check {
    frames: u32,
    secs: f64,
    first_key: bool,
    /// Plus grand écart entre deux images, horodatages qui reculent, plus grand écart entre deux images clés.
    max_gap: f64,
    back: u32,
    key_gap: f64,
}

/// Lignes « pts,dts,flags » de ffprobe (un paquet vidéo par ligne, dans l'ordre du fichier).
fn check_packets<'a>(lines: impl Iterator<Item = &'a str>) -> Check {
    let mut c = Check::default();
    let (mut prev, mut last_key, mut first, mut any) = (None::<f64>, None::<f64>, None::<f64>, false);
    for l in lines {
        let f: Vec<&str> = l.split(',').collect();
        let (Some(pts), Some(dts)) = (f.first().and_then(|v| v.trim().parse::<f64>().ok()), f.get(1).and_then(|v| v.trim().parse::<f64>().ok())) else { continue };
        let key = f.get(2).is_some_and(|v| v.contains('K'));
        // Le décodage part du tout premier paquet, même caché par la liste d'édition (début coupé pile).
        if !any {
            (any, c.first_key) = (true, key);
        }
        if f.get(2).is_some_and(|v| v.contains('D')) {
            if key {
                last_key = Some(pts);
            }
            continue;
        }
        if c.frames == 0 {
            first = Some(dts);
        }
        c.frames += 1;
        if let Some(p) = prev {
            if dts <= p {
                c.back += 1;
            }
            c.max_gap = c.max_gap.max(dts - p);
        }
        prev = Some(dts);
        if key {
            if let Some(k) = last_key {
                c.key_gap = c.key_gap.max(pts - k);
            }
            last_key = Some(pts);
        }
    }
    c.secs = prev.zip(first).map_or(0.0, |(a, b)| a - b);
    c
}

/// Anomalies d'un clip : `want` = durée attendue (0 = inconnue), `fps` = images/s demandées.
fn problems(c: &Check, want: f64, fps: u32) -> Vec<String> {
    let mut p = vec![];
    if c.frames == 0 {
        return vec!["aucune image".into()];
    }
    if !c.first_key {
        p.push("ne commence pas par une image clé".into());
    }
    if c.back > 0 {
        p.push(format!("{} horodatages qui reculent", c.back));
    }
    if c.max_gap > 0.25 {
        p.push(format!("trou de {:.1} s entre deux images", c.max_gap));
    }
    if c.key_gap > f64::from(2 * SEG_SECS) + 1.0 {
        p.push(format!("images clés espacées de {:.1} s", c.key_gap));
    }
    if want > 0.0 && (c.secs - want).abs() > f64::from(2 * SEG_SECS) {
        p.push(format!("durée {:.1} s pour {want:.1} s attendues", c.secs));
    }
    if c.secs > 1.0 && f64::from(c.frames) / c.secs < f64::from(fps) * 0.8 {
        p.push(format!("{:.0} images/s au lieu de {fps}", f64::from(c.frames) / c.secs));
    }
    p
}

/// Contrôle d'un clip juste après sa création (ffprobe lit l'index du fichier, rien n'est décodé) : première
/// image clé, horodatages croissants, aucun trou entre deux images, images clés régulières, durée attendue,
/// cadence réelle. Le résultat va au journal ; une anomalie y est écrite en clair.
pub fn verify(cfg: &Config, clip: &Path, want: f64) -> bool {
    let out = Command::new(ffprobe(cfg))
        .creation_flags(CREATE_NO_WINDOW | 0x0000_0040)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,dts_time,flags", "-of", "csv=p=0"])
        .arg(clip)
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let c = check_packets(String::from_utf8_lossy(&out).lines());
    let bad = problems(&c, want, cfg.clip.fps);
    let name = clip.file_name().unwrap_or_default().to_string_lossy();
    if bad.is_empty() {
        crate::log(&format!("clip vérifié ({name}) : {} images en {:.1} s, image clé toutes les {:.1} s au plus, écart max {:.0} ms", c.frames, c.secs, c.key_gap, c.max_gap * 1000.0));
    } else {
        crate::log(&format!("clip ANORMAL ({name}) : {}", bad.join(" ; ")));
    }
    bad.is_empty()
}

/// Écart entre deux images de la planche d'aperçus : 2 s (une image clé sur deux secondes), plus pour un
/// long REC (100 images au plus).
pub fn strip_step(secs: f64) -> f64 {
    (secs / 100.0).max(2.0)
}

/// Planche d'aperçus d'un clip (« nom~p.jpg » à côté de sa miniature) : une image de 160x90 toutes les
/// \`strip_step\` secondes, 10 par rangée. Seules les images clés sont décodées (une toutes les 2 s) :
/// mesuré sur 48 s en 1080p60, 0,5 s de processeur une seule fois, 38 Ko. Priorité « inactive ».
pub fn make_strip(cfg: &Config, clip: &Path) -> bool {
    let secs = probe(cfg, clip).secs;
    if secs <= 0.0 {
        return false;
    }
    let iv = strip_step(secs);
    let rows = ((secs / iv).floor() as u32 + 1).div_ceil(10);
    let mut c = ffmpeg(cfg);
    c.creation_flags(CREATE_NO_WINDOW | 0x0000_0040);
    c.args(["-loglevel", "quiet", "-skip_frame", "nokey", "-i"]).arg(clip);
    c.args(["-an", "-sn", "-vf", &format!("fps=1/{iv},scale=160:90:force_original_aspect_ratio=decrease,pad=160:90:(ow-iw)/2:(oh-ih)/2,tile=10x{rows}"), "-frames:v", "1", "-q:v", "6", "-y"]);
    c.arg(strip_path(clip));
    let Ok(mut ch) = c.spawn() else { return false };
    ch.wait().is_ok_and(|s| s.success())
}

/// Clé des miniatures d'un clip : sa date de création (pas de 100 ns), qui ne change pas quand on le renomme.
/// Avant : le nom du fichier. Deux clips renommés pareil (dans deux dossiers de jeu) partageaient une miniature,
/// et un clip renommé avant la fin de sa miniature (faite après la partie) la perdait.
pub fn thumb_key(clip: &Path) -> String {
    use std::os::windows::fs::MetadataExt;
    std::fs::metadata(clip).map_or_else(|_| clip.file_stem().unwrap_or_default().to_string_lossy().into_owned(), |m| format!("c{:x}", m.creation_time()))
}

/// Fabrique la miniature d'un clip (image à 1 s, sinon la première), à priorité « inactive ».
fn make_thumb(cfg: &Config, clip: &Path) -> bool {
    // Une seule fabrication à la fois par clip : l'onglet Vidéos, rafraîchi à l'annonce du clip, la demandait
    // aussi (deux ffmpeg écrivant le même fichier).
    static BUSY: Mutex<Vec<PathBuf>> = Mutex::new(vec![]);
    {
        let mut b = BUSY.lock().unwrap();
        if b.iter().any(|p| p == clip) {
            return false;
        }
        b.push(clip.to_path_buf());
    }
    let ok = make_thumb_now(cfg, clip);
    BUSY.lock().unwrap().retain(|p| p != clip);
    ok
}

/// Fonction prévenue quand une miniature est prête (main.rs : l'onglet Vidéos et la barre la relisent).
pub static ON_THUMB: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

fn thumbs_ready() {
    if let Some(f) = ON_THUMB.get() {
        f();
    }
}

fn make_thumb_now(cfg: &Config, clip: &Path) -> bool {
    let out = thumb_path(clip);
    let _ = std::fs::create_dir_all(crate::config::dir().join("thumbs"));
    for at in ["1", "0"] {
        let mut t = ffmpeg(cfg);
        t.creation_flags(CREATE_NO_WINDOW | 0x0000_0040);
        t.args(["-loglevel", "quiet", "-threads", "1", "-ss", at, "-i"]).arg(clip).args(["-frames:v", "1", "-vf", "scale=320:-2", "-y"]).arg(&out);
        if run_now(t) && out.exists() {
            return true;
        }
    }
    false
}

/// Miniatures absentes (cache effacé, clip enregistré par une autre copie de Clipper, ancien clip) : refaites
/// une à une en tâche de fond, jamais pendant un jeu plein écran. Avant, une miniature manquante le restait
/// pour toujours. Vrai si au moins une a été faite. Une seule tournée à la fois.
pub fn make_missing_thumbs(cfg: &Config, clips: &[PathBuf]) -> bool {
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::Relaxed) {
        return false;
    }
    let mut made = 0;
    for clip in clips.iter().filter(|c| !thumb_path(c).exists()) {
        while crate::foreground::fullscreen_foreground() {
            std::thread::sleep(Duration::from_secs(5));
        }
        made += usize::from(make_thumb(cfg, clip));
    }
    if made > 0 {
        crate::log(&format!("miniatures refaites : {made}"));
    }
    BUSY.store(false, Ordering::Relaxed);
    made > 0
}

/// Miniature d'un clip (onglet Vidéos, barre).
pub fn thumb_path(clip: &Path) -> PathBuf {
    crate::config::dir().join("thumbs").join(format!("{}.jpg", thumb_key(clip)))
}

/// Fichier de la planche d'aperçus d'un clip.
pub fn strip_path(clip: &Path) -> PathBuf {
    crate::config::dir().join("thumbs").join(format!("{}~p.jpg", thumb_key(clip)))
}

/// Miniature et planche d'avant la clé par date (nommées comme le fichier) : renommées une fois à la clé.
pub fn migrate_thumbs(clip: &Path) {
    let dir = crate::config::dir().join("thumbs");
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let key = thumb_key(clip);
    if key == stem {
        return;
    }
    for suffix in [".jpg", "~p.jpg"] {
        let (old, new) = (dir.join(format!("{stem}{suffix}")), dir.join(format!("{key}{suffix}")));
        if !new.exists() && old.exists() {
            let _ = std::fs::rename(old, new);
        }
    }
}

/// Vrai quand aucun ffmpeg de capture ne tourne (clipping désactivé ou au repos).
pub fn is_idle() -> bool {
    !CAPTURING.load(Ordering::Relaxed)
}
/// Un ffmpeg de buffer tourne (pour le journal de mesure).
pub static CAPTURING: AtomicBool = AtomicBool::new(false);
/// Fil du dernier superviseur lancé : le suivant attend sa fin (voir Buffer::start).
static PREV: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// Encodeur saturé depuis plus de 10 s : la capture est relancée en mode allégé, jusqu'au retour au calme.
pub static LIGHT: AtomicBool = AtomicBool::new(false);

/// Un seul clip assemblé à la fois (le disque n'écrit pas deux fichiers en même temps). Chacun a pris son
/// instantané à l'appui, avant d'attendre son tour : deux appuis rapprochés donnent deux clips corrects.
static SAVING: Mutex<()> = Mutex::new(());
/// Horloge du buffer, marque du dernier clip et segments retenus (registry.rs), partagés par le superviseur
/// de capture, les clips et le REC.
static REGISTRY: std::sync::LazyLock<Mutex<crate::registry::Registry>> = std::sync::LazyLock::new(Default::default);

/// Ce qu'un clip emporte, figé à l'instant de l'appui : les segments de chaque capture, dans l'ordre, et la
/// durée qu'ils couvrent. La capture continue pendant l'assemblage ; rien n'est retiré ni vidé du buffer.
pub struct Snapshot {
    parts: Vec<Vec<PathBuf>>,
    secs: f64,
    /// Durée exacte des images emportées (PTS des segments), si tous sont des .ts.
    media: Option<f64>,
    pin: Option<u64>,
    /// Copie du segment en cours d'écriture, arrêtée à sa dernière image entière.
    tail: Option<PathBuf>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Some(n) = self.pin {
            REGISTRY.lock().unwrap().release(n);
        }
        if let Some(t) = &self.tail {
            let _ = std::fs::remove_file(t);
        }
    }
}

/// Instantané du buffer : les segments choisis par `pick` sur l'horloge du buffer, retenus contre le
/// nettoyage, et le segment en cours copié jusqu'à sa dernière image entière. Quelques millisecondes : le
/// clip va jusqu'à l'appui sans attendre la fin du segment (avant : jusqu'à 2,7 s d'attente).
/// `pick` choisit les segments sous le verrou du registre (ils y sont retenus avant qu'il soit relâché : le
/// nettoyage ne peut plus les effacer) et peut rendre une valeur de plus (durée visée et marque d'un clip).
fn take<T>(dir: &Path, fps: u32, pick: impl FnOnce(&mut crate::registry::Registry, f64, &[u64]) -> (crate::registry::Pick, T)) -> (Snapshot, T) {
    let files = segments(dir);
    let numbers: Vec<u64> = files.iter().map(|p| seg_number(p)).collect();
    let now = crate::timeline::now();
    let (crate::registry::Pick { parts: picked, secs, pin }, extra) = {
        let mut reg = REGISTRY.lock().unwrap();
        let r = pick(&mut reg, now, &numbers);
        let (n, runs, age) = reg.stats(now);
        crate::log(&format!("horloge du buffer : {n} segments datés, {runs} capture(s), plus ancien il y a {age:.1} s, {} sur le disque", numbers.len()));
        r
    };
    let mut snap = Snapshot { parts: vec![], secs, media: Some(0.0), pin, tail: None };
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for part in picked {
        let mut paths: Vec<PathBuf> = part.segs.iter().filter_map(|n| files.iter().find(|p| seg_number(p) == *n).cloned()).collect();
        if part.live {
            // Segment en cours d'écriture par ffmpeg : sa partie déjà écrite, copiée à part.
            if let Some(cur) = paths.pop() {
                let data = if cur.extension().is_some_and(|e| e == "ts") { std::fs::read(&cur).unwrap_or_default() } else { vec![] };
                let cut = ts_cut(&data);
                let tail = dir.join(format!("tail-{}.ts", SEQ.fetch_add(1, Ordering::Relaxed)));
                if cut > 0 && std::fs::write(&tail, &data[..cut]).is_ok() {
                    paths.push(tail.clone());
                    snap.tail = Some(tail);
                }
            }
        }
        paths.retain(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0));
        if !paths.is_empty() {
            let span = ts_span(&paths, fps);
            snap.media = snap.media.zip(span).map(|(a, b)| a + b);
            snap.parts.push(paths);
        }
    }
    (snap, extra)
}

/// AV1 (segments MKV) : un segment en cours d'écriture ne se lit pas. On attend qu'il se ferme (2 s au plus)
/// pour que le clip aille jusqu'à l'appui. Capture arrêtée ou figée : on n'attend pas plus.
fn wait_segment_close(dir: &Path) {
    let pressed = std::time::Instant::now();
    let current = segments(dir).last().map(|p| seg_number(p));
    while current.is_some() && pressed.elapsed() < Duration::from_millis(u64::from(SEG_SECS) * 1000 + 700) {
        if segments(dir).last().map(|p| seg_number(p)) != current {
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
}

/// Clip : les dernières secondes avant l'appui (durée du buffer), sans réencodage.
pub fn save_clip(cfg: &Config, dir: &Path) -> Result<(PathBuf, f64), String> {
    let t0 = std::time::Instant::now();
    if seg_ext(&cfg.encoder) == "mkv" {
        wait_segment_close(dir);
    }
    // Début du clip : pas avant la fin du clip précédent (registry.rs). Marque posée à l'instant de
    // l'instantané : deux appuis rapprochés se suivent sans se recouvrir.
    let (snap, (target, mark)) = take(dir, cfg.clip.fps, |reg, now, disk| {
        let (pick, w, m) = reg.clip(now, disk, f64::from(cfg.buffer_secs));
        (pick, (w, m))
    });
    // Clip raté : la marque revient, ces secondes iront dans le prochain clip.
    let undo = || REGISTRY.lock().unwrap().undo(mark);
    let t_snap = t0.elapsed().as_secs_f32();
    if snap.parts.is_empty() {
        undo();
        let err = std::fs::read_to_string(dir.join("ffmpeg.log")).unwrap_or_default();
        crate::log(&format!("clip impossible : {} segments dans le dossier, capture en marche = {}, ffmpeg : {}", segments(dir).len(), !is_idle(), err.lines().last().unwrap_or("aucun message")));
        return Err(if is_idle() { "La capture ne tournait pas : rien à enregistrer. Clipper la relance.".into() } else { "Le buffer vient de démarrer : réessayez dans quelques secondes.".into() });
    }
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let t1 = std::time::Instant::now();
    let out = save(cfg, &snap, "Clip", target).inspect_err(|_| undo())?;
    crate::log(&format!(
        "clip : instantané {:.0} ms, attente {:.0} ms, assemblage {:.0} ms ({} segments, {} capture(s), {:.0} s, images {:.2} s, durée visée {:.1} s)",
        t_snap * 1000.0,
        (t1 - t0).as_secs_f32() * 1000.0 - t_snap * 1000.0,
        t1.elapsed().as_secs_f32() * 1000.0,
        snap.parts.iter().map(Vec::len).sum::<usize>(),
        snap.parts.len(),
        snap.secs,
        snap.media.unwrap_or(0.0),
        target
    ));
    Ok(out)
}

/// REC : on garde les segments du buffer à partir de maintenant (zéro coût en plus).
pub fn rec_start(buf: &Buffer) {
    let n = segments(&buf.dir).last().map_or(0, |p| seg_number(p));
    *buf.rec_from.lock().unwrap() = Some(n);
}

/// Attend la fin du segment en cours pour ne rien couper, puis assemble.
pub fn rec_stop(cfg: &Config, buf: &Buffer) -> Result<(PathBuf, f64), String> {
    let from = buf.rec_from.lock().unwrap().ok_or("Aucun REC en cours.")?;
    // Instantané à l'arrêt, segment en cours compris (avant : 2,6 s d'attente avant l'assemblage).
    if seg_ext(&cfg.encoder) == "mkv" {
        wait_segment_close(&buf.dir);
    }
    let (snap, ()) = take(&buf.dir, cfg.clip.fps, |reg, now, disk| (reg.rec(now, disk, from), ()));
    let r = if snap.parts.is_empty() { Err("Le REC n'a rien enregistré.".to_string()) } else { save(cfg, &snap, "REC", f64::from(cfg.buffer_secs)) };
    *buf.rec_from.lock().unwrap() = None;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_on_16_10_screen() {
        assert_eq!(output_size(1080, (1920, 1200)), Some((1920, 1200)));
        assert_eq!(output_size(1440, (1920, 1200)), None);
        assert_eq!(output_size(1080, (1920, 1080)), Some((1920, 1080)));
        assert_eq!(output_size(1080, (3840, 2160)), Some((1920, 1080)));
        assert_eq!(output_size(1440, (3840, 2400)), Some((2304, 1440)));
    }

    /// Écrans ultralarges et 4:3 : la fenêtre affiche ces tailles (front_a appelle output_size/final_size).
    #[test]
    fn sizes_on_wide_and_4_3_screens() {
        assert_eq!(output_size(1440, (3440, 1440)), Some((3440, 1440)));
        assert_eq!(output_size(1440, (5120, 1440)), Some((5120, 1440)));
        assert_eq!(output_size(1080, (3440, 1440)), Some((2580, 1080)));
        assert_eq!(output_size(1440, (2560, 1080)), None);
        assert_eq!(output_size(1080, (1280, 960)), None);
        assert_eq!(output_size(720, (1280, 960)), Some((960, 720)));
        assert_eq!(bitrate_kbps(final_size(&Config::default(), crate::config::Quality { height: 1440, fps: 60 }, (3440, 1440)).1, 60, "h264_nvenc", 1.0), 45_000);
    }

    #[test]
    fn stretch_is_display_aspect_not_cpu_scaling() {
        let mut c = Config::default();
        (c.encoder, c.stretch) = ("hevc_nvenc".into(), "16:9".into());
        let a = capture_args(&c, crate::config::Quality { height: 1080, fps: 60 }, (1280, 960), &[]).join(" ");
        assert!(!a.contains("hwdownload") && !a.contains("scale="), "l'image ne doit pas quitter le GPU : {a}");
        assert!(a.contains("-aspect 1706:960"), "{a}");
    }

    #[test]
    fn encoders_follow_chosen_chip() {
        let ok = ["h264_nvenc", "hevc_nvenc", "h264_amf", "hevc_amf", "libx264"];
        let gpus = vec![Gpu { id: "nvidia".into(), name: "RTX".into() }, Gpu { id: "amd".into(), name: "Radeon".into() }];
        let mut c = Config::default();
        c.encode_chip = "nvidia".into();
        c.encoder = "hevc_amf".into();
        assert_eq!(resolve_encoder(&c, &ok, &gpus), "h264_nvenc");
        c.encoder = "hevc_nvenc".into();
        assert_eq!(resolve_encoder(&c, &ok, &gpus), "hevc_nvenc");
        c.encode_chip = "igpu".into();
        assert_eq!(resolve_encoder(&c, &ok, &gpus), "h264_amf");
        c.encoder = "libx264".into();
        assert_eq!(resolve_encoder(&c, &ok, &gpus), "libx264");
        c.encode_chip = "auto".into();
        c.encoder = "h264_amf".into();
        assert_eq!(resolve_encoder(&c, &ok, &gpus), "h264_nvenc");
    }

    #[test]
    fn monitor_names_matched_by_size_then_order() {
        let s = |g: &str, w, h, n: &str| (g.to_string(), (w, h), n.to_string());
        let screens = vec![s(r"\\.\DISPLAY2", 1920, 1080, "DELL P2419H"), s(r"\\.\DISPLAY1", 2560, 1440, "LG ULTRAGEAR")];
        let n = match_names(&[(2560, 1440), (1920, 1080), (800, 600)], screens);
        let s2 = |g: &str, n: &str| Some((g.to_string(), n.to_string()));
        assert_eq!(n, vec![s2(r"\\.\DISPLAY1", "LG ULTRAGEAR"), s2(r"\\.\DISPLAY2", "DELL P2419H"), None]);
    }

    /// Plafond WGC : double du clip sur un écran plus rapide (sans le dépasser), sinon celui du clip.
    #[test]
    fn wgc_frame_cap_follows_the_screen() {
        assert_eq!(wgc_max_rate(60, 144), 120);
        assert_eq!(wgc_max_rate(30, 144), 60);
        assert_eq!(wgc_max_rate(60, 90), 90);
        assert_eq!(wgc_max_rate(60, 60), 60);
        assert_eq!(wgc_max_rate(120, 60), 120);
        assert_eq!(wgc_max_rate(60, 0), 120, "fréquence inconnue : double");
    }

    /// Pas de cadre jaune, écran choisi, taille réduite seulement si demandée.
    #[test]
    fn wgc_source_has_no_border() {
        let cfg = Config::default();
        let native = gfx_source(&cfg, (1920, 1080), (1920, 1080), 60);
        assert!(native.starts_with("gfxcapture=") && native.contains("display_border=0") && native.ends_with(",fps=60"), "{native}");
        assert!(!native.contains("width="), "taille native : aucun redimensionnement");
        let small = gfx_source(&cfg, (1280, 720), (1920, 1080), 60);
        assert!(small.contains("width=1280:height=720"), "{small}");
    }

    #[test]
    fn bitrate_scales() {
        assert_eq!(bitrate_kbps(1080, 60, "h264_nvenc", 1.0), 28_000);
        assert_eq!(bitrate_kbps(1080, 30, "h264_nvenc", 1.0), 18_200);
        assert_eq!(bitrate_kbps(2160, 60, "hevc_nvenc", 1.0), 56_000);
        assert_eq!(bitrate_kbps(1080, 60, "h264_nvenc", 2.0), 56_000); // plafond du curseur
    }

    /// ffprobe trouvé dans le même dossier que ffmpeg, quel que soit le nom du dossier.
    #[test]
    fn ffprobe_sits_next_to_ffmpeg() {
        let mut cfg = Config::default();
        cfg.ffmpeg = r"C:\Outils\ffmpeg-7.1-full\bin\ffmpeg.exe".into();
        assert_eq!(ffprobe(&cfg), r"C:\Outils\ffmpeg-7.1-full\bin\ffprobe.exe");
        cfg.ffmpeg = "ffmpeg".into();
        assert_eq!(ffprobe(&cfg), "ffprobe");
    }

    /// Segment coupé en pleine écriture : on garde tout jusqu'à la dernière image entière.
    #[test]
    fn ts_is_cut_before_its_last_unfinished_frame() {
        let pkt = |pid: u16, start: bool| {
            let mut p = vec![0u8; 188];
            (p[0], p[1], p[2]) = (0x47, (pid >> 8) as u8 & 0x1f | if start { 0x40 } else { 0 }, pid as u8);
            p
        };
        // image 1 (3 paquets), son, image 2 (2 paquets), son, image 3 commencée, puis 100 octets d'un paquet coupé
        let seq = [(0x100, true), (0x100, false), (0x100, false), (0x101, true), (0x100, true), (0x100, false), (0x101, true), (0x100, true)];
        let mut data: Vec<u8> = seq.iter().flat_map(|(pid, s)| pkt(*pid, *s)).collect();
        data.extend(vec![0x47; 100]);
        assert_eq!(ts_cut(&data), 7 * 188, "coupé au début de l'image 3");
        assert_eq!(ts_cut(&data[..188 * 3]), 0, "une seule image, peut-être incomplète : rien d'utilisable");
        assert_eq!(ts_cut(&[]), 0);
        // Segment tout juste ouvert : en-têtes (PAT, PMT), son, puis une seule image commencée -> rien d'entier.
        let fresh: Vec<u8> = [(0x0, true), (0x1000, true), (0x101, true), (0x100, true)].iter().flat_map(|(pid, s)| pkt(*pid, *s)).collect();
        assert_eq!(ts_cut(&fresh), 0, "aucune image entière dans un segment tout juste ouvert");
    }

    /// En-tête PES avec un PTS, dans un paquet TS vidéo (PID 0x100, début d'unité).
    fn pes(pts: u64) -> Vec<u8> {
        let mut p = vec![0xffu8; 188];
        p[..4].copy_from_slice(&[0x47, 0x41, 0x00, 0x10]);
        let t = [(0x21 | ((pts >> 29) & 0x0e)) as u8, (pts >> 22) as u8, (((pts >> 14) & 0xfe) | 1) as u8, (pts >> 7) as u8, (((pts << 1) & 0xfe) | 1) as u8];
        p[4..18].copy_from_slice(&[0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5, t[0], t[1], t[2], t[3], t[4]]);
        p
    }

    #[test]
    fn pts_are_read_from_ts_headers() {
        // Images à 1,0 s, 1,5 s puis 1,25 s (ordre de décodage ≠ ordre d'affichage) : 1,0 à 1,5 s.
        let data: Vec<u8> = [90_000u64, 135_000, 112_500].iter().flat_map(|t| pes(*t)).collect();
        assert_eq!(ts_pts_range(&data), Some((1.0, 1.5)));
        assert_eq!(ts_pts_range(&[]), None);
    }

    #[test]
    fn hidden_preroll_is_not_counted_by_the_check() {
        // 0,5 s de pré-roll caché (D) avant 2 s d'images visibles : clip de 2 s, qui part d'une image clé.
        let mut l: Vec<String> = (0..30).map(|i| format!("{0:.6},{0:.6},{1}", -0.5 + i as f64 / 60.0, if i == 0 { "KD_" } else { "_D_" })).collect();
        l.extend((0..120).map(|i| format!("{0:.6},{0:.6},___", i as f64 / 60.0)));
        let c = check_packets(l.iter().map(String::as_str));
        assert_eq!((c.frames, c.first_key), (120, true));
        assert!(problems(&c, 2.0, 60).is_empty(), "{:?}", problems(&c, 2.0, 60));
    }

    #[test]
    fn clip_check_reports_each_anomaly() {
        // 60 images/s pendant 4 s, image clé toutes les 2 s : rien à signaler.
        let good: Vec<String> = (0..240).map(|i| format!("{0:.6},{0:.6},{1}", i as f64 / 60.0, if i % 120 == 0 { "K__" } else { "___" })).collect();
        let c = check_packets(good.iter().map(String::as_str));
        assert_eq!((c.frames, c.first_key, c.back), (240, true, 0));
        assert!(problems(&c, 4.0, 60).is_empty(), "{:?}", problems(&c, 4.0, 60));
        // Trou de 13 s (capture figée), horodatage qui recule, pas d'image clé au début.
        let bad = ["0.0,0.0,___", "0.016,0.016,___", "13.0,13.0,K__", "12.9,12.9,___"];
        let c = check_packets(bad.into_iter());
        let p = problems(&c, 60.0, 60).join(" ; ");
        for word in ["image clé", "reculent", "trou de 13.0 s", "durée", "images/s"] {
            assert!(p.contains(word), "« {word} » absent de : {p}");
        }
        assert_eq!(problems(&Check::default(), 60.0, 60), vec!["aucune image".to_string()]);
    }
}

#[cfg(test)]
mod live_fullscreen {
    #[test]
    #[ignore = "dépend de la fenêtre au premier plan"]
    fn report() {
        println!("jeu plein écran au premier plan : {}", crate::foreground::fullscreen_foreground());
    }

    #[test]
    #[ignore = "lit les GPU et les écrans"]
    fn hardware() {
        for g in super::gpus() {
            println!("GPU {} : {}", g.id, g.name);
        }
        println!("écrans Windows {:?}", super::monitor_names());
        println!("sorties ddagrab {:?}", super::monitors(&crate::config::Config::load()));
    }
}

#[cfg(test)]
mod segs {
    #[test]
    fn rec_file_is_not_a_segment() {
        let d = std::env::temp_dir().join(format!("clipper-segs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for n in ["0000000001.ts", "0000000002.ts", "rec-2026-11-07 21-00-00.ts", "ffmpeg.log"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        let s = super::segments(&d);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|p| !p.to_string_lossy().contains("rec-")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
