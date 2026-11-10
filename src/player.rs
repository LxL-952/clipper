//! Lecteur vidéo natif : ffmpeg décode le clip (H.264, HEVC, AV1, MP4 / MKV / MOV, sans extension Windows
//! ni copie de lecture) et envoie des images brutes déjà à la taille du lecteur dans un tube ; le son part
//! d'un second ffmpeg vers WASAPI. Pause, saut, vitesse : les deux ffmpeg sont relancés à la position voulue.
use crate::gfx::{Canvas, R};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Le processus de Clipper tourne sous la normale (le jeu passe avant) et ses enfants en héritent : le
/// décodage du lecteur, lui, sert l'utilisateur qui regarde, il garde la priorité normale.
const NORMAL_PRIORITY: u32 = 0x0000_0020;

/// Images reçues de ffmpeg (diagnostic : comparé au nombre de dessins dans le journal).
pub static FRAMES: AtomicU32 = AtomicU32::new(0);

struct Frame {
    w: u32,
    h: u32,
    data: Vec<u8>,
}

pub struct Player {
    ffmpeg: String,
    /// Demande un nouveau dessin de la fenêtre (image arrivée, fin de lecture).
    wake: Arc<dyn Fn() + Send + Sync>,
    pub file: Option<PathBuf>,
    pub dur: f64,
    audio_tracks: usize,
    pub playing: bool,
    pub rate: f32,
    pub vol: f32,
    pub muted: bool,
    gain: Arc<AtomicU32>,
    /// Position au lancement des ffmpeg en cours, et instant de ce lancement.
    base: f64,
    t0: Option<Instant>,
    /// Taille d'image demandée à ffmpeg (pixels physiques, paire).
    size: (u32, u32),
    /// Numéro de lancement : les fils d'un ancien lancement s'arrêtent d'eux-mêmes.
    run: Arc<AtomicU64>,
    frame: Arc<Mutex<Option<Frame>>>,
    ended: Arc<AtomicBool>,
    kids: Vec<Child>,
    /// Saut ou changement de taille en attente (regroupés : un lancement toutes les 120 ms au plus).
    dirty: bool,
    last_spawn: Instant,
}

impl Player {
    pub fn new(ffmpeg: &str, wake: Arc<dyn Fn() + Send + Sync>) -> Player {
        Player {
            ffmpeg: ffmpeg.into(),
            wake,
            file: None,
            dur: 0.0,
            audio_tracks: 0,
            playing: false,
            rate: 1.0,
            vol: 1.0,
            muted: false,
            gain: Arc::new(AtomicU32::new(1f32.to_bits())),
            base: 0.0,
            t0: None,
            size: (640, 360),
            run: Arc::new(AtomicU64::new(0)),
            frame: Arc::new(Mutex::new(None)),
            ended: Arc::new(AtomicBool::new(false)),
            kids: vec![],
            dirty: false,
            last_spawn: Instant::now(),
        }
    }

    /// Ouvre un clip, à l'arrêt sur sa première image. `audio_tracks` : pistes à mélanger (0 = muet).
    pub fn open(&mut self, file: &Path, dur: f64, audio_tracks: usize) {
        self.kill();
        self.file = Some(file.to_path_buf());
        (self.dur, self.audio_tracks, self.base, self.playing) = (dur, audio_tracks, 0.0, false);
        *self.frame.lock().unwrap() = None;
        self.spawn();
    }

    /// Ferme le clip et libère l'image (onglet quitté, barre fermée, vidéo supprimée).
    pub fn close(&mut self) {
        self.kill();
        self.file = None;
        self.playing = false;
        *self.frame.lock().unwrap() = None;
    }

    pub fn has_frame(&self) -> bool {
        self.frame.lock().unwrap().is_some()
    }

    /// Position de lecture, en secondes.
    pub fn pos(&self) -> f64 {
        let p = match (self.playing, self.t0) {
            (true, Some(t)) => self.base + t.elapsed().as_secs_f64() * self.rate as f64,
            _ => self.base,
        };
        p.clamp(0.0, self.dur.max(0.0))
    }

