//! Rendus hors écran de l'interface (tests) : chaque onglet est dessiné dans un fichier, sans fenêtre,
//! avec les mêmes données d'exemple que le mode démo de l'ancienne page — pour comparer les deux à l'œil.
//! `cargo test shots -- --ignored` ; images dans le dossier donné par CLIPPER_SHOTS.
use crate::front::{Front, Tab};
use crate::gfx::Canvas;
use crate::widgets::{Input, Ui, UiState};
use serde_json::{Value, json};
use std::sync::Arc;

fn demo() -> Value {
    json!({"page": "index", "tab": null,
        "config": {"buffer_secs": 180, "clip": {"height": 1080, "fps": 60}, "rec": {"height": 1080, "fps": 60}, "encoder": "h264_nvenc", "quality": 1.0,
            "container": "mp4", "audio_tracks": ["game", "mic"], "out_device": null, "mic_device": "{0.0.1.00000000}.{b2c1}", "game_volume": 1.0,
            "mic_volume": 0.8, "mic_muted": false, "key_clip": "Alt+KeyX", "key_rec": "Alt+KeyR", "key_bar": "Alt+KeyZ",
            "out_dir": "C:\\Users\\Kurero\\Videos\\Clipper", "ffmpeg": "ffmpeg", "driver_branch": "game_ready", "encode_chip": "auto", "monitor": 0,
            "stretch": "off", "desktop_capture": true, "buffer_dir": "C:\\Users\\Kurero\\AppData\\Local\\Temp\\Clipper\\buf", "overlay_mode": "hotkey",
            "key_overlay": "Alt+KeyF", "overlay_items": ["fps", "frametime", "cpu", "gpu"], "intro": false},
        "encoders": ["h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "libx264"], "native": [1920, 1200], "ffmpeg_ok": true,
        "gpus": [{"id": "nvidia", "name": "NVIDIA GeForce RTX 4060 Laptop GPU"}, {"id": "amd", "name": "AMD Radeon(TM) 780M"}],
        "monitors": [{"idx": 0, "w": 1920, "h": 1200, "name": "Écran 1 (interne)"}, {"idx": 1, "w": 1920, "h": 1080, "name": "Écran 2"}],
        "outputs": [{"id": "a1", "name": "Haut-parleurs (Realtek(R) Audio)"}, {"id": "a2", "name": "Casque (HyperX Cloud III S)"}],
        "inputs": [{"id": "{0.0.1.00000000}.{b2c1}", "name": "Micro (HyperX Cloud III S)"}, {"id": "b2", "name": "Réseau de micros (Realtek(R) Audio)"}],
        "hotkeys": {"clip": "Alt+X", "rec": "Ctrl+Alt+R", "bar": "Alt+Maj+Z", "overlay": "Alt+F"}, "hotkey_conflicts": ["Alt+R", "Alt+Z"],
        "recording": false, "rec_secs": 0, "gpu": {"name": "NVIDIA GeForce RTX 4060 Laptop GPU", "driver": "621.01"},
        "elevated": false, "gpu_scale": true, "cpu_temp": true, "autostart": false})
}

fn shot(f: &mut Front, name: &str, w: u32, h: u32, scale: f32, input: &Input) {
    let Ok(dir) = std::env::var("CLIPPER_SHOTS") else { return };
    let mut c = Canvas::new(w, h, scale);
    let mut st = UiState::default();
    let mut best = f64::MAX;
    for _ in 0..6 {
        let t = std::time::Instant::now();
        let mut ui = Ui { c: &mut c, i: input, s: &mut st, now: std::time::Instant::now() };
        f.frame(&mut ui);
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("dessin {name} {w}x{h} : {best:.2} ms");
    f.out.clear();
    let raw = std::path::Path::new(&dir).join(format!("{name}.bgra"));
    std::fs::write(&raw, c.pm.data()).unwrap();
    let ok = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", &format!("{w}x{h}"), "-i"])
        .arg(&raw)
        .arg(std::path::Path::new(&dir).join(format!("{name}.png")))
        .status()
        .is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(raw);
    assert!(ok, "image {name} non écrite");
}

#[test]
#[ignore = "écrit des images dans CLIPPER_SHOTS"]
fn every_tab() {
    let mut f = Front::new(&demo(), Arc::new(|| {}));
    f.free.insert(f.cfg.out_dir.clone(), 212 * 1_073_741_824);
    f.free.insert(f.cfg.buffer_dir.clone(), 58 * 1_073_741_824);
    let none = Input { mouse: (-1.0, -1.0), ..Default::default() };
    let open = std::env::var("CLIPPER_OPEN").is_ok();
    if open {
        for k in ["clip:cap", "clip:q", "clip:k", "clip:l", "clip:s", "clip:ui", "clip:adv", "adv:chip", "adv:str", "adv:lim", "aud:a", "aud:g", "aud:m", "tools:clean"] {
            f.folds.insert(k.into(), true);
        }
    } else {
        f.folds.clear();
    }
    let tall = if open { 3300 } else { 620 };
    f.tab = Tab::Clip;
    shot(&mut f, "clip", 900, tall, 1.0, &none);
    f.tab = Tab::Stats;
    shot(&mut f, "stats", 900, if open { 1500 } else { 1250 }, 1.0, &none);
    f.cfg.overlay_graphs = vec!["fps".into()];
    shot(&mut f, "stats-125", 1125, 775, 1.25, &none);
    f.tab = Tab::Advanced;
    shot(&mut f, "advanced", 900, if open { 1500 } else { 900 }, 1.0, &none);
    // Largeur minimale de la fenêtre, 125 % : sept onglets, rien ne se chevauche.
    f.update = Some("1.2.0".into());
    shot(&mut f, "advanced-min", 1050, 650, 1.25, &none);
    f.update = None;
    f.tab = Tab::Audio;
    f.levels = (0.55, 0.3);
    shot(&mut f, "audio", 900, if open { 1100 } else { 620 }, 1.0, &none);
    f.tab = Tab::Videos;
    let items: Vec<Value> = (0..7)
        .map(|i| json!({"path": format!("C:\\Users\\Kurero\\Videos\\Clipper\\Clip 2026-11-0{} 21-14-03.mp4", i + 1), "name": format!("Clip 2026-11-0{} 21-14-03.mp4", i + 1),
            "kind": if i == 2 { "REC" } else { "Clip" }, "bytes": 152_000_000u64 * (i + 1), "secs": 61.0 * (i as f64 + 1.0), "date": 1_794_000_000u64 + i * 86_400, "video": false, "audio": 1}))
        .collect();
    f.on(&json!({"type": "videos", "items": items}));
    shot(&mut f, "videos", 900, 620, 1.0, &none);
    f.tab = Tab::Drivers;
    f.on(&json!({"type": "driver", "installed": "621.01", "latest": "626.14", "branch": "game_ready", "status": "update"}));
    shot(&mut f, "drivers", 900, 620, 1.0, &none);
    f.tab = Tab::Tools;
    f.on(&json!({"type": "clean", "items": [
        {"id": "temp", "name": "Fichiers temporaires", "bytes": 1_840_000_000u64, "files": 4213, "admin": false, "default": true, "warn": ""},
        {"id": "wu", "name": "Cache de Windows Update", "bytes": 3_200_000_000u64, "files": 812, "admin": true, "default": false, "warn": "Windows retéléchargera les mises à jour en attente."}]}));
    f.on(&json!({"type": "ram", "free_before": 4_100_000_000u64, "free_after": 9_600_000_000u64, "total": 16_000_000_000u64}));
    f.on(&json!({"type": "toast", "kind": "clip", "text": "Clip enregistré · 3 min 00 s", "path": "C:\\Users\\Kurero\\Videos\\Clipper\\Clip.mp4"}));
    shot(&mut f, "tools", 900, 620, 1.0, &none);
    // Mise à l'échelle 150 % : mêmes proportions, texte net.
    f.tab = Tab::Clip;
    f.toasts.clear();
    shot(&mut f, "clip-150", 1350, 930, 1.5, &none);
}

#[test]
#[ignore = "écrit des images dans CLIPPER_SHOTS"]
fn bar() {
    let Ok(dir) = std::env::var("CLIPPER_SHOTS") else { return };
    let cfg: crate::config::Config = serde_json::from_value(demo()["config"].clone()).unwrap();
    let mut b = crate::barui::BarUi::new(&cfg, Arc::new(|| {}));
    b.on(&json!({"type": "show", "config": cfg, "hotkeys": demo()["hotkeys"], "native": [1920, 1200], "overlay_on": true, "recording": false, "rec_secs": 0}));
    b.on(&json!({"type": "recent", "items": [
        {"path": "C:\\a\\Clip 1.mp4", "name": "Clip 2026-11-07 21-14-03.mp4", "secs": 180.0, "video": true, "audio": 1},
        {"path": "C:\\a\\Clip 2.mp4", "name": "Clip 2026-11-06 20-02-41.mp4", "secs": 61.0, "video": true, "audio": 1}]}));
    b.on(&json!({"type": "toast", "kind": "clip", "text": "Clip enregistré · 3 min 00 s", "path": "C:\\a\\Clip 1.mp4"}));
    std::thread::sleep(std::time::Duration::from_millis(400)); // animation d'entrée terminée
    let (w, h) = (1040u32, 420u32);
    let mut c = Canvas::new(w, h, 1.0);
    let mut st = UiState::default();
    let input = Input { mouse: (-1.0, -1.0), ..Default::default() };
    let mut ui = Ui { c: &mut c, i: &input, s: &mut st, now: std::time::Instant::now() };
    b.frame(&mut ui);
    // Fond gris moyen sous la transparence, pour voir les bords.
    let mut px = c.pm.data().to_vec();
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for ch in &mut p[..3] {
            *ch = (*ch as u32 + 96 * (255 - a) / 255).min(255) as u8;
        }
        p[3] = 255;
    }
    let raw = std::path::Path::new(&dir).join("bar.bgra");
    std::fs::write(&raw, px).unwrap();
    let ok = std::process::Command::new("ffmpeg").args(["-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", &format!("{w}x{h}"), "-i"]).arg(&raw).arg(std::path::Path::new(&dir).join("bar.png")).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(raw);
    assert!(ok);
}

/// Le lecteur reçoit-il les images au rythme du clip ? Lit 4 s d'un vrai fichier (CLIPPER_CLIP) et compte.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_rate() {
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let n = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let n2 = n.clone();
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(move || { n2.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }));
    p.set_size(594.0, 334.0);
    p.open(std::path::Path::new(&clip), 60.0, 1);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    println!("image fixe reçue : {}", p.has_frame());
    n.store(0, std::sync::atomic::Ordering::Relaxed);
    p.toggle();
    let mut last = 0;
    for s in 1..=4 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let c = n.load(std::sync::atomic::Ordering::Relaxed);
        println!("seconde {s} : {} images, position {:.1} s", c - last, p.pos());
        last = c;
    }
    p.close();
}

