# ADR 0006 — Sorties : étage de sortie, encodage, transport

- **Statut** : **accepté** (mainteneur, 2026-10-29)
- **Date** : 2026-10-29
- **Contexte** : BRIEF §4 (Phase 2), §5 (architecture des sorties), §8 (budgets) ; ADR 0001
  (PTS = f(*n*), perte du GPU, encodeur de secours en CI), ADR 0004 et son amendement 1, ADR 0005
  (sorties en Y'CbCr 709 plage limitée) ; spec [`docs/specs/phase-2.md`](../specs/phase-2.md)

## Décision

### 1. Un étage de sortie unique, calculé une fois par tick
Le thread de rendu ajoute à chaque tick, **seulement si au moins une sortie est active** :
1. **FTB** (fondu au noir) appliqué au Program `Rgba16Float` → cible `out16` (la vue Program de
   l'UI n'est pas concernée, comportement de la référence) ;
2. **conversion R'G'B' → Y'CbCr Rec.709 plage limitée** (ADR 0005), sous-échantillonnage 4:2:0,
   vers deux textures `R8Unorm` (Y) et `Rg8Unorm` (CbCr) : **NV12**, format d'entrée commun aux
   encodeurs matériels et logiciels ;
3. `copy_texture_to_buffer` vers un **anneau de 4 tampons de relecture** `MAP_READ`, puis
   `map_async(Read)` après `queue.submit` (même mécanisme que l'amendement 1 de l'ADR 0004, dans
   l'autre sens) ;
4. la sortie plein écran lit `out16` converti en `Rgba8Unorm` par un triple buffer, comme la vue
   Program de l'UI (invariant ADR 0001 inchangé : seuls le rendu et l'UI soumettent).

Une seule conversion et une seule relecture par tick, **quel que soit le nombre de sorties**.
Si aucun tampon de l'anneau n'est libre (sorties en retard de 3 images), le rendu **ne bloque
pas** : l'image *n* n'est pas relue, compteur `output_dropped` incrémenté (BRIEF §5).

### 2. Thread `vma-output` : distribution sans copie supplémentaire
Le rappel de `map_async` (sur le rendu, qui fait `device.poll`) pose un drapeau atomique. Le
thread `vma-output` récupère les tampons mappés dans l'ordre de *n* (canal borné, `try_send`
côté rendu), copie l'image **une fois** dans un `gst::Buffer` pris dans un pool, rend le tampon
wgpu (`unmap`), et pousse **le même `gst::Buffer`** (compté par références, lecture seule) dans
l'`appsrc` de chaque sortie active. Chaque `appsrc` a une file bornée (`max-buffers`, mode
*leaky* en amont) : une sortie lente perd ses propres images (compteur par sortie) sans
retarder ni le rendu ni les autres sorties.

### 3. Horodatage : tout dérive de *n*
- Vidéo : `PTS(n) = n × den / num` (ADR 0001), en ns, durée = PTS(n+1) − PTS(n). Une image
  manquée au rendu laisse un trou ; le PTS des suivantes ne bouge pas.
