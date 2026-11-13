//! Audio : WASAPI loopback (son du jeu) + micro -> PCM f32 48 kHz stéréo -> named pipes vers ffmpeg.
use crate::config::Config;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasapi::{Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

pub const RATE: usize = 48_000;
const CH: usize = 2;
/// Au-delà, on jette le plus ancien : la RAM ne grossit jamais (0,5 s max par source).
const MAX_QUEUE: usize = RATE * CH / 2;
/// Réserve avant de puiser dans une source (60 ms) : absorbe le décalage entre lecture et mélange,
/// sans trous dans le son. Reconstituée après chaque coupure (silence du loopback).
const CUSHION: usize = RATE * CH * 60 / 1000;
/// Réveils de capture et de mélange : 50 par seconde au lieu d'un par période audio (~100).
const TICK: Duration = Duration::from_millis(20);

#[derive(Default)]
pub struct Source {
    queue: Mutex<VecDeque<f32>>,
    /// Réserve atteinte : le mélangeur puise. Remis à faux dès que la file se vide.
    primed: AtomicBool,
    /// Crête du dernier paquet, 0..1 en bits f32 : pour le vu-mètre de l'UI.
    pub peak: AtomicU32,
}

/// (id, nom) des périphériques actifs.
pub fn devices(dir: Direction) -> Vec<(String, String)> {
    // COM en MTA dans un fil à part : le fil principal doit rester STA (fenêtres, glisser-déposer).
    std::thread::spawn(move || devices_mta(dir)).join().unwrap_or_default()
}

fn devices_mta(dir: Direction) -> Vec<(String, String)> {
    let _ = wasapi::initialize_mta();
    let Ok(e) = DeviceEnumerator::new() else { return vec![] };
    let Ok(col) = e.get_device_collection(dir) else { return vec![] };
    (0..col.get_nbr_devices().unwrap_or(0))
        .filter_map(|i| col.get_device_at_index(i).ok())
        .filter_map(|d| Some((d.get_id().ok()?, d.get_friendlyname().ok()?)))
        .collect()
}

fn find(dir: Direction, id: &Option<String>) -> Option<Device> {
    let e = DeviceEnumerator::new().ok()?;
    if let Some(id) = id {
        let col = e.get_device_collection(dir).ok()?;
        for i in 0..col.get_nbr_devices().ok()? {
            let d = col.get_device_at_index(i).ok()?;
            if d.get_id().ok().as_ref() == Some(id) {
                return Some(d);
            }
        }
    }
    e.get_default_device(dir).ok()
}

/// Capture continue d'un périphérique. `loopback` = périphérique de sortie écouté (son du jeu).
fn capture(src: Arc<Source>, dir: Direction, id: Option<String>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || while !stop.load(Ordering::Relaxed) {
        let _ = wasapi::initialize_mta();
        let res = (|| -> Result<(), Box<dyn std::error::Error>> {
            let dev = find(dir, &id).ok_or("périphérique introuvable")?;
            let mut client = dev.get_iaudioclient()?;
            // autoconvert : Windows convertit en 48 kHz f32 stéréo, aucun rééchantillonnage chez nous.
            let fmt = WaveFormat::new(32, 32, SampleType::Float, RATE, CH, None);
            let (def, _) = client.get_device_period()?;
            // Interrogation toutes les 20 ms ; tampon de 10 périodes pour ne rien perdre entre deux lectures.
            let mode = StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: def * 10 };
            client.initialize_client(&fmt, Direction::Capture, mode, None)?;
            let cap = client.get_audiocaptureclient()?;
            client.start_stream()?;
            let mut bytes = VecDeque::new();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                // Tous les paquets arrivés depuis la dernière lecture (aucun si le loopback est silencieux).
                loop {
                    let before = bytes.len();
                    cap.read_from_device_to_deque(&mut bytes)?;
                    if bytes.len() == before {
                        break;
                    }
                }
                let n = bytes.len() / 4;
                let mut peak = 0f32;
                let mut q = src.queue.lock().unwrap();
                // Par blocs de 4 octets d'une zone contiguë (avant : 4 pop_front par échantillon, ~400 000 par
                // seconde et par source pendant le jeu). Un reste incomplet attend la lecture suivante.
                for c in bytes.make_contiguous()[..n * 4].chunks_exact(4) {
                    let s = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    peak = peak.max(s.abs());
                    q.push_back(s);
                }
                bytes.drain(..n * 4);
                let over = q.len().saturating_sub(MAX_QUEUE);
                q.drain(..over);
                drop(q);
                if n > 0 {
                    src.peak.store(peak.to_bits(), Ordering::Relaxed);
                }
            }
            Ok(())
        })();
        if res.is_err() {
            // Périphérique débranché ou changé : on réessaie (rechargement automatique).
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

pub struct Audio {
    pub game: Arc<Source>,
    pub mic: Arc<Source>,
    /// Pipes par instance ffmpeg : slot 0 = buffer, slot 1 = REC aux réglages différents.
    /// Chaque slot reçoit un pipe par piste activée : (piste, pipe).
    slots: Arc<Mutex<[Vec<(Track, File)>; 2]>>,
    stop: Arc<AtomicBool>,
    /// Volume du PC, gain du micro (volume x boost, 0 = coupé) et boost, en bits f32 : changés en direct par
    /// `levels` (avant : chaque changement relançait ffmpeg, ~1 s de trou dans le buffer).
    gains: [AtomicU32; 3],
}

impl Audio {
    pub fn start(cfg: &Config) -> Arc<Self> {
        let a = Arc::new(Self { game: Arc::default(), mic: Arc::default(), slots: Arc::default(), stop: Arc::default(), gains: Default::default() });
        a.levels(cfg);
        capture(a.game.clone(), Direction::Render, cfg.out_device.clone(), a.stop.clone());
        capture(a.mic.clone(), Direction::Capture, cfg.mic_device.clone(), a.stop.clone());
        let (w, cfg) = (a.clone(), cfg.clone());
        std::thread::spawn(move || w.mix_loop(&cfg));
        a
    }

    /// Sourdine, volumes et boost du micro, appliqués au prochain tour du mélangeur (20 ms).
    pub fn levels(&self, cfg: &Config) {
        // Boost du micro en dB, en plus du volume (+6 dB = x2, +20 dB = x10).
        let boost = 10f32.powf(cfg.mic_boost_db.clamp(0.0, 30.0) / 20.0);
        let mv = if cfg.mic_muted { 0.0 } else { cfg.mic_volume * boost };
        for (g, v) in self.gains.iter().zip([cfg.game_volume, mv, boost]) {
            g.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// Toutes les 20 ms : envoie exactement le nombre d'échantillons dû selon l'horloge.
    /// Les trous (silence, micro absent) sont comblés par des zéros : l'audio ne dérive jamais.
    /// Le même morceau part vers chaque ffmpeg connecté (buffer et, pendant un REC, la 2e capture).
    fn mix_loop(&self, cfg: &Config) {
        // Seulement si le micro finit dans un clip (piste Tout ou Micro) : sinon aucun calcul.
        let mic_recorded = tracks(cfg).iter().any(|t| matches!(t, Track::Mix | Track::Mic));
        // Créé même micro coupé (la sourdine se lève en direct) ; coupé, il ne calcule rien (voir plus bas).
        let mut denoiser = (cfg.mic_denoise && mic_recorded).then(Denoiser::new);
        let epoch = Instant::now();
        let mut sent = 0usize;
        while !self.stop.load(Ordering::Relaxed) {
            std::thread::sleep(TICK);
            let due = (epoch.elapsed().as_secs_f64() * RATE as f64) as usize * CH;
            let n = due.saturating_sub(sent);
            sent = due;
            if n == 0 {
                continue;
            }
            let [gv, mv, boost] = self.gains.each_ref().map(|g| f32::from_bits(g.load(Ordering::Relaxed)));
            let take = |s: &Source, g: f32| -> Vec<f32> {
                let mut q = s.queue.lock().unwrap();
                if !s.primed.load(Ordering::Relaxed) {
                    if q.len() < CUSHION {
                        return vec![0.0; n];
                    }
                    s.primed.store(true, Ordering::Relaxed);
                }
                if q.len() < n {
                    s.primed.store(false, Ordering::Relaxed); // coupure : on reconstitue la réserve
                }
                let k = n.min(q.len());
                let mut v: Vec<f32> = q.drain(..k).map(|x| x * g).collect();
                v.resize(n, 0.0);
                v
            };
            let g = take(&self.game, gv);
            // Réduction de bruit : sur le micro brut (RNNoise attend des niveaux naturels), puis volume et boost.
            let mut m = match denoiser.as_mut() {
                Some(d) if mv > 0.0 => {
                    let mut m = take(&self.mic, 1.0);
                    d.process(&mut m);
                    m.iter_mut().for_each(|s| *s *= mv);
                    m
                }
                _ => take(&self.mic, mv),
            };
            if boost > 1.0 {
                m.iter_mut().for_each(|s| *s = soft_limit(*s));
            }
            let bytes = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|x| x.to_le_bytes()).collect() };
            let mut slots = self.slots.lock().unwrap();
            if slots.iter().all(Vec::is_empty) {
                continue;
            }
            let mix = bytes(&g.iter().zip(&m).map(|(a, b)| (a + b).clamp(-1.0, 1.0)).collect::<Vec<_>>());
            let (gb, mb) = (bytes(&g), bytes(&m));
            for pipes in slots.iter_mut() {
                let ok = pipes.iter_mut().all(|(t, f)| {
                    f.write_all(match t {
                        Track::Mix => &mix,
                        Track::Game => &gb,
                        Track::Mic => &mb,
                    })
                    .is_ok()
                });
                if !ok {
                    pipes.clear(); // ffmpeg arrêté : on attend les pipes de la prochaine instance
                }
            }
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        *self.slots.lock().unwrap() = Default::default();
    }

    pub fn close_slot(&self, slot: usize) {
        self.slots.lock().unwrap()[slot].clear();
    }

    /// Crée les pipes d'une nouvelle instance ffmpeg dans `slot`. Renvoie les chemins à passer en `-i`.
    pub fn open_pipes(self: &Arc<Self>, slot: usize, tracks: &[Track], tag: u64) -> Vec<String> {
        let count = tracks.len();
        // Numéro unique au processus : après une relance de capture, l'ancien pipe peut encore exister
        // (fil bloqué dans ConnectNamedPipe) et un nom réutilisé faisait échouer CreateNamedPipeW (buffer sans son).
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        self.slots.lock().unwrap()[slot].clear();
        let names: Vec<String> = (0..count).map(|i| format!(r"\\.\pipe\clipper-{}-{slot}-{tag}-{seq}-{i}", std::process::id())).collect();
        let mut files = vec![];
        for n in &names {
            match create_pipe(n) {
                Some(h) => files.push((tracks[files.len()], unsafe { File::from_raw_handle(h as _) })),
                None => return vec![],
            }
        }
        let a = self.clone();
        // ConnectNamedPipe bloque jusqu'à ce que ffmpeg ouvre le pipe : dans un fil à part.
        std::thread::spawn(move || {
            use std::os::windows::io::AsRawHandle;
            for (_, f) in &files {
                unsafe { windows_sys::Win32::System::Pipes::ConnectNamedPipe(f.as_raw_handle() as _, std::ptr::null_mut()) };
            }
            a.slots.lock().unwrap()[slot] = files;
        });
        names
    }
}

/// Réduction de bruit du micro : RNNoise (réseau de neurones de Xiph, celui des outils de visio),
/// en Rust pur. Travaille en mono par trames de 10 ms ; 10 ms de retard ajoutés, constant.
struct Denoiser {
    st: Box<nnnoiseless::DenoiseState<'static>>,
    pending: Vec<f32>,
    ready: VecDeque<f32>,
}

impl Denoiser {
    const FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

    fn new() -> Self {
        // Une trame de silence d'avance : il y a toujours de quoi rendre autant d'échantillons que reçus.
        Self { st: nnnoiseless::DenoiseState::new(), pending: Vec::with_capacity(Self::FRAME * 2), ready: VecDeque::from(vec![0.0; Self::FRAME]) }
    }

    /// `buf` : stéréo entrelacé, débruité sur place (même son dans les deux canaux, comme un micro).
    fn process(&mut self, buf: &mut [f32]) {
        // RNNoise attend des valeurs à l'échelle 16 bits.
        self.pending.extend(buf.chunks_exact(CH).map(|f| (f[0] + f[1]) * 0.5 * 32767.0));
        let mut out = [0f32; Self::FRAME];
        let mut done = 0;
        while self.pending.len() - done >= Self::FRAME {
            let frame = &self.pending[done..done + Self::FRAME];
            // Silence complet (micro absent, coupé par Windows) : rien à débruiter, aucun calcul.
            if frame.iter().all(|s| s.abs() < 1.0) {
                self.ready.extend(std::iter::repeat_n(0.0, Self::FRAME));
            } else {
                self.st.process_frame(&mut out, frame);
                self.ready.extend(out.iter().map(|s| s / 32767.0));
            }
            done += Self::FRAME;
        }
        self.pending.drain(..done);
        for f in buf.chunks_exact_mut(CH) {
            let s = self.ready.pop_front().unwrap_or(0.0);
            f.fill(s);
        }
    }
}

/// Limiteur doux pour le micro boosté : intact jusqu'à 0,9, puis les crêtes sont arrondies vers 1,0
/// au lieu d'être écrêtées net (pas de grésillement quand on crie).
fn soft_limit(s: f32) -> f32 {
    const KNEE: f32 = 0.9;
    let a = s.abs();
    if a <= KNEE { s } else { s.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh()) }
}

fn create_pipe(name: &str) -> Option<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_OUTBOUND;
    use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
    let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let h = unsafe { CreateNamedPipeW(w.as_ptr(), PIPE_ACCESS_OUTBOUND, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 1 << 18, 0, 0, std::ptr::null()) };
    (h != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE).then_some(h)
}

/// Arguments ffmpeg des entrées audio (à placer après l'entrée vidéo).
pub fn input_args(pipes: &[String]) -> Vec<String> {
    pipes
        .iter()
        .flat_map(|p| ["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", p].map(String::from))
        .collect()
}

/// Arguments de sortie audio : mapping + AAC 160 kb/s + nom de chaque piste (visible au montage).
pub fn output_args(tracks: &[Track]) -> Vec<String> {
    let mut a: Vec<String> = vec!["-map".into(), "0:v".into()];
    for i in 1..=tracks.len() {
        a.extend(["-map".into(), format!("{i}:a")]);
    }
    if !tracks.is_empty() {
        // Codeur AAC « fast » au lieu de « twoloop » (défaut) : nettement moins de processeur pour 3 pistes
        // encodées en continu, sans différence audible à 160 kb/s.
        a.extend(["-c:a", "aac", "-aac_coder", "fast", "-b:a", "160k"].map(String::from));
    }
    for (i, t) in tracks.iter().enumerate() {
        a.extend([format!("-metadata:s:a:{i}"), format!("title={}", t.title())]);
    }
    a
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Track {
    /// Tout le son : PC + micro mélangés.
    Mix,
    /// Son du PC seul.
    Game,
    /// Micro seul.
    Mic,
}

impl Track {
    pub fn title(self) -> &'static str {
        match self {
            Track::Mix => "Tout",
            Track::Game => "Environnement",
            Track::Mic => "Micro",
        }
    }
}

/// Pistes activées, toujours dans l'ordre Tout, PC, Micro.
pub fn tracks(cfg: &Config) -> Vec<Track> {
    [("mix", Track::Mix), ("game", Track::Game), ("mic", Track::Mic)]
        .into_iter()
        .filter(|(k, _)| cfg.audio_tracks.iter().any(|t| t == k))
        .map(|(_, t)| t)
        .collect()
}

pub fn pipe_count(cfg: &Config) -> usize {
    tracks(cfg).len()
}

#[cfg(test)]
mod boost {
    #[test]
    fn soft_limit_keeps_quiet_samples_and_never_clips() {
        assert_eq!(super::soft_limit(0.5), 0.5);
        assert_eq!(super::soft_limit(-0.9), -0.9);
        for s in [1.0f32, 2.0, 10.0, -5.0] {
            let y = super::soft_limit(s);
            assert!(y.abs() <= 1.0 && y.abs() > 0.9 && y.signum() == s.signum());
        }
    }
}

#[cfg(test)]
mod denoise {
    /// Bruit de fond grave (ventilateur, ronflement : bruit « brun ») fortement atténué, et autant
    /// d'échantillons en sortie qu'en entrée. (Le bruit blanc synthétique passe : RNNoise le prend pour une voix.)
    #[test]
    fn rnnoise_removes_background_noise_and_keeps_length() {
        let mut d = super::Denoiser::new();
        let (mut seed, mut brown) = (1u32, 0f32);
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let white = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            brown = 0.98 * brown + 0.02 * white;
            brown * 0.3
        };
        let (mut e_in, mut e_out) = (0f64, 0f64);
        for tick in 0..150 {
            // 20 ms stéréo par tour, comme le mélangeur
            let mut buf: Vec<f32> = (0..960).flat_map(|_| { let s = noise(); [s, s] }).collect();
            let before: f64 = buf.iter().map(|s| (*s as f64).powi(2)).sum();
            d.process(&mut buf);
            assert_eq!(buf.len(), 1920);
            if tick >= 100 {
                e_in += before;
                e_out += buf.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
            }
        }
        let db = 10.0 * (e_out / e_in).log10();
        assert!(db < -15.0, "réduction seulement {db:.1} dB");
    }
}

