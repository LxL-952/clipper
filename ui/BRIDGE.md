# Pont UI <-> Rust

Fichiers servis par Rust via le protocole `clipper://app/<chemin>` (embarqués dans l'exe) :
- `ui/index.html` : fenêtre principale (onglets Clipper · Vidéos · Pilotes · Outils), 900x620, sans bordure Windows (barre de titre dessinée en HTML, zone `data-drag` = déplacement).
- `ui/bar.html` : barre Alt+Z, fenêtre transparente 780x80 en haut de l'écran, fond `transparent` sur html/body.
- `ui/fonts/*.woff2` : polices.

## État initial
Avant le chargement, Rust définit `window.__INIT__` :
```js
{
  page: "index" | "bar",
  config: { ...tous les champs de config.json (voir src/config.rs) },
  encoders: ["h264_nvenc","hevc_nvenc","av1_nvenc","h264_amf","hevc_amf","av1_amf","libx264"], // ceux qui marchent
  native: [1920,1200],             // taille de l'écran capturé
  monitors: [{idx:0,w:1920,h:1080,name:"Écran 1"}, ...],
  outputs: [{id,name}], inputs: [{id,name}],   // périphériques audio
  hotkeys: {clip:"Alt+X", rec:"Ctrl+Alt+R", bar:"Alt+Maj+Z", overlay:"Alt+F"}, // réellement enregistrés
  hotkey_conflicts: ["Alt+R","Alt+Z"],         // pris par une autre app
  recording: false, rec_secs: 0,
  gpu: {name:"NVIDIA GeForce RTX 4060 Laptop GPU", driver:"621.01"} | null
}
```

## JS -> Rust
`window.ipc.postMessage(JSON.stringify(msg))` avec `msg` :
| cmd | champs | effet |
|---|---|---|
| `set` | `key`, `value` | change un champ de config, sauvegarde, relance le buffer si besoin |
| `clip` | | enregistre un clip |
| `rec` | | démarre / arrête le REC |
| `open_settings` | `tab?` | ouvre la fenêtre principale (depuis la barre) |
| `close` | | ferme la fenêtre courante (détruite, RAM libérée) |
| `minimize` | | |
| `drag` | | commence le déplacement de la fenêtre (mousedown sur la barre de titre) |
| `pick_folder` | `key` (`out_dir` / `buffer_dir`) | sélecteur Windows natif |
| `videos` | | demande la liste des vidéos |
| `video_action` | `action` (`open_folder`/`rename`/`delete`), `path`, `name?` | |
| `driver_check` | `branch` (`game_ready`/`studio`) | interroge NVIDIA |
| `driver_install` | | télécharge puis installe (après confirmation dans l'UI) |
| `clean_scan` | | analyse les fichiers temporaires |
| `clean_run` | `ids` (liste des catégories cochées) | supprime |
| `ram_clean` | | nettoyage RAM (UAC) |
| `levels` | | demande les niveaux audio (appelé ~15 fois/s seulement quand le vu-mètre est visible) |
| `disk_free` | `path` | espace libre |

## Rust -> JS
Rust appelle `window.__on(msg)` (objet) :
| type | champs |
|---|---|
| `state` | `recording`, `rec_secs` |
| `toast` | `kind` (`clip`/`rec`/`error`), `text`, `path?` |
| `config` | `config` (après `set`) |
| `folder` | `key`, `path`, `free_bytes` |
| `videos` | `items: [{path, name, kind:"Clip"|"REC", bytes, secs, date, thumb}]` (`thumb` = URL `clipper://thumb/<nom>.jpg`, vidéo = `clipper://video/<chemin encodé>`) |
| `driver` | `installed`, `latest`, `branch`, `url`, `bytes`, `status` (`ok`/`update`/`error`), `error?` |
| `driver_progress` | `done`, `total`, `phase` (`download`/`verify`/`install`/`done`/`error`), `error?` |
| `clean` | `items: [{id, name, bytes, files, admin, default, warn?}]` |
| `clean_done` | `freed_bytes`, `skipped` |
| `ram` | `free_before`, `free_after`, `total` |
| `levels` | `game`, `mic` (0..1) |
| `disk` | `path`, `free_bytes` |
