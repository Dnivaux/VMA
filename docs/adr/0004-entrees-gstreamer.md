# ADR 0004 — Entrées GStreamer et dépendances de la Phase 1

- **Statut** : accepté (mainteneur, 2026-10-26) ; **amendement 1 accepté le 2026-10-29**
  (upload hors de la `Queue` partagée, en fin de document)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 (upload GPU), §6 (stack), §2.6 (licences) ; spec
  [`docs/specs/io-gst.md`](../specs/io-gst.md)

## Décision

### GStreamer via gstreamer-rs, lié dynamiquement
- Crates `gstreamer`, `gstreamer-app`, `gstreamer-video` **0.25** (MIT OR Apache-2.0, MSRV 1.92
  ≤ notre 1.95). Elles se lient aux **bibliothèques GStreamer du système** (≥ 1.24).
- GStreamer (cœur, base, good, bad, libav) est **LGPL-2.1-or-later**, chargé **dynamiquement** :
  compatible avec un binaire MIT/Apache. `cargo deny` ne voit que les crates Rust ; la règle est
  donc écrite ici et dans `docs/dev-setup.md`.
- **Aucun plugin GPL** n'est requis ni demandé par nom (pas de `x264enc`, pas de
  `gst-plugins-ugly`). Le jour où l'on embarque GStreamer dans un installeur Windows, la liste des
  plugins embarqués fera l'objet d'un ADR de packaging (exclusion des plugins GPL).
- `io-gst` est une dépendance **obligatoire** (pas une feature) : sans GStreamer, pas de
  capture ni de fichier. L'application démarre quand même si l'initialisation de GStreamer
  échoue : seuls les inputs « couleur » et « image » restent disponibles, avec un message.

### Un thread `vma-io` possède les pipelines
Création, changements d'état, seeks et redémarrages des pipelines peuvent bloquer : ils vivent
sur un thread `vma-io` piloté par `vma-control` (ADR 0003) via un canal. Les messages du bus
sont relayés par un *sync handler* (sur les threads GStreamer, sans traitement lourd) vers
`vma-io`, qui en fait des événements pour `vma-control` (`Live`, `SignalLost`, …).

### Upload : sur le thread de streaming, dans un triple buffer de textures
*(Remplacé par l'amendement 1.)* Le callback `new-sample` de l'appsink (thread de streaming GStreamer) écrit les plans de l'image
dans la texture « back » de l'input (`queue.write_texture`, même `Queue` que le rendu) puis
publie. **Même invariant que le triple buffer Program (ADR 0001)**, avec le rendu comme
consommateur : il ne change d'image qu'en début de tick, avant d'encoder. Le zero-copy
(interop CUDA / Vulkan / D3D11) reste une optimisation ultérieure, derrière un trait.

### Autres dépendances
| Crate | Licence | Usage |
|---|---|---|
| gstreamer, gstreamer-app, gstreamer-video 0.25 | MIT OR Apache-2.0 | entrées (`io-gst`) |
| serde 1, serde_json 1 | MIT OR Apache-2.0 | état, commandes, journal (BRIEF §6) |
| image 0.25 (PNG, JPEG) | MIT OR Apache-2.0 | input « image fixe » (déjà en dev-dépendance) |
| rfd 0.17 | MIT | sélecteur de fichier natif (portail XDG sous Linux) — *question 6* ; features choisies pour ne pas tirer de runtime async lourd, `cargo deny` fera foi |

### CI
- Ubuntu : `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`,
  `gstreamer1.0-plugins-{base,good}`, `gstreamer1.0-libav` par apt.
- Windows : installeurs MSVC officiels runtime + development (msiexec silencieux), mis en cache.
- Les tests `io-gst` utilisent uniquement `videotestsrc` et des fichiers générés pendant le test
  (`videotestsrc ! … ! matroskamux`, plugins base / good) : rien de propriétaire, rien de GPL.

## Conséquences
- Temps de CI en hausse (installation de GStreamer sous Windows).
- Prérequis de dev : GStreamer dev + `pkg-config` (déjà documentés dans `docs/dev-setup.md`).
- Le crate `io-gst` porte aussi l'input « image fixe » (décodée par `image`, pas par
  GStreamer) pour garder un seul propriétaire du thread `vma-io` et des producteurs d'images.