/// Délais du lecteur sur un vrai clip (CLIPPER_CLIP) : clic sur un clip -> 1re image, lecture -> mouvement,
/// saut pendant la lecture, saut à l'arrêt, glissement sur la barre (images vues pendant le geste).
/// `tick` est appelé toutes les 2 ms, comme le ferait l'interface.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_latency() {
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::time::{Duration, Instant};
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let n = Arc::new(AtomicU32::new(0));
    let n2 = n.clone();
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(move || { n2.fetch_add(1, Relaxed); }));
    p.set_size(384.0, 216.0);
    let wait = |p: &mut crate::player::Player, ok: &dyn Fn(&crate::player::Player, u32) -> bool| -> u128 {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            p.tick();
            if ok(p, n.load(Relaxed)) {
                return t.elapsed().as_millis();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        9999
    };
    for round in 0..3 {
        p.open(std::path::Path::new(&clip), 48.0, 3);
        let n0 = n.load(Relaxed);
        let first = wait(&mut p, &|_, c| c > n0);
        p.toggle();
        let play = wait(&mut p, &|p, _| p.pos() > 0.001);
        std::thread::sleep(Duration::from_millis(500));
        p.seek(20.0);
        let n1 = n.load(Relaxed);
        let seek_img = wait(&mut p, &|_, c| c > n1);
        let seek_play = wait(&mut p, &|p, _| p.pos() > 20.001);
        std::thread::sleep(Duration::from_millis(300));
        p.pause();
        std::thread::sleep(Duration::from_millis(100));
        let n2 = n.load(Relaxed);
        p.seek(30.0);
        let seek_paused = wait(&mut p, &|_, c| c > n2);
        // Glissement de 5 s à 35 s en 1 s (une position toutes les 16 ms), puis lâcher en lecture.
        p.toggle();
        std::thread::sleep(Duration::from_millis(400));
        let n3 = n.load(Relaxed);
        let t = Instant::now();
        let mut i = 0;
        while t.elapsed() < Duration::from_secs(1) {
            p.scrub(5.0 + 30.0 * t.elapsed().as_secs_f64());
            p.tick();
            i += 1;
            std::thread::sleep(Duration::from_millis(16));
        }
        let seen = n.load(Relaxed) - n3;
        p.scrub_end();
        let end = p.pos();
        let release = wait(&mut p, &|p, _| p.pos() > end + 0.001);
        println!("tour {round} : 1re image {first} ms, lecture {play} ms, saut en lecture : image {seek_img} ms puis mouvement {seek_play} ms, saut à l'arrêt {seek_paused} ms, glissement : {seen} images sur {i} positions, reprise au lâcher {release} ms");
        p.close();
    }
}

