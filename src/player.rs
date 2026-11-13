//! Lecteur vidéo natif : ffmpeg décode le clip (H.264, HEVC, AV1, MP4 / MKV / MOV, sans extension Windows
//! ni copie de lecture) et envoie des images brutes déjà à la taille du lecteur dans un tube ; le son part
//! d'un second ffmpeg vers WASAPI. Pause, saut, vitesse : les deux ffmpeg sont relancés à la position voulue.
//!
//! Deux sortes de décodage :
//! - image fixe (ouverture d'un clip, saut, glissement sur la barre) : décodage logiciel d'une seule image,
//!   la plus rapide à sortir (80 à 130 ms). Une seule à la fois : pendant un glissement, la suivante part
//!   dès que la précédente est affichée, vers le dernier point demandé (avant, chaque nouvelle position
//!   tuait la précédente avant qu'elle ait fini : aucune image pendant tout le geste) ;
//! - lecture : décodage et mise à l'échelle sur la carte NVIDIA (cuda) quand il y en a une, sinon d3d11va.
use crate::gfx::{Canvas, R};
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Le processus de Clipper tourne sous la normale (le jeu passe avant) et ses enfants en héritent : le
/// décodage du lecteur, lui, sert l'utilisateur qui regarde, il garde la priorité normale.
const NORMAL_PRIORITY: u32 = 0x0000_0020;
/// Cadence des images envoyées par ffmpeg pendant la lecture (clips à 30 ou 60 images/s).
const PLAY_FPS: f64 = 60.0;
/// Fondu de la miniature vers la 1re vraie image d'un clip (la miniature est prise à 1 s, en 320 px :
/// sans fondu, l'image « sautait » à l'arrivée de la vidéo).
const FADE: Duration = Duration::from_millis(160);

/// Décodeur de la lecture : 0 pas encore choisi, 1 cuda (carte NVIDIA), 2 d3d11va.
static DECODER: AtomicU8 = AtomicU8::new(0);
#[cfg(test)]
pub static BREAK_CUDA: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub fn decoder() -> u8 {
    DECODER.load(Ordering::Relaxed)
}

/// Lecture sur la carte NVIDIA : décodage NVDEC et réduction à la taille du lecteur par la carte. Mesuré sur
/// un clip 1080p60 (5 s) : 0,43 s de processeur au lieu de 1,35 s (petit lecteur), 0,67 s au lieu de 3,57 s
/// (lecteur de la barre). Avant, d3d11va décodait sur la carte mais la réduction Lanczos se faisait sur le
/// processeur : autant de calcul qu'un décodage logiciel.
fn use_cuda() -> bool {
    if DECODER.load(Ordering::Relaxed) == 0 {
        let nv = crate::capture::gpus().iter().any(|g| g.id == "nvidia");
        let _ = DECODER.compare_exchange(0, if nv { 1 } else { 2 }, Ordering::Relaxed, Ordering::Relaxed);
    }
    DECODER.load(Ordering::Relaxed) == 1
}

