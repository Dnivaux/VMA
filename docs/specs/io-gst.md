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
Exception (Phase 3, étape 6) : l'entrée SRT relâche chaque image juste avant le tick que
donne son temps de flux (« Entrée SRT », tampon de gigue calé sur les ticks).

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
- **Conteneurs** : MKV `matroskamux` *(étape 5 : `streamable=false`, voir plus bas ; à l'étape 4
  `streamable=true`)* ; MP4 `mp4mux` fragmenté (1 s) en mode
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

## Phase 2 — Sorties sur `vma-io` (étape 5, 2026-10-31)
Module `outputs` : `vma-io` exécute `IoRequest::StartOutput { output, run, spec }` et
`StopOutput { output }`, et rend compte de chaque run à `vma-control` (`IoEvent::OutputRunning
{ encoder, target }`, `OutputStopped`, `OutputFailed { reason }`).
- **Enregistrement** : dossier choisi, ou dossier Vidéos de l'utilisateur
  (`glib::user_special_dir(Videos)` : XDG `VIDEOS` sous Linux, dossier connu Vidéos sous
  Windows ; à défaut le dossier personnel), créé s'il manque ; un dossier choisi qui n'existe
  pas fait échouer la sortie (« folder … does not exist »). Nom : `VMA AAAA-MM-JJ HH-MM-SS.mkv`
  (heure locale, `glib::DateTime`), suivi de ` (2)`, ` (3)`… si le nom est pris : jamais
  d'écrasement. Encodeurs annoncés « nvh264enc + avenc_aac » (avec la raison d'un repli).
- **MKV non `streamable`** : avec `streamable=true` (étape 4), `matroskamux` n'écrivait ni la
  durée ni l'index : un fichier arrêté normalement n'avait pas de durée et se parcourait mal dans
  les lecteurs. Désormais l'arrêt normal écrit durée et index (relu : 9,84 s, 491 images pour un
  soak de 10 s) ; un processus tué laisse un fichier lu jusqu'à son dernier bloc complet (B4
  inchangé : 73 images sur 75).
- **Arrêt sans blocage** : le feed est détaché du hub (EOS après sa dernière image), puis
  `GstOutput::poll_stop` est interrogé à chaque tour de `vma-io` jusqu'au fichier finalisé
  (5 s au plus, EOS forcé à mi-délai) ; les inputs continuent d'être servis pendant ce temps.
- **Sortie de l'application** : à l'arrêt de `vma-io` (après le moteur), les sorties en cours
  sont arrêtées et finalisées (5 s au plus en tout) : un MP4 enregistré à la fermeture est
  complet.
- **Activité** publiée 4 fois par seconde (`IoRuntime::output_activity`) : début, octets écrits,
  débit sur la dernière seconde. Une sortie qui s'arrête n'en a plus.
- **RTMP et SRT** : refusés au démarrage avec une raison explicite (« not available yet (Phase 2,
  step 6 / 7) ») jusqu'aux étapes 6 et 7. Reste à faire à l'étape 6 : ne jamais laisser la clé
  passer dans un message d'erreur de GStreamer (l'URL complète de `rtmp2sink` la contient).

### Critères (étape 5)
| Critère | Test (`vma-io-gst`, `outputs::tests`) | Où |
|---|---|---|
| Jalon : le bouton Enregistrer (commandes seules) donne un fichier lisible dans le dossier choisi, encodeur et fichier dans l'état, activité publiée, compteurs du moteur | `record_button_writes_a_readable_file_into_the_chosen_folder` (vrai moteur, vrai encodeur) | CI + local |
| Un enregistrement en cours à la fermeture est finalisé (MP4 lisible en entier) | `a_recording_running_at_exit_is_finalised` | CI + local |
| *(Étape 9)* **B14** de bout en bout : GPU perdu pendant un enregistrement MKV → images noires une période après l'autre jusqu'à la fin du délai, puis finalisé (« à l'arrêt », pas « en erreur ») et lisible jusqu'au bout | `a_recording_through_a_gpu_loss_goes_black_then_is_finalised` (vrai moteur, vrai encodeur, délai 1 s) | CI + local |
| Démarrage refusé (dossier absent, RTMP pas encore là) : seule la sortie échoue, avec la raison ; le Program continue | `refused_starts_fail_the_output_only` | CI |
| Noms de fichier datés, jamais d'écrasement | `file_names_carry_the_date_and_never_overwrite` | CI |

## Phase 2 — Streaming RTMP(S) (étape 6, 2026-10-31)
```text
appsrc NV12 ─► … ─► encodeur H.264 (CBR) ─► h264parse ─► queue ─┐
                                                                 ├─► flvmux streamable ─► queue (2 s) ─► rtmp2sink
appsrc F32 ─► … ─► AAC ─► aacparse ─► queue ─────────────────────┘
```
- **Réglages** (`StreamSettings`, `phase-2.md` § 6) : H.264 à débit constant, 6 Mb/s en
  1080p50, 4,5 Mb/s à 30p et en dessous, proportionnels à la taille d'image (500 kb/s au
  minimum) ; GOP 2 s ; pas d'images B ; AAC 160 kb/s. Adresse `rtmp://` ou `rtmps://`
  (`rtmp2sink` gère les deux), la clé ajoutée comme nom de flux (`adresse/clé`).
