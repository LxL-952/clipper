# Plan : Clipper

Chaque tâche se termine par une vérification réelle (lancement, ffprobe, typeperf). Rien n'est marqué fait sans preuve.

## Phase 1 — Base (core)
- [x] T1 Squelette Rust : boucle `tao`, icône tray (Ouvrir / Quitter), priorité `BELOW_NORMAL`, sous-système `windows` (pas de console).
  - Vérif : double-clic sur l'exe, icône visible, Quitter ferme. `typeperf` : RAM privée < 10 Mo, CPU 0 %.
- [ ] T2 Config `%APPDATA%\Clipper\config.json` (lecture, valeurs par défaut, écriture) + raccourcis `global-hotkey` (Alt+X, Alt+R, Alt+Z).
  - Vérif : tests unitaires config ; appui raccourci = log.

## Phase 2 — Capture (capture, clip-rec)
- [ ] T3 Détection encodeurs : essai réel d'encodage 1 frame par encodeur (nvenc h264/hevc/av1, amf, qsv, x264). Garde ceux qui marchent.
  - Vérif : liste affichée = encodeurs qui fonctionnent sur ta machine.
- [ ] T4 Buffer : ffmpeg `ddagrab` → encodeur → `-f segment` `.ts` 2 s dans `%TEMP%\Clipper\buf`, ffmpeg en `BELOW_NORMAL`, nettoyage des vieux segments, redémarrage auto si ffmpeg meurt.
  - Vérif : segments créés toutes les 2 s, nombre stable. Mesure perf 1080p60 natif.
- [ ] T5 Clip (Alt+X) : liste des N derniers segments → concat `-c copy` → `.mp4/.mkv/.mov` dans `Vidéos\Clipper`. Son de confirmation.
  - Vérif : `ffprobe` durée, résolution, fps. Temps < 1 s.
- [ ] T6 REC (Alt+R) : segments réservés depuis le début ; 2e ffmpeg si réglages différents ; assemblage à l'arrêt.
  - Vérif : REC 30 s → fichier 30 s ± 2 s.
- [ ] T7 Résolutions non natives (chemin CPU `fast_bilinear`) + débits + estimation de taille.
  - Vérif : mesures 1080p60 / 1440p60 / 2160p60 (tableau).

## Phase 3 — Audio
- [ ] T8 Liste des périphériques (sortie + entrée), rechargement à chaud.
- [ ] T9 Capture WASAPI loopback + micro → named pipes → ffmpeg. Modes : aucun / 1 piste mixée / 2 pistes. Gain jeu, gain micro, mute, niveau.
  - Vérif : `ffprobe` nombre de pistes ; écoute d'un clip ; mesure CPU avant/après audio.

## Phase 4 — Interface (ui)
- [ ] T10 Fenêtre réglages (wry) à la demande, détruite à la fermeture. Onglets Clipper / Pilotes. Pont JS ↔ Rust (IPC).
  - Vérif : ouverture/fermeture ; 0 processus `msedgewebview2` après fermeture.
- [ ] T11 Barre Alt+Z : sans bordure, transparente, au premier plan, `WS_EX_NOACTIVATE`, animation, fermeture auto, compteur REC, avertissements perf.
  - Vérif : la barre ne prend pas le focus d'une autre fenêtre.
- [ ] T12 Revue design (impeccable, accessibilité, textes).

## Phase 5 — Pilotes (drivers)
- [ ] T13 Détection GPU + version (`nvidia-smi`), requête API NVIDIA (Game Ready / Studio), comparaison.
  - Vérif : affiche la vraie dernière version.
- [ ] T14 Téléchargement (domaine `*.nvidia.com` uniquement, progression, contrôle de taille) + confirmation + `setup.exe -s`.
  - Vérif : téléchargement testé ; installation **non lancée sans ton accord**.

## Phase 5b — Ajouts
- [ ] T16 Sélecteur de dossier natif (clips + buffer) avec espace libre du disque.
- [ ] T17 Mode étiré 16:9 / 16:10 (scale CPU seulement si la capture n'a pas déjà le ratio cible).
- [ ] T18 Interrupteur « Capture du bureau » : détection plein écran du premier plan, arrêt/reprise de ffmpeg.
- [ ] T19 Overlay statistiques natif (Win32, sans WebView) : FPS via ETW, CPU, GPU, temp, VRAM ; éléments au choix.
  - Vérif : RAM overlay < 5 Mo, CPU < 0,5 %, FPS cohérents avec ceux du jeu.

- [ ] T20 Puce d'encodage (NVIDIA / puce intégrée AMF via hwdownload).
- [ ] T21 720p, overlay masqué / raccourci / toujours visible.
- [ ] T22 Onglet Vidéos : miniatures, lecteur, actions.
- [ ] T23 Onglet Outils : analyse et nettoyage des temp/caches, RAM (`--clean-ram` élevé).
  - Vérif : tailles avant/après ; RAM libre avant/après ; aucun OpenProcess dans le code.

## Phase 6 — Fin
- [ ] T15 Tableau perf final (repos, 1080p60, 1440p60, 2160p60), revue de code, liste de ce qui n'est pas testé.

## Risques
- Synchro audio/vidéo via pipes : horodatage à l'arrivée ; à valider à l'écoute (T9).
- PC hybride : si l'écran passe par l'iGPU AMD, ddagrab + NVENC peut échouer → repli `hwdownload` ou AMF.
- 2160p sur écran 1200p = agrandissement CPU coûteux : avertissement obligatoire.
