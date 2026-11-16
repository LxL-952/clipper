//! Rendus hors Ã©cran de l'interface (tests) : chaque onglet est dessinÃ© dans un fichier, sans fenÃªtre,
//! avec les mÃªmes donnÃ©es d'exemple que le mode dÃ©mo de l'ancienne page â€” pour comparer les deux Ã  l'Å“il.
//! `cargo test shots -- --ignored` ; images dans le dossier donnÃ© par CLIPPER_SHOTS.
use crate::front::{Front, Tab};
use crate::gfx::Canvas;
use crate::widgets::{Input, Ui, UiState};
use serde_json::{Value, json};
use std::sync::Arc;

fn demo() -> Value {
    json!({"page": "index", "tab": null,
        "config": {"buffer_secs": 180, "clip": {"height": 1080, "fps": 60}, "encoder": "h264_nvenc", "quality": 1.0,
            "container": "mp4", "audio_tracks": ["game", "mic"], "out_device": null, "mic_device": "{0.0.1.00000000}.{b2c1}", "game_volume": 1.0,
            "mic_volume": 0.8, "mic_muted": false, "key_clip": "Alt+KeyX", "key_rec": "Alt+KeyR", "key_bar": "Alt+KeyZ",
            "out_dir": "C:\\Users\\Joueur\\Videos\\Clipper", "ffmpeg": "ffmpeg", "driver_branch": "game_ready", "encode_chip": "auto", "monitor": 0,
            "stretch": "off", "desktop_capture": true, "buffer_dir": "C:\\Users\\Joueur\\AppData\\Local\\Temp\\Clipper\\buf", "overlay_mode": "hotkey",
            "key_overlay": "Alt+KeyF", "overlay_items": ["fps", "frametime", "cpu", "gpu"], "intro": false},
        "encoders": ["h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "libx264"], "native": [1920, 1200], "ffmpeg_ok": true,
        "gpus": [{"id": "nvidia", "name": "NVIDIA GeForce RTX 4060 Laptop GPU"}, {"id": "amd", "name": "AMD Radeon(TM) 780M"}],
        "monitors": [{"idx": 0, "w": 1920, "h": 1200, "name": "Ã‰cran 1 (interne)"}, {"idx": 1, "w": 1920, "h": 1080, "name": "Ã‰cran 2"}],
        "outputs": [{"id": "a1", "name": "Haut-parleurs (Realtek(R) Audio)"}, {"id": "a2", "name": "Casque (HyperX Cloud III S)"}],
        "inputs": [{"id": "{0.0.1.00000000}.{b2c1}", "name": "Micro (HyperX Cloud III S)"}, {"id": "b2", "name": "RÃ©seau de micros (Realtek(R) Audio)"}],
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
    assert!(ok, "image {name} non Ã©crite");
}

#[test]
#[ignore = "Ã©crit des images dans CLIPPER_SHOTS"]
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
    // Largeur minimale de la fenÃªtre, 125 % : sept onglets, rien ne se chevauche.
    shot(&mut f, "advanced-min", 1050, 650, 1.25, &none);
    // Nouvelle version : page imposÃ©e, Ã  la taille minimale et en grand, puis avec un Ã©chec rÃ©seau.
    f.update = Some("1.3.0".into());
    shot(&mut f, "update-min", 1050, 650, 1.25, &none);
    f.upd_err = Some("Download failed. Check your connection and try again.".into());
    shot(&mut f, "update-err", 1280, 800, 1.0, &none);
    (f.update, f.upd_err) = (None, None);
    f.tab = Tab::Audio;
    f.levels = (0.55, 0.3);
    shot(&mut f, "audio", 900, if open { 1100 } else { 620 }, 1.0, &none);
    f.tab = Tab::Videos;
    let items: Vec<Value> = (0..7)
        .map(|i| json!({"path": format!("C:\\Users\\Joueur\\Videos\\Clipper\\Clip 2026-11-0{} 21-14-03.mp4", i + 1), "name": format!("Clip 2026-11-0{} 21-14-03.mp4", i + 1),
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
        {"id": "wu", "name": "Cache de Windows Update", "bytes": 3_200_000_000u64, "files": 812, "admin": true, "default": false, "warn": "Windows retÃ©lÃ©chargera les mises Ã  jour en attente."}]}));
    f.on(&json!({"type": "ram", "free_before": 4_100_000_000u64, "free_after": 9_600_000_000u64, "total": 16_000_000_000u64}));
    f.on(&json!({"type": "toast", "kind": "clip", "text": "Clip enregistrÃ© Â· 3 min 00 s", "path": "C:\\Users\\Joueur\\Videos\\Clipper\\Clip.mp4"}));
    shot(&mut f, "tools", 900, 620, 1.0, &none);
    // Mise Ã  l'Ã©chelle 150 % : mÃªmes proportions, texte net.
    f.tab = Tab::Clip;
    f.toasts.clear();
    shot(&mut f, "clip-150", 1350, 930, 1.5, &none);
}

#[test]
#[ignore = "Ã©crit des images dans CLIPPER_SHOTS"]
fn bar() {
    let Ok(dir) = std::env::var("CLIPPER_SHOTS") else { return };
    let cfg: crate::config::Config = serde_json::from_value(demo()["config"].clone()).unwrap();
    let mut b = crate::barui::BarUi::new(&cfg, Arc::new(|| {}));
    b.on(&json!({"type": "show", "config": cfg, "hotkeys": demo()["hotkeys"], "native": [1920, 1200], "overlay_on": true, "recording": false, "rec_secs": 0}));
    b.on(&json!({"type": "recent", "items": [
        {"path": "C:\\a\\Clip 1.mp4", "name": "Clip 2026-11-07 21-14-03.mp4", "secs": 180.0, "video": true, "audio": 1},
        {"path": "C:\\a\\Clip 2.mp4", "name": "Clip 2026-11-06 20-02-41.mp4", "secs": 61.0, "video": true, "audio": 1}]}));
    b.on(&json!({"type": "toast", "kind": "clip", "text": "Clip enregistrÃ© Â· 3 min 00 s", "path": "C:\\a\\Clip 1.mp4"}));
    std::thread::sleep(std::time::Duration::from_millis(400)); // animation d'entrÃ©e terminÃ©e
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

/// Le lecteur reÃ§oit-il les images au rythme du clip ? Lit 4 s d'un vrai fichier (CLIPPER_CLIP) et compte.
/// Son prêt 1,5 s après l'image : il doit être recalé (journal « son prêt … recalé »), pas joué en retard.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_late_audio() {
    use std::sync::atomic::Ordering::Relaxed;
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let log = crate::config::dir().join("log.txt");
    let before = std::fs::read_to_string(&log).unwrap_or_default().len();
    crate::player::AUDIO_DELAY_MS.store(1500, Relaxed); // après la 1re image, même avec cuda (~0,9 s)
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
    p.set_size(384.0, 216.0);
    p.open_playing(std::path::Path::new(&clip), 48.0, 3);
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_millis(3000) {
        p.tick();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    p.close();
    crate::player::AUDIO_DELAY_MS.store(0, Relaxed);
    let all = std::fs::read_to_string(&log).unwrap_or_default();
    let new = &all[before.min(all.len())..];
    println!("{}", new.lines().filter(|l| l.contains("son prêt")).collect::<Vec<_>>().join("\n"));
    assert!(new.contains("son prêt"), "son en retard non recalé");
}

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
    println!("image fixe reÃ§ue : {}", p.has_frame());
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

/// DÃ©lais du lecteur sur un vrai clip (CLIPPER_CLIP) : clic sur un clip -> 1re image, lecture -> mouvement,
/// saut pendant la lecture, saut Ã  l'arrÃªt, glissement sur la barre (images vues pendant le geste).
/// `tick` est appelÃ© toutes les 2 ms, comme le ferait l'interface.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_latency() {
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use std::time::{Duration, Instant};
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let n = Arc::new(AtomicU32::new(0));
    let n2 = n.clone();
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(move || { n2.fetch_add(1, Relaxed); }));
    let (w, h) = std::env::var("CLIPPER_SIZE").ok().and_then(|s| s.split_once('x').map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))).unwrap_or((384.0, 216.0));
    p.set_size(w, h);
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
        // Clic sur un clip de la barre : lecture directe, sans image fixe.
        p.open_playing(std::path::Path::new(&clip), 48.0, 3);
        let direct = wait(&mut p, &|p, _| p.pos() > 0.001);
        println!("tour {round} : clic dans la barre, mouvement après {direct} ms");
        p.close();
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
        // Glissement de 5 s Ã  35 s en 1 s (une position toutes les 16 ms), puis lÃ¢cher en lecture.
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
        println!("tour {round} : 1re image {first} ms, lecture {play} ms, saut en lecture : image {seek_img} ms puis mouvement {seek_play} ms, saut Ã  l'arrÃªt {seek_paused} ms, glissement : {seen} images sur {i} positions, reprise au lÃ¢cher {release} ms");
        p.close();
    }
}

