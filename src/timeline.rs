//! Horloge du buffer : quel segment couvre quel moment, sur une seule horloge monotone (celle de
//! `Instant`, QueryPerformanceCounter sous Windows), jamais l'heure de Windows ni la date des fichiers
//! (elles sautent avec la synchro d'heure, le changement d'heure, un réglage manuel).
//!
//! Le superviseur de capture (capture.rs) y note chaque lancement de ffmpeg (« capture »), chaque segment
//! vu sur le disque et chaque arrêt. Un clip = les segments qui couvrent [appui − durée, appui], pris tels
//! quels : rien n'est vidé ni remis à zéro, deux clips rapprochés se chevauchent librement.
//!
//! Logique pure (le temps est passé en secondes) : testée avec une horloge simulée.

/// Secondes écoulées sur l'horloge monotone depuis le premier appel.
pub fn now() -> f64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

#[derive(Clone, Debug, PartialEq)]
struct Seg {
    n: u64,
    run: u32,
    /// Moment où le fichier est apparu (début du segment).
    start: f64,
}

#[derive(Clone, Debug)]
struct Run {
    id: u32,
    /// Format des segments (encodeur, taille, images/s, pistes) : deux formats ne s'assemblent pas sans réencoder.
    sig: String,
    /// Arrêt de cette capture ; None = elle tourne.
    end: Option<f64>,
}

/// Segments consécutifs d'une même capture (horodatages continus : assemblables en un seul flux).
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub segs: Vec<u64>,
    /// Le dernier segment est encore en cours d'écriture par ffmpeg.
    pub live: bool,
}

#[derive(Default)]
pub struct Timeline {
    runs: Vec<Run>,
    segs: Vec<Seg>,
    next_run: u32,
}

impl Timeline {
    /// ffmpeg lancé à `now`, au format `sig`. Les segments qu'il écrira lui seront rattachés.
    pub fn begin(&mut self, now: f64, sig: &str) {
        self.end(now);
        self.runs.push(Run { id: self.next_run, sig: sig.to_string(), end: None });
        self.next_run += 1;
    }

    /// Capture arrêtée (ffmpeg tué, mort, écran perdu) à `now`.
    pub fn end(&mut self, now: f64) {
        if let Some(r) = self.runs.last_mut().filter(|r| r.end.is_none()) {
            r.end = Some(now);
        }
    }

    /// Numéros des segments présents sur le disque à `now` : les nouveaux sont datés et rattachés à la
    /// capture en cours, ceux qui ont disparu (nettoyage) sont oubliés.
    pub fn observe(&mut self, now: f64, on_disk: &[u64]) {
        // Recherche dichotomique : pendant un long REC, des milliers de segments restent sur le disque et
        // `contains` coûtait n² comparaisons quatre fois par seconde.
        let mut disk = on_disk.to_vec();
        disk.sort_unstable();
        self.segs.retain(|s| disk.binary_search(&s.n).is_ok());
        let Some(run) = self.runs.last().filter(|r| r.end.is_none()).map(|r| r.id) else { return };
        let newest = self.segs.last().map(|s| s.n);
        for &n in on_disk {
            if newest.is_none_or(|m| n > m) {
                self.segs.push(Seg { n, run, start: now });
            }
        }
        self.runs.retain(|r| r.end.is_none() || self.segs.iter().any(|s| s.run == r.id));
    }

    fn run(&self, id: u32) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    /// Fin du segment d'indice `i` : début du suivant de la même capture, sinon l'arrêt de la capture,
    /// sinon `now` (segment en cours d'écriture).
    fn end_of(&self, i: usize, now: f64) -> f64 {
        let s = &self.segs[i];
        match self.segs.get(i + 1).filter(|n| n.run == s.run) {
            Some(next) => next.start,
            None => self.run(s.run).and_then(|r| r.end).unwrap_or(now),
        }
    }

    /// Segments qui couvrent les `secs` secondes avant `now`, du plus ancien au plus récent, groupés par
    /// capture. S'arrête à un changement de format (résolution changée : la partie d'avant ne s'assemble
    /// pas avec celle d'après). Vide : rien n'a été enregistré dans cette fenêtre.
    pub fn pick(&self, now: f64, secs: f64) -> Vec<Part> {
        self.collect(now, |me, i| me.end_of(i, now) > now - secs)
    }

