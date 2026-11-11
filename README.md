# Clipper

Application Windows légère : enregistrement de clips de jeu (type Instant Replay), REC, statistiques à l'écran, mise à jour des pilotes NVIDIA et nettoyage du PC.

- **Clipper** : buffer circulaire (ddagrab + encodeur matériel NVENC / AMF / x264), clip instantané sans réencodage, REC, audio jeu + micro (1 ou 2 pistes), barre Alt+Z, overlay FPS / CPU / GPU.
- **Audio** : son du jeu et micro (1 ou 2 pistes), périphériques, volumes, vu-mètres.
- **Vidéos** : galerie et lecteur intégré.
- **Pilotes** : détection du GPU NVIDIA, dernière version Game Ready ou Studio, téléchargement depuis `*.nvidia.com` uniquement (taille et signature vérifiées), installation silencieuse.
- **Outils** : nettoyage des fichiers temporaires et caches, nettoyage de la RAM.

## Anti-cheat
Aucune injection, aucun hook, aucune lecture de la mémoire d'un jeu. Capture par Desktop Duplication, audio par WASAPI, raccourcis par `RegisterHotKey`, FPS par les événements ETW publics (comme PresentMon). Détails : [SPEC.md](SPEC.md).

## Installer
Télécharger `Clipper-Setup-<version>.exe` (assistant : contrat de licence, dossier, démarrage avec Windows, FFmpeg via winget ; PresentMon d'Intel inclus pour les FPS). Installation pour l'utilisateur seul, sans droits administrateur.

## Prérequis
- Windows 10/11, WebView2 (installé avec Windows 11).
- `ffmpeg` et `ffprobe` dans le PATH (non fournis : licence GPL ; l'installateur propose de les installer avec winget).

## Compiler
```
set CARGO_TARGET_DIR=D:\ct
cargo build --release
cargo test --release
```
L'exe : `D:\ct\release\clipper.exe` (environ 2,3 Mo, interface et logo inclus : `build.rs` dessine le trombone et l'intègre à l'exe).

Installateur (Inno Setup 6) :
```
ISCC.exe /DBin=D:\ct\release installer\clipper.iss
```

## État
En développement. Voir [tasks/plan.md](tasks/plan.md) pour ce qui est fait, testé, et ce qui reste.

## Licence
Tous droits réservés, voir [LICENSE](LICENSE). Le code est publié pour être consulté uniquement : pas de copie, de modification, de redistribution ni d'usage commercial sans autorisation écrite. Les versions de l'ancienne numérotation (avant le 10 novembre 2026, jusqu'à la 1.3.1) ont été publiées sous licence MIT ; la numérotation est repartie à 1.0.0 le 11 novembre 2026. Conditions d'utilisation affichées par l'installateur : [installer/conditions.txt](installer/conditions.txt).

## Polices
Geist et Geist Mono : licence SIL Open Font License 1.1.
