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