/// Lecture de 5 s au rythme rÃ©el (taille CLIPPER_SIZE, ex. 384x216) : le processeur des ffmpeg est relevÃ©
/// Ã  cÃ´tÃ© (scratchpad/cpu.ps1).
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
    std::thread::sleep(Duration::from_millis(300)); // relevÃ© final avant l'arrÃªt
    p.close();
}

/// Temps passÃ© dans le fil de la fenÃªtre par chaque geste du lecteur (CLIPPER_CLIP) : ouvrir, lire, sauter,
/// mettre en pause. Tout ce qui dÃ©passe quelques ms se voit comme une saccade de l'interface.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_ui_block() {
    use std::time::{Duration, Instant};
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
    p.set_size(1152.0, 648.0);
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    for round in 0..3 {
        let t = Instant::now();
        p.open(std::path::Path::new(&clip), 48.0, 3);
        let open = ms(t);
        std::thread::sleep(Duration::from_millis(300));
        let t = Instant::now();
        p.toggle();
        p.tick();
        let play = ms(t);
        std::thread::sleep(Duration::from_millis(600));
        let t = Instant::now();
        p.seek(20.0);
        p.tick();
        let seek = ms(t);
        std::thread::sleep(Duration::from_millis(200));
        let t = Instant::now();
        p.tick();
        let respawn = ms(t);
        std::thread::sleep(Duration::from_millis(600));
        let t = Instant::now();
        p.pause();
        let pause = ms(t);
        let t = Instant::now();
        p.close();
        let close = ms(t);
        println!("tour {round} : ouvrir {open:.1} ms, lire {play:.1} ms, sauter {seek:.1} ms, relance {respawn:.1} ms, pause {pause:.1} ms, fermer {close:.1} ms");
    }
}

