//! Décisions du superviseur de capture (capture.rs, `Buffer::start`), sans Windows ni processus : chaque
//! seuil réglé sur de vrais jeux est fixé ici et vérifié par les tests. La boucle du superviseur lit l'état
//! (segments, plein écran, charge de l'encodeur, horloge), demande la décision ici, puis agit.
use std::time::Duration;

/// Plus aucune image depuis 2 s (`still` : secondes sans que le dernier segment grossisse) : point de
/// maintien WGC montré, même par-dessus un jeu plein écran (keepalive.rs).
pub fn nudge(still: u32) -> bool {
    still >= 2
}

/// Capture figée, ffmpeg à relancer : 3 s sans image, ou 1 s juste après l'entrée ou la sortie d'un plein
/// écran (`since_flip`, u32::MAX : aucune ; vu au lancement de CS2 : la session WGC s'y fige et ne repart
/// jamais seule). ffmpeg mort : laissé au chemin « arrêté », qui relance avec une attente croissante.
pub fn frozen(still: u32, since_flip: u32, alive: bool) -> bool {
    alive && (still >= 3 || (still >= 1 && since_flip <= 3))
}

/// Garde-fou de l'encodeur, relevé toutes les 2 s : saturé (> 90 %) 10 s d'affilée -> mode allégé ; calme
/// (< 50 %) 2 min d'affilée en mode allégé -> retour au profil choisi. Renvoie (hot, calm, bascule).
pub fn encoder_guard(load: u32, hot: u32, calm: u32, light: bool) -> (u32, u32, bool) {
    let (hot, calm) = if load > 90 { (hot + 2, 0) } else if load < 50 { (0, calm + 2) } else { (0, 0) };
    (hot, calm, (!light && hot >= 10) || (light && calm >= 120))
}

/// Un pas de 250 ms qui a duré plus de 5 s sur l'horloge du buffer (qui compte la veille) : le PC dormait.
pub fn woke_from_sleep(last: f64, now: f64) -> bool {
    now - last > 5.0
}

/// ffmpeg mort juste après son lancement (disque plein, écran indisponible…) : attente avant la relance,
/// croissante avec les échecs d'affilée (0,3 puis 2, 4, 8, 16, 30 s). Avant : relancé toutes les 1,3 s sans fin.
pub fn backoff(fails: u32) -> Duration {
    if fails == 0 { Duration::from_millis(300) } else { Duration::from_secs((1u64 << fails.min(5)).min(30)) }
}

/// Relevé d'une seconde de capture, fait par la boucle du superviseur.
#[derive(Clone, Copy, Debug)]
pub struct Obs {
    /// ffmpeg tourne encore.
    pub alive: bool,
    /// Dernier segment : (numéro, taille). Il grossit image par image (flush_packets=1).
    pub newest: Option<(u64, u64)>,
    /// Un jeu plein écran est au premier plan.
    pub fullscreen: bool,
    /// Charge de l'encodeur en % (lue seulement quand `Watch::reads` le demande).
    pub encoder: Option<u32>,
    /// Mode allégé en cours.
    pub light: bool,
    /// Un REC est en cours : jamais de bascule de mode (elle couperait l'enregistrement ~1 s).
    pub rec: bool,
    /// Taille de l'écran différente de celle capturée (lue seulement quand `Watch::reads` le demande).
    pub screen_changed: bool,
    /// Réglage « capture du bureau ».
    pub desktop_capture: bool,
    /// Secondes depuis le lancement de ce ffmpeg.
    pub age: f64,
}

/// Fin d'une capture décidée par `Watch::step`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    /// Encodeur saturé 10 s (vers le mode allégé) ou calme 2 min (retour au normal) : `true` = vers l'allégé.
    Light(bool),
    /// Résolution de l'écran changée : relancer à la nouvelle taille.
    Screen,
    /// Plus aucune image depuis tant de secondes, ffmpeg vivant : capture figée.
    Frozen(u32),
    /// Retour au bureau depuis plus de 5 s (capture du bureau désactivée) : ffmpeg coupé.
    Away,
    /// ffmpeg s'est arrêté seul. `log` : ligne de journal à écrire (1er, 2e, 4e, 8e… échec d'affilée).
    Died { log: bool },
}