    /// Segments à partir du numéro `from` (REC) : même groupement, même arrêt au changement de format.
    pub fn since(&self, now: f64, from: u64) -> Vec<Part> {
        self.collect(now, |me, i| me.segs[i].n >= from)
    }

    fn collect(&self, now: f64, keep: impl Fn(&Self, usize) -> bool) -> Vec<Part> {
        let Some(sig) = self.segs.last().and_then(|s| self.run(s.run)).map(|r| r.sig.clone()) else { return vec![] };
        let mut parts: Vec<(u32, Part)> = vec![];
        for i in (0..self.segs.len()).rev() {
            let s = &self.segs[i];
            let Some(run) = self.run(s.run) else { break };
            if run.sig != sig || !keep(self, i) {
                break;
            }
            // Segment sans durée (capture arrêtée à peine lancée) : rien dedans.
            if self.end_of(i, now) <= s.start && run.end.is_some() {
                continue;
            }
            match parts.last_mut().filter(|(r, _)| *r == s.run) {
                Some((_, p)) => p.segs.push(s.n),
                None => parts.push((s.run, Part { segs: vec![s.n], live: run.end.is_none() && i + 1 == self.segs.len() })),
            }
        }
        parts.reverse();
        parts.into_iter().map(|(_, mut p)| { p.segs.reverse(); p }).collect()
    }

    /// Durée couverte par ces segments (pour contrôler le clip produit).
    pub fn span(&self, parts: &[Part], now: f64) -> f64 {
        parts.iter().flat_map(|p| &p.segs).filter_map(|n| self.segs.iter().position(|s| s.n == *n)).map(|i| self.end_of(i, now) - self.segs[i].start).sum()
    }

    /// Journal : segments datés, captures connues, âge du plus ancien segment (s).
    pub fn stats(&self, now: f64) -> (usize, usize, f64) {
        (self.segs.len(), self.runs.len(), self.segs.first().map_or(0.0, |s| now - s.start))
    }