## Alternatives écartées
- **FFmpeg direct** (bindings `ffmpeg-next`) : capture multiplateforme à réécrire, et le BRIEF
  impose GStreamer.
- **Upload sur le thread de rendu** : copie de 3 Mo par input et par tick dans le chemin chaud.
- **Conversion systématique par `videoconvert` en RGBA** : coût CPU élevé à 4 × 1080p50 ; la
  conversion YUV → RGB se fait en shader (BRIEF §5), `videoconvert` ne sert que pour les
  formats non pris en charge.

---

## Amendement 1 — Upload hors de la `Queue` partagée (proposé le 2026-10-28, accepté le 2026-10-29)

### Constat
Rapports [`2026-10-27-phase1-render-cpu-0-input.md`](../../tests/soak/reports/2026-10-27-phase1-render-cpu-0-input.md)
et [`…-4-inputs.md`](../../tests/soak/reports/2026-10-27-phase1-render-cpu-4-inputs.md), puis
6 soaks de 30 s du 2026-10-28 (1080p50, 4 sources synthétiques 1080p50 NV12, RTX 5090, Vulkan) :

| Configuration | Tick complet p50 | p99 | max |
|---|---|---|---|
| Sans input (2026-10-27) | 0,09 ms | 0,16 ms | 0,49 ms |
| 4 inputs, `write_texture` actuel, 3 runs | 0,17 · 7,1 · 7,3 ms | 5,8 · 9,4 · 10,0 ms | 9,9 · 9,6 · 11,0 ms |
| 4 inputs, essai `write_buffer_with` (copie hors verrou, voir plus bas), 3 runs | 2,6 · 2,9 · 2,2 ms | 4,7 · 5,0 · 5,2 ms | 7,3 · 6,6 · 6,6 ms |

