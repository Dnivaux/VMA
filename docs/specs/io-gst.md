# Spec — `io-gst` : inputs (Phase 1)

- **Statut** : validé (2026-10-26) ; lecture vidéo ajoutée par l'alignement sur la référence
  ([`phase-1.md`](phase-1.md) §7, validé le 2026-10-26)

## Types d'input
| Input | Pipeline / source | Notes |
|---|---|---|
| Capture (webcam, carte) | élément créé par `gst::DeviceMonitor` (classe `Video/Source`) → `decodebin` si compressé (MJPEG) → caps → appsink | Linux : V4L2 (les doublons PipeWire sont masqués) ; Windows : Media Foundation (`mfvideosrc`) |
| Fichier vidéo | `uridecodebin` → vidéo → caps → appsink ; audio vers `fakesink` (Phase 3) | lecture / pause, redémarrage, position ; **Loop** (désactivé par défaut) : boucle sans trou par seek de segment, puis seek non-flush à 0 sur `SEGMENT_DONE` ; sans Loop, la dernière image reste affichée à la fin |
| Image fixe | décodée une fois par `image` (PNG, JPEG) sur `vma-io`, uploadée une fois | pas de pipeline |
| Couleur unie | aucune ressource : couleur passée au shader | — |
| Source de test (`InputSpec::Synthetic`, étape 9) | `videotestsrc is-live=true pattern=ball`, NV12, à la cadence / résolution demandées | CI, soaks (`--inputs`), tests de cadence |

**Formats acceptés à l'appsink** (pas de conversion CPU) : `NV12`, `I420`, `YUY2`, `UYVY`,
`RGBA`, `BGRA`. Tout autre format passe par `videoconvert` vers `NV12` (compté dans les stats).
**Colorimétrie** lue dans les caps (matrice 601 / 709, plage limitée / pleine) ; si absente :
709 pour une hauteur ≥ 720, sinon 601 ; plage limitée pour du YUV, pleine pour du RGB.

**Mode de capture** : choisi automatiquement parmi ceux annoncés par le périphérique :
résolution du Program si possible (sinon la plus grande inférieure), cadence la plus proche
au-dessus de la cadence maître, format brut préféré au MJPEG si la cadence le permet. Le mode
choisi est affiché dans l'UI.

## Transport des images
- Par input, 3 jeux de textures (un par plan : NV12 = Y `R8` + UV `Rg8` ; I420 = 3 × `R8` ;
  YUY2 / UYVY = `Rgba8` de largeur w/2 ; RGBA / BGRA = `Rgba8` / `Bgra8`), créés quand les
  caps sont connues par `FeedRegistry::create` (appelé par `vma-io`, hors rendu) puis attachés
  par `vma-control` (spec `engine.md`, étape 6).
- Callback `new-sample` (thread de streaming) : copie de chaque plan dans le **tampon de
  transfert mappé** du slot « back » (`InputFrame::stage`, aucun appel à la `Queue`, ADR 0004
  amendement 1), métadonnées en atomiques (PTS source, heure d'arrivée monotone, n° d'image
  source), puis publication (triple buffer d'indices). Le thread de rendu enregistre la copie
  tampon → textures au premier tick rendu qui prend l'image, puis remappe le tampon. Un slot
  dont le tampon n'est pas encore remappé (GPU en retard d'au moins un tick) : image sautée,
  comptée dans `upload_busy` (Statistiques, rapports de soak, doit rester à 0).
- `appsink` : `max-buffers=1`, `drop=true` ; `sync=true` pour un fichier (lecture à vitesse
  réelle), `sync=false` pour une capture ou une source live (latence minimale).
- **Changement de caps** en cours de route (résolution, format) : nouvelles textures demandées
  à `vma-control` ; en attendant, l'input garde sa dernière image.