/// Commandes typÃ©es (cmd.rs) envoyÃ©es par la fenÃªtre, sans fenÃªtre ni ffmpeg : onglet VidÃ©os, rÃ©glages.
#[test]
fn front_sends_typed_commands() {
    use crate::cmd::Cmd;
    let mut f = Front::new(&demo(), Arc::new(|| {}));
    f.out.clear();
    f.select_tab(Tab::Videos);
    assert_eq!(f.out, vec![Cmd::Videos]);
    f.out.clear();
    f.set("clip.fps", json!(30));
    f.flush();
    let mut want = serde_json::Map::new();
    want.insert("clip.fps".into(), json!(30));
    assert_eq!(f.out, vec![Cmd::SetMany(want)]);
}

/// Supprimer ou renommer un clip juste aprÃ¨s avoir fermÃ© le lecteur (CLIPPER_CLIP copiÃ© Ã  part) : ffmpeg,
/// arrÃªtÃ© sans attendre sa fin, ne doit plus tenir le fichier.
#[test]
#[ignore = "lit un vrai clip avec ffmpeg"]
fn player_close_releases_file() {
    use std::time::Duration;
    let Ok(clip) = std::env::var("CLIPPER_CLIP") else { return };
    let dir = std::env::temp_dir().join("clipper-close-test");
    let _ = std::fs::create_dir_all(&dir);
    for (round, rename) in [(0, false), (1, true), (2, false)] {
        let f = dir.join(format!("c{round}.mp4"));
        std::fs::copy(&clip, &f).unwrap();
        let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
        p.set_size(1152.0, 648.0);
        p.open(&f, 48.0, 3);
        std::thread::sleep(Duration::from_millis(300));
        p.toggle();
        p.tick();
        std::thread::sleep(Duration::from_millis(500));
        p.close();
        let r = if rename { std::fs::rename(&f, dir.join(format!("r{round}.mp4"))) } else { std::fs::remove_file(&f) };
        println!("tour {round} : {} juste aprÃ¨s close : {r:?}", if rename { "renommer" } else { "supprimer" });
        assert!(r.is_ok());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fichier sans image (abîmé) : la lecture s'arrête sur place, elle ne saute pas à la fin comme un clip fini.
#[test]
#[ignore = "lance ffmpeg"]
fn player_stops_in_place_when_no_frame_decodes() {
    let f = std::env::temp_dir().join("clipper-broken-clip.mp4");
    std::fs::write(&f, b"pas une video").unwrap();
    let mut p = crate::player::Player::new("ffmpeg", Arc::new(|| {}));
    p.set_size(384.0, 216.0);
    p.open_playing(&f, 30.0, 1);
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(3) {
        p.tick();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(p.pos() < 1.0, "position {} : le lecteur a sauté à la fin", p.pos());
    p.close();
    let _ = std::fs::remove_file(&f);
}
