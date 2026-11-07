# Spec : Clipper

## Objectif
Application Windows unique (un `.exe`, double-clic) avec deux onglets :
- **Clipper** : buffer de relecture permanent (type Instant Replay), clip instantané, REC continu, audio jeu + micro.
- **Pilotes** : détection du GPU NVIDIA, comparaison avec le dernier pilote officiel, téléchargement + installation silencieuse.

Priorité n°1 : quasi invisible en jeu. Qualité de capture maximale à coût minimal.

## Carte des capacités

| Module | Rôle | Dépend de |
|---|---|---|
| `core` | Processus Rust : tray, hotkeys (`RegisterHotKey`), config JSON, priorité basse, boucle d'événements | — |
| `capture` | Détection encodeurs, pilotage ffmpeg (ddagrab → encodeur matériel → segments `.ts` de 2 s), rotation du buffer | core |
| `audio` | WASAPI loopback (jeu) + entrée (micro), gain, mute, niveau, envoi PCM à ffmpeg par named pipe | core |
| `clip-rec` | Clip (`-c copy` des N derniers segments → `.mp4`), REC (conserve les segments puis assemble) | capture, audio |
| `ui` | WebView (wry) créée à la demande : barre Alt+Z + fenêtre réglages, détruite à la fermeture | core, clip-rec |
| `drivers` | `nvidia-smi`, API NVIDIA, téléchargement HTTPS limité à `*.nvidia.com`, `setup.exe -s` | core, ui |

Ordre : core → capture → clip-rec → audio → ui → drivers → mesures.

## Hypothèses (à corriger maintenant sinon je pars dessus)
1. **ffmpeg n'est pas embarqué** : l'app utilise le `ffmpeg.exe` du PATH (ou un chemin réglable). Licence GPL du build installé = pas de redistribution.
2. **Audio** : ffmpeg n'a pas d'entrée WASAPI loopback. Rust capture l'audio (crate `wasapi`) et l'envoie en PCM s16le 48 kHz à ffmpeg via named pipes `\\.\pipe\clipper-*`. Encodage AAC 160 kb/s par piste (coût CPU faible).
3. **Clip et REC ont des réglages indépendants** mais une seule capture si possible :
   - Si REC = mêmes résolution/FPS que le buffer : REC garde les segments du buffer (zéro coût en plus).
   - Sinon : une **deuxième instance ffmpeg** (ddagrab séparé) tourne seulement pendant le REC. Coût GPU affiché en avertissement.
4. **Résolution** : natif (1920x1200 ici) = zéro copie GPU. Autre résolution = `hwdownload,format=bgra,scale=W:H:flags=fast_bilinear,format=nv12` (CPU, avertissement affiché). Les cibles « 1080p/1440p/2160p » gardent le ratio écran (16:10 → 1920x1200, 2304x1440, 3456x2160). Au-dessus du natif = grisé.
5. **FPS** : 30 ou 60 uniquement. `ddagrab framerate=N`, GOP = 2 s (`-g 2N`, keyframe forcée) pour couper proprement les segments.
6. **Débit** (H.264, base 60 fps, ×0,65 à 30 fps, HEVC/AV1 ×0,7) : 1080p 20 Mb/s, 1440p 35 Mb/s, 2160p 70 Mb/s. Curseur qualité ×0,5 à ×1,5. Mode CBR/VBR NVENC, preset p1–p4 selon qualité.
7. **Segments** : dossier `%TEMP%\Clipper\buf`. Rust supprime les segments plus vieux que la durée du buffer (+ ceux réservés par un REC en cours).
8. **Pilotes** : API publique `gfwsl.geforce.com/services_toolkit/services/com/nvidia/services/AjaxDriverService.php` (la même que le site NVIDIA). Le lien retourné doit être `https://*.nvidia.com/...`, sinon refus. Installation = UAC demandé par Windows (pas de contournement).
9. **Raccourcis par défaut** : Alt+X clip, Alt+R REC, Alt+Z barre (conflit NVIDIA signalé ; alternative proposée Alt+Shift+Z). Raccourcis modifiables.
10. Langue de l'interface : français.
11. **Format de sortie réglable** dans les réglages : MP4 (défaut), MKV ou MOV. Un seul réglage, utilisé par le Clip et par le REC. Toujours sans réencodage (`-c copy` depuis les segments `.ts`).

