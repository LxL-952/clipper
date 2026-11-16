//! Drapeaux de lancement des processus de Clipper (ffmpeg, schtasks, sc…) : toujours sans fenêtre, à une
//! priorité nommée. Avant, chaque fichier avait sa copie de CREATE_NO_WINDOW et des nombres bruts (0x20, 0x40).

/// Priorité du processus lancé, par rapport au jeu.
#[derive(Clone, Copy)]
pub enum Priority {
    /// Celle de Clipper (sous la normale en fond, normale quand une fenêtre est affichée).
    Inherit = 0,
    /// Seulement quand le processeur n'a rien d'autre à faire : miniatures, planches, vérifications.
    Idle = 0x0000_0040,
    /// Après le jeu : outils de fond.
    Below = 0x0000_4000,
    /// Comme le jeu : la capture (perdre une image se voit dans le clip) et ce qu'on attend à l'écran.
    Normal = 0x0000_0020,
}

/// Drapeaux pour `CommandExt::creation_flags` : sans fenêtre de console, à la priorité `p`.
pub const fn hidden(p: Priority) -> u32 {
    0x0800_0000 | p as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_match_the_windows_constants() {
        use windows_sys::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS};
        assert_eq!(hidden(Priority::Inherit), CREATE_NO_WINDOW);
        assert_eq!(hidden(Priority::Idle), CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
        assert_eq!(hidden(Priority::Below), CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        assert_eq!(hidden(Priority::Normal), CREATE_NO_WINDOW | NORMAL_PRIORITY_CLASS);
    }
}
