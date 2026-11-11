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

pub fn probe_encoders(cfg: &Config) -> Vec<&'static str> {
    // AV1 : segments MKV (voir seg_ext), le MPEG-TS de ffmpeg le transformait en flux de données illisible.
    const ALL: [&str; 8] = ["h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "av1_amf", "h264_qsv", "libx264"];
    ALL.into_iter()
        .filter(|e| {
            ffmpeg(cfg)
                .args(["-loglevel", "quiet", "-f", "lavfi", "-i", "color=black:s=256x256:d=0.1", "-frames:v", "1", "-c:v", e, "-f", "null", "-"])
                .status()
                .is_ok_and(|s| s.success())
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
fn constant_quality(encoder: &str) -> bool {
    encoder.ends_with("_nvenc") || encoder == "libx264"
}

/// Preset le plus léger compatible avec la qualité visée.
fn preset_args(encoder: &str, quality: f32) -> Vec<&'static str> {
    if encoder.ends_with("_nvenc") {
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

/// Résolution actuelle d'un écran GDI.
fn gdi_size(gdi: &str) -> Option<(u32, u32)> {
    use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
    let name: Vec<u16> = gdi.encode_utf16().chain(Some(0)).collect();
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    (unsafe { EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) } != 0).then_some((dm.dmPelsWidth, dm.dmPelsHeight))
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
    let out = Command::new(cfg.ffmpeg.replace("ffmpeg", "ffprobe"))
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-v", "error", "-f", "lavfi", "-i", &format!("ddagrab=output_idx={}", cfg.monitor)])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (w, h) = s.trim().split_once(',')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Écrans vus par ddagrab (index 0..3), pour le réglage « Écran capturé ».
pub fn monitors(cfg: &Config) -> Vec<serde_json::Value> {
    let sizes: Vec<(u32, u32)> = (0..4)
        .map_while(|i| {
            let mut c = cfg.clone();
            c.monitor = i;
            native_size(&c)
        })
        .collect();
    let screens = monitor_names().into_iter().filter_map(|(gdi, name)| Some((gdi.clone(), gdi_size(&gdi)?, name))).collect();
    let names = match_names(&sizes, screens);
    *MONITOR_GDI.lock().unwrap() = names.iter().map(|n| n.as_ref().map(|(gdi, _)| gdi.clone())).collect();
    sizes
        .iter()
        .zip(names)
        .enumerate()
        .map(|(i, (&(w, h), name))| {
            let name = name.map(|(_, n)| n).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Écran {}", i + 1));
            serde_json::json!({"idx": i, "w": w, "h": h, "name": name})
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

/// Source gfxcapture (Windows Graphics Capture) qui livre directement des images à la taille voulue :
/// le redimensionnement se fait sur la carte graphique, sans copie de l'écran entier vers le processeur.
/// Mesuré (1440p -> 720p, 60 i/s, NVENC) : 6 % d'un cœur au lieu de 30 à 87 % par le processeur.
fn gfx_source(cfg: &Config, (w, h): (u32, u32), fps: u32) -> String {
    let gdi = MONITOR_GDI.lock().unwrap().get(cfg.monitor as usize).cloned().flatten();
    let screen = gdi.as_deref().and_then(hmonitor).map_or(format!("monitor_idx={}", cfg.monitor), |m| format!("hmonitor={m}"));
    format!("gfxcapture={screen}:width={w}:height={h}:resize_mode=scale:scale_mode=bilinear:max_framerate={fps}:capture_cursor=1,fps={fps}")
}

/// Résultat du dernier essai de redimensionnement sur la carte graphique (None = pas encore essayé).
static GPU_SCALE: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// Le chemin GPU marche-t-il sur ce PC avec cet encodeur ? Essai réel de 3 images (8 s max), gardé en cache.
/// Échec (portable AMD + NVIDIA, Windows trop ancien…) : retour au redimensionnement processeur.
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
    crate::log(&format!("redimensionnement sur la carte graphique : {}", if ok { "oui" } else { "non, retour au processeur" }));
    *GPU_SCALE.lock().unwrap() = Some((key, ok));
    ok
}

/// Pour l'UI : le redimensionnement passe-t-il par la carte graphique ? (None = pas encore essayé.)
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
    let out = Command::new(cfg.ffmpeg.replace("ffmpeg", "ffprobe"))
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
    let mut vf = format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", cfg.monitor, q.fps);
    // Les images de ddagrab vivent sur la carte NVIDIA : AMF/QSV/x264 doivent passer par la mémoire.
    let off_gpu = !cfg.encoder.ends_with("_nvenc");
    let gfx = gfx_source(cfg, (w, h), q.fps);
    if (w, h) != native && gpu_scale_ok(cfg, &gfx, off_gpu) {
        // Réduction sur la carte graphique ; seule la petite image quitte le GPU si l'encodeur n'est pas NVIDIA.
        vf = gfx;
        if off_gpu {
            vf += ",hwdownload,format=bgra,format=nv12";
        }
    } else if (w, h) != native || off_gpu {
        // Image hors du GPU : changement de résolution (720p…) ou encodeur non NVIDIA. scale_d3d11 (sur
        // la carte) échoue sur les portables AMD + NVIDIA : redimensionnement processeur, filtre bilinéaire.
        vf += ",hwdownload,format=bgra";
        if (w, h) != native {
            vf += &format!(",scale={w}:{h}:flags=bilinear");
        }
        vf += &format!(",format=nv12,fps={}", q.fps);
    }
    let kb = bitrate_kbps(h, q.fps, &cfg.encoder, cfg.quality);
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &vf].iter().map(|s| s.to_string()).collect();
    a.extend(crate::audio::input_args(pipes));
    let kinds = crate::audio::tracks(cfg);
    a.extend(crate::audio::output_args(if kinds.len() == pipes.len() { &kinds } else { &[] }));
    a.extend(["-c:v".into(), cfg.encoder.clone()]);
    a.extend(preset_args(&cfg.encoder, cfg.quality).iter().map(|s| s.to_string()));
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

/// Choisit les segments à mettre dans un clip de `secs` secondes.
/// `segs` est trié du plus ancien au plus récent ; le dernier est encore en cours d'écriture par ffmpeg.
pub fn pick_segments(segs: &[PathBuf], secs: u32) -> &[PathBuf] {
    let ready = &segs[..segs.len().saturating_sub(1)];
    let n = secs.div_ceil(SEG_SECS) as usize;
    &ready[ready.len().saturating_sub(n)..]
}

/// Vrai si la fenêtre au premier plan couvre tout son écran (jeu plein écran ou fenêtré sans bordure).
/// Lit seulement la taille de la fenêtre : rien n'est ouvert dans le processus du jeu.
pub fn fullscreen_foreground() -> bool {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetShellWindow, GetWindowRect};
    unsafe {
        let w = GetForegroundWindow();
        if w.is_null() || w == GetShellWindow() {
            return false;
        }
        let mut r = std::mem::zeroed();
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetWindowRect(w, &mut r) == 0 || GetMonitorInfoW(MonitorFromWindow(w, MONITOR_DEFAULTTONEAREST), &mut mi) == 0 {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom && !is_desktop(w)
    }
}

/// Le bureau (Progman / WorkerW) couvre aussi l'écran : il ne compte pas comme un jeu.
fn is_desktop(w: windows_sys::Win32::Foundation::HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(w, buf.as_mut_ptr(), 32) } as usize;
    let c = String::from_utf16_lossy(&buf[..n]);
    c == "Progman" || c == "WorkerW"
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
    /// 2e capture pendant un REC aux réglages différents du Clip : (ffmpeg, fichier .ts).
    rec2: Mutex<Option<(Child, PathBuf)>>,
    audio: Option<Arc<crate::audio::Audio>>,
    native: (u32, u32),
}

impl Buffer {
    /// Lance le superviseur : relance ffmpeg si ddagrab perd l'écran (887a0026), nettoie les vieux segments.
    pub fn start(cfg: Config, native: (u32, u32), dir: PathBuf, audio: Option<Arc<crate::audio::Audio>>) -> Self {
        let audio_keep = audio.clone();
        let _ = std::fs::create_dir_all(&dir);
        purge_if_format_changed(&cfg, native, &dir, audio.as_ref().map_or(String::new(), |_| format!("{:?}", crate::audio::tracks(&cfg))));
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None::<Child>));
        let rec_from = Arc::new(Mutex::new(None::<u64>));
        let (s, c, r, d) = (stop.clone(), child.clone(), rec_from.clone(), dir.clone());
        std::thread::spawn(move || {
            let keep = (cfg.buffer_secs / SEG_SECS + 3) as usize;
            let mut spawns = 0u64;
            let mut away = 0u32;
            while !s.load(Ordering::Relaxed) {
                // « Capture du bureau » désactivée : rien ne tourne tant qu'aucun jeu plein écran n'est au premier plan.
                if !cfg.desktop_capture && !fullscreen_foreground() {
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                // Le ffmpeg précédent a été arrêté net (réglage changé, écran perdu) : son dernier segment est
                // tronqué, parfois vide. Un seul segment illisible faisait échouer tout clip qui le contenait
                // (« ffmpeg n'a pas pu assembler le clip »). On l'enlève avant de repartir.
                if let Some(last) = segments(&d).last().filter(|p| !valid_seg(&cfg, p)) {
                    crate::log(&format!("buffer : segment tronqué retiré ({})", last.file_name().unwrap_or_default().to_string_lossy()));
                    let _ = std::fs::remove_file(last);
                }
                let next = segments(&d).last().map_or(0, |p| seg_number(p) + 1);
                let mut cmd = ffmpeg_capture(&cfg);
                let _ = std::fs::rename(d.join("ffmpeg.log"), d.join("ffmpeg.prev.log"));
                spawns += 1;
                if let Ok(f) = std::fs::File::create(d.join("ffmpeg.log")) {
                    cmd.stderr(f); // dernière erreur gardée pour le diagnostic
                }
                let pipes = audio.as_ref().map_or(vec![], |a| a.open_pipes(0, &crate::audio::tracks(&cfg), spawns));
                let args = buffer_args(&cfg, native, &d, next, &pipes);
                let _ = std::fs::write(d.join("ffmpeg-args.txt"), format!("pipes={pipes:?} audio={}
{}", audio.is_some(), args.join(" ")));
                match cmd.args(args).spawn() {
                    Ok(mut ch) if s.load(Ordering::Relaxed) => {
                        let _ = ch.kill();
                        break;
                    }
                    Ok(ch) => {
                        no_eco(&ch); // enregistrement continu : jamais ralenti par Windows
                        *c.lock().unwrap() = Some(ch);
                    }
                    Err(_) => {
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
                let mut tick = 0u32;
                // Chien de garde : ffmpeg vivant mais plus aucun segment (capture figée) -> relancé.
                let (mut last_seg, mut still) = (None::<u64>, 0u32);
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    tick += 1;
                    // Notre propre ffmpeg (jamais un autre processus) : rend la RAM froide du pilote.
                    // Mesuré : 100 Mo -> ~14 Mo de RAM physique, sans perte d'images.
                    if tick == 5 || tick % 60 == 0 {
                        if let Some(ch) = c.lock().unwrap().as_ref() {
                            trim(ch);
                        }
                    }
                    let alive = c.lock().unwrap().as_mut().is_some_and(|ch| matches!(ch.try_wait(), Ok(None)));
                    let segs = segments(&d);
                    let newest = segs.last().map(|p| seg_number(p));
                    still = if newest == last_seg { still + 1 } else { 0 };
                    last_seg = newest;
                    if alive && still > 4 * SEG_SECS + 4 {
                        crate::log(&format!("buffer : ffmpeg ne produit plus de segments depuis {still} s, relance"));
                        if let Some(mut ch) = c.lock().unwrap().take() {
                            let _ = ch.kill();
                            let _ = ch.wait();
                        }
                        break;
                    }
                    if !alive && !s.load(Ordering::Relaxed) {
                        let err = std::fs::read_to_string(d.join("ffmpeg.log")).unwrap_or_default();
                        crate::log(&format!("buffer : ffmpeg s'est arrêté, relance ({})", err.lines().last().unwrap_or("aucun message")));
                    }
                    let floor = r.lock().unwrap().unwrap_or(u64::MAX);
                    for p in segs.iter().take(segs.len().saturating_sub(keep)) {
                        if seg_number(p) < floor {
                            let _ = std::fs::remove_file(p);
                        }
                    }
                    // Retour au bureau plus de 5 s : on coupe ffmpeg (0 % GPU), les segments déjà écrits restent.
                    away = if !cfg.desktop_capture && !fullscreen_foreground() { away + 1 } else { 0 };
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
                std::thread::sleep(Duration::from_millis(300));
            }
            // stop() peut passer pendant un spawn : sans ça, le ffmpeg lancé juste après continuait seul.
            if let Some(mut ch) = c.lock().unwrap().take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        });
        Self { dir, stop, child, rec_from, rec2: Mutex::new(None), audio: audio_keep, native }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut ch) = self.child.lock().unwrap().take() {
            let _ = ch.kill();
        }
        if let Some((mut ch, _)) = self.rec2.lock().unwrap().take() {
            let _ = ch.kill();
        }
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
    for e in std::fs::read_dir(out_dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ours(&name) && (name.ends_with(".part") || name.ends_with(".txt")) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn concat(cfg: &Config, segs: &[PathBuf], out: &Path) -> Result<(), String> {
    if segs.is_empty() {
        return Err("Le buffer est encore vide.".into());
    }
    let list = out.with_extension("txt");
    let body: String = segs.iter().map(|p| format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"))).collect();
    std::fs::write(&list, body).map_err(|e| e.to_string())?;
    // Écrit sous un nom .part puis renommé : l'onglet Vidéos ne voit jamais un clip à moitié écrit.
    let part = out.with_extension(format!("{}.part", cfg.container));
    let format = match cfg.container.as_str() {
        "mkv" => "matroska",
        "mov" => "mov",
        _ => "mp4",
    };
    let run = |fast: bool| {
        let mut c = ffmpeg_capture(cfg);
        c.args(["-loglevel", "error"]);
        let dir = segs[0].parent().filter(|d| segs.iter().all(|p| p.parent() == Some(*d)));
        match dir {
            // Segments .ts lus comme un seul flux (protocole concat) : 2 min assemblées en 0,6 s au lieu de
            // 3,3 s avec le démultiplexeur concat, qui ouvre et analyse chaque fichier. Noms courts, dossier
            // courant : la ligne de commande reste courte même pour un long REC.
            Some(d) if fast => {
                let names: Vec<String> = segs.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
                c.current_dir(d).arg("-i").arg(format!("concat:{}", names.join("|")));
            }
            _ => {
                c.args(["-f", "concat", "-safe", "0", "-i"]).arg(&list);
            }
        }
        // Pas de « +faststart » : il réécrivait tout le fichier une 2e fois (double écriture disque). Disque
        // occupé par un autre programme, un clip de 54 s passait de 2 s à 26 s. Les lecteurs, navigateurs et
        // Discord lisent très bien l'index en fin de fichier.
        c.args(["-map", "0", "-c", "copy"]);
        c.args(["-f", format, "-y"]).arg(&part);
        run_now(c)
    };
    // Flux continu attendu : durée proche du nombre de segments. Sinon (capture relancée entre deux
    // segments, horodatages repartis de zéro), assemblage refait fichier par fichier.
    let want = f64::from(segs.len() as u32 * SEG_SECS);
    let t0 = std::time::Instant::now();
    let fast_ran = run(true);
    let t_fast = t0.elapsed().as_secs_f32();
    let got = if fast_ran { probe(cfg, &part).secs } else { 0.0 };
    let fast_ok = fast_ran && (got - want).abs() <= f64::from(3 * SEG_SECS);
    let ok = (fast_ok || run(false)) && std::fs::rename(&part, out).is_ok();
    crate::log(&format!(
        "assemblage : {} segments en {:.1} s ({}, rapide {t_fast:.1} s, durée {got:.0} s pour {want:.0} s)",
        segs.len(),
        t0.elapsed().as_secs_f32(),
        if fast_ok { "rapide" } else { "lent, fichier par fichier" }
    ));
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

/// Assemble, puis crée la miniature de l'onglet Vidéos.
fn save(cfg: &Config, segs: &[PathBuf], kind: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&cfg.out_dir).map_err(|e| format!("Dossier de sortie inaccessible : {e}"))?;
    let mut out = cfg.out_dir.join(format!("{kind} {}.{}", stamp(), cfg.container));
    // Deux enregistrements dans la même seconde : le second ne remplace pas le premier.
    let mut n = 2;
    while out.exists() {
        out = cfg.out_dir.join(format!("{kind} {} ({n}).{}", stamp(), cfg.container));
        n += 1;
    }
    concat(cfg, segs, &out)?;
    let thumbs = crate::config::dir().join("thumbs");
    let _ = std::fs::create_dir_all(&thumbs);
    let name = out.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let mut t = ffmpeg_capture(cfg);
    t.args(["-loglevel", "quiet", "-ss", "1", "-i"]).arg(&out)
        .args(["-frames:v", "1", "-vf", "scale=320:-2", "-y"]).arg(thumbs.join(name + ".jpg"));
    run_now(t);
    Ok(out)
}

/// Un seul clip assemblé à la fois : deux appuis rapprochés ne s'écrasent plus.
static SAVING: Mutex<()> = Mutex::new(());
/// Heure d'écriture du dernier segment du clip précédent : le clip suivant commence après.
static LAST_CLIP_END: Mutex<Option<std::time::SystemTime>> = Mutex::new(None);

pub fn save_clip(cfg: &Config, dir: &Path) -> Result<PathBuf, String> {
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    // Le clip va jusqu'à l'appui : on attend que ffmpeg ferme le segment en cours (2 s au plus), au lieu
    // de le laisser de côté. Capture arrêtée ou figée : on n'attend pas plus, on prend ce qui est écrit.
    let pressed = std::time::Instant::now();
    let current = segments(dir).last().map(|p| seg_number(p));
    while current.is_some() && pressed.elapsed() < Duration::from_millis(u64::from(SEG_SECS) * 1000 + 700) {
        if segments(dir).last().map(|p| seg_number(p)) != current {
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    crate::log(&format!("clip : segment en cours attendu {:.1} s", pressed.elapsed().as_secs_f32()));
    let all = segments(dir);
    let mut segs = recent(all.clone(), cfg.buffer_secs, std::time::SystemTime::now());
    let newest = segs.pop(); // encore en cours d'écriture : jamais dans le clip, vide ou non
    segs.retain(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0));
    segs.extend(newest);
    // Deux clips rapprochés : le second commence où le premier s'est arrêté (clip de 2 min puis un autre
    // 20 s plus tard = 20 s, pas encore 2 min dont 1 min 40 déjà dans le premier). Par l'heure d'écriture,
    // pas le numéro : les numéros repartent de zéro quand la capture est relancée.
    let mtime = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let last = *LAST_CLIP_END.lock().unwrap();
    if let Some(end) = last {
        segs.retain(|p| mtime(p).is_some_and(|t| t > end));
        if segs.is_empty() {
            return Err("Rien de nouveau depuis le dernier clip.".into());
        }
    }
    let pick = pick_segments(&segs, cfg.buffer_secs);
    if let Some(t) = pick.last().and_then(mtime) {
        *LAST_CLIP_END.lock().unwrap() = Some(t);
    }
    if pick.is_empty() {
        let age = all.last().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).and_then(|t| t.elapsed().ok()).map_or(-1, |d| d.as_secs() as i64);
        let err = std::fs::read_to_string(dir.join("ffmpeg.log")).unwrap_or_default();
        crate::log(&format!(
            "clip impossible : {} segments dans le dossier, {} récents, dernier écrit il y a {age} s, ffmpeg : {}",
            all.len(),
            segs.len(),
            err.lines().last().unwrap_or("aucun message")
        ));
        return Err(if (0..6).contains(&age) || all.is_empty() {
            "Le buffer vient de démarrer : réessayez dans quelques secondes.".into()
        } else {
            "La capture ne tournait pas : rien à enregistrer. Clipper la relance.".into()
        });
    }
    let out = match save(cfg, pick, "Clip") {
        Ok(o) => o,
        Err(e) => {
            // Assemblage refusé : un segment illisible dans le lot. On garde seulement les segments sains.
            let ok = valid_segs(cfg, pick);
            crate::log(&format!("clip : assemblage refusé, {} segments illisibles sur {} écartés", pick.len() - ok.len(), pick.len()));
            if ok.is_empty() || ok.len() == pick.len() {
                return Err(e);
            }
            return save(cfg, &ok, "Clip");
        }
    };
    // Durée contrôlée : un segment au temps cassé (PC sorti de veille, capture figée puis reprise) allongeait
    // le clip avec une image vieille de plusieurs minutes. On refait alors le clip après ce segment.
    let want = f64::from(pick.len() as u32 * SEG_SECS);
    let got = probe(cfg, &out).secs;
    if got > want + f64::from(4 * SEG_SECS) {
        let bad = pick.iter().rposition(|p| seg_secs(cfg, p) > f64::from(3 * SEG_SECS));
        crate::log(&format!("clip : {got:.0} s au lieu de {want:.0} s, segment anormal {bad:?} : clip refait sans lui"));
        if let Some(i) = bad.filter(|i| i + 1 < pick.len()) {
            let _ = std::fs::remove_file(&out);
            return save(cfg, &pick[i + 1..], "Clip");
        }
    }
    Ok(out)
}

/// Segment lisible : non vide et d'une durée connue.
fn valid_seg(cfg: &Config, p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.len() > 0) && seg_secs(cfg, p) > 0.0
}

/// Les segments lisibles de `segs`, dans l'ordre (ffprobe sur 8 fils).
fn valid_segs(cfg: &Config, segs: &[PathBuf]) -> Vec<PathBuf> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let ok: Vec<AtomicBool> = segs.iter().map(|_| AtomicBool::new(false)).collect();
    std::thread::scope(|s| {
        for _ in 0..segs.len().min(8) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(p) = segs.get(i) else { break };
                ok[i].store(valid_seg(cfg, p), Ordering::Relaxed);
            });
        }
    });
    segs.iter().zip(&ok).filter(|(_, v)| v.load(Ordering::Relaxed)).map(|(p, _)| p.clone()).collect()
}

/// Durée d'un segment (ffprobe direct, sans cache).
fn seg_secs(cfg: &Config, p: &Path) -> f64 {
    Command::new(cfg.ffmpeg.replace("ffmpeg", "ffprobe"))
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(p)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0.0)
}

/// Segments écrits pendant la durée du buffer, à l'horloge. Après une relance de Clipper ou une pause de la
/// capture, les vieux segments restés dans le dossier n'entrent plus dans le clip (on clippait 3 min d'il y a
/// 20 min au lieu des 3 dernières).
fn recent(segs: Vec<PathBuf>, secs: u32, now: std::time::SystemTime) -> Vec<PathBuf> {
    let max = std::time::Duration::from_secs(u64::from(secs + 2 * SEG_SECS));
    segs.into_iter()
        .filter(|p| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() <= max))
        .collect()
}

/// REC aux mêmes réglages que le Clip : on garde les segments du buffer (zéro coût en plus).
/// Réglages différents : 2e capture dédiée, seulement pendant le REC.
pub fn rec_start(cfg: &Config, buf: &Buffer) -> Result<(), String> {
    if cfg.rec == cfg.clip {
        let n = segments(&buf.dir).last().map_or(0, |p| seg_number(p));
        *buf.rec_from.lock().unwrap() = Some(n);
        return Ok(());
    }
    let file = buf.dir.join(format!("rec-{}.{}", stamp(), seg_ext(&cfg.encoder)));
    let pipes = buf.audio.as_ref().map_or(vec![], |a| a.open_pipes(1, &crate::audio::tracks(cfg), 0));
    let mut a = capture_args(cfg, cfg.rec, buf.native, &pipes);
    // Un seul .ts : lisible même si le PC s'arrête en plein REC.
    let format = if seg_ext(&cfg.encoder) == "mkv" { "matroska" } else { "mpegts" };
    a.extend(["-f".into(), format.into(), file.to_string_lossy().into_owned()]);
    let mut cmd = ffmpeg_capture(cfg);
    if let Ok(f) = std::fs::File::create(buf.dir.join("ffmpeg-rec.log")) {
        cmd.stderr(f);
    }
    let child = cmd.stdin(Stdio::piped()).args(a).spawn().map_err(|e| format!("2e capture impossible : {e}"))?;
    *buf.rec2.lock().unwrap() = Some((child, file));
    Ok(())
}

/// Attend la fin du segment en cours pour ne rien couper, puis assemble.
pub fn rec_stop(cfg: &Config, buf: &Buffer) -> Result<PathBuf, String> {
    if let Some((mut child, file)) = buf.rec2.lock().unwrap().take() {
        // « q » = arrêt propre de ffmpeg (fin de fichier écrite) ; sinon arrêt forcé après 5 s.
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(b"q");
        }
        let t = std::time::Instant::now();
        while matches!(child.try_wait(), Ok(None)) && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        if let Some(a) = &buf.audio {
            a.close_slot(1);
        }
        let r = save(cfg, std::slice::from_ref(&file), "REC");
        if r.is_ok() {
            let _ = std::fs::remove_file(&file); // en cas d'échec, le .ts reste récupérable
        }
        return r;
    }
    let from = buf.rec_from.lock().unwrap().ok_or("Aucun REC en cours.")?;
    std::thread::sleep(Duration::from_millis(SEG_SECS as u64 * 1000 + 600));
    let segs: Vec<PathBuf> = segments(&buf.dir).into_iter().filter(|p| seg_number(p) >= from).collect();
    let r = save(cfg, &segs[..segs.len().saturating_sub(1)], "REC");
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

    #[test]
    fn bitrate_scales() {
        assert_eq!(bitrate_kbps(1080, 60, "h264_nvenc", 1.0), 28_000);
        assert_eq!(bitrate_kbps(1080, 30, "h264_nvenc", 1.0), 18_200);
        assert_eq!(bitrate_kbps(2160, 60, "hevc_nvenc", 1.0), 56_000);
        assert_eq!(bitrate_kbps(1080, 60, "h264_nvenc", 2.0), 56_000); // plafond du curseur
    }

    /// Vieux segments d'avant une relance (30 min) : jamais dans le clip, même s'il en manque pour 3 min.
    #[test]
    fn clip_ignores_segments_older_than_buffer() {
        let dir = std::env::temp_dir().join(format!("clipper-recent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now();
        let segs: Vec<PathBuf> = (0..10u64)
            .map(|i| {
                let p = dir.join(format!("{i:010}.ts"));
                let f = std::fs::File::create(&p).unwrap();
                let age = if i < 5 { 30 * 60 } else { (10 - i) * 2 };
                f.set_modified(now - std::time::Duration::from_secs(age)).unwrap();
                p
            })
            .collect();
        let kept = recent(segs, 180, now);
        assert_eq!(kept.len(), 5);
        assert!(kept.iter().all(|p| seg_number(p) >= 5));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pick_excludes_segment_in_progress() {
        let segs: Vec<PathBuf> = (0..100).map(|i| PathBuf::from(format!("{i:010}.ts"))).collect();
        let p = pick_segments(&segs, 60);
        assert_eq!(p.len(), 30);
        assert_eq!(p.last().unwrap(), &segs[98]);
        assert_eq!(pick_segments(&segs[..1], 60).len(), 0);
    }
}

#[cfg(test)]
mod live_fullscreen {
    #[test]
    #[ignore = "dépend de la fenêtre au premier plan"]
    fn report() {
        println!("jeu plein écran au premier plan : {}", super::fullscreen_foreground());
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
