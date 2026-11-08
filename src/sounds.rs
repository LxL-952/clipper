//! Sons de confirmation (début / fin de REC, clip, erreur), synthétisés en mémoire : aucun fichier,
//! joués par PlaySound en asynchrone (rien ne bloque).
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub enum Sound {
    RecStart,
    RecStop,
    Clip,
    Error,
}

const RATE: u32 = 44_100;

/// Notes (fréquence en Hz, durée en ms) jouées à la suite.
fn notes(s: Sound) -> &'static [(f32, u32)] {
    match s {
        Sound::RecStart => &[(660.0, 70), (990.0, 110)],
        Sound::RecStop => &[(990.0, 70), (660.0, 110)],
        Sound::Clip => &[(1320.0, 45), (1760.0, 75)],
        Sound::Error => &[(220.0, 170)],
    }
}

/// WAV 16 bits mono : sinus doux (attaque et extinction progressives, sans clic).
fn wav(s: Sound) -> Vec<u8> {
    let mut pcm: Vec<i16> = vec![];
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
    static CACHE: [OnceLock<Vec<u8>>; 4] = [const { OnceLock::new() }; 4];
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
}
