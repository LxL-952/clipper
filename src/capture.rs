//! Buffer ffmpeg : ddagrab -> encodeur matériel -> segments .ts de 2 s.
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

/// Encodeurs qui encodent vraiment une image sur cette machine (pas seulement compilés).
pub fn probe_encoders(cfg: &Config) -> Vec<&'static str> {
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
        0..=1200 => 20_000.0,
        1201..=1600 => 35_000.0,
        _ => 70_000.0,
    };
    let fps_k = if fps <= 30 { 0.65 } else { 1.0 };
    let codec_k = if encoder.starts_with("hevc") || encoder.starts_with("av1") { 0.7 } else { 1.0 };
    (base * fps_k * codec_k * quality.clamp(0.5, 1.5)) as u32
}

/// Preset le plus léger compatible avec la qualité visée.
fn preset_args(encoder: &str, quality: f32) -> Vec<&'static str> {
    if encoder.ends_with("_nvenc") {
        // Mesuré à 1200p60 : p1 = 7 % NVENC, p3 = 14 %. À débit CBR égal, l'écart de qualité est faible.
        let p = if quality <= 1.0 { "p1" } else if quality <= 1.3 { "p2" } else { "p4" };
        vec!["-preset", p, "-tune", "ll", "-rc", "cbr"]
    } else if encoder.ends_with("_amf") {
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

/// Encodeur réel selon la puce choisie, parmi ceux qui marchent sur la machine.
pub fn resolve_encoder(cfg: &Config, ok: &[&'static str]) -> String {
    let want: &[&str] = match cfg.encode_chip.as_str() {
        "igpu" => &["h264_amf", "hevc_amf", "h264_qsv"],
        _ => &[],
    };
    want.iter()
        .chain(std::iter::once(&cfg.encoder.as_str()))
        .chain(["h264_nvenc", "h264_amf", "h264_qsv", "libx264"].iter())
        .find(|e| ok.contains(e))
        .map_or("libx264".into(), |e| e.to_string())
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
    (0..4)
        .map_while(|i| {
            let mut c = cfg.clone();
            c.monitor = i;
            native_size(&c).map(|(w, h)| serde_json::json!({"idx": i, "w": w, "h": h, "name": format!("Écran {}", i + 1)}))
        })
        .collect()
}

/// Durée en secondes (ffprobe), 0 si illisible.
pub fn duration(cfg: &Config, p: &Path) -> f64 {
    Command::new(cfg.ffmpeg.replace("ffmpeg", "ffprobe"))
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(p)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0.0)
}

pub fn buffer_args(cfg: &Config, native: (u32, u32), dir: &Path, start: u64, pipes: &[String]) -> Vec<String> {
    let mut a = capture_args(cfg, cfg.clip, native, pipes);
    for s in [
        "-f".into(), "segment".into(), "-segment_time".into(), SEG_SECS.to_string(),
        "-segment_format".into(), "mpegts".into(), "-segment_start_number".into(), start.to_string(),
        dir.join("%010d.ts").to_string_lossy().into_owned(),
    ] {
        a.push(s);
    }
    a
}

/// Entrées (écran + audio) et encodage pour la qualité `q`, sans la sortie.
fn capture_args(cfg: &Config, q: crate::config::Quality, native: (u32, u32), pipes: &[String]) -> Vec<String> {
    let (w, h) = final_size(cfg, q, native);
    let mut vf = format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", cfg.monitor, q.fps);
    // Les images de ddagrab vivent sur la carte NVIDIA : AMF/QSV/x264 doivent passer par la mémoire.
    let off_gpu = !cfg.encoder.ends_with("_nvenc");
    if (w, h) != native || off_gpu {
        // Seul cas où l'image quitte le GPU : redimensionnement CPU rapide (ou simple copie).
        vf += ",hwdownload,format=bgra";
        if (w, h) != native {
            vf += &format!(",scale={w}:{h}:flags=fast_bilinear");
        }
        vf += &format!(",format=nv12,fps={}", q.fps);
    }
    let kb = bitrate_kbps(h, q.fps, &cfg.encoder, cfg.quality);
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", &vf].iter().map(|s| s.to_string()).collect();
    a.extend(crate::audio::input_args(pipes));
    a.extend(crate::audio::output_args(pipes.len()));
    a.extend(["-c:v".into(), cfg.encoder.clone()]);
    a.extend(preset_args(&cfg.encoder, cfg.quality).iter().map(|s| s.to_string()));
    for s in [
        "-b:v".into(), format!("{kb}k"), "-maxrate".into(), format!("{kb}k"), "-bufsize".into(), format!("{kb}k"),
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
        .filter(|p| p.extension().is_some_and(|x| x == "ts") && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
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
                let next = segments(&d).last().map_or(0, |p| seg_number(p) + 1);
                let mut cmd = ffmpeg(&cfg);
                let _ = std::fs::rename(d.join("ffmpeg.log"), d.join("ffmpeg.prev.log"));
                spawns += 1;
                if let Ok(f) = std::fs::File::create(d.join("ffmpeg.log")) {
                    cmd.stderr(f); // dernière erreur gardée pour le diagnostic
                }
                let pipes = audio.as_ref().map_or(vec![], |a| a.open_pipes(0, crate::audio::pipe_count(&cfg), spawns));
                let args = buffer_args(&cfg, native, &d, next, &pipes);
                let _ = std::fs::write(d.join("ffmpeg-args.txt"), format!("pipes={pipes:?} audio={}
{}", audio.is_some(), args.join(" ")));
                match cmd.args(args).spawn() {
                    Ok(ch) => *c.lock().unwrap() = Some(ch),
                    Err(_) => {
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
                let mut tick = 0u32;
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
        });
        Self { dir, stop, child, rec_from, rec2: Mutex::new(None), audio: audio_keep, native }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut ch) = self.child.lock().unwrap().take() {
            let _ = ch.kill();
        }
    }
}

/// Assemble des segments sans réencodage (`-c copy`).
pub fn concat(cfg: &Config, segs: &[PathBuf], out: &Path) -> Result<(), String> {
    if segs.is_empty() {
        return Err("Le buffer est encore vide.".into());
    }
    let list = out.with_extension("txt");
    let body: String = segs.iter().map(|p| format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"))).collect();
    std::fs::write(&list, body).map_err(|e| e.to_string())?;
    let mut c = ffmpeg(cfg);
    c.args(["-loglevel", "error", "-f", "concat", "-safe", "0", "-i"]).arg(&list).args(["-map", "0", "-c", "copy"]);
    if cfg.container != "mkv" {
        c.args(["-movflags", "+faststart"]);
    }
    let ok = c.arg("-y").arg(out).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(&list);
    if ok { Ok(()) } else { Err("ffmpeg n'a pas pu assembler le clip.".into()) }
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
    let out = cfg.out_dir.join(format!("{kind} {}.{}", stamp(), cfg.container));
    concat(cfg, segs, &out)?;
    let thumbs = crate::config::dir().join("thumbs");
    let _ = std::fs::create_dir_all(&thumbs);
    let name = out.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let _ = ffmpeg(cfg)
        .args(["-loglevel", "quiet", "-ss", "1", "-i"]).arg(&out)
        .args(["-frames:v", "1", "-vf", "scale=320:-2", "-y"]).arg(thumbs.join(name + ".jpg"))
        .status();
    Ok(out)
}

pub fn save_clip(cfg: &Config, dir: &Path) -> Result<PathBuf, String> {
    let segs = segments(dir);
    save(cfg, pick_segments(&segs, cfg.buffer_secs), "Clip")
}

/// REC aux mêmes réglages que le Clip : on garde les segments du buffer (zéro coût en plus).
/// Réglages différents : 2e capture dédiée, seulement pendant le REC.
pub fn rec_start(cfg: &Config, buf: &Buffer) -> Result<(), String> {
    if cfg.rec == cfg.clip {
        let n = segments(&buf.dir).last().map_or(0, |p| seg_number(p));
        *buf.rec_from.lock().unwrap() = Some(n);
        return Ok(());
    }
    let file = buf.dir.join(format!("rec-{}.ts", stamp()));
    let pipes = buf.audio.as_ref().map_or(vec![], |a| a.open_pipes(1, crate::audio::pipe_count(cfg), 0));
    let mut a = capture_args(cfg, cfg.rec, buf.native, &pipes);
    // Un seul .ts : lisible même si le PC s'arrête en plein REC.
    a.extend(["-f".into(), "mpegts".into(), file.to_string_lossy().into_owned()]);
    let mut cmd = ffmpeg(cfg);
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
    fn bitrate_scales() {
        assert_eq!(bitrate_kbps(1080, 60, "h264_nvenc", 1.0), 20_000);
        assert_eq!(bitrate_kbps(1080, 30, "h264_nvenc", 1.0), 13_000);
        assert_eq!(bitrate_kbps(2160, 60, "hevc_nvenc", 1.0), 49_000);
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