/// Décision d'une seconde.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// Rendre la RAM froide du pilote (5 s après le lancement, puis toutes les minutes).
    pub trim: bool,
    /// Montrer le point de maintien WGC (aucune image depuis 2 s).
    pub nudge: bool,
    /// Capture à arrêter (puis relancer après `backoff(fails)`). `Died` et `Away` : après le nettoyage.
    pub end: Option<End>,
}

/// Automate d'une capture (un lancement de ffmpeg). `fails` (échecs d'affilée) passe d'un lancement au suivant.
pub struct Watch {
    tick: u32,
    hot: u32,
    calm: u32,
    last_seg: Option<(u64, u64)>,
    still: u32,
    full: bool,
    since_flip: u32,
    away: u32,
    pub fails: u32,
}

impl Watch {
    pub fn new(fullscreen: bool, fails: u32) -> Self {
        Self { tick: 0, hot: 0, calm: 0, last_seg: None, still: 0, full: fullscreen, since_flip: u32::MAX, away: 0, fails }
    }

    /// Relevés coûteux attendus à la seconde suivante : (charge de l'encodeur, taille de l'écran), une
    /// seconde sur deux chacun.
    pub fn reads(&self) -> (bool, bool) {
        let next = self.tick + 1;
        (next % 2 == 0, next % 2 == 1)
    }