struct Frame {
    w: u32,
    h: u32,
    data: Vec<u8>,
    /// Image venue de la lecture (et non d'une image fixe), pour le point visé n° `epoch`.
    play: bool,
    epoch: u64,
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
    /// Position au lancement des ffmpeg en cours, et instant où la première image est arrivée : le son
    /// attend cet instant pour partir, et la position n'avance qu'à partir de lui (son et image calés).
    base: f64,
    t0: Arc<Mutex<Option<Instant>>>,
    /// Taille d'image demandée à ffmpeg (pixels physiques, paire).
    size: (u32, u32),
    /// Numéro de lancement : les fils d'un ancien lancement s'arrêtent d'eux-mêmes.
    run: Arc<AtomicU64>,
    /// Numéro du clip ouvert : une image fixe d'un clip refermé n'est jamais affichée.
    opened: Arc<AtomicU64>,
    /// Numéro du point visé (ouverture, saut) : une image fixe arrivée après les premières images de la
    /// lecture au même point est ignorée (pas de retour en arrière).
    epoch: Arc<AtomicU64>,
    frame: Arc<Mutex<Option<Frame>>>,
    /// Arrivée de la première image du clip ouvert (fondu depuis la miniature).
    shown: Arc<Mutex<Option<Instant>>>,
    ended: Arc<AtomicBool>,
    /// La lecture n'a sorti aucune image : décodeur peut-être refusé par la carte (repli d3d11va).
    failed: Arc<AtomicBool>,
    kids: Vec<Child>,
    /// Image fixe en cours de décodage (une seule à la fois) et sa fin, signalée par son fil (un drapeau par
    /// image : celui d'une image tuée ne doit pas marquer la suivante comme finie).
    still: Option<(Child, Instant, Arc<AtomicBool>)>,
    /// Prochaine image fixe demandée : seul le dernier point compte.
    want: Option<f64>,
    /// Lecture à relancer (saut, vitesse, taille) : regroupé, un lancement toutes les 120 ms au plus.
    dirty: bool,
    last_spawn: Instant,
    /// Glissement en cours sur la barre de temps.
    scrubbing: bool,
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
            t0: Arc::new(Mutex::new(None)),
            size: (640, 360),
            run: Arc::new(AtomicU64::new(0)),
            opened: Arc::new(AtomicU64::new(0)),
            epoch: Arc::new(AtomicU64::new(0)),
            frame: Arc::new(Mutex::new(None)),
            shown: Arc::new(Mutex::new(None)),
            ended: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            kids: vec![],
            still: None,
            want: None,
            dirty: false,
            last_spawn: Instant::now(),
            scrubbing: false,
        }
    }

    /// Ouvre un clip, à l'arrêt sur sa première image. `audio_tracks` : pistes à mélanger (0 = muet).
    pub fn open(&mut self, file: &Path, dur: f64, audio_tracks: usize) {
        self.reset();
        self.file = Some(file.to_path_buf());
        (self.dur, self.audio_tracks, self.base) = (dur, audio_tracks, 0.0);
        self.want = Some(0.0);
        self.tick(); // image fixe lancée tout de suite, sans attendre le prochain dessin
    }

    /// Ferme le clip et libère l'image (onglet quitté, barre fermée, vidéo supprimée).
    pub fn close(&mut self) {
        self.reset();
        self.file = None;
    }

    fn reset(&mut self) {
        self.kill();
        self.kill_still();
        self.opened.fetch_add(1, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Relaxed);
        (self.playing, self.dirty, self.scrubbing, self.want) = (false, false, false, None);
        *self.frame.lock().unwrap() = None;
        *self.shown.lock().unwrap() = None;
    }

    #[cfg(test)]
    pub fn has_frame(&self) -> bool {
        self.frame.lock().unwrap().is_some()
    }

    /// Position de lecture, en secondes.
    pub fn pos(&self) -> f64 {
        let p = match (self.playing, *self.t0.lock().unwrap()) {
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
            // Pause : on garde l'image affichée et on arrête ffmpeg, sans redécoder une image fixe (elle
            // tombait un peu à côté : l'image sautait en arrière puis revenait).
            self.base = self.pos();
            self.playing = false;
            self.dirty = false;
            self.kill();
            return;
        }
        if self.dur > 0.0 && self.base >= self.dur - 0.05 {
            self.base = 0.0; // fin atteinte : on repart du début
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
        self.playing = true;
        if !self.scrubbing {
            self.spawn();
        }
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.toggle();
        }
    }

    /// Saute à `t` secondes. La barre montre le point visé tout de suite ; une image fixe y est décodée
    /// (~100 ms), puis la lecture reprend si elle était en cours. Sauts rapprochés : seul le dernier compte.
    pub fn seek(&mut self, t: f64) {
        self.base = t.clamp(0.0, self.dur.max(0.0));
        // Position figée sur le point visé jusqu'à la première image du nouveau lancement.
        *self.t0.lock().unwrap() = None;
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.want = Some(self.base);
        if self.playing {
            self.kill(); // plus aucune image de l'ancien point (sinon l'image repartait en arrière)
            self.dirty = true;
        }
    }

    /// Glissement sur la barre de temps : images fixes seules (sans son, sans lecture) jusqu'au lâcher.
    pub fn scrub(&mut self, t: f64) {
        self.scrubbing = true;
        self.seek(t);
        self.dirty = false;
    }

    /// Fin du glissement : la lecture (si elle était en cours) reprend au point lâché, sans attendre.
    pub fn scrub_end(&mut self) {
        self.scrubbing = false;
        if self.playing {
            self.dirty = true;
            self.last_spawn = Instant::now() - Duration::from_secs(1);
        }
    }

    pub fn set_rate(&mut self, rate: f32) {
        self.base = self.pos();
        self.rate = rate;
        self.dirty = self.playing;
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
                if self.playing {
                    self.dirty = true;
                } else {
                    self.want = Some(self.base); // image fixe refaite à la nouvelle taille
                }
            }
        }
    }

    /// Opacité de l'image vidéo pendant le fondu depuis la miniature (1 : fondu fini ou sans objet).
    pub fn fade(&self) -> f32 {
        self.shown.lock().unwrap().map_or(1.0, |t| (t.elapsed().as_secs_f32() / FADE.as_secs_f32()).min(1.0))
    }

    /// À appeler à chaque dessin : lance les changements en attente, détecte la fin. Vrai : redessiner bientôt.
    pub fn tick(&mut self) -> bool {
        if self.ended.swap(false, Ordering::Relaxed) && self.playing {
            self.playing = false;
            self.base = self.dur;
        }
        if self.failed.swap(false, Ordering::Relaxed) && self.playing {
            // Aucune image loin de la fin : cuda refusé (pilote, format du clip). Repli sur d3d11va.
            if DECODER.swap(2, Ordering::Relaxed) == 1 {
                crate::log("lecteur : décodage cuda refusé, repli sur d3d11va");
                self.dirty = true;
                self.last_spawn = Instant::now() - Duration::from_secs(1);
            }
        }
        // Image fixe finie (ou bloquée plus de 2 s) : place à la suivante, vers le dernier point demandé.
        if self.still.as_ref().is_some_and(|(_, t, done)| done.load(Ordering::Relaxed) || t.elapsed() > Duration::from_secs(2)) {
            self.kill_still();
        }
        if self.still.is_none() {
            if let Some(t) = self.want.take() {
                self.spawn_still(t);
            }
        }
        if self.dirty && !self.playing {
            self.dirty = false;
        }
        if self.dirty && !self.scrubbing && self.last_spawn.elapsed() > Duration::from_millis(120) {
            self.spawn();
        }
        self.dirty || self.playing || self.still.is_some() || self.want.is_some() || self.fade() < 1.0
    }

    fn kill(&mut self) {
        self.run.fetch_add(1, Ordering::Relaxed);
        for mut k in self.kids.drain(..) {
            let _ = k.kill();
            let _ = k.wait();
        }
    }

    fn kill_still(&mut self) {
        if let Some((mut k, _, _)) = self.still.take() {
            let _ = k.kill();
            let _ = k.wait();
        }
    }

    /// Filtre commun : image entière dans le cadre, bandes noires autour si le format diffère.
    fn pad(&self) -> String {
        let (w, h) = self.size;
        format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=bgra")
    }

    /// Une seule image au point `t`, décodée par le processeur (sortie la plus rapide).
    fn spawn_still(&mut self, t: f64) {
        let Some(file) = self.file.clone() else { return };
        let (w, h) = self.size;
        let mut c = Command::new(&self.ffmpeg);
        // Lanczos : réduction la plus nette (bicubique laissait l'aperçu un peu flou).
        let vf = format!("scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos+accurate_rnd,{}", self.pad());
        c.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY).args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"]).arg(&file);
        c.args(["-map", "0:v:0", "-an", "-sn", "-frames:v", "1", "-vf", &vf, "-f", "rawvideo", "-"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let Ok(mut child) = c.spawn() else { return };
        let done = Arc::new(AtomicBool::new(false));
        if let Some(mut out) = child.stdout.take() {
            let (frame, shown, wake, done) = (self.frame.clone(), self.shown.clone(), self.wake.clone(), done.clone());
            let (opened, epoch) = (self.opened.clone(), self.epoch.clone());
            let my_open = opened.load(Ordering::Relaxed);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; (w * h * 4) as usize];
                if out.read_exact(&mut buf).is_ok() {
                    let mut f = frame.lock().unwrap();
                    let now = epoch.load(Ordering::Relaxed);
                    // Clip refermé (vérifié sous le verrou), ou point déjà montré par la lecture : image ignorée.
                    if opened.load(Ordering::Relaxed) == my_open && !f.as_ref().is_some_and(|f| f.play && f.epoch == now) {
                        if f.is_none() {
                            *shown.lock().unwrap() = Some(Instant::now());
                        }
                        *f = Some(Frame { w, h, data: buf, play: false, epoch: now });
                    }
                }
                done.store(true, Ordering::Relaxed);
                wake();
            });
        }
        self.still = Some((child, Instant::now(), done));
    }

    /// (Re)lance la lecture à la position `base` : images au rythme du clip, et le son.
    fn spawn(&mut self) {
        self.kill();
        self.dirty = false;
        self.last_spawn = Instant::now();
        self.ended.store(false, Ordering::Relaxed);
        *self.t0.lock().unwrap() = None;
        let Some(file) = self.file.clone() else { return };
        let run = self.run.load(Ordering::Relaxed);
        let (w, h) = self.size;
        let mut c = Command::new(&self.ffmpeg);
        // Pas de -readrate : il réglait aussi le rythme des images décodées avant le point visé (jusqu'à
        // une image clé, 2 s plus tôt), d'où une image très en retard sur le son après un saut. ffmpeg
        // décode au plus vite, le fil de lecture ci-dessous cale chaque image sur l'horloge.
        c.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY).args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", self.base)]);
        let cuda = use_cuda();
        // En lecture, 60 images/s fixes (cadence du fil).
        let vf = if cuda {
            c.args(["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]);
            #[cfg(test)]
            if BREAK_CUDA.load(Ordering::Relaxed) {
                c.args(["-hwaccel_device", "9"]); // carte inexistante : cuda refusé, comme sur un PC sans NVIDIA
            }
            format!("fps={PLAY_FPS},scale_cuda={w}:{h}:interp_algo=lanczos:force_original_aspect_ratio=decrease:force_divisible_by=2:format=yuv420p,hwdownload,format=yuv420p,{}", self.pad())
        } else {
            // Autres cartes : décodage d3d11va, réduction Lanczos par le processeur.
            c.args(["-hwaccel", "d3d11va"]);
            format!("fps={PLAY_FPS},scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,{}", self.pad())
        };
        c.arg("-i").arg(&file).args(["-map", "0:v:0", "-an", "-sn"]);
        let step = Duration::from_secs_f64(1.0 / (PLAY_FPS * self.rate.max(0.1) as f64));
        c.args(["-vf", &vf, "-f", "rawvideo", "-"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Ok(mut child) = c.spawn() {
            if let Some(mut out) = child.stdout.take() {
                let (cur, frame, ended, wake, t0) = (self.run.clone(), self.frame.clone(), self.ended.clone(), self.wake.clone(), self.t0.clone());
                let (shown, failed, epoch) = (self.shown.clone(), self.failed.clone(), self.epoch.load(Ordering::Relaxed));
                let near_end = self.dur > 0.0 && self.base > self.dur - 0.5;
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; (w * h * 4) as usize];
                    let mut i = 0u32;
                    while out.read_exact(&mut buf).is_ok() {
                        if cur.load(Ordering::Relaxed) != run {
                            return;
                        }
                        // Image n à t0 + n images : la première fixe t0 (et libère le son).
                        let start = *t0.lock().unwrap().get_or_insert_with(Instant::now);
                        let due = start + step * i;
                        i += 1;
                        if let Some(wait) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(wait);
                        }
                        let mut f = frame.lock().unwrap();
                        // Vérifié sous le verrou : `reset` change de numéro avant de vider l'image, une image
                        // de l'ancien clip ne peut plus arriver juste après le changement de clip.
                        if cur.load(Ordering::Relaxed) != run {
                            return;
                        }
                        if f.is_none() {
                            *shown.lock().unwrap() = Some(Instant::now());
                        }
                        match f.as_mut().filter(|f| f.w == w && f.h == h) {
                            Some(f) => {
                                f.data.copy_from_slice(&buf);
                                (f.play, f.epoch) = (true, epoch);
                            }
                            None => *f = Some(Frame { w, h, data: buf.clone(), play: true, epoch }),
                        }
                        drop(f);
                        wake();
                    }
                    if cur.load(Ordering::Relaxed) == run {
                        if i == 0 && !near_end {
                            failed.store(true, Ordering::Relaxed);
                        } else {
                            ended.store(true, Ordering::Relaxed);
                        }
                        wake();
                    }
                });
            }
            self.kids.push(child);
        }
        if self.audio_tracks > 0 {
            self.spawn_audio(&file, run);
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
            let (cur, gain, t0) = (self.run.clone(), self.gain.clone(), self.t0.clone());
            std::thread::spawn(move || {
                let _ = play_pcm(out, &cur, run, &gain, &t0);
            });
        }
        self.kids.push(child);
    }

    /// Dessine l'image en cours dans `dst`, avec l'opacité `alpha` (fondu d'arrivée). Faux : pas encore d'image.
    /// ffmpeg livre l'image à la taille du lecteur : elle est recopiée telle quelle (aucun filtrage).
    /// Taille différente (changement en cours, plein écran sur grand écran) : mise à l'échelle.
    pub fn draw(&self, c: &mut Canvas, dst: R, alpha: f32) -> bool {
        let f = self.frame.lock().unwrap();
        let Some(f) = f.as_ref() else { return false };
        let (pw, ph) = ((dst.w * c.s).round() as i64, (dst.h * c.s).round() as i64);
        if (f.w as i64 - pw).abs() <= 2 && (f.h as i64 - ph).abs() <= 2 {
            if alpha < 1.0 {
                c.blit_alpha(&f.data, f.w, f.h, dst.x, dst.y, alpha);
            } else {
                c.blit(&f.data, f.w, f.h, dst.x, dst.y);
            }
            return true;
        }
        // Plein écran (image limitée à 1920x1080 par le tube) : agrandie sans filtrage, bandes noires déjà
        // dans l'image. Les coins arrondis du cadre sont repeints par l'appelant.
        c.blit_scaled(&f.data, f.w, f.h, dst);
        true
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.kill();
        self.kill_still();
    }
}

