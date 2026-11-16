//! Registre du buffer : l'horloge des segments (timeline.rs), la marque du dernier clip (clipplan::Ledger)
//! et les segments retenus par les clips en cours d'assemblage. Un seul verrou pour les trois : avant,
//! trois globales de capture.rs se prenaient dans un ordre implicite (horloge, puis marque, puis retenues).
//!
//! Logique pure (temps et numéros de segments passés en paramètres) : testée sans disque ni ffmpeg.
use crate::clipplan::{Ledger, Mark};
use crate::timeline::{Part, Timeline};

/// Ce qu'un instantané emporte : les segments choisis, la durée qu'ils couvrent sur l'horloge, et le premier
/// segment retenu contre le nettoyage (à rendre par `release`).
#[derive(Debug, PartialEq)]
pub struct Pick {
    pub parts: Vec<Part>,
    pub secs: f64,
    pub pin: Option<u64>,
}

#[derive(Default)]
pub struct Registry {
    tl: Timeline,
    ledger: Ledger,
    pins: Vec<u64>,
}

impl Registry {
    /// ffmpeg lancé à `now` au format `sig`.
    pub fn begin(&mut self, now: f64, sig: &str) {
        self.tl.begin(now, sig);
    }

    /// Capture arrêtée à `now`.
    pub fn end(&mut self, now: f64) {
        self.tl.end(now);
    }

    /// Segments présents sur le disque à `now`.
    pub fn observe(&mut self, now: f64, on_disk: &[u64]) {
        self.tl.observe(now, on_disk);
    }

    /// Clip demandé à `now` : au plus `buffer` secondes, sans reprendre le clip précédent. Rend aussi la
    /// durée visée et la marque (à rendre par `undo` si le clip échoue).
    pub fn clip(&mut self, now: f64, on_disk: &[u64], buffer: f64) -> (Pick, f64, Mark) {
        self.tl.observe(now, on_disk);
        let (w, m) = self.ledger.mark(buffer, now);
        let parts = self.tl.pick(now, w);
        (self.pin(parts, now), w, m)
    }

    /// REC arrêté à `now` : les segments à partir du numéro `from`.
    pub fn rec(&mut self, now: f64, on_disk: &[u64], from: u64) -> Pick {
        self.tl.observe(now, on_disk);
        let parts = self.tl.since(now, from);
        self.pin(parts, now)
    }

    fn pin(&mut self, parts: Vec<Part>, now: f64) -> Pick {
        let pin = parts.first().and_then(|p| p.segs.first().copied());
        self.pins.extend(pin);
        Pick { secs: self.tl.span(&parts, now), parts, pin }
    }

    /// Clip assemblé (ou abandonné) : ses segments peuvent de nouveau être nettoyés.
    pub fn release(&mut self, pin: u64) {
        if let Some(i) = self.pins.iter().position(|x| *x == pin) {
            self.pins.swap_remove(i);
        }
    }

    /// Clip raté : ses secondes iront dans le prochain clip.
    pub fn undo(&mut self, m: Mark) {
        self.ledger.undo(m);
    }

    /// Plus petit segment retenu par un clip en cours : le nettoyage n'efface rien à partir de là.
    pub fn floor(&self) -> Option<u64> {
        self.pins.iter().copied().min()
    }

    /// Relevé pour le journal : segments datés, captures, âge du plus ancien.
    pub fn stats(&self, now: f64) -> (usize, usize, f64) {
        self.tl.stats(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une capture lancée à 0, un segment de 2 s apparu toutes les 2 s, jusqu'à `until`.
    fn running(until: u64) -> (Registry, Vec<u64>) {
        let mut r = Registry::default();
        r.begin(0.0, "h264 1920x1080 60");
        let mut disk = vec![];
        for n in 0..=until / 2 {
            disk.push(n);
            r.observe((n * 2) as f64, &disk);
        }
        (r, disk)
    }

    #[test]
    fn a_clip_pins_its_first_segment_until_released() {
        let (mut r, disk) = running(100);
        let (p, w, _) = r.clip(100.0, &disk, 20.0);
        assert_eq!(w, 20.0);
        let first = p.parts[0].segs[0];
        assert_eq!(p.pin, Some(first));
        assert_eq!(r.floor(), Some(first), "le nettoyage s'arrête au clip en cours");
        r.release(first);
        assert_eq!(r.floor(), None);
    }

    #[test]
    fn two_clips_in_flight_keep_the_oldest_floor() {
        let (mut r, disk) = running(100);
        let (a, _, _) = r.clip(100.0, &disk, 60.0);
        let (b, _, _) = r.clip(110.0, &disk, 60.0);
        assert!(b.pin > a.pin);
        assert_eq!(r.floor(), a.pin);
        r.release(a.pin.unwrap());
        assert_eq!(r.floor(), b.pin, "le second clip reste protégé");
    }

    #[test]
    fn a_failed_clip_gives_its_seconds_to_the_next_one() {
        let (mut r, disk) = running(200);
        let (_, w1, _) = r.clip(100.0, &disk, 60.0);
        assert_eq!(w1, 60.0);
        let (_, w2, m2) = r.clip(130.0, &disk, 60.0);
        assert_eq!(w2, 30.0, "pas de reprise du clip précédent");
        r.undo(m2);
        let (_, w3, _) = r.clip(140.0, &disk, 60.0);
        assert_eq!(w3, 40.0, "les 30 s du clip raté reviennent");
    }

    #[test]
    fn rec_takes_every_segment_from_its_start() {
        let (mut r, disk) = running(40);
        let p = r.rec(40.0, &disk, 10);
        assert_eq!(p.parts[0].segs.first(), Some(&10));
        assert_eq!(p.parts[0].segs.last(), Some(&20));
        assert_eq!(p.pin, Some(10));
    }
}