    pub fn step(&mut self, o: &Obs) -> Step {
        self.tick += 1;
        if self.tick == 10 {
            self.fails = 0; // a tourné normalement
        }
        let trim = self.tick == 5 || self.tick % 60 == 0;
        let mut out = Step { trim, nudge: false, end: None };
        if let Some(load) = o.encoder {
            let flip;
            (self.hot, self.calm, flip) = encoder_guard(load, self.hot, self.calm, o.light);
            if flip && !o.rec {
                out.end = Some(End::Light(!o.light));
                return out;
            }
        }
        if o.screen_changed {
            out.end = Some(End::Screen);
            return out;
        }
        self.still = if o.newest == self.last_seg { self.still + 1 } else { 0 };
        self.last_seg = o.newest;
        out.nudge = nudge(self.still);
        self.since_flip = if o.fullscreen != self.full { 0 } else { self.since_flip.saturating_add(1) };
        self.full = o.fullscreen;
        if frozen(self.still, self.since_flip, o.alive) {
            out.end = Some(End::Frozen(self.still));
            return out;
        }
        if !o.alive {
            if o.age < 6.0 {
                self.fails += 1;
            }
            out.end = Some(End::Died { log: self.fails <= 1 || self.fails.is_power_of_two() });
            return out;
        }
        self.away = if !o.desktop_capture && !o.fullscreen { self.away + 1 } else { 0 };
        if self.away > 5 {
            out.end = Some(End::Away);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NO_FLIP: u32 = u32::MAX;

    /// Une seconde de capture normale : le segment `n` a grossi.
    fn ok(n: u64) -> Obs {
        Obs { alive: true, newest: Some((n, n * 1000)), fullscreen: true, encoder: None, light: false, rec: false, screen_changed: false, desktop_capture: false, age: 30.0 }
    }

    #[test]
    fn game_going_fullscreen_then_freezing_restarts_after_one_second() {
        let mut w = Watch::new(false, 0);
        for n in 1..=3 {
            assert_eq!(w.step(&Obs { fullscreen: false, desktop_capture: true, ..ok(n) }).end, None);
        }
        assert_eq!(w.step(&ok(4)).end, None, "bascule plein écran, une image encore");
        assert_eq!(w.step(&ok(4)).end, Some(End::Frozen(1)), "1 s sans image juste après la bascule");
    }

    #[test]
    fn steady_stall_nudges_at_two_seconds_and_restarts_at_three() {
        let mut w = Watch::new(true, 0);
        for n in 1..=10 {
            w.step(&ok(n));
        }
        let s = w.step(&ok(10));
        assert!(!s.nudge && s.end.is_none());
        let s = w.step(&ok(10));
        assert!(s.nudge && s.end.is_none());
        assert_eq!(w.step(&ok(10)).end, Some(End::Frozen(3)));
    }

    #[test]
    fn saturated_encoder_goes_light_but_never_during_a_rec() {
        let run = |rec: bool| {
            let mut w = Watch::new(true, 0);
            let mut ends = vec![];
            for n in 1..=10 {
                let encoder = w.reads().0.then_some(95);
                ends.extend(w.step(&Obs { encoder, rec, ..ok(n) }).end);
            }
            ends
        };
        assert_eq!(run(false), [End::Light(true)], "10 s saturé (relevé une seconde sur deux)");
        assert!(run(true).is_empty(), "pendant un REC : rien");
    }

    #[test]
    fn screen_change_is_checked_every_other_second() {
        let w = Watch::new(true, 0);
        assert_eq!(w.reads(), (false, true));
        let mut w = w;
        assert_eq!(w.step(&Obs { screen_changed: true, ..ok(1) }).end, Some(End::Screen));
    }

    #[test]
    fn quick_deaths_count_and_ten_good_seconds_reset_them() {
        let mut w = Watch::new(true, 3);
        assert_eq!(w.step(&Obs { alive: false, age: 2.0, ..ok(1) }).end, Some(End::Died { log: true }), "4e échec : journalisé");
        assert_eq!(w.fails, 4);
        let mut w = Watch::new(true, 4);
        assert_eq!(w.step(&Obs { alive: false, age: 2.0, ..ok(1) }).end, Some(End::Died { log: false }), "5e : silencieux");
        let mut w = Watch::new(true, 4);
        for n in 1..=10 {
            w.step(&ok(n));
        }
        assert_eq!(w.fails, 0);
        assert_eq!(w.step(&Obs { alive: false, age: 600.0, ..ok(11) }).end, Some(End::Died { log: true }));
        assert_eq!(w.fails, 0, "mort après un long moment : pas un échec de lancement");
    }

    #[test]
    fn back_on_the_desktop_stops_after_five_seconds() {
        let mut w = Watch::new(true, 0);
        let desk = |n| Obs { fullscreen: false, ..ok(n) };
        let ends: Vec<_> = (1..=6).map(|n| w.step(&desk(n)).end).collect();
        assert_eq!(ends[..5], [None; 5]);
        assert_eq!(ends[5], Some(End::Away));
        let mut w = Watch::new(true, 0);
        assert!((1..=8).all(|n| w.step(&Obs { fullscreen: false, desktop_capture: true, ..ok(n) }).end.is_none()), "capture du bureau : continue");
    }

    #[test]
    fn driver_ram_is_trimmed_at_five_seconds_then_every_minute() {
        let mut w = Watch::new(true, 0);
        let trims: Vec<u32> = (1..=120).filter(|&n| w.step(&ok(u64::from(n))).trim).collect();
        assert_eq!(trims, [5, 60, 120]);
    }

    #[test]
    fn stalled_capture_is_nudged_then_restarted() {
        assert!(!nudge(1) && nudge(2) && nudge(9));
        assert!(!frozen(2, NO_FLIP, true));
        assert!(frozen(3, NO_FLIP, true));
    }

    #[test]
    fn fullscreen_flip_restarts_after_one_second() {
        assert!(frozen(1, 0, true) && frozen(1, 3, true));
        assert!(!frozen(1, 4, true) && !frozen(0, 0, true));
    }

    #[test]
    fn dead_ffmpeg_is_left_to_the_restart_path() {
        assert!(!frozen(9, 0, false));
    }

    #[test]
    fn encoder_guard_needs_ten_hot_seconds_and_two_calm_minutes() {
        let (mut hot, mut calm, mut flip) = (0, 0, false);
        for _ in 0..4 {
            (hot, calm, flip) = encoder_guard(95, hot, calm, false);
        }
        assert!(!flip, "8 s saturé : pas encore");
        (hot, calm, flip) = encoder_guard(95, hot, calm, false);
        assert!(flip && hot == 10 && calm == 0);
        assert_eq!(encoder_guard(70, 8, 0, false), (0, 0, false), "un relevé moyen remet à zéro");
        assert_eq!(encoder_guard(30, 0, 118, true), (0, 120, true));
        assert_eq!(encoder_guard(30, 0, 118, false), (0, 120, false), "déjà en mode normal : rien");
    }

    #[test]
    fn sleep_is_a_step_longer_than_five_seconds() {
        assert!(!woke_from_sleep(100.0, 100.25));
        assert!(!woke_from_sleep(100.0, 105.0));
        assert!(woke_from_sleep(100.0, 105.1));
    }

    #[test]
    fn backoff_grows_to_thirty_seconds() {
        let s: Vec<f32> = (0..8).map(|f| backoff(f).as_secs_f32()).collect();
        assert_eq!(s, [0.3, 2.0, 4.0, 8.0, 16.0, 30.0, 30.0, 30.0]);
    }
}