/// Envoie le son décodé (f32, 48 kHz, stéréo) à la sortie par défaut de Windows, au rythme de la carte son.
/// Le tampon est rempli d'avance, mais la lecture ne démarre qu'avec la première image (`t0`) : sans ça, le
/// son partait avant l'image (démarrage du décodage vidéo, saut jusqu'au point visé).
fn play_pcm(mut out: impl Read, cur: &AtomicU64, run: u64, gain: &AtomicU32, t0: &Mutex<Option<Instant>>) -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};
    let _ = wasapi::initialize_mta();
    let dev = DeviceEnumerator::new()?.get_default_device(Direction::Render)?;
    let mut client = dev.get_iaudioclient()?;
    let fmt = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
    // Tampon de 100 ms : assez pour ne pas hacher, assez court pour que pause et saut soient immédiats.
    client.initialize_client(&fmt, Direction::Render, StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: 1_000_000 }, None)?;
    let render = client.get_audiorenderclient()?;
    let mut buf = vec![0u8; 8 * 4800];
    let mut started = false;
    while cur.load(Ordering::Relaxed) == run {
        if !started && t0.lock().unwrap().is_some() {
            client.start_stream()?;
            started = true;
        }
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
    if started {
        let _ = client.stop_stream();
    }
    Ok(())
}
