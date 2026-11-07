# Clipper

Application Windows légère : enregistrement de clips de jeu (type Instant Replay), REC, statistiques à l'écran, mise à jour des pilotes NVIDIA et nettoyage du PC.

- **Clipper** : buffer circulaire (ddagrab + encodeur matériel NVENC / AMF / x264), clip instantané sans réencodage, REC, audio jeu + micro (1 ou 2 pistes), barre Alt+Z, overlay FPS / CPU / GPU.
- **Vidéos** : galerie et lecteur intégré.
- **Pilotes** : détection du GPU NVIDIA, dernière version Game Ready ou Studio, téléchargement depuis `*.nvidia.com` uniquement (taille et signature vérifiées), installation silencieuse.
- **Outils** : nettoyage des fichiers temporaires et caches, nettoyage de la RAM.

## Anti-cheat
Aucune injection, aucun hook, aucune lecture de la mémoire d'un jeu. Capture par Desktop Duplication, audio par WASAPI, raccourcis par `RegisterHotKey`, FPS par les événements ETW publics (comme PresentMon). Détails : [SPEC.md](SPEC.md).

## Prérequis
- Windows 10/11, WebView2 (installé avec Windows 11).
- `ffmpeg` et `ffprobe` dans le PATH (non fournis : licence GPL).

## Compiler
```
set CARGO_TARGET_DIR=D:\ct
cargo build --release
cargo test --release
```
L'exe : `D:\ct\release\clipper.exe` (environ 2,3 Mo, interface incluse).

## État
En développement. Voir [tasks/plan.md](tasks/plan.md) pour ce qui est fait, testé, et ce qui reste.

## Polices
Archivo, JetBrains Mono, Schibsted Grotesk, Figtree, Instrument Serif, Public Sans, Barlow, Saira Condensed : licence SIL Open Font License 1.1 (fichiers venant de Fontsource).