    pub fn toggle(&mut self) {
        if self.file.is_none() {
            return;
        }
        if self.playing {
            self.base = self.pos();
            self.playing = false;
        } else {
            if self.dur > 0.0 && self.base >= self.dur - 0.05 {
                self.base = 0.0; // fin atteinte : on repart du début
            }
            self.playing = true;
        }
        self.spawn();
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.toggle();
        }
    }

    /// Saute à `t` secondes (pendant un glissement : regroupé, voir `tick`).
    pub fn seek(&mut self, t: f64) {
        self.base = t.clamp(0.0, self.dur.max(0.0));
        if self.playing {
            self.t0 = Some(Instant::now());
        }
        self.dirty = true;
    }

    pub fn set_rate(&mut self, rate: f32) {
        self.base = self.pos();
        self.rate = rate;
        self.dirty = true;
    }

    pub fn set_volume(&mut self, vol: f32, muted: bool) {
        (self.vol, self.muted) = (vol.clamp(0.0, 1.0), muted);
        self.gain.store(if muted { 0.0 } else { self.vol }.to_bits(), Ordering::Relaxed);
    }

    /// Taille du lecteur en pixels physiques : ffmpeg livre des images à cette taille exacte.
    pub fn set_size(&mut self, w: f32, h: f32) {
        // Au plus 1920x1080 par le tube (plein écran sur un grand écran : l'image est agrandie à l'affichage).
        let k = (1920.0 / w).min(1080.0 / h).min(1.0);
        let s = (((w * k) as u32).max(16) & !1, ((h * k) as u32).max(16) & !1);
        if s != self.size {
            self.size = s;
            if self.file.is_some() {
                self.base = self.pos();
                self.dirty = true;
            }
        }
    }

    /// À appeler à chaque dessin : lance les changements en attente, détecte la fin. Vrai : redessiner bientôt.
    pub fn tick(&mut self) -> bool {
        if self.ended.swap(false, Ordering::Relaxed) && self.playing {
            self.playing = false;
            self.base = self.dur;
        }
        if self.dirty && self.last_spawn.elapsed() > Duration::from_millis(120) {
            self.spawn();
        }
        self.dirty || self.playing
    }

    fn kill(&mut self) {
        self.run.fetch_add(1, Ordering::Relaxed);
        for mut k in self.kids.drain(..) {
            let _ = k.kill();
            let _ = k.wait();
        }
    }

    /// (Re)lance ffmpeg à la position `base` : lecture si `playing`, sinon une seule image.
    fn spawn(&mut self) {
        self.kill();
        self.dirty = false;
        self.last_spawn = Instant::now();
        self.ended.store(false, Ordering::Relaxed);
        let Some(file) = self.file.clone() else { return };
        let run = self.run.load(Ordering::Relaxed);
        let (w, h) = self.size;
        let mut c = Command::new(&self.ffmpeg);
        c.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY).args(["-hide_banner", "-loglevel", "error", "-hwaccel", "auto", "-ss", &format!("{:.3}", self.base)]);
        if self.playing {
            c.args(["-readrate", &self.rate.to_string()]);
            // Sans avance initiale : par défaut ffmpeg lit d'abord 0,5 s d'un coup, l'image partait donc
            // avec une demi-seconde d'avance sur le son.
            if no_burst(&self.ffmpeg) {
                c.args(["-readrate_initial_burst", "0.001"]);
            }
        }
        c.arg("-i").arg(&file).args(["-map", "0:v:0", "-an", "-sn"]);
        if !self.playing {
            c.args(["-frames:v", "1"]);
        }
        // Image entière dans le cadre, bandes noires autour si le format diffère.
        let vf = format!("scale={w}:{h}:force_original_aspect_ratio=decrease:flags=fast_bilinear,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=bgra");
        c.args(["-vf", &vf, "-f", "rawvideo", "-"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Ok(mut child) = c.spawn() {
            if let Some(mut out) = child.stdout.take() {
                let (cur, frame, ended, wake, playing) = (self.run.clone(), self.frame.clone(), self.ended.clone(), self.wake.clone(), self.playing);
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; (w * h * 4) as usize];
                    while out.read_exact(&mut buf).is_ok() {
                        if cur.load(Ordering::Relaxed) != run {
                            return;
                        }
                        let mut f = frame.lock().unwrap();
                        match f.as_mut().filter(|f| f.w == w && f.h == h) {
                            Some(f) => f.data.copy_from_slice(&buf),
                            None => *f = Some(Frame { w, h, data: buf.clone() }),
                        }
                        drop(f);
                        FRAMES.fetch_add(1, Ordering::Relaxed);
                        wake();
                    }
                    if playing && cur.load(Ordering::Relaxed) == run {
                        ended.store(true, Ordering::Relaxed);
                        wake();
                    }
                });
            }
            self.kids.push(child);
        }
        if self.playing {
            self.t0 = Some(Instant::now());
            if self.audio_tracks > 0 {
                self.spawn_audio(&file, run);
            }
        } else {
            self.t0 = None;
        }
    }

    fn spawn_audio(&mut self, file: &Path, run: u64) {
        // atempo accepte 0,5 à 100 : 0,25 = deux fois 0,5.
        let tempo = match self.rate {
            x if (x - 1.0).abs() < 0.01 => String::new(),
            x if x < 0.5 => ",atempo=0.5,atempo=0.5".into(),
            x => format!(",atempo={x}"),
        };
        let mut c = Command::new(&self.ffmpeg);
        c.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY).args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", self.base), "-i"]).arg(file);
        let n = self.audio_tracks;
        let ins: String = (0..n).map(|i| format!("[0:a:{i}]")).collect();
        c.args(["-filter_complex", &format!("{ins}amix=inputs={n}:normalize=0,aresample=48000{tempo}[a]"), "-map", "[a]", "-vn", "-sn"]);
        c.args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = c.spawn() else { return };
        if let Some(out) = child.stdout.take() {
            let (cur, gain) = (self.run.clone(), self.gain.clone());
            std::thread::spawn(move || {
                let _ = play_pcm(out, &cur, run, &gain);
            });
        }
        self.kids.push(child);
    }

    /// Dessine l'image en cours dans `dst`. Faux : pas encore d'image.
    /// ffmpeg livre l'image à la taille du lecteur : elle est recopiée telle quelle (aucun filtrage).
    /// Taille différente (changement en cours, plein écran sur grand écran) : mise à l'échelle.
    pub fn draw(&self, c: &mut Canvas, dst: R, rad: f32) -> bool {
        let f = self.frame.lock().unwrap();
        let Some(f) = f.as_ref() else { return false };
        let (pw, ph) = ((dst.w * c.s).round() as i64, (dst.h * c.s).round() as i64);
        if (f.w as i64 - pw).abs() <= 2 && (f.h as i64 - ph).abs() <= 2 {
            c.blit(&f.data, f.w, f.h, dst.x, dst.y);
            return true;
        }
        // Plein écran (image limitée à 1920x1080 par le tube) : agrandie sans filtrage, bandes noires déjà
        // dans l'image. Les coins arrondis du cadre sont repeints par l'appelant.
        c.blit_scaled(&f.data, f.w, f.h, dst);
        let _ = rad;
        true
    }
}