## Ajouts (demande du 2026-11-07)
12. **Emplacements** : deux dossiers choisis avec le sélecteur Windows natif (n'importe quel disque) :
    - dossier des clips et REC (défaut `Vidéos\Clipper`) ;
    - dossier du buffer temporaire (défaut `%TEMP%\Clipper\buf`), pour mettre les ~9 Go/h d'écriture sur un autre disque. L'UI affiche l'espace libre du disque choisi.
13. **Mode étiré (stretch)** : pour qui joue en 4:3 étiré (ex. 1440x1080 sur écran 16:10). Réglage : « Désactivé / Étirer en 16:9 / Étirer en 16:10 ».
    - Quand le jeu passe l'écran en 1440x1080, ddagrab capture du 1440x1080. Le clip est alors étiré à la largeur cible.
    - Méthode retenue : étirement réel `scale` en CPU `fast_bilinear` (lisible partout : Discord, YouTube). Coût affiché.
    - Alternative gratuite non retenue par défaut : métadonnée d'aspect (SAR) seule, ignorée par certains sites.
14. **Capture du bureau (comme NVIDIA « Desktop capture »)** : interrupteur.
    - Activé : le buffer tourne tout le temps.
    - Désactivé : le buffer ne tourne que si la fenêtre au premier plan est un jeu en plein écran (fenêtre qui couvre tout le moniteur). Sinon ffmpeg est arrêté : 0 % GPU sur le bureau.
15. **Statistiques à l'écran (FPS, CPU, GPU…)** : overlay optionnel, éléments au choix dans les réglages :
    - FPS et temps d'image du jeu au premier plan, CPU %, GPU %, températures CPU et GPU, VRAM, RAM.
    - Position (4 coins), taille, opacité.
    - **Sans injection** : fenêtre Win32 native transparente, toujours au premier plan, qui ne capte pas les clics. Pas de WebView : environ 2 Mo.
    - FPS mesurés par les événements ETW de présentation d'images (méthode de PresentMon). Ça demande les droits admin ou le groupe « Utilisateurs du journal de performances » : l'UI le dit clairement.
    - GPU via NVML (`nvml.dll` du pilote) et CPU via PDH. Rafraîchi 2 fois par seconde.
    - **Limite** : invisible en plein écran exclusif (comme la barre). Non capturé dans les clips par défaut (`WDA_EXCLUDEFROMCAPTURE`), réglable.

16. **Plusieurs écrans** (constaté sur la machine) : écran externe 1920x1080 @ 179 Hz branché sur l'AMD 780M + écran interne 1920x1200 @ 144 Hz sur la RTX 4060.
    - Réglage « Écran capturé » : Automatique (écran de la fenêtre au premier plan) ou un écran fixe.
    - Les résolutions proposées dépendent de l'écran choisi.
    - Si l'écran est sur l'AMD, on encode en AMF si NVENC refuse les images de l'autre GPU (mesuré, pas supposé).
17. **Reprise auto de la capture** : ddagrab s'arrête avec `AcquireNextFrame failed: 887a0026` (`DXGI_ERROR_ACCESS_LOST`) dès que le jeu change de mode d'affichage, ou qu'un écran UAC/verrouillage apparaît (constaté en test). Rust relance ffmpeg en moins de 500 ms. Les segments déjà écrits restent dans le buffer.

18. **Option « Puce d'encodage » (PC portables hybrides)** : Automatique / Carte graphique (NVIDIA) / Puce graphique intégrée (AMD 780M, Intel).
    - But : laisser la carte graphique tranquille pour le jeu, et faire encoder le buffer en arrière-plan par la puce intégrée.
    - Mesuré ici : `ddagrab` crée ses images sur la NVIDIA, et AMF refuse ces images directement (`AMF failed to initialise on the given D3D11 device: 4`). Passer par la mémoire système marche : `hwdownload,format=bgra,format=nv12` puis `h264_amf`. Résultat : 1920x1200 à ~57 fps, ~6 % de CPU (total 16 threads), encodeur NVENC à 0 %.
    - Note honnête affichée dans l'UI : NVENC est déjà une puce séparée des cœurs qui font tourner le jeu (impact typique 1–3 % de FPS). Passer sur la puce intégrée libère NVENC, mais coûte ~6 % de CPU et de la bande passante mémoire. Le bon choix dépend du jeu : si le jeu est limité par le CPU, mieux vaut rester sur NVIDIA. Le tableau de mesures final comparera les deux.
    - Automatique = NVIDIA. La puce intégrée n'est proposée que si un encodeur AMF/QSV passe le test réel.

## Ajouts (2e demande du 2026-11-07)
19. **Overlay de stats : affichage au choix**, réglage « Affichage » :
    - Masqué ;
    - Avec raccourci : afficher / masquer, comme NVIDIA (défaut Alt+F, modifiable) ;
    - Toujours visible.
20. **Résolution 720p** ajoutée au Clip et au REC (classes : 720p / 1080p / 1440p / 2160p).
21. **Aperçu après un clip ou un REC (onglet Vidéos)** :
    - Le toast « Clip enregistré » a un bouton « Voir ».
    - Onglet **Vidéos** : grille des clips et REC (miniature, durée, date, taille, Clip ou REC), lecteur intégré (lecture, avance, volume), et les actions Ouvrir le dossier, Renommer, Supprimer (corbeille).
    - Miniature : 1 image JPEG faite par ffmpeg juste après l'enregistrement, stockée dans `%APPDATA%\Clipper\thumbs`.
    - Lecteur : `<video>` de WebView2. H.264 et AV1 se lisent directement. HEVC demande l'extension Windows « Vidéo HEVC » : si elle manque, l'UI le dit et propose « Ouvrir avec le lecteur Windows ».
22. **Onglet Outils : nettoyage du PC**
    - **Fichiers temporaires et caches**. D'abord une analyse qui affiche la taille par catégorie, cochable. Ensuite une confirmation, puis la suppression. Les fichiers en cours d'utilisation sont ignorés.
      - Temp utilisateur `%TEMP%` et Temp Windows `C:\Windows\Temp` (admin).
      - Cache DirectX `%LOCALAPPDATA%\D3DSCache`, cache NVIDIA `%LOCALAPPDATA%\NVIDIA\DXCache` et `GLCache`, cache AMD `%LOCALAPPDATA%\AMD\DxCache`. Avertissement : le prochain lancement de chaque jeu recompile ses shaders, avec quelques saccades le temps d'un chargement.
      - Cache Windows Update `C:\Windows\SoftwareDistribution\Download` (admin, service arrêté puis relancé), Optimisation de distribution, rapports d'erreurs `%LOCALAPPDATA%\CrashDumps` et `C:\ProgramData\Microsoft\Windows\WER`, cache des miniatures.
      - Caches des navigateurs Edge, Chrome et Firefox (dossiers `Cache` seulement, jamais les mots de passe, l'historique ou les cookies). Décochés par défaut.
      - Corbeille : décochée par défaut.
      - **Volontairement absent** : le dossier `Prefetch`. Le vider ralentit le lancement des applications, c'est un mythe des vidéos d'optimisation. L'UI l'explique.
    - **Nettoyer la RAM** (comme « Clean memory » de QuickCPU) :
      - Appel système `NtSetSystemInformation(SystemMemoryListInformation)` : vider les working sets (`MemoryEmptyWorkingSets`), puis la liste standby (`MemoryPurgeStandbyList`).
      - Aucun `OpenProcess`, donc aucun contact avec le jeu.
      - Demande les droits admin à chaque clic (UAC), via un processus séparé `clipper.exe --clean-ram`. L'app principale reste sans droits admin.
      - Affiche la RAM libre avant et après.
      - Note honnête : les apps rechargent ensuite leurs pages, avec un petit ralentissement possible. C'est utile surtout quand la mémoire standby gonfle. Ce n'est jamais automatique en pleine partie.
23. **Onglets finaux** : Clipper · Vidéos · Pilotes · Outils.

## Anti-cheat (priorité absolue : zéro risque de ban)
Principe : Clipper ne touche jamais au processus du jeu. Il utilise seulement des API Windows publiques, les mêmes qu'OBS, Discord ou la Xbox Game Bar.

| Fonction | Méthode | Contact avec le jeu |
|---|---|---|
| Capture vidéo | Desktop Duplication (`ddagrab`), comme OBS « capture d'écran » | Aucun (lit l'écran composé par Windows) |
| Audio | WASAPI loopback (sortie du périphérique) | Aucun |
| Raccourcis | `RegisterHotKey` | Aucun (pas de hook clavier `SetWindowsHookEx`) |
| Barre Alt+Z | Fenêtre séparée au premier plan | Aucun (rien dessiné dans le jeu) |
| FPS | Lecture des événements ETW publics de présentation d'images (DXGI/DxgKrnl), comme PresentMon et l'Xbox Game Bar | Aucun (pas d'ouverture du processus, pas de lecture mémoire) |
| CPU / GPU / temp | PDH + NVML | Aucun |
| Détection plein écran | Taille de la fenêtre au premier plan (`GetForegroundWindow`) | Aucun |

Interdits dans le code (vérifiés en revue avant livraison, avec recherche dans le source) : `OpenProcess` sur un autre processus, `ReadProcessMemory`, `WriteProcessMemory`, `CreateRemoteThread`, `SetWindowsHookEx`, injection DLL, hook DirectX/Vulkan, pilote noyau, `SendInput` vers le jeu.

Précautions en plus :
- Pas de blocage par jeu : l'overlay et la barre marchent partout, Valorant compris. L'overlay est une fenêtre Windows séparée, comme celles de Discord ou de la Xbox Game Bar. Elle n'injecte rien.
- Le nettoyage de la RAM n'ouvre **aucun** processus (voir 22) : pas de handle sur le jeu.
- Les avertissements de l'UI disent la vérité : on ne peut pas **garantir** l'absence de ban, car chaque éditeur décide seul. On garantit qu'aucune technique de triche n'est utilisée.

Vérification possible ici : revue du code + recherche des API interdites + test avec des jeux installés. Je ne peux pas lancer de jeu protégé moi-même : ce test sera fait par toi et marqué « non testé » sinon.

## Pile
Rust stable (MSVC) : `tao`, `wry`, `tray-icon`, `global-hotkey`, `serde_json`, `wasapi`, `ureq` (HTTPS, rustls), `windows` (priorité, WS_EX_NOACTIVATE). UI : un seul `ui.html` embarqué (`include_str!`), CSS/JS vanilla, aucun framework.

## Commandes
```
$env:CARGO_TARGET_DIR="D:\ct"
cargo build --release
cargo test
D:\ct\release\clipper.exe
typeperf "\Process(clipper)\% Processor Time" "\Process(clipper)\Working Set - Private" -sc 30
```

## Structure
```
D:\clipper
  Cargo.toml
  src/main.rs       boucle tao, tray, hotkeys, dispatch
  src/config.rs     config.json (%APPDATA%\Clipper)
  src/capture.rs    encodeurs, args ffmpeg, buffer, clip, REC
  src/audio.rs      WASAPI + named pipes
  src/drivers.rs    NVIDIA
  src/ui.rs         fenêtres wry à la demande
  ui/ui.html        barre + réglages
  SPEC.md, tasks/plan.md
```

## Style de code
Minimal (ponytail) : pas d'abstraction sans deuxième usage, pas de crate pour 10 lignes. Erreurs remontées à l'UI en texte clair, jamais de panic silencieux.

## Tests
- Unitaires : calcul résolution/débit/taille, construction des args ffmpeg, sélection des segments pour un clip, parsing réponse NVIDIA.
- Intégration réelle : lancer le buffer 30 s, faire un clip, vérifier avec `ffprobe` (durée, résolution, fps, pistes audio).
- Perf : `typeperf` + `nvidia-smi dmon` au repos et en charge pour 1080p60 / 1440p60 / 2160p60. Impact FPS jeu : mesure avec un jeu ou un benchmark GPU que tu lances (je ne peux pas jouer à ta place, donc marqué « à mesurer par toi » si non fait).

## Limites
- **Toujours** : priorité `BELOW_NORMAL` (app + ffmpeg), encodeur matériel, `-c copy` pour clip/REC, avertir dans l'UI si un réglage dépasse les objectifs.
- **Demander d'abord** : installer un pilote, nouvelle dépendance lourde, écrire hors de `%APPDATA%\Clipper`, `%TEMP%\Clipper`, dossier de sortie.
- **Jamais** : injection DLL, hook DirectX, lecture mémoire d'un jeu, overlay dans le jeu, téléchargement hors `*.nvidia.com`.

## Critères de réussite
- Repos (buffer actif, barre fermée) : CPU app ≈ 0 %, RAM privée app < 30 Mo, 0 processus `msedgewebview2` lancé par l'app.
- Clip Alt+X : fichier `.mp4` lisible en < 1 s, durée = buffer ± 2 s.
- REC Alt+R : compteur visible dans la barre, fichier final assemblé sans réencodage.
- Audio : 0/1/2 pistes conformes au mode, `ffprobe` le confirme.
- Tableau mesuré CPU / RAM / GPU (enc %) pour 1080p60, 1440p60, 2160p60.
- Pilotes : affiche « À jour » ou « Mise à jour disponible (X) », téléchargement avec progression et contrôle de taille.

## Limites honnêtes affichées dans l'UI
- Barre invisible en plein écran exclusif ; clics parfois bloqués si le jeu capture la souris : utiliser les raccourcis.
- Écriture disque ≈ 9 Go/h à 1080p60 (buffer tournant, pas d'accumulation).
- Ordinateur hybride : ddagrab doit tourner sur le GPU qui affiche l'écran ; si l'écran est branché à l'iGPU AMD, le zéro copie NVENC peut échouer → repli automatique.

## Décisions
- Format de sortie : réglage utilisateur commun Clip + REC (hypothèse 11).
- Hypothèse 3 (2e ffmpeg pendant le REC seulement si réglages différents) : retenue par défaut.