- Audio (Phase 2, voir § 5) : le bloc audio de l'image *n* couvre les échantillons
  `[⌊n × 48000 × den / num⌋, ⌊(n+1) × 48000 × den / num⌋)` — exact en arithmétique entière à
  toutes les cadences (1 601 ou 1 602 échantillons à 29,97p, jamais de cumul d'erreur). La
  dérive A/V est nulle par construction dans le moteur ; les tests mesurent qu'elle le reste
  après encodeur, multiplexeur et réseau.
- Les pipelines de sortie tournent **sans horloge** (`sync=false` sur les sinks, `appsrc`
  en `format=time`, `is-live=true`, `do-timestamp=false`) : c'est le moteur qui cadence.

### 4. Un pipeline GStreamer par sortie, isolé
`appsrc (NV12) ─► encodeur vidéo ─┐
                                   ├─► mux ─► sink`
`appsrc (audio F32 48 kHz stéréo) ─► encodeur AAC ─┘`

| Sortie | Mux | Sink |
|---|---|---|
| Enregistrement MKV (défaut) | `matroskamux` (`streamable`, sans index final requis : lisible après coupure) | `filesink` |
| Enregistrement MP4 (option) | `mp4mux` fragmenté (fragments de 1 s : lisible après coupure jusqu'au dernier fragment) | `filesink` |
| Stream RTMP / RTMPS (1 à 3) | `flvmux streamable=true` | `rtmp2sink` (gère `rtmps://`, vérifié sur 1.28.8) |
| SRT caller / listener | `mpegtsmux` | `srtsink` (`mode`, `latency`, `passphrase` optionnelle) |

- Un pipeline **par sortie** (pas de `tee` entre sorties) : une erreur, un EOS ou une
  reconnexion n'affecte que cette sortie. Coût : un encodage par sortie (NVENC : 4 sessions
  pour 1 enregistrement + 3 streams, dans les limites des cartes grand public récentes, à
  vérifier par sortie à l'ouverture et signalé en cas de refus).
- Les pipelines sont créés, démarrés, arrêtés (EOS puis attente bornée, puis `NULL`) sur
  `vma-io` (ADR 0004), jamais sur le rendu ni sur l'UI. Le bus est relayé comme pour les inputs.
- **Reconnexion** des streams (RTMP, SRT caller) : même politique que les inputs (1, 2, 5,
  10 s puis toutes les 10 s), nouveau pipeline, la sortie reste « active, en reconnexion » ;
  l'enregistrement n'est jamais touché. SRT listener : attend un appelant, sans erreur.
- **Enregistrement** : erreur d'écriture (disque plein, droits) → l'enregistrement s'arrête,
  erreur visible, le reste continue. Arrêt normal = EOS, le fichier est finalisé.

### 5. Audio des sorties en Phase 2 : piste générée, verrouillée sur *n*
Le mélangeur audio arrive en Phase 3. En Phase 2, chaque sortie porte une piste **AAC-LC
48 kHz stéréo** générée par le moteur à partir de *n* (§ 3) :
- **silence** par défaut (les plateformes de streaming exigent souvent une piste audio) ;
- **mode synchro** (`--sync-test`) : l'étage de sortie remplace l'image *n* par un blanc plein
  écran quand `n mod période = 0` (période = 1 s arrondie à l'image), et la piste audio
  contient un bip de 1 kHz qui commence exactement au premier échantillon de cette image.
  Un analyseur (test et outil de soak) relit le flux reçu et mesure le décalage flash ↔ bip.

Le passage à l'audio réel (Phase 3) remplace la source de la piste, pas le transport.

### 6. Choix de l'encodeur : échelle de repli, par nom d'élément, sans GPL
Ordre essayé à l'ouverture d'une sortie (l'élément doit exister **et** passer à `READY`) :

| Codec | Ordre | Remarque |
|---|---|---|
| H.264 | NVENC (`nvh264enc` / `nvd3d11h264enc`) → QSV (`qsvh264enc`) → VA-API (`vah264enc`) → Media Foundation (`mfh264enc`, Windows) → **`openh264enc`** (BSD-2, logiciel) | `openh264enc` = repli et encodeur de la CI (accord du mainteneur, ADR 0001) |
| HEVC | NVENC → QSV → VA-API → Media Foundation | **pas de repli logiciel** sans GPL (`x265enc` exclu) : refus explicite si aucun matériel |
| AAC | `avenc_aac` (gst-libav, LGPL) → `voaacenc` (Apache-2.0) → `mfaacenc` (Windows) | `fdkaacenc` non demandé (licence FDK) |

`x264enc`, `x265enc` et `gst-plugins-ugly` ne sont **jamais** demandés (ADR 0004). L'encodeur
retenu et la raison d'un repli sont visibles dans l'UI et les rapports. Réglages par défaut :
voir `phase-2.md` § 6.

### 7. Perte du GPU avec sorties actives (point différé de l'ADR 0001)
**Retenu** (question Q4 de `phase-2.md`, 2026-10-29) : l'horloge continuant
pendant une perte du device (ADR 0001), `vma-output` reçoit du rendu un signal « device perdu »
et pousse à chaque tick une **image noire NV12 pré-allouée** (CPU) avec le PTS de *n* : les
streams continuent, les enregistrements restent cohérents. Les sorties sont ensuite finalisées
proprement (EOS) avant que l'application sorte en erreur, au bout d'un délai réglable (10 s par
défaut) ou sur ordre de l'opérateur. La recréation à chaud du device (et de l'UI) reste un ADR
à part, plus tard.

## Dépendances
Aucune nouvelle crate Rust prévue (`gstreamer`, `gstreamer-app`, `gstreamer-video` déjà là ;
`gstreamer-audio` 0.25, MIT OR Apache-2.0, si l'API des caps audio le demande). Plugins système
nécessaires (LGPL sauf mention), à documenter dans `docs/dev-setup.md` :
- Arch : `gst-plugins-bad` (`srtsink`, `openh264enc`, `nvcodec`), `gst-plugin-va`, `gst-libav`,
  `libopenh264` (BSD-2), `srt` (MPL-2.0) ;
- Windows : installeur MSVC officiel (contient `nvcodec`, `qsv`, `mediafoundation`, `srt`,
  `openh264`) ;
- CI Ubuntu : paquets `gstreamer1.0-plugins-bad` et `gstreamer1.0-libav` ; la présence de
  `openh264enc` et `srtsink` sur les runners est **à vérifier** à l'étape 4 (sinon test marqué
  avec sa raison).
- Tests RTMP : **MediaMTX** (MIT), binaire téléchargé en CI avec somme de contrôle épinglée,
  jamais lié ni distribué.

## Mise en œuvre côté moteur (étape 3, 2026-10-31)
Précisions et écarts, **validés par le mainteneur le 2026-10-31**, à condition que le
comportement observable reste celui de la référence. Les deux écarts ne se voient qu'en
situation dégradée : une sortie qui ne suit plus ne perd que ses propres images (BRIEF §5, B6) ;
une image non relue donne une image tenue une période de plus dans le fichier ou le stream, le
son restant continu. À confirmer sur la référence (questions 14 et 15 de
[`observation-reference-phase2.md`](../manual-tests/observation-reference-phase2.md)) ; si elle
diffère, seul le remplissage audio (`AudioTrack`) change, pas le découpage.
- **Un thread par sortie** (`vma-out-<id>`) avec une file de 3 images, en plus de
  `vma-output` : `vma-output` fait la copie unique et un `try_send` par sortie, et ne bloque
  donc jamais, même si le consommateur d'une sortie bloque (B6). Le § 2 prévoyait une file par
  `appsrc` poussée directement depuis `vma-output` ; un `appsrc` mal réglé (ou un `push` qui
  attend) aurait alors bloqué toutes les sorties. Une sortie qui bloque encore à l'arrêt est
  laissée à elle-même au bout d'une seconde (journalisé), l'arrêt ne pend pas.
- **Image partagée** : `Arc<OutputFrame>` (NV12 tassé + audio), pris dans un pool de 32 images
  au plus, réutilisées quand plus aucune sortie ne les tient. `io-gst` l'enveloppera dans un
  `gst::Buffer` sans copie (`Buffer::from_slice`). Pool vide → image perdue pour toutes,
  comptée (`pool_exhausted`) : une sortie ne doit pas garder plus de quelques images.
- **Audio continu malgré un trou vidéo** : l'image qui suit une image non relue porte les
  échantillons des images sautées (jusqu'à 1 s ; au-delà, reprise sur l'image avec un drapeau
  de discontinuité). Le PTS vidéo, lui, ne bouge pas (§ 3). Évite les trous audio, mal tolérés
  par les encodeurs AAC et les plateformes.
- **Bip de synchro** : 1 kHz, 100 ms, −6 dBFS, sur les deux canaux ; flash et bip toutes les
  `⌈num / den⌉` images (25, 30, 30, 50, 60, 60).
- **Relecture** : le rappel de mappage (`map_buffer_on_submit`) capture un `Arc` : une petite
  allocation par image relue sur le rendu, dans la dette déjà notée ci-dessous. wgpu 30 n'expose
  pas `map_state()` en natif, d'où le rappel.
- Mesure (RTX 5090, 1080p50, mire, une sortie, 20 s) : tick p50 0,22 ms, p99 0,62 ms ;
  enregistrement de l'étage de sortie p99 0,011 ms ; 0 image manquée, 0 non relue.

## Mise en œuvre des pipelines (étape 4, 2026-10-31)
Précisions (détail dans la spec `io-gst.md`, « Phase 2 — Sorties ») :
- **Pas d'`appsrc` *leaky*** (§ 2) : `appsrc` bloquant avec une file courte ; la perte se fait
  dans la file de 3 images de l'étape 3, où elle est comptée. Une erreur du pipeline réveille
  un feed bloqué (EOS).
- **Temps 0 de chaque sortie** à sa première image *n₀* : image *k* = *n* − *n₀* à
  `video_pts_ns(k)`, audio à partir de `audio_sample_at(n₀)`. Une sortie démarrée en cours de
  route commence donc à 0, comme un fichier ou un stream ordinaire.
- **Pas d'images B**, enregistrement compris ; **MP4** fragmenté (1 s) puis finalisé en MP4
  classique à l'arrêt normal (`first-moov-then-finalise`) ; `filesink` non tamponné.
- Le sondage s'arrête à `READY` (§ 6) : un refus à l'ouverture de la session d'encodage (limite
  de sessions NVENC) fait échouer la sortie au premier frame, raison affichée.

## Mise en œuvre des commandes et de l'UI (étape 5, 2026-10-31)
Précisions (détail dans `commands.md`, `ui.md` et `io-gst.md`, « Phase 2 ») :
- **MKV non `streamable`** (§ 4) : l'arrêt normal écrit la durée et l'index ; la lecture après
  un arrêt brutal reste assurée (B4 inchangé).
- Un **numéro de run** par démarrage relie les événements de `vma-io` au bon démarrage.
- Les 3 destinations de stream sont chacune **RTMP ou SRT** (`phase-2.md` § 1 : 1 à 3
  destinations, SRT en plus) ; à confirmer sur la référence (question 17).

## Mise en œuvre du streaming (étape 6, 2026-10-31)
- **File de 2 s après l'encodeur** (`flvmux` → `queue` → `rtmp2sink`) : la marge réseau porte
  sur des données encodées, pas sur les images partagées du moteur (§ 2, pas d'`appsrc`
  *leaky*).
- Un stream est « en cours » quand du média est parti (> 16 ko), et « perdu » sur erreur, sur
  5 s sans envoi ou sur 10 s sans connexion ; la reconnexion garde le même run.
- La clé n'apparaît dans aucune raison ni aucun log de VMA (masquage des messages de
  GStreamer).

## Mise en œuvre du SRT (étape 7, 2026-10-31)
- Mode et latence dans l'URI de `srtsink` ; reconnexion interne de `srtsink` coupée au profit
  de la politique commune (visible) ; une sortie en écoute ne bloque jamais le moteur
  (`wait-for-connection=false`) et reste « en cours » sans appelant.

## Mise en œuvre de la perte du GPU (étape 9, 2026-11-01)
§ 7 tel quel ; précisions (détail dans `engine.md` et `commands.md`, « Phase 2 ») :
- **Le noir** : à chaque tick où le device est perdu, le rendu envoie `Delivery::Black(n)` à
  `vma-output` (même canal que les tampons relus, si une sortie est attachée) ; `vma-output`
  remplit une image de son pool (allouée une fois) en noir NV12 limité (Y' 16, Cb Cr 128) et
  la distribue avec le PTS, la durée et l'audio de *n*, comme une image relue. Compteur
  `black` de la distribution, dans le rapport de soak.
- **À l'instant de la perte**, les images encore sur le GPU (3 au plus, ≈ 60 ms à 50p) ne sont
  jamais relues : un trou, PTS inchangés et audio continu, comme toute image non relue
  (questions 14–15).
- **`vma-control`** voit la perte par `RenderFeedback::device_lost_at()` : événement
  `DeviceLost { frame }` (journalisé, `State::device_lost`), démarrages refusés
  (`Rejection::DeviceLost`) ; au bout du délai (`EngineConfig::device_lost_grace`, 10 s,
  `--gpu-loss-grace`), `DeviceLostGraceOver` arrête toutes les sorties qui produisent
  (finalisation normale par `vma-io`, état final « à l'arrêt », pas « en erreur »).
- **L'application** garde la fenêtre (figée : elle partage le device) jusqu'à ce qu'aucune
  sortie autre que le plein écran ne soit active, puis la ferme et sort en erreur. Si `vma-io`
  ne confirme pas dans les 10 s qui suivent le délai, elle ferme quand même (la fermeture
  finalise aussi les sorties, étape 5). « Sur ordre de l'opérateur » : fermer la fenêtre, qui
  finalise tout de suite ; l'UI ne peut plus être dessinée pour un arrêt sortie par sortie.
- **Plein écran** : rien à faire, il partage le device perdu (il garde sa dernière image).

## Amorçage des encodeurs AAC (étape 10, 2026-11-01)
Écart au § 3 trouvé par l'analyseur de synchro : un encodeur AAC fait précéder le son de
échantillons d'amorçage (1 024 pour `avenc_aac`, 1 600 pour `voaacenc`) que les décodeurs jouent (rien ne les signale dans nos
conteneurs). Le son des sorties encodées arrivait donc 21,3 ms après l'image. Corrigé dans
`io-gst` : l'audio donné à l'encodeur commence d'autant d'échantillons plus loin, au temps 0
(détail dans `io-gst.md`, étape 10). Les PTS vidéo restent ceux de *n* ; le son joué à
l'instant de l'image *k* est celui de `audio_sample_at(k)`, comme le § 3 le veut.

## Conséquences
- Temps de rendu par tick : + une passe FTB, une passe de conversion 4:2:0 et une copie vers
  tampon (≈ 3 Mo en 1080p) quand une sortie est active ; mesurés par l'histogramme par phase
  existant (nouvelle phase « sortie »).
- Allocations internes à wgpu sur le rendu : `map_async` (une `Box` par tick), à ajouter à la
  dette de l'ADR 0001, comme pour l'amendement 1 de l'ADR 0004.
- Mémoire : 4 × 3,1 Mo (1080p NV12) de tampons de relecture, plus le pool `gst::Buffer`.
- Le zero-copy vers NVENC (interop CUDA / D3D11) reste une optimisation ultérieure : elle
  remplacerait les étapes 3 et la copie de `vma-output`, derrière le même découpage.

## Alternatives examinées
- **Un pipeline unique avec `tee`** vers toutes les sorties (un seul encodage) : moins de
  charge GPU, mais une sortie qui bloque ou qui se reconnecte perturbe les autres, et chaque
  destination ne peut plus avoir ses réglages. Écarté ; on pourra partager un encodeur entre
  sorties de réglages identiques plus tard si la charge le demande.
- **Conversion en NV12 par `videoconvert`** (relire du RGBA) : coût CPU élevé en 1080p50 et
  2× plus de données relues. Écarté (la conversion se fait en shader, ADR 0005).
- **Relecture bloquante** (`map_async` puis attente) sur le rendu : interdit (BRIEF §2.3).
- **Horodatage à l'heure murale** (`do-timestamp=true`) : interdit (ADR 0001).
- **Encodeur x264 en repli** : GPL (ADR 0001, accord pour `openh264enc`).
