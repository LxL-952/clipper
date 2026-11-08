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
                for _ in 0..n {
                    let b = [bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap()];
                    let s = f32::from_le_bytes(b);
                    peak = peak.max(s.abs());
                    q.push_back(s);
                }
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
}

impl Audio {
    pub fn start(cfg: &Config) -> Arc<Self> {
        let a = Arc::new(Self { game: Arc::default(), mic: Arc::default(), slots: Arc::default(), stop: Arc::default() });
        capture(a.game.clone(), Direction::Render, cfg.out_device.clone(), a.stop.clone());
        capture(a.mic.clone(), Direction::Capture, cfg.mic_device.clone(), a.stop.clone());
        let (w, cfg) = (a.clone(), cfg.clone());
        std::thread::spawn(move || w.mix_loop(&cfg));
        a
    }

    /// Toutes les 20 ms : envoie exactement le nombre d'échantillons dû selon l'horloge.
    /// Les trous (silence, micro absent) sont comblés par des zéros : l'audio ne dérive jamais.
    /// Le même morceau part vers chaque ffmpeg connecté (buffer et, pendant un REC, la 2e capture).
    fn mix_loop(&self, cfg: &Config) {
        let (gv, mv) = (cfg.game_volume, if cfg.mic_muted { 0.0 } else { cfg.mic_volume });
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
            let (g, m) = (take(&self.game, gv), take(&self.mic, mv));
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
        a.extend(["-c:a", "aac", "-b:a", "160k"].map(String::from));
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