- **Marge réseau** : une file de 2 s de données encodées (quelques centaines de ko) entre le
  multiplexeur et `rtmp2sink` absorbe la connexion, un accusé lent ou un à-coup du réseau.
  Agrandir les files en amont aurait retenu des images partagées du moteur (pool de 32).
- **En cours** quand plus de 16 ko de média sont partis (statistiques de `rtmp2sink`), pas au
  lancement du pipeline : l'état reste « Démarrage » tant que le serveur n'a rien reçu.
- **Connexion perdue** : erreur du pipeline (serveur coupé, refus), rien d'envoyé pendant 5 s,
  ou pas de connexion au bout de 10 s. Le pipeline est détruit, le feed détaché, la sortie passe
  « en reconnexion » (`IoEvent::OutputReconnecting { attempt, reason }`), puis un nouveau
  pipeline est construit après 1, 2, 5, puis toutes les 10 s, sans limite (même politique que
  les inputs) ; il recommence à 0 (nouvelle session RTMP). L'enregistrement n'est pas touché.
  Une erreur de construction (pas d'encodeur) reste un échec.
- **Arrêt** pendant une attente de reconnexion : immédiat. Un stream qui ne peut pas finir son
  flux (connexion coupée pendant l'arrêt) est arrêté quand même au bout de 5 s.
- **Secrets** : la clé et l'adresse complète sont retirées de toutes les raisons et de tous les
  logs (`scrub` : les erreurs de GStreamer citent l'adresse) ; `Debug` des réglages et du plan
  ne les montre pas. Reste hors de notre portée : les traces de GStreamer lui-même
  (`GST_DEBUG`), à ne pas activer avec une vraie clé.
- **Débit et octets** des streams : octets envoyés sur la connexion en cours.

### Critères (étape 6)
| Critère | Test (`vma-io-gst`, `outputs::tests`) | Où |
|---|---|---|
| **B7** : 3 destinations démarrées ensemble, chacune relue sous sa clé depuis MediaMTX, 0 image perdue | `three_destinations_stream_at_once` | CI Ubuntu (MediaMTX requis), local avec `VMA_MEDIAMTX` |
| **B7** : serveur coupé → reconnexion (1 s, puis 2, 5, 10 s), de nouveau en cours, relu ; l'enregistrement voisin sans perte ni trou | `lost_server_reconnects_while_the_recording_runs` (local : perdu en 91 ms, de nouveau en cours après 2,0 s) | idem |
| Serveur injoignable : essais 1 puis 2, la clé dans aucune raison, aucun état, aucun log | `unreachable_server_is_retried_and_the_key_never_shows` | CI (les 2 OS) |
| Réglages par défaut, adresse, aucun secret dans `Debug` ; masquage | `output::tests::stream_settings_defaults_and_location`, `secrets_are_scrubbed_from_messages` | CI |

Sans `VMA_MEDIAMTX`, les deux tests MediaMTX sont sautés (message) ; la CI Ubuntu télécharge
MediaMTX v1.21.2 (somme épinglée) et pose `VMA_REQUIRE_MEDIAMTX` : absent, ils échouent. Sous
Windows (pas de MediaMTX en CI), ils sont sautés.

## Phase 2 — SRT (étape 7, 2026-10-31)
```text
… ─► encodeur H.264 (CBR) ─► h264parse config-interval=-1 ─┐
                                                           ├─► mpegtsmux alignment=7 ─► queue (2 s) ─► srtsink
… ─► AAC ─► aacparse ──────────────────────────────────────┘
```
- **Réglages** (`SrtSettings`, `phase-2.md` § 6) : H.264 à débit constant, 8 Mb/s en 1080p50
  (même mise à l'échelle que RTMP), **GOP 1 s** (un récepteur qui arrive décode plus vite ;
  choix non fixé par la spec), AAC 160 kb/s ; MPEG-TS par paquets de 7 × 188 octets (une charge
  utile SRT), SPS / PPS répétés à chaque image clé.
- **Adresse** : `srt://hôte:port?mode=caller|listener&latency=ms`. Le mode doit être dans
  l'URI : sinon, `srtsink` en écoute se lie à `localaddress` / `localport` (7001 par défaut)
  et pas à l'adresse donnée (constaté en 1.28.8). Hôte vide en écoute = toutes les interfaces ;
  IPv6 entre crochets. La **phrase de passe** est une propriété, jamais dans l'URI, retirée de
  tous les messages.
- **Appelant** : « en cours » quand le média part (comme RTMP), reconnexion 1 / 2 / 5 / 10 s
  quand le récepteur disparaît (« Socket is broken or closed ») ou refuse (mauvaise phrase de
  passe : même message générique, `srtsink` ne remonte pas la raison du refus). La
  reconnexion interne de `srtsink` (`auto-reconnect`) est coupée : la nôtre est visible dans
  l'état et les statistiques.
- **Écoute** : « en cours » dès qu'elle écoute (cible affichée « srt://…:port (listening) »),
  sans chien de garde de connexion ni de silence ; tant que personne n'appelle, `srtsink`
  jette les données (`wait-for-connection=false`) : aucune image perdue côté moteur. Un
  récepteur qui part ne la fait pas tomber. Octets envoyés : somme sur ses appelants.
- Port déjà pris (écoute) ou élément absent : échec au démarrage, avec la raison.

### Critères (étape 7)
| Critère | Test (`vma-io-gst`, `outputs::tests`) | Où |
|---|---|---|
| **B8** appelant + phrase de passe : reçu et analysé par un `srtsrc` local (99 images en 2 s à 50p), récepteur parti → reconnexion, revenu → en cours en 2 s, relu | `srt_caller_reaches_its_receiver_and_reconnects` | CI (les 2 OS, SRT exigé) ; local avec `gst-plugins-bad` |
| **B8** mauvaise phrase de passe : refusée (reconnexion), rien de déchiffré, aucune phrase dans l'état | `srt_wrong_passphrase_is_refused_and_never_shown` | idem |
| **B8** écoute : en cours seule, sans perte ; un appelant `srtsrc` reçoit le flux ; son départ ne la fait pas tomber | `srt_listener_waits_for_its_receivers` | idem |
| Réglages, adresse, IPv6, aucun secret dans `Debug` | `output::tests::srt_settings_defaults_and_location` | CI |

## Phase 2 — Analyseur de synchro et amorçage AAC (étape 10, 2026-11-01)
- **`SyncAnalyser`** (`sync.rs`, thread `vma-sync`) : relit une URI (`rtmp://…`,
  `file:///…`) produite avec `--sync-test` par `uridecodebin` **avec décodeurs logiciels**
  (`force-sw-decoders` : un décodeur matériel peut rendre des images en mémoire GPU que
  `videoconvert` ne lit pas, quand un flux en direct démarre avant que la branche soit reliée).
  Images réduites à 32 × 18 en GRAY8 ; **flash** = image presque toute blanche (moyenne ≥ 200,
  minimum ≥ 170), comptée sur sa première image ; **bip** = premier échantillon ≥ 0,25 (moitié
  du niveau du bip) après ≥ 200 ms en dessous. Chaque flash est apparié au bip à ± 500 ms
  près, dans l'ordre où ils arrivent (vidéo et audio d'un fichier sont décodés à leur rythme) ;
  ce qui reste seul est compté. Un flux pris en cours commence sa vidéo à une image clé, après
  son audio : les bips d'avant la première image (à 5 ms près : arrondi de Matroska et
  décalages) ne sont pas comptés. Le son d'un fichier commence par l'amorçage AAC (≤ 40 ms
  de silence), puis éventuellement par la fin d'un bip dont le flash précède l'enregistrement
  (le flash dure une image, le bip 100 ms ; vu en CI) : ni flash ni bip n'est compté avant une
  **coupure** placée à mi-chemin entre deux images après l'amorçage (2,5 images à 50p, 50 ms),
  pour qu'un flash et son bip tombent toujours du même côté.
  Source en direct arrêtée (erreur, fin) → rouverte toutes les secondes ; une
  **interruption** est comptée si des images étaient arrivées. Résultat (`SyncSnapshot`) :
  images, trous (> 1,5 période), flashs, bips, décalages bip − flash, non appariés,
  interruptions, dernière erreur.
- **Amorçage des encodeurs AAC** (trouvé par l'analyseur) : la première trame d'un encodeur
  AAC se décode en échantillons d'amorçage (1 024 pour `avenc_aac`), et aucun décodeur ne les retire (GStreamer
  ne signale ce délai ni en MKV, ni en MP4, ni en FLV). Sans correction, le son des sorties
  encodées arrivait **21,3 ms après l'image** (plus d'une image à 50p). Correction : la sortie
  donne à l'encodeur l'audio à partir de l'échantillon `audio_sample_at(n₀) + amorçage`, à
  partir du temps 0 ; image *k* et échantillon `audio_sample_at(k)` sont ainsi joués ensemble,
  les deux pistes commencent à 0 et les 21 premières ms de son sont perdues (retarder la vidéo
  à la place empêchait de relire au-delà du premier fragment un MP4 tué, B5).
  `encoder::aac_priming`, mesuré par encodeur (bip encodé puis décodé par `avdec_aac`) :
  `avenc_aac` 1 024 (1 025 à l'échantillon près de la détection, Linux et CI), `voaacenc`
  1 600 (CI), `mfaacenc` 0 (CI Windows : l'encodeur le retire lui-même). Sans encodeur audio (MKV brut), rien ne change (B3).

### Critères (étape 10)
| Critère | Test (`vma-io-gst`) | Où |
|---|---|---|
| Appariement flash / bip dans les deux ordres, abandon d'un flash seul, distance max, réouverture ; début d'une piste après l'autre ; paire coupée par le début d'un fichier ; front du bip ; reconnaissance du flash | `sync::tests` (5 tests) | CI |
| Amorçage de chaque encodeur AAC présent = `aac_priming` (± 4 échantillons) : bip encodé puis décodé | `output::tests::aac_priming_matches_the_encoders` | CI (les 2 OS) |
| Enregistrement `--sync-test` (vrais encodeurs) relu par l'analyseur : chaque flash a son bip, décalage ≤ 2 ms (milliseconde de Matroska), aucun trou | `outputs::tests::sync_test_recording_has_its_beeps_on_its_flashes` | CI + local (≤ 0,8 ms) |
| Flux RTMP `--sync-test` relu depuis MediaMTX : 5 flashs appariés, ≤ 2 ms, aucune interruption ni trou | `outputs::tests::sync_analyser_reads_a_live_stream_back` (exécuté seul) | CI Ubuntu (MediaMTX) ; local (−0,6 à +0,4 ms) |

## Phase 3 — Horloge, temps de flux et son des inputs (étape 2, 2026-11-02)
- **Horloge** (ADR 0007 § 1) : chaque pipeline d'entrée utilise l'horloge système GStreamer
  **monotone** (`clock::master_clock`), la même que les `Instant` de l'horloge maître.
  `stream_clock()` apparie un `Instant` et le temps de cette horloge (la plus serrée de 16
  lectures) : le moteur y convertit ses ticks (`vma_clock::StreamClock`). Les pipelines de
  sortie restent sans horloge (cadencés par le moteur, ADR 0006 § 3).
- **Temps de flux d'une image** : `base_time + running_time(PTS)` (`pipeline::stream_time`),
  transmis avec l'image (`PictureOut::push`, `FrameSink::push_timed`) et gardé par le feed
  avec le numéro d'image. Source live : instant de capture ; fichier : instant où l'image est
  à montrer. L'image de *preroll* d'un fichier (pas encore jouée) n'en a pas ; celle d'une
  source live (livrée en *preroll* pendant la lecture) en a un.
- **Son d'un fichier** : le premier flux audio de `uridecodebin` passe par `audioconvert !
  audioresample ! F32LE 48 kHz stéréo ! appsink` (cadencé par l'horloge, comme les images)
  vers l'anneau de l'input (`Feeds::audio`, crate `audio`), chaque tampon daté par son temps
  de flux. Mono dupliqué ; plus de 2 canaux : mixage par la matrice par défaut
  d'`audioconvert` (tous les canaux restent audibles ; écart à la lettre de Q9, « les 2
  premiers », **accepté** par le mainteneur le 2026-11-03). Les autres flux audio vont à un `fakesink`. Sans anneau
  (`Feeds::audio` → `None`), le son est jeté comme avant.
- **Capture** : le son d'une webcam ou d'une carte de capture est un périphérique audio à
  part (ALSA / PipeWire, WASAPI) : il arrive avec les périphériques audio (étape 5).
- **Source de synchro** (`InputSpec::SyncSource`, `sync_source.rs`) : deux `appsrc` (images
  NV12 320 × 180 par défaut, ou `resolution` — Program du soak C14 —, son mono `f32` à sa
  fréquence) dans les mêmes branches qu'une capture
  (`videoconvert ! appsink`, `audioconvert ! audioresample ! appsink`, sans attente
  d'horloge). Un thread pousse chaque image et chaque bloc de 10 ms quand son temps de source
  est venu, daté par son PTS comme une capture ; son horloge avance de `drift_ppm` sur le
  maître. Flash : image blanche toutes les secondes de la source (arrondi à l'image) ; bip
  1 kHz de 100 ms au même instant, `audio_lead_ms` plus tôt. Il attend que le pipeline joue :
  un pipeline live reste `PAUSED` (`PLAYING` en attente) jusqu'au premier tampon de ses sinks,
  son `base_time` est alors fixé.
  **Depuis l'étape 9** : son horloge part de l'origine de l'horloge monotone commune (`drift_ppm`
  plus vite), et non plus de son démarrage : sans dérive, toutes les sources synchro flashent
  sur les secondes entières de cette horloge, quel que soit leur démarrage ;
  `next_sync_flash_ns(rate, drift, t)` donne le prochain flash. Le soak C14 s'en sert pour
  placer ses transitions entre les flashs, et pour compter ceux qu'il met à l'antenne :
  l'analyseur doit en apparier autant, pas un par seconde. Une transition placée après un
  flash de la source quittée et avant un flash de la suivante les rapproche (un de plus),
  dans l'autre ordre les éloigne (un de moins). L'origine étant le démarrage de la machine,
  la phase des sources qui dérivent est quelconque au départ : sur 1 h, le second soak a
  apparié 3522 flashs pour 3585 secondes, le premier 3653, sans flash ni bip seul (simulé
  avec le plan des transitions : −75 à +74 selon la phase). Les deux images (blanche, noire) sont faites une
  fois, chaque envoi partage leur mémoire.

### Critères (étape 2)
| Critère | Test | Statut |
|---|---|---|
| L'horloge GStreamer et `Instant` sont la même horloge (2 appariements à 500 ms : < 200 µs ; prédiction à 1 s : < 1 ms) | `clock::tests::the_pipeline_clock_is_the_master_clock` | CI (2 OS) |
| Images d'une source live datées : 20 ms d'écart, prises moins de 100 ms avant leur arrivée | `pipeline::tests::live_pictures_carry_their_stream_time` | CI |
| Son d'un fichier dans son anneau à 48 kHz stéréo, au temps de ses images (< 5 ms ; mesuré 0) | `pipeline::tests::file_sound_reaches_its_ring_on_the_time_of_the_pictures` | CI |
| Le temps de flux suit la case du feed | `engine` : `feed::tests::each_new_picture_is_uploaded_once` | CI |
| Source de synchro : bip au temps de son flash, ou de son avance plus tôt, dans l'anneau, par le vrai chemin (29,97p, 44,1 kHz → 48 kHz, +100 ppm) : < 2 ms (mesuré 0,2–0,3 ms ; avance 80 ms → −79,8 ms) | `sync_source::tests::the_beep_reaches_the_ring_on_the_time_of_its_flash` | CI |

## Phase 3 — Périphériques audio et casque (étape 5, 2026-11-03)
- **Inventaire** (`audio_devices.rs`) : un second `gst::DeviceMonitor` (classes
  `Audio/Source` et `Audio/Sink`), **sans** les fournisseurs masqués : sous Linux, PipeWire
  masque les vues PulseAudio et ALSA des mêmes cartes (vérifié sur la machine de dev : 4
  sources, 6 sorties, une vue chacune). Identifiant stable : `<device.api>:<node.name>`
  (« alsa:alsa_input.usb-…-00.mono-fallback », gardé d'un branchement à l'autre, contrairement
  à `object.serial`), sinon `device.strid`, `device.id` (point de terminaison WASAPI sous
  Windows), `device.path`. Les moniteurs des sorties (`device.class = monitor`) sont écartés.
  Relu toutes les secondes, comme les caméras ; `IoRuntime::audio_devices()`.
- **Input son seul** (`InputSpec::AudioDevice`, `AudioInputSlot`) : l'élément du périphérique
  (`pipewiresrc`, `wasapi2src`…) ` ! audioconvert ! audioresample ! F32LE 48 kHz stéréo !
  appsink` vers l'anneau de l'input (même branche que le son des fichiers), sur l'horloge
  maître, `appsink` non cadencé (source live). `IoEvent::AudioLive` au premier tampon après
  chaque démarrage ; perdu sur une erreur, une fin de flux, **2 s sans tampon** (5 s au
  démarrage) ; périphérique absent : « audio device … is not connected » ; reconnexion 1 / 2 /
  5 / 10 s, recherché par son identifiant à chaque démarrage, nouvel anneau.
- **Casque** (`headphones.rs`) : `appsrc ! audioconvert ! audioresample ! <sink du
  périphérique>` (`autoaudiosink` sans choix), sur l'horloge maître (le sink s'y asservit).
  Un thread `vma-phones` pousse toutes les 10 ms les nouveaux échantillons de l'anneau du
  casque (horodatés à l'arrivée) ; en retard de plus de 250 ms, il saute aux plus récents.
  `IoRequest::SetMonitor` change de périphérique (pipeline reconstruit) ; absent ou en erreur :
  raison publiée (`IoRuntime::headphones()`), nouvel essai 1 / 2 / 5 / 10 s. **Activé par
  `IoOptions::headphones`** : l'application l'active sauf `--no-headphones` et pendant un
  soak ; les tests ne jouent jamais rien sur les haut-parleurs de la machine.
- **Faux périphériques en CI** : sous-classe de `gst::Device` dont l'élément est un bin
  `gst-launch` (comme la fausse caméra) : `audiotestsrc` (micro, 44,1 kHz mono) ou `appsink` /
  `fakesink` (casque). Point d'entrée de test de `IoRuntime` avec une liste donnée (sans
  moniteur).
- Non vérifié : sous Windows, que `wasapi2` masque bien les vues WASAPI / DirectSound (sinon
  un micro apparaîtrait deux fois) ; à voir avec la checklist sous Windows (⏸).

### Critères (étape 5)
| Critère | Test | Statut |
|---|---|---|
| Identifiant stable : `node.name` avant le numéro de série, point de terminaison WASAPI, moniteurs écartés | `audio_devices::tests::the_node_name_is_the_stable_key` | CI |
| **C12** (micro, CI) : son dans l'anneau, daté sur l'horloge maître ; en marche ; débranché → perdu → « not connected » → rebranché, retrouvé par son identifiant, nouvel anneau | `audio_devices::tests::a_microphone_is_heard_lost_and_found_again` | CI |
| **C12** (casque, CI) : l'anneau joué sur le périphérique choisi (1 s de son reçue, niveau exact) ; débranché → raison, nouvel essai ; rebranché → joue | `headphones::tests::the_headphones_play_the_ring_on_their_device` | CI |
| **C12** de bout en bout (moteur + `vma-io`, faux périphériques) : micro en marche, casque sur le périphérique choisi par commande ; micro au Master ; muet en solo PFL → Master silencieux, casque au niveau du micro | `runtime::tests::a_microphone_and_the_headphones_through_the_engine` | CI |
| **C12** (vrai matériel) : micro, débranchement, casque, solo audible | checklist [`phase3-audio.md`](../manual-tests/phase3-audio.md) | manuel |

## Phase 3 — Entrée SRT (étape 6, 2026-11-03)
- **Réglages** (`InputSpec::Srt(SrtSource)`) : les mêmes que la sortie SRT (mode appelant /
  écoute, hôte, port, latence 20–8 000 ms, phrase de passe vide ou de 10 à 79 caractères),
  validés par la même fonction. Écart de forme à `phase-3.md` § 5 (qui donnait une
  `uri`) : mêmes champs que la sortie, pour un seul formulaire et une seule validation.
- **Pipeline** : `uridecodebin` sur `srt://hôte:port?mode=…&latency=…` (`srtsrc ! tsdemux !`
  décodeurs), la phrase de passe posée sur `srtsrc` au `source-setup` (jamais dans l'URI ni
  un message), `auto-reconnect` coupé : perte → politique commune 1 / 2 / 5 / 10 s. Même
  aiguillage que les fichiers (première vidéo, premier son vers l'anneau, le reste jeté), son
  non cadencé (source live). En **écoute**, l'input attend son appelant sans limite (pas de
  « no picture after 5 s ») ; une fois des images reçues, le silence le perd et il se remet à
  l'écoute.
- **Tampon de gigue calé sur les ticks** (`Pacer`, entrée SRT ; refait le 2026-11-07, voir
  ci-dessous ; étendu aux entrées NDI® et aux inputs live locaux depuis, étapes 7 et 9) : les images d'un flux SRT arrivent par rafales ; avec le choix
  « dernière image arrivée » du rendu, 243 images sur 582 n'étaient jamais montrées (source
  25p dans un maître 50p). Chaque image est due au premier tick à ou après
  `temps de flux + délai`, et relâchée **une demi-période du maître avant ce tick** : le tick
  qui la montre découle de son seul temps de flux, jamais de l'instant où un thread se
  réveille. Le moteur publie la grille de ses ticks en temps de flux (`IoPort::ticks`,
  `vma_clock::TickGrid`). Le temps de flux de l'image (et donc le son) ne change pas.