## Lecture des fichiers vidéo
Commandes `Play`, `Pause`, `Restart`, `SetPosition`, `SetLoop` (spec `commands`) exécutées sur
`vma-io` : pause = pipeline en `PAUSED` (l'image courante reste affichée), lecture = `PLAYING`,
redémarrage et position = seek précis. Position et durée lues 4 fois par seconde sur `vma-io`
et publiées à côté de l'état (`Timelines`, voir « Mise en œuvre (étape 8a) »). À l'ouverture, une vidéo est en pause sur sa première image (elle
démarre avec une transition si l'auto-lecture est active, ou par `Play`).

## Rééchantillonnage sur l'horloge maître
À chaque tick, le rendu prend **la dernière image publiée** de chaque input (BRIEF §5) : une
source plus lente est répétée, une plus rapide voit des images sautées. Stats par input :
images reçues / s, répétées, sautées (écrasées avant d'être vues), latence arrivée → tick
(histogramme, p50 / p99 / max). Le rééchantillonnage par PTS avec une image de latence (moins
de saccades sur une source irrégulière) est une évolution possible, hors Phase 1.

## Signal perdu et reconnexion
- `SignalLost` si : erreur du pipeline, EOS d'une capture, ou aucune image depuis
  `max(500 ms, 10 périodes de la source)`.
- Le rendu affiche du **noir** à la place de l'input (*question 3* de `phase-1.md`) ; l'UI montre
  l'état et le nombre de tentatives.
- Reconnexion automatique, sans fin : pipeline détruit puis recréé après 1 s, 2 s, 5 s, puis
  toutes les 10 s. Un périphérique de capture qui réapparaît est reconnu par son identifiant
  stable (chemin du périphérique).
- Aucune erreur GStreamer ne remonte en panique : tout passe en `IoEvent` typés (`thiserror`).

## Interface (esquisse)
```rust
pub struct IoRuntime;            // possède le thread vma-io
impl IoRuntime {
    pub fn start(events: Sender<IoEvent>) -> Result<Self, IoError>;  // échec => GStreamer indisponible
    pub fn devices(&self) -> Vec<CaptureDevice>;                      // nom affiché + DeviceId stable
    pub fn open(&self, input: InputId, spec: &InputSpec, sink: FrameSink) -> Result<(), IoError>;
    pub fn close(&self, input: InputId);
}
pub enum IoEvent { Caps { input, caps }, Live { input }, SignalLost { input, reason }, … }
```
`FrameSink` est fourni par `engine` (textures + indices + métadonnées) ; `io-gst` ne connaît pas
le compositeur.

## Mise en œuvre (étape 7 du plan, 2026-10-27)
- `IoRuntime::start(port)` : `gst::init` puis thread `vma-io`, branché sur le moteur par
  `RenderEngine::take_io_port()` (`IoPort` : demandes de `vma-control`, envoi d'`IoEvent`,
  `FeedRegistry`). `IoRequest::Open` porte désormais la spec de l'input. Sans GStreamer
  (init en échec) : le runtime démarre quand même, les images fixes marchent, capture et
  fichier passent en « signal perdu » (raison journalisée) ; `gstreamer_error()`.
- **Image fixe** : décodée sur `vma-io` (`still`), feed RGBA créé, image poussée une fois ;
  fichier absent ou illisible → `SignalLost` (sans nouvelle tentative).
- **Pipeline** (`pipeline.rs`) : `<source> ! videoconvert ! appsink` (caps = les 6 formats,
  `max-buffers=1`, `drop=true`, `sync` selon la source). Le callback `new-sample` (thread de
  streaming) lit le format négocié (colorimétrie des caps, sinon la règle par défaut),
  crée le feed au premier format ou à un changement, et pousse les plans (*strides* de
  GStreamer). `videoconvert` reste en passthrough pour les formats acceptés ; sinon la
  conversion est détectée (exposée dans les stats avec l'étape 8).
- **Perte** : erreur du bus ou EOS (relayés par un *sync handler*), ou silence (aucune image
  après 5 s au démarrage, puis plus de `max(500 ms, 10 périodes)`) pour les sources live.
  **Reconnexion** (`PipelineSlot`) : pipeline détruit, recréé après 1, 2, 5, puis 10 s ;
  le compteur repart de zéro dès que des images arrivent ; chaque redémarrage crée un nouveau
  feed, donc l'input repasse `Live` par `Event::InputLive`. `Event::SignalLost` →
  état « signal perdu » (première image de perte conservée, tentatives), rendu en noir.
- Les pipelines ne sont ouverts en production qu'avec la capture et les fichiers (étape 8) ;
  à l'étape 7 ils sont testés avec `videotestsrc`. `TestSource` reste la mire du compositeur
  (spec `commands.md`). Le critère **A9** (fichier en boucle) passe à l'étape 8 avec
  l'input fichier.

## Mise en œuvre (étape 8a, 2026-10-27) : fichiers vidéo
- **Pipeline** : `uridecodebin` (chemin rendu absolu, puis URI) ; le premier flux vidéo va vers
  `videoconvert ! appsink` (`sync=true`), tout autre flux (audio, 2ᵉ vidéo) vers un `fakesink`
  cadencé par l'horloge (audio : Phase 3). Pas de *watchdog* : un fichier en pause ou fini se
  tait sans être perdu ; une erreur du bus (fichier illisible, décodeur absent) ou un fichier
  absent → `SignalLost`, puis reconnexion comme les autres inputs. Après une reconnexion, la
  lecture et la boucle demandées sont réappliquées, la position repart de 0.
- **Ouverture** : pipeline en `PAUSED` ; l'image de *preroll* (callback `new-preroll` de
  l'appsink) est poussée dans le feed : l'input est live, en pause sur sa première image. Au
  premier `ASYNC_DONE`, `vma-io` fait le premier seek de segment (à 0, ou à une position
  demandée avant), puis passe en `PLAYING` si `Play` a déjà été demandé. Une image de preroll
  n'est pas poussée deux fois (même PTS au passage en lecture, ou nouveau preroll au même
  endroit).
- **Boucle** : tous les seeks sont des seeks de *segment* (`SEGMENT | ACCURATE`, `FLUSH` sauf
  pour la boucle). À la fin, `SEGMENT_DONE` est traité dans le *sync handler* du bus : avec
  Loop, seek non-flush à 0 lancé aussitôt sur un thread de GStreamer (`call_async`), donc la
  boucle suivante est mise en file derrière les dernières images de la précédente ; sans Loop,
  rien ne vient plus et le feed garde la dernière image. « Fin atteinte » (`ended`) est
  déclarée quand plus aucune image n'arrive depuis `max(100 ms, 3 périodes)` après
  `SEGMENT_DONE` (les images encore en file sont montrées d'abord). `Play` après la fin repart
  du début.
- **N° d'image source** d'un fichier : son temps de flux (PTS ramené au segment) en images,
  arrondi (Matroska stocke les temps à la milliseconde) ; il repart de 0 à chaque boucle.
- **Position et durée** : `query_position` / `query_duration` 4 fois par seconde, publiées
  dans `Timelines` (moteur : `IoPort::timelines` en écriture, `RenderEngine::timelines()` en
  lecture) avec `ended`. Écart à la spec d'origine (« publiées dans l'état ») : dans l'état
  versionné, la position aurait changé la version et rempli le journal 4 fois par seconde et
  par vidéo ; ce qui est commandé (lecture, boucle) reste dans l'état.
- Les fichiers de test sont générés par les tests : MJPEG (`jpegenc`) + Vorbis dans du
  Matroska, des plugins `-base` / `-good` présents en CI sur les deux OS.

## Mise en œuvre (étape 8b, 2026-10-27) : capture
- **Liste des périphériques** (`devices.rs`) : un `gst::DeviceMonitor` (classe `Video/Source`,
  `show-all` pour voir V4L2 même derrière PipeWire) tourne sur `vma-io` tant que le runtime
  vit ; la liste est relue toutes les secondes (les fournisseurs suivent eux-mêmes le
  branchement à chaud) et publiée sans verrou : `IoRuntime::devices()` → `CaptureDevice { id,
  name }`. Une seule API par OS (`device.api`) : `v4l2` sous Linux (les vues `pipewire` et
  `libcamera` des mêmes caméras sont masquées), `mediafoundation` sous Windows. Sans
  fournisseur (ou sans GStreamer) : liste vide, erreur journalisée.
- **Identifiant stable** (`DeviceId`) : `<api>:<clé>`, la clé étant le premier champ présent
  parmi `device.bus_path` (chemin du port USB / PCI : stable au rebranchement sur le même
  port, même si `/dev/videoN` change), `device.path`, `device.id`, `device.serial`. Deux
  périphériques de même clé (deux nœuds de capture sur un port) : `#2`, `#3`… dans l'ordre des
  clés.
- **Ouverture** (à chaque démarrage et reconnexion) : le périphérique est cherché par son
  identifiant dans la liste courante (absent → « not connected » : signal perdu, nouvelle
  tentative selon le *backoff*, donc reconnu dès qu'il revient) ; mode choisi dans ses caps
  (`mode.rs`, règles de « Mode de capture » ci-dessus : en cas d'égalité de cadence, formats
  bruts envoyés tels quels d'abord, puis les autres bruts, puis MJPEG) ; élément créé par
  `Device::create_element`. Pipeline : `<périphérique> ! capsfilter (mode) [! decodebin] !
  videoconvert ! appsink sync=false`, *watchdog* actif (silence = perte, comme les autres
  sources live). Le mode choisi est journalisé et publié avec la conversion
  (`IoRuntime::pipeline_info(input)` → `PipelineInfo { mode, converting }`), affichés par
  l'UI (survol des vignettes, Statistiques).
- Les caps autres que `video/x-raw` et `image/jpeg` (caméras H.264…) sont ignorées en Phase 1.
- **Tests sans caméra** (CI et machine de dev n'en ont pas) : une fausse caméra (sous-classe de
  `gst::Device` dans les tests) annonce des caps de webcam et joue `videotestsrc`, brut ou via
  `jpegenc`. La capture réelle (V4L2, Media Foundation, débranchement) est dans la checklist
  manuelle.

## Critères d'acceptation
| Critère | Test | Où |
|---|---|---|
| `videotestsrc` 1080p50 → images publiées dans l'ordre, au bon rythme (± 2 %) | `pipeline::tests::live_test_source_delivers_in_order_at_its_rate` (± 1 image/s) | CI |
| Chaque format accepté arrive sans `videoconvert` ; un format non listé passe par lui | `pipeline::tests::accepted_formats_arrive_without_conversion` (Y444 converti) | CI |
| Fichier en boucle : ≥ 3 boucles, n° d'image continu modulo la durée, 0 image noire (A9) | `pipeline::tests::looping_file_has_no_gap` (MKV généré, 25 i/s : écart max entre images 40,2 ms en local, seuil 100 ms) | CI |
| Lecture / pause / redémarrage / position ; fin sans Loop = dernière image tenue (A20) | `pipeline::tests::file_playback_controls` ; de bout en bout (commandes → `vma-control` → `vma-io` → rendu) : `tests/runtime.rs::video_file_follows_the_playback_commands` | CI |
| Fichier absent ou illisible → signal perdu | `pipeline::tests::missing_or_unreadable_file_is_lost` | CI |
| Erreur forcée du pipeline → `SignalLost`, puis reconnexion → `Live` (A8) | `pipeline::tests::errors_end_of_stream_and_silence_are_losses` (`identity error-after`, EOS, `valve` fermée), `runtime::tests::lost_pipeline_is_reported_and_reconnected`, `failing_start_counts_attempts_with_backoff` | CI |
| Débrancher / rebrancher la webcam pendant le Program | checklist `docs/manual-tests/phase1-inputs.md` | manuel |
| GStreamer absent → l'application démarre, inputs capture / fichier désactivés avec message | `runtime::tests::without_gstreamer_images_still_work` ; message dans l'UI : `vma-ui` `switcher::without_gstreamer_capture_and_video_are_disabled` | CI |
| Image fixe au Program (fit), image absente = signal perdu, retrait = feed détaché | `tests/runtime.rs` (3 tests, moteur réel) | CI |
| Mode de capture choisi selon les règles ci-dessus | `mode::tests` (6 tests sur des caps de webcam, de caméra virtuelle à plages, H.264 ignoré) | CI |
| Périphérique ouvert dans le mode choisi ; MJPEG décodé sans conversion, brut tel quel | `pipeline::tests::capture_opens_the_chosen_mode` (fausse caméra) | CI |
| Identifiant stable, une API par OS (doublons PipeWire masqués) | `devices::tests` (3 tests) | CI |
| Caméra débranchée → signal perdu, « not connected » tant qu'elle est absente, reconnue par son identifiant au retour (nouveau feed) | `runtime::tests::unplugged_camera_comes_back_by_its_identifier` ; moteur réel : `tests/runtime.rs::capture_of_a_missing_device_is_a_lost_signal` | CI |
| Vraie webcam (V4L2, Media Foundation), débranchement / rebranchement, mode affiché | checklist `docs/manual-tests/phase1-inputs.md` (avec l'UI de l'étape 8c) | manuel |

## Phase 2 — Sorties (étape 4, 2026-10-31)
Module `output` (ADR 0006 § 4–6) : un pipeline GStreamer par sortie, construit et démarré par
`GstOutput::record` (sur `vma-io` à partir de l'étape 5), alimenté par un `OutputFeed`, le
consommateur à attacher à l'`OutputHub` du moteur.

```text
appsrc NV12 (BT.709 limitée, chroma « left ») ─► videoconvert ─► encodeur ─► parseur ─► queue ─┐
                                                                                                ├─► mux ─► filesink
appsrc F32 48 kHz stéréo ─► audioconvert ─► encodeur AAC ─► aacparse ─► queue ────────────────────┘
```
- **Horodatage** : l'image *k* de la sortie (comptée depuis la première image qu'elle reçoit,
  *k* = *n* − *n₀*) a `video_pts_ns(k)` ; l'audio commence à l'échantillon de *n₀*
  (`audio_sample_at(n₀)`, l'audio d'images sautées avant *n₀* est écarté), l'échantillon *s* est à
  *s* / 48 kHz (arrondi au-dessus). Une image non relue laisse un trou d'une période ; l'audio
  reste continu (étape 3, validé). Pipeline **sans horloge** (`use_clock(None)`), `appsrc` en
  `format=time`, `is-live`, sans horodatage automatique : le moteur cadence.
- **Image sans copie** : le NV12 de l'`OutputFrame` partagée est enveloppé dans un `gst::Buffer`
  (`Buffer::from_slice`), avec un `VideoMeta` (plans tassés). L'audio est copié (≈ 8 ko par image).
- **Files** : `appsrc` bloquant au-delà de 2 images (vidéo) et 4 blocs (audio). Une sortie lente
  bloque son propre thread (`vma-out-<id>`) ; le moteur compte les images qu'elle perd (B6).
- **Couleur** (B10) : caps `colorimetry=bt709` (plage limitée) et `chroma-site=mpeg2` ; le
  Matroska et le MP4 les portent dans leur en-tête (relu : `bt709`). Limite : avant GStreamer
  1.26, `matroskamux` n'écrit pas l'implantation de la chroma (Ubuntu 24.04 : 1.24) ; le flux
  H.264 la porte alors seul (VUI, « left » étant la valeur par défaut de H.264).
- **Encodeurs** : échelle de l'ADR 0006 § 6, élément présent **et** passé à `READY`
  (`EncoderChoice` : élément retenu et raisons des rejets, pour l'UI et les rapports). Réglages
  par famille (noms de propriétés propres à NVENC, QSV, VA, Media Foundation, OpenH264), appliqués
  seulement si la propriété existe, jamais de panique sur une valeur refusée. **Pas d'images B**,
  enregistrement compris (pas de délai de réordonnancement, moins d'images partagées retenues).
  Débit d'enregistrement : 40 Mb/s × (débit de pixels / 1080p50), 500 kb/s au minimum ; GOP 1 s ;
  AAC 160 kb/s. Un encodeur refusé seulement à l'ouverture de sa session (limite NVENC) fait
  échouer la sortie au premier frame, raison affichée.
- **Conteneurs** : MKV `matroskamux streamable=true` ; MP4 `mp4mux` fragmenté (1 s) en mode
  `first-moov-then-finalise` : MP4 classique (index complet) après un arrêt normal, lisible
  jusqu'au dernier fragment après un arrêt brutal. `filesink` **non tamponné** : chaque bloc est
  écrit tout de suite (B4).
- **Arrêt** : détacher le feed du hub ; le feed relâché envoie EOS après sa dernière image ;
  `GstOutput::stop(délai)` attend la fin du fichier (EOS forcé à mi-délai si le feed est encore
  attaché), puis `NULL`.
- **Erreur d'écriture** (disque plein, droits) : l'erreur du bus réveille un feed bloqué
  (EOS sur les `appsrc`), `poll()` rend la raison et arrête ce pipeline ; les autres sorties et le
  Program continuent. Dossier absent : refus au démarrage, avec la raison.

### Critères (étape 4)
| Critère | Test (`vma-io-gst`, `output::tests`) | Où |
|---|---|---|
| B3 : fichier relu, PTS de l'image *k* = *k* × den / num (à la ms du Matroska), échantillons audio exacts et continus, trou d'une période pour une image manquée, aux 6 cadences, sortie démarrée en cours de route | `file_carries_the_pts_and_audio_of_n_at_every_master_rate` (vidéo et audio bruts en MKV, indépendants des encodeurs) | CI |
| B3 avec le moteur réel : chaque image relue à *k* × 20 ms, flash blanc et bip au même instant (± 1 ms), aucune image perdue, pool jamais vide | `engine_frames_reach_the_file_with_flash_and_beep_aligned` | CI |
| B4 : MKV décodable jusqu'à ≤ 2 s avant que le processus soit tué | `mkv_survives_a_killed_process` (sous-processus tué après 3 s ; 73 images sur 75 en local) | CI |
| B5 : MP4 lisible jusqu'au dernier fragment après un arrêt brutal ; MKV et MP4 complets (toutes les images, l'audio à un bloc AAC près) après un arrêt normal | `mp4_survives_a_killed_process_up_to_its_last_fragment` (50 / 75 en local), `recordings_are_complete_after_a_normal_stop` | CI |
| B6 (`io-gst`) : un sink qui bloque (50 ms par tampon) perd ses images, comptées ; l'enregistrement voisin n'en perd aucune ; 0 tick manqué ; l'arrêt de la sortie lente ne pend pas | `blocking_sink_loses_only_its_own_frames` | CI |
| B9 : matériel masqué → `openh264enc` (ou refus explicite s'il est absent), raisons listées ; HEVC sans matériel → refus explicite ; rien ne tombe | `encoder::tests` (3), `masked_hardware_falls_back_or_refuses_explicitly` ; NVENC retenu sur la machine de dev | CI + manuel |
| B10 (`io-gst`) : `bt709` et chroma `mpeg2` relus du MKV brut (chroma : GStreamer ≥ 1.26, Windows en CI) ; `bt709` relu des MKV et MP4 encodés | `file_carries_…`, `recordings_are_complete_…` | CI |
| Dossier absent → refus au démarrage ; disque plein → la sortie échoue avec sa raison, l'enregistrement voisin est finalisé | `unwritable_path_is_refused_at_start`, `disk_full_fails_the_output_only` (`/dev/full`, Linux seulement) | CI |
