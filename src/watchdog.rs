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

#[cfg(test)]
mod tests {
    use super::*;
    const NO_FLIP: u32 = u32::MAX;

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