- **Délai** : il couvre les arrivées les plus tardives (arrivée − temps de flux, mémorisées
  sur 15 à 30 s), plus une période de source (place pour une rafale de deux images), une
  demi-période du maître et 2 ms. Il ne change que sur événement, pour que la latence des
  images (et la synchro) reste fixe : une image arrivée à moins d'un quart de période de son
  tick (en retard) le remet à jour ; une image à plus de 500 ms des arrivées récentes
  (horodatage qui saute, flux figé) repart de zéro (arrivées oubliées) ; quand les arrivées
  permettent un délai plus court d'au moins une période du maître, et qu'il n'a pas changé
  depuis 15 s, il baisse **de périodes entières** (une image sautée, phase des ticks gardée).
  Il peut être négatif : un récepteur qui date ses images par leur présentation les livre
  avant leur temps de flux. Coût : une période de source et une demi-période du maître de
  latence pour cet input, plus la gigue mesurée.
- **Arrivées réelles** : le rappel de l'`appsink` date l'image et la confie (échantillon
  GStreamer, sans copie) à un thread de relâche par input (`vma-pace-<input>`, file de 16
  images), qui la pousse dans le feed à l'heure dite. Une première version attendait dans le
  thread de streaming : l'image suivante ne pouvait alors pas « arriver » avant que la
  précédente soit relâchée, les arrivées mesurées dépendaient de l'attente elle-même, et la
  référence montait en continu (mesuré : décalage son / image qui dérive de −2 ms/s, puis
  saute d'une période).
- **`tsdemux` sans correction de dérive** (`ignore-pcr=true`, posé au `deep-element-added` ;
  avertissement si la propriété manque) : par défaut, `tsdemux` établit la correspondance
  entre l'horloge de l'émetteur et la nôtre d'après les PCR, et la corrige en continu d'après
  les heures d'arrivée ; mesuré, les temps de flux glissent alors de plus de 30 ms sur les
  20 premières secondes (jusqu'à 3 500 ppm, −52 ms au départ en CI), le son avec eux
  (fréquence estimée jusqu'à −900 ppm), et le lissage de la latence du son suit avec retard.
  Avec `ignore-pcr`, les temps viennent des PTS, établis une fois : écart tick − image
  constant au µs, son à 0 ppm. `skew-corrections=false` donne le même résultat mais n'existe
  qu'à partir de GStreamer 1.28 (Ubuntu 24.04 : 1.24) : un seul chemin pour toutes les
  versions. La dérive réelle d'une horloge d'émetteur est absorbée par le tampon de gigue
  (une image sautée ou répétée de temps en temps) et la fréquence du son (ADR 0007 § 4).
- `appsink sync=true` (pacer par l'horloge) essayé et écarté : le pipeline se fige (même avec
  `gst-launch`), les temps du flux ne sont pas alignés sur l'horloge locale.
- **Synchro (C9) : < 1 image.** Mesuré le 2026-11-07 (deux moteurs, sortie SRT en mode
  synchro à 25p → entrée SRT appelante avec phrase de passe → enregistrement 50p relu, 20 s,
  trois essais) : 20 paires flash / bip sur 20, **décalage entre −0,25 et +0,42 ms** ;
  578 images reçues, 0 sautée, chacune montrée sur deux ticks, latence publication → tick
  fixe (9,93 ms, une demi-période). Avant : −30 à +29 ms, avec des sauts d'une période.
- Les autres inputs live gardaient le choix « dernière image arrivée » (dette de la Phase 1,
  étape 9) : le même mécanisme leur sert depuis (entrée NDI®, étape 7 ; inputs live locaux,
  étape 9, « Inputs live calés sur les ticks »).

### Critères (étape 6)
| Critère | Test | Statut |
|---|---|---|
| Réglages validés comme la sortie ; phrase de passe jamais écrite (journal, logs, état) | `engine` : `state::tests::srt_inputs_are_checked`, `control::tests::secrets_never_reach_the_journal_the_logs_nor_the_published_state` | CI |
| **C9** : écoute sans limite ; mauvaise phrase de passe sans image ; la bonne → en marche ; émetteur arrêté → perdu ; revenu → reconnecté (deux moteurs, notre sortie SRT appelante) | `outputs::tests::srt_input_listens_and_reconnects` | CI (si `srtsrc`, comme B8) |
| **C9** : synchro < 1 image (appelant + phrase de passe vers notre sortie SRT en écoute, mode synchro) | `outputs::tests::srt_input_receives_our_srt_output_in_sync` | CI Linux (si `srtsrc`) ; **ignoré sous Windows** (WARP sur 2 cœurs, comme les autres mesures de synchro) |
| Tampon de gigue : chaque image sur le tick de son temps de flux, avec un quart de période de marge, aucune écrasée sans être vue, délai fixe (10 min simulées, rafales de deux), images en avance sur leur temps de flux, source plus rapide que le maître, délai raccourci de périodes entières, retard, saut d'horodatage et flux figé | `pipeline::tests` : `paced_pictures_land_on_the_tick_of_their_stream_time`, `pictures_ahead_of_their_stream_time_are_paced_too`, `a_faster_paced_source_is_resampled_by_stream_time`, `the_delay_shortens_by_whole_periods_and_recovers_from_a_late_picture`, `a_jump_of_the_timestamps_or_a_stall_starts_afresh` ; grille : `vma-clock` `tick_grid_finds_the_first_tick_at_or_after_a_time` | CI (temps simulé) |

## Phase 3 — Entrée NDI® (étape 7, 2026-11-07)
NDI® est une marque déposée de Vizrt NDI AB. ADR 0008.
- **Plugin et runtime** (`vma-io-ndi`) : `gst-plugin-ndi` 0.15.4 (MPL-2.0) enregistré comme
  plugin statique ; le runtime est chargé une fois, au démarrage de `vma-io` (avant les
  moniteurs de périphériques), en amenant un `ndisrc` à READY : `IoRuntime::ndi()` =
  `Available` ou `Unavailable { reason }`. Pas de nouvel essai pendant l'exécution.
  **Écart à l'ADR 0008 § 3** : pas de version du runtime (`Available { version }`) — le
  plugin ne l'expose pas, et l'obtenir demanderait de charger nous-mêmes la bibliothèque
  (`unsafe`) ; la fenêtre « À propos » donne la version du plugin.
- **Découverte** : un `gst::DeviceMonitor` sur la classe `Source/Network` (fournisseur
  `ndideviceprovider`), sur `vma-io`, seulement si le runtime est là ; liste des noms NDI
  (`MACHINE (Source)`) relue toutes les secondes, publiée sans verrou
  (`IoRuntime::ndi_sources()`). Les sources NDI que voit aussi le moniteur des caméras
  (classe `Video/Source`) y sont écartées par le filtre d'API (`device.api`).
- **Input** (`InputSpec::Ndi { name }`, nom par défaut : la partie entre parenthèses) :
  `ndisrc ndi-name=… connect-timeout=5000 ! ndisrcdemux`, mêmes branches que les autres
  inputs (première vidéo vers `videoconvert ! appsink`, premier son vers l'anneau). Source
  réseau : **même tampon de gigue calé sur les ticks que l'entrée SRT**. Source absente du
  réseau → erreur après 5 s → signal perdu → politique commune 1 / 2 / 5 / 10 s, par son nom.
  Mode d'horodatage de `ndisrc` laissé par défaut (`auto`) : à vérifier avec le runtime
  (C11), pour que les temps de flux soient dans le domaine maître.
- **Runtime absent** (C10) : l'input NDI passe aussitôt en « signal perdu » avec la raison,
  **sans nouvel essai** ; l'UI grise « Ajouter → NDI®… » et dit comment installer le runtime
  (survol) ; rien d'autre ne change.

### Critères (étape 7)
| Critère | Test | Statut |
|---|---|---|
| **C10** : runtime détecté une fois ; absent → raison qui dit quoi faire | `vma-io-ndi` `tests::the_runtime_is_detected_once` (exige le runtime avec `VMA_REQUIRE_NDI`) | CI |
| **C10** : runtime absent → input NDI perdu avec sa raison, jamais réessayé ; l'input voisin en marche ; aucune source listée | `runtime::tests::without_the_ndi_runtime_an_ndi_input_is_refused_with_its_reason` | CI |
| **C10** (UI) : « NDI®… » grisé sans runtime, le reste du menu marche ; « À propos » porte la mention de marque et la licence MPL-2.0 | `vma-ui` `switcher::without_the_ndi_runtime_ndi_is_greyed` | CI |
| Ajout d'un input NDI : source découverte choisie, ou nom saisi (épuré) ; rien avant « Ajouter » ; nom par défaut, validation | `vma-ui` `switcher::an_ndi_input_is_added_from_the_sources_seen` ; `engine` `command::tests::ndi_inputs_are_named_after_their_source` | CI |
| **C11** (entrée) : une source NDI locale découverte ; input en marche, nommé d'après elle, à sa cadence (45 à 55 images en 2 s à 25p), avec son ; émetteur arrêté → perdu ; relancé → en marche | `outputs::tests::an_ndi_source_is_found_received_lost_and_found_again` | conditionnel (runtime NDI ; exigé par `VMA_REQUIRE_NDI`) : machine de dev |

## Phase 3 — Sortie NDI® (étape 8, 2026-11-07)
- **Réglages** (`OutputSpec::Ndi(NdiSpec { name })`) : le nom annoncé, « VMA Program » par
  défaut (NDI y ajoute celui de la machine : `MACHINE (VMA Program)`) ; une sortie NDI au plus
  (`Rejection::NdiExists`) ; bus au choix comme les autres sorties (Master par défaut).
- **Pipeline** : `appsrc` NV12 (étage de sortie, BT.709 limitée) et `appsrc` F32 48 kHz du
  bus → `ndisinkcombiner` → `ndisink ndi-name=…`, **sans encodage ni conteneur** ; le NV12 et le
  son entrelacé sont acceptés tels quels. Même cadencement par le moteur que les autres
  sorties (pipeline sans horloge, PTS dérivés de *n*).
- **En marche dès le départ**, comme une sortie SRT en écoute : des récepteurs vont et
  viennent, aucun n'est attendu ; une erreur du pipeline → reconnexion par la politique
  commune. Runtime absent → refus avec la raison (sortie « en échec ») ; l'UI grise
  « + NDI® » avec l'explication.

### Critères (étape 8)
| Critère | Test | Statut |
|---|---|---|
| **C11** : la sortie NDI d'un moteur (mode synchro, 25p) reçue par l'entrée NDI d'un autre (50p) et enregistrée : toutes les paires flash / bip, aucun trou, **synchro < 1 image** (mesuré : 20 / 20, −4,6 ms au premier flash puis ≤ 0,5 ms) | `outputs::tests::our_ndi_output_reaches_an_ndi_input_in_sync` | conditionnel (runtime NDI) : machine de dev |
| Une sortie NDI au plus ; nom vide refusé ; ne change pas de genre | `engine` `state::tests::one_ndi_output_with_a_name` | CI |
| UI : « + NDI® » ajoute la sortie par défaut ; grisé sans runtime | `vma-ui` `switcher::the_ndi_output_is_added_unless_the_runtime_is_missing` | CI |

## Phase 3 — Inputs live calés sur les ticks (étape 9, 2026-11-07)
- **Pourquoi** : premier soak C14 (1 h) en échec, pire écart son − image 20,44 ms. Une
  source synchro 50p à ± 100 ppm, prise « dernière image arrivée », basculait d'un tick à
  l'autre pendant des secondes chaque fois que sa phase passait sur un tick (images
  publiées juste au moment du tick) : 447 sautées et 429 répétées en 1 h, pour 18 attendues.
- **Quoi** : les inputs live locaux (`videotestsrc` synthétique, source synchro, capture)
  passent par le même `Pacer` que les entrées SRT et NDI®, **sans la place d'une rafale**
  (`Pace::local` ; réseau : `Pace::network`, une période de source de plus). Délai : arrivées
  les plus tardives + une demi-période du maître + 2 ms (13 ms mesurés pour une source
  synchro 50p). Chaque image est montrée sur le tick que donne son temps de flux ; une source
  qui dérive saute ou répète une image par période de dérive, nette. Les fichiers restent
  cadencés par l'horloge (`appsink sync=true`).
- **Coût** : environ une demi-période du maître de latence en plus pour ces inputs (latence
  publication → tick fixe, comme SRT).
- **Son** : il saute avec l'image sur ce glissement (ADR 0007, « Précision … étape 9 »).

### Critères (étape 9)
| Critère | Test | Statut |
|---|---|---|
| Source locale 50p à ± 100 ppm, arrivées à 1–1,8 ms de son temps de flux (en phase avec les ticks) : chaque image sur le tick de son temps de flux, délai fixe, 2 images sautées (ou répétées) en 400 s, pas plus | `pipeline::tests::a_drifting_local_source_skips_one_picture_per_period_of_drift` | CI (temps simulé) |
| Le son saute avec chaque glissement (un saut de latence par glissement), à 0,2 ms de l'image montrée à chaque tick, l'image répétée comprise ; une image déplacée seule : aller et retour ; étalement 24p dans 50p : aucun saut | `engine` `audio::tests::the_sound_jumps_with_the_slips_of_a_drifting_source`, `audio::tests::one_picture_out_of_place_and_a_wide_spread_are_not_slips` | CI |
| **C14** : soak 1 h (6 sources synchro 1080p50 avec son, dont 2 à ± 100 ppm, mélangeur, enregistrement + stream + NDI®) | `tests/soak/phase3-c14.sh` | manuel (Linux) |
