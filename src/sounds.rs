//! Sons de confirmation (début / fin de REC, clip, erreur), synthétisés en mémoire : aucun fichier,
//! joués par PlaySound en asynchrone (rien ne bloque).
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub enum Sound {
    RecStart,
    RecStop,
    Clip,
    Error,
    /// Intro de la fenêtre : claquement du trombone à 1,09 s, deux notes douces à 1,3 s.
    Intro,
}

const RATE: u32 = 44_100;

/// Notes (fréquence en Hz, durée en ms) jouées à la suite.
fn notes(s: Sound) -> &'static [(f32, u32)] {
    match s {
        Sound::RecStart => &[(660.0, 70), (990.0, 110)],
        Sound::RecStop => &[(990.0, 70), (660.0, 110)],
        Sound::Clip => &[(1320.0, 45), (1760.0, 75)],
        Sound::Error => &[(220.0, 170)],
        Sound::Intro => &[],
    }
}

/// Son de l'intro, repris de l'ancienne page (Web Audio) et calé sur l'animation :
/// 0,25 s chute du trombone (souffle qui monte), 1,09 s il se clipse (claquement + tintement métallique),
/// 1,3 s le nom apparaît (deux notes douces).
fn intro_pcm() -> Vec<i16> {
    let sr = RATE as f32;
    let mut buf = vec![0.0f32; (sr * 2.1) as usize];
    let at = |t: f32| (t * sr) as usize;
    let mut seed = 0x9e37_79b9u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    // Décroissance exponentielle de `v` à 0,001 entre t0 et t1 (exponentialRampToValueAtTime).
    let expo = |v: f32, t: f32, t0: f32, t1: f32| v * (0.001f32 / v).powf(((t - t0) / (t1 - t0)).clamp(0.0, 1.0));
    // Filtre biquad (formules RBJ, comme Web Audio) ; `band` : passe-bande, sinon passe-haut.
    let biquad = |band: bool, f: f32, q: f32| {
        let w = 2.0 * std::f32::consts::PI * f / sr;
        let (cw, a) = (w.cos(), w.sin() / (2.0 * q));
        let (b0, b1, b2) = if band { (a, 0.0, -a) } else { ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0) };
        let a0 = 1.0 + a;
        [b0 / a0, b1 / a0, b2 / a0, -2.0 * cw / a0, (1.0 - a) / a0]
    };
    let run = |c: [f32; 5], x: f32, z: &mut [f32; 4]| {
        let y = c[0] * x + c[1] * z[0] + c[2] * z[1] - c[3] * z[2] - c[4] * z[3];
        *z = [x, z[0], y, z[2]];
        y
    };
    // Souffle : bruit en passe-bande de 400 à 2600 Hz, monte jusqu'à 0,9 s puis s'éteint à 1,1 s.
    let mut z = [0.0; 4];
    for i in at(0.25)..at(1.15) {
        let t = i as f32 / sr;
        let f = 400.0 * (2600.0f32 / 400.0).powf(((t - 0.25) / 0.8).min(1.0));
        let g = if t < 0.9 { 0.16 * (t - 0.25) / 0.65 } else { expo(0.16, t, 0.9, 1.1) };
        buf[i] += run(biquad(true, f, 1.2), noise(), &mut z) * g;
    }
    // Claquement : bruit aigu très court.
    let (hp, mut z) = (biquad(false, 2500.0, 0.85), [0.0; 4]);
    for i in at(1.09)..at(1.125) {
        buf[i] += run(hp, noise(), &mut z) * expo(0.8, i as f32 / sr, 1.09, 1.125);
    }
    // Tintement métallique, puis les deux notes douces.
    for (hz, v) in [(2350.0, 0.2), (3720.0, 0.11), (5100.0, 0.05)] {
        for i in at(1.09)..at(1.36) {
            let t = i as f32 / sr;
            buf[i] += (2.0 * std::f32::consts::PI * hz * (t - 1.09)).sin() * expo(v, t, 1.09, 1.34);
        }
    }
    for (hz, t0) in [(660.0, 1.3), (990.0, 1.37)] {
        for i in at(t0)..at(t0 + 0.65) {
            let t = i as f32 / sr - t0;
            let g = if t < 0.02 { 0.07 * t / 0.02 } else { expo(0.07, t, 0.02, 0.6) };
            buf[i] += (2.0 * std::f32::consts::PI * hz * t).sin() * g;
        }
    }
    buf.iter().map(|v| ((v * 0.55).clamp(-1.0, 1.0) * i16::MAX as f32) as i16).collect()
}

/// WAV 16 bits mono : sinus doux (attaque et extinction progressives, sans clic).
fn wav(s: Sound) -> Vec<u8> {
    let mut pcm: Vec<i16> = if matches!(s, Sound::Intro) { intro_pcm() } else { vec![] };
    for &(f, ms) in notes(s) {
        let n = (RATE * ms / 1000) as usize;
        let edge = (RATE / 200) as usize; // 5 ms
        for i in 0..n {
            let env = (i.min(n - 1 - i) as f32 / edge as f32).min(1.0);
            let fade = 1.0 - i as f32 / n as f32 * 0.5;
            let v = (2.0 * std::f32::consts::PI * f * i as f32 / RATE as f32).sin() * 0.32 * env * fade;
            pcm.push((v * i16::MAX as f32) as i16);
        }
    }
    let data = pcm.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend(b"RIFF");
    w.extend((36 + data).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend(RATE.to_le_bytes());
    w.extend((RATE * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend(data.to_le_bytes());
    pcm.iter().for_each(|x| w.extend(x.to_le_bytes()));
    w
}

/// Joue le son tout de suite (asynchrone). Les WAV restent en mémoire : PlaySound les lit pendant la lecture.
pub fn play(s: Sound) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    static CACHE: [OnceLock<Vec<u8>>; 5] = [const { OnceLock::new() }; 5];
    let buf = CACHE[s as usize].get_or_init(|| wav(s));
    unsafe { PlaySoundW(buf.as_ptr() as _, std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_matches_data() {
        let w = super::wav(super::Sound::RecStart);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data);
        // 70 + 110 ms à 44,1 kHz, 2 octets par échantillon.
        assert_eq!(data, (44_100 * 70 / 1000 + 44_100 * 110 / 1000) * 2);
    }

    #[test]
    fn intro_fits_and_is_audible() {
        let pcm = super::intro_pcm();
        assert!(pcm.iter().any(|&v| v.unsigned_abs() > 1000));
    }
}
