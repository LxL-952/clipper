# Clipper

Application Windows légère : clips de jeu instantanés (type Instant Replay), REC, statistiques à l'écran, mise à jour des pilotes NVIDIA et nettoyage du PC. Interface native en Rust, sans WebView.

- **Clipper** : buffer circulaire de 1 à 10 min (ddagrab + encodeur matériel NVENC / AMF ou x264), clip en 2 s environ sans réencodage, REC, barre Alt+Z avec derniers clips et lecteur, overlay de statistiques.
- **Overlay** : FPS (frame gen DLSS / FSR compris, « DLSS » / « DLSS FG » indiqués), 1 % low, temps par image, CPU, GPU, températures, VRAM, RAM ; une ou plusieurs cartes graphiques au choix ; jeux reconnus automatiquement (Steam, Epic, Xbox, Riot, Ubisoft, EA, GOG).
- **Audio** : son du jeu et micro, mélangés ou sur des pistes séparées, volumes, boost et réduction de bruit du micro, vu-mètres.
- **Vidéos** : galerie avec miniatures et lecteur intégré, renommer, supprimer.
- **Pilotes** : détection du GPU NVIDIA, dernière version Game Ready ou Studio, téléchargement depuis `*.nvidia.com` uniquement (taille et signature vérifiées), installation silencieuse.
- **Outils** : nettoyage des fichiers temporaires et caches, nettoyage de la RAM.
- **Mises à jour** intégrées : installateur de la dernière release GitHub, empreinte SHA-256 vérifiée.

## Anti-cheat
Aucune injection, aucun hook, aucune lecture de la mémoire d'un jeu, aucun processus de jeu ouvert. Capture par Desktop Duplication, audio par WASAPI, raccourcis par `RegisterHotKey`, FPS et DLSS par les événements ETW publics (comme PresentMon). Détails : [SPEC.md](SPEC.md).

## Installer
Télécharger `Clipper-Setup-<version>.exe` dans les [releases](https://github.com/LxL-952/clipper/releases) (assistant : contrat de licence, dossier, démarrage avec Windows, FFmpeg via winget ; PresentMon d'Intel inclus pour les FPS). Installation pour l'utilisateur seul, sans droits administrateur.

## Prérequis
- Windows 10 ou 11.
- `ffmpeg` et `ffprobe` dans le PATH (non fournis : licence GPL ; l'installateur propose de les installer avec winget).
- Pour les FPS : mode administrateur (réglage « Démarrer en administrateur », une seule demande Windows).

## Compiler
```
set CARGO_TARGET_DIR=D:\ct
cargo build --release
cargo test --release
```
L'exe : `D:\ct\release\clipper.exe` (environ 4 Mo, interface et logo inclus : `build.rs` dessine le trombone et l'intègre à l'exe).

Installateur (Inno Setup 6) :
```
ISCC.exe /DBin=D:\ct\release installer\clipper.iss
```

## État
Version 1.0.0. Voir [tasks/plan.md](tasks/plan.md) pour ce qui est fait, testé, et ce qui reste.

## Licence
Tous droits réservés, voir [LICENSE](LICENSE). Le code est publié pour être consulté uniquement : pas de copie, de modification, de redistribution ni d'usage commercial sans autorisation écrite. Les versions de l'ancienne numérotation (avant le 10 novembre 2026, jusqu'à la 1.3.1) ont été publiées sous licence MIT ; la numérotation est repartie à 1.0.0 le 11 novembre 2026. Conditions d'utilisation affichées par l'installateur : [installer/conditions.txt](installer/conditions.txt).

## Polices
Interface : Arial de Windows, lue sur le PC et non incluse. Geist et Geist Mono (incluses, de secours et pour les chiffres) : licence SIL Open Font License 1.1.