- Le thread de rendu **attend** : il ne consomme que ≈ 1,4 ms de CPU par tick (`schedstat`).
- **Cause identifiée** dans le code de wgpu-core 30.0.1 (`device/queue.rs`) : `Queue::write_texture`
  prend le mutex `pending_writes` **avant** d'allouer le tampon de transfert et d'y copier
  l'image (≈ 3 Mo en 1080p NV12), et le garde jusqu'à la fin. `queue.submit` et
  `queue.write_buffer` (uniforms, pendant l'encodage) prennent ce même mutex : le rendu attend
  derrière la copie de chaque producteur.
- Le résultat est **bimodal** d'un run à l'autre (p50 0,17 ms ou ≈ 7 ms) : des sources 50p
  sont en phase avec le maître 50p, et le décalage de phase tiré au démarrage décide si les
  copies tombent ou non sur le tick. Un soak unique peut donc passer à côté.
- Budget tenu aujourd'hui (0 image manquée en A1), mais la marge (20 ms à 50p, 16,7 ms à 60p)
  fond avec la résolution (4K = × 4 par image) et le nombre d'inputs, et la Phase 2 ajoute les
  readbacks des sorties sur la même `Queue`.

### Décision : tampons de transfert mappés en permanence, copie enregistrée par le rendu
1. Chaque emplacement du triple buffer d'un input (`InputFrame`) reçoit, en plus de ses
   textures, **un tampon de transfert** `MAP_WRITE | COPY_SRC` (mémoire visible de l'hôte),
   lignes alignées sur `COPY_BYTES_PER_ROW_ALIGNMENT` (256 octets), créé mappé
   (`mapped_at_creation`) hors du thread de rendu.
2. **Producteur** (thread de streaming GStreamer, chargeur d'image, source de test) : copie les
   plans dans le tampon **mappé** de son emplacement arrière (`memcpy` ligne par ligne, aucun
   verrou de wgpu tenu), `unmap()` (chemin sans verrou de la `Queue`, vérifié dans
   `resource.rs`), marque l'emplacement « à copier », publie. **Il n'appelle plus la `Queue`.**
3. **Thread de rendu**, en début de tick, pour chaque input qui a pris une image nouvelle
   marquée « à copier » : `encoder.copy_buffer_to_texture` (un par plan) dans **son propre
   encodeur**, avant les passes de conversion ; après `queue.submit`, `map_async(Write)` sur ces
   tampons. Le rappel (sur le thread qui fait `device.poll`, donc le rendu) ne fait que poser
   un drapeau atomique « mappé ».
4. Si une image arrive alors que le tampon de l'emplacement arrière n'est pas encore re-mappé
   (GPU en retard d'au moins un tick), le producteur **ne bloque pas** : l'image est sautée et
   comptée dans un nouveau compteur `upload_busy` des statistiques de l'input (affiché, et
   vérifié à 0 dans les soaks).
5. Les écritures ponctuelles hors cadence (création d'un `InputFrame`, couleur, uniforms
   d'un input, image fixe chargée une fois) peuvent rester sur `queue.write_*`.

Propriété des tampons, passée par atomiques : *mappé* (au producteur) → *rempli* (publié) →
*copie soumise* (au rendu) → *mappé* (rappel de `map_async`). Le triple buffer d'indices et
son invariant ne changent pas ; l'invariant de l'ADR 0001 (une seule `Queue`, exécution dans
l'ordre de soumission) devient **plus simple** : seuls le rendu et l'UI soumettent, et les
textures d'un input ne sont plus écrites que par le rendu, dans l'ordre de ses propres
soumissions.

### Coûts et limites
- **Mémoire** : 3 tampons de transfert par input, en mémoire hôte mappée : ≈ 3,3 Mo chacun en
  1080p NV12 (≈ 10 Mo par input), ≈ 12,4 Mo en 2160p. Les textures restent en VRAM comme
  aujourd'hui.
- **Une copie GPU de plus** (tampon → texture, faite aujourd'hui par wgpu de la même façon dans
  ses `pending_writes`) : même travail GPU, simplement enregistré par nous.
- **Allocation interne à wgpu sur le thread de rendu** : `map_async` met le rappel dans une
  `Box` (une par image nouvelle et par input, ≈ 200 / s à 4 × 50p) et prend brièvement le verrou
  du suivi de vie (`lock_life`), déjà pris par `submit` / `poll`. À ajouter à la dette
  « allocations internes à wgpu » de l'ADR 0001.
- **`unsafe`** : aucun (API publique de wgpu : `get_mapped_range_mut`, `unmap`, `map_async`).
- Le `memcpy` GStreamer → tampon reste sur le thread du producteur (même coût CPU qu'aujourd'hui,
  mais hors verrou). Supprimer cette copie (pool de tampons GStreamer alloué dans notre
  mémoire mappée) ou passer au zero-copy (interop CUDA / Vulkan / D3D11) reste une
  optimisation ultérieure, derrière le même découpage.

### Critères d'acceptation de l'amendement
- 4 × 1080p50 en 1080p50 : **tick complet p99 ≤ 1 ms** et max ≤ 3 ms, sur **3 soaks de 2 min**
  (pour couvrir les deux régimes de phase), `upload_busy` = 0, 0 image manquée / répétée.
- Même mesure à 60p avec 4 sources 60p, et un run 4 × 2160p30 en 2160p30 (informatif, pour
  la Phase 2).
- Inchangés et verts : golden `golden_frame` (A7, chemin d'upload réel, plus seulement
  `write_texture`), `hot_add_and_remove_without_missed_frames` (A12), `triple_buffer_gpu_stress`,
  tests `io-gst`, en CI (lavapipe, WARP) et sur RTX 5090 + Intel ARL.
- Un nouveau test : un producteur qui publie plus vite que le GPU ne rend les tampons →
  images sautées comptées dans `upload_busy`, jamais de blocage ni d'image corrompue.

### Résultats de l'implémentation (2026-10-29, RTX 5090, Vulkan)
Soaks de 2 min dans l'application réelle, 4 sources synthétiques NV12 : **0 image manquée,
0 répétée, `upload_busy` = 0** partout ; le thread de rendu ne se bloque plus (≈ 1 changement
de contexte par tick, celui de l'horloge). Critère de temps : **tenu hors phase, pas en phase**.

| Configuration | Tick p50 | p99 | max | Rapport |
|---|---|---|---|---|
| 1080p50, 4 × 1080p50, 3 runs | 1,57 · 1,38 · 1,44 ms | 3,67 · 3,80 · 3,41 ms | 6,0 · 9,4 · 7,4 ms | `2026-10-29-phase2-upload-1080p50-run{1,2,3}.md` |
| 1080p60, 4 × 1080p60 | 0,23 ms | 0,49 ms | 2,2 ms | `…-1080p60.md` |
| 2160p30, 4 × 2160p30 (informatif) | 0,28 ms | 0,89 ms | 2,4 ms | `…-2160p30.md` |

Essais de 20 s (non commités) pour trouver la cause du cas 1080p50 :
- maître et sources de cadences différentes (la phase glisse) : p50 ≈ 0,25 ms, p99 1,9 à 3 ms ;
  50p / 50p : p50 1,1 ms ; 60p / 60p : p50 0,2 ms. Le coût apparaît quand les copies des
  producteurs tombent pendant le tick ;
- le thread de rendu **calcule** au lieu d'attendre (`schedstat` : ≈ 2 ms de CPU par tick,
  attente active de l'horloge comprise) ; toutes ses phases (poll, encodage, submit) sont ≈ 5×
  plus lentes ;
- copie supprimée : p50 0,16 à 0,22 ms, p99 0,34 à 0,43 ms ; **même copie vers de la RAM
  ordinaire** : p99 0,31 à 0,36 ms. La cause est la **destination** : wgpu-hal place tout tampon
  `MAP_WRITE` en `CpuToGpu`, que gpu-allocator met de préférence en mémoire GPU visible de
  l'hôte (ReBAR, 32 Go sur cette carte). 4 × 3,1 Mo écrits à travers le PCIe en rafale ralentissent
  les écritures du pilote faites par le rendu (tampons de commandes, transferts de
  `write_buffer`, dans la même mémoire).

Ce placement n'est pas réglable par l'API sûre de wgpu. Pistes, plus tard : mémoire hôte
importée (`VK_EXT_external_memory_host`, `unsafe`), zero-copy, ou étaler les copies dans le tick.
Sous DX12, `MAP_WRITE` va dans le tas `UPLOAD` (RAM système) : comportement à mesurer sous Windows.

### Alternatives examinées
- **`queue.write_buffer_with` vers un tampon GPU par emplacement**, copie vers la texture par le
  rendu : la copie se fait hors verrou, mais wgpu alloue un tampon de transfert à chaque image
  et reprend `pending_writes` pour enregistrer la copie. **Essayé** (correctif jetable, non
  commité) : p50 ÷ 3, mais p99 toujours ≈ 5 ms (tableau ci-dessus). Écarté au profit de la
  décision retenue, qui ne touche plus du tout la `Queue` côté producteur.
- **Upload sur le thread de rendu** (`write_texture` en début de tick) : déjà écarté (copie de
  ≈ 3 Mo par input et par tick dans le chemin chaud).
- **Un thread d'upload dédié** qui fait les `write_texture` : même mutex, même contention.
- **Un second `Device`** pour les uploads : pas de partage de textures sans copie ni interop,
  déjà écarté par l'ADR 0001.
- **Zero-copy (interop)** : dépend de la plateforme et du décodeur, `unsafe`. C'est l'étape
  suivante possible, pas un remplacement de celle-ci.