/// cuda refusé (carte NVIDIA absente ou pilote trop ancien) : la lecture repart seule en d3d11va.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_fallback() {
    use std::time::{Duration, Instant};
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    crate::player::BREAK_CUDA.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
    p.set_size(384.0, 216.0);
    p.open(std::path::Path::new(&clip), 48.0, 0);
    p.toggle();
    let t = Instant::now();
    while p.pos() < 0.001 && t.elapsed() < Duration::from_secs(5) {
        p.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("décodeur {} (2 = d3d11va), mouvement après {} ms", crate::player::decoder(), t.elapsed().as_millis());
    assert_eq!(crate::player::decoder(), 2);
    assert!(p.pos() > 0.0);
}

/// Lecture de 5 s au rythme réel (taille CLIPPER_SIZE, ex. 384x216) : le processeur des ffmpeg est relevé
/// à côté (scratchpad/cpu.ps1).
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_cpu() {
    use std::time::{Duration, Instant};
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let (w, h) = std::env::var("CLIPPER_SIZE").ok().and_then(|s| s.split_once('x').map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))).unwrap_or((384.0, 216.0));
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
    p.set_size(w, h);
    p.open(std::path::Path::new(&clip), 48.0, 3);
    std::thread::sleep(Duration::from_millis(300));
    p.toggle();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        p.tick();
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("position {:.2} s", p.pos());
    std::thread::sleep(Duration::from_millis(300)); // relevé final avant l'arrêt
    p.close();
}