    /// Plus ancien segment encore daté (rien avant lui ne sert plus).
    #[cfg(test)]
    pub fn oldest(&self) -> Option<u64> {
        self.segs.first().map(|s| s.n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture simulée : un segment toutes les 2 s à partir de `t0`, vu par le superviseur à sa création.
    /// `keep` = segments gardés sur le disque (le nettoyage retire les plus vieux).
    struct Sim {
        tl: Timeline,
        disk: Vec<u64>,
        next: u64,
        keep: usize,
    }

    impl Sim {
        fn new(keep: usize) -> Self {
            Sim { tl: Timeline::default(), disk: vec![], next: 0, keep }
        }

        /// Fait tourner la capture de `from` à `to` (secondes), au format `sig`.
        fn run(&mut self, from: f64, to: f64, sig: &str) {
            self.tl.begin(from, sig);
            let mut t = from;
            while t < to {
                self.disk.push(self.next);
                self.next += 1;
                if self.disk.len() > self.keep {
                    self.disk.remove(0);
                }
                self.tl.observe(t, &self.disk);
                t += 2.0;
            }
        }

        fn stop(&mut self, at: f64) {
            self.tl.end(at);
        }

        fn all(parts: &[Part]) -> Vec<u64> {
            parts.iter().flat_map(|p| p.segs.clone()).collect()
        }
    }

    #[test]
    fn one_clip_is_the_last_minute() {
        let mut s = Sim::new(40);
        s.run(0.0, 300.0, "a"); // segments 0..150, le 149 (298-300 s) en cours
        let parts = s.tl.pick(299.0, 60.0);
        assert_eq!(parts.len(), 1);
        // [239, 299] : du segment 119 (238-240) au 149, en cours d'écriture.
        assert_eq!(parts[0].segs, (119..=149).collect::<Vec<_>>());
        assert!(parts[0].live);
        assert!((s.tl.span(&parts, 299.0) - 61.0).abs() < 0.01);
    }

    #[test]
    fn second_clip_30_s_later_overlaps_the_first() {
        let mut s = Sim::new(40);
        s.run(0.0, 200.0, "a");
        let first = Sim::all(&s.tl.pick(199.0, 60.0));
        // Le buffer continue : rien n'est vidé par le premier clip.
        let mut t = 200.0;
        while t < 230.0 {
            s.disk.push(s.next);
            s.next += 1;
            s.disk.remove(0);
            s.tl.observe(t, &s.disk);
            t += 2.0;
        }
        let second = Sim::all(&s.tl.pick(229.0, 60.0));
        assert_eq!(second.len(), first.len(), "le second clip fait aussi une minute");
        assert_eq!(*second.last().unwrap(), 114, "il va jusqu'à l'appui");
        let shared = second.iter().filter(|n| first.contains(n)).count();
        assert_eq!(shared, 16, "30 s communes avec le premier, 30 s nouvelles");
    }

    #[test]
    fn two_presses_at_the_same_moment_give_the_same_clip() {
        let mut s = Sim::new(40);
        s.run(0.0, 100.0, "a");
        assert_eq!(s.tl.pick(99.5, 60.0), s.tl.pick(99.5, 60.0));
    }

    #[test]
    fn buffer_not_full_gives_a_shorter_clip() {
        let mut s = Sim::new(40);
        s.run(0.0, 11.0, "a"); // 6 segments, 11 s de capture
        let parts = s.tl.pick(11.0, 60.0);
        assert_eq!(Sim::all(&parts), vec![0, 1, 2, 3, 4, 5]);
        assert!((s.tl.span(&parts, 11.0) - 11.0).abs() < 0.01);
        assert!(Timeline::default().pick(5.0, 60.0).is_empty(), "rien d'enregistré : pas de clip");
    }

    #[test]
    fn capture_restart_splits_the_clip_in_two_parts() {
        let mut s = Sim::new(40);
        s.run(0.0, 30.0, "a"); // segments 0..15
        s.stop(31.0); // écran perdu
        s.run(32.5, 50.0, "a"); // relance 1,5 s après : segments 15..24
        let parts = s.tl.pick(49.0, 60.0);
        assert_eq!(parts.len(), 2, "une partie par capture : horodatages remis à zéro entre les deux");
        assert_eq!(parts[0].segs, (0..15).collect::<Vec<_>>());
        assert!(!parts[0].live);
        assert_eq!(parts[1].segs, (15..24).collect::<Vec<_>>());
        assert!(parts[1].live);
        // 31 s avant la coupure + 16,5 s après : le trou de 1,5 s n'est pas compté.
        assert!((s.tl.span(&parts, 49.0) - 47.5).abs() < 0.01);
    }

    #[test]
    fn resolution_change_never_mixes_two_formats() {
        let mut s = Sim::new(40);
        s.run(0.0, 40.0, "1920x1080");
        s.stop(40.0);
        s.run(41.0, 61.0, "1280x960"); // Valorant étiré : la résolution change
        let parts = s.tl.pick(60.0, 60.0);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].segs, (20..30).collect::<Vec<_>>(), "seulement la partie à la résolution actuelle");
    }

    #[test]
    fn old_segments_from_before_a_long_pause_are_left_out() {
        let mut s = Sim::new(40);
        s.run(0.0, 60.0, "a");
        s.stop(60.0); // capture en pause (retour au bureau, veille du PC)
        // 20 minutes plus tard, 10 s de capture : le clip ne contient que ces 10 s.
        s.run(1260.0, 1270.0, "a");
        let parts = s.tl.pick(1270.0, 60.0);
        assert_eq!(Sim::all(&parts), (30..35).collect::<Vec<_>>());
        // Capture arrêtée depuis plus longtemps que le buffer : plus rien à clipper.
        s.stop(1270.0);
        assert!(s.tl.pick(1400.0, 60.0).is_empty());
        // Arrêtée depuis 20 s : les 40 dernières secondes enregistrées restent clippables.
        let mut s = Sim::new(40);
        s.run(0.0, 100.0, "a");
        s.stop(100.0);
        assert_eq!(Sim::all(&s.tl.pick(120.0, 60.0)), (30..50).collect::<Vec<_>>());
    }

    #[test]
    fn cleaned_segments_are_forgotten() {
        let mut s = Sim::new(10); // le disque ne garde que 10 segments
        s.run(0.0, 100.0, "a");
        let parts = s.tl.pick(99.0, 60.0);
        assert_eq!(Sim::all(&parts), (40..50).collect::<Vec<_>>());
        assert_eq!(s.tl.oldest(), Some(40));
    }

    #[test]
    fn rec_takes_everything_since_its_start() {
        let mut s = Sim::new(1000);
        s.run(0.0, 400.0, "a");
        let parts = s.tl.since(399.0, 150);
        assert_eq!(Sim::all(&parts), (150..200).collect::<Vec<_>>());
        assert!(parts[0].live);
    }
}