/// Ce ffmpeg connaît-il `-readrate_initial_burst` (versions 6.1 et suivantes) ? Essai unique, gardé.
fn no_burst(ffmpeg: &str) -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        Command::new(ffmpeg)
            .creation_flags(CREATE_NO_WINDOW)
            .args(["-v", "quiet", "-readrate", "1", "-readrate_initial_burst", "0.001", "-f", "lavfi", "-i", "color=s=16x16:d=0.02", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

impl Drop for Player {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Envoie le son décodé (f32, 48 kHz, stéréo) à la sortie par défaut de Windows, au rythme de la carte son.
fn play_pcm(mut out: impl Read, cur: &AtomicU64, run: u64, gain: &AtomicU32) -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};
    let _ = wasapi::initialize_mta();
    let dev = DeviceEnumerator::new()?.get_default_device(Direction::Render)?;
    let mut client = dev.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
    // Tampon de 100 ms : assez pour ne pas hacher, assez court pour que pause et saut soient immédiats.
    client.initialize_client(&fmt, Direction::Render, StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: 1_000_000 }, None)?;
    let render = client.get_audiorenderclient()?;
    client.start_stream()?;
    let mut buf = vec![0u8; 8 * 4800];
    while cur.load(Ordering::Relaxed) == run {
        let free = client.get_available_space_in_frames()? as usize;
        if free < 480 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let n = (free * 8).min(buf.len());
        if out.read_exact(&mut buf[..n]).is_err() {
            break; // fin du clip
        }
        let g = f32::from_bits(gain.load(Ordering::Relaxed));
        if (g - 1.0).abs() > 0.001 {
            for s in buf[..n].chunks_exact_mut(4) {
                let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]) * g;
                s.copy_from_slice(&v.to_le_bytes());
            }
        }
        render.write_to_device(&buf[..n], None)?;
    }
    let _ = client.stop_stream();
    Ok(())
}
