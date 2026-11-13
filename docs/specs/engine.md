# Spec — `engine`

La couche de commandes et l'état (Phase 1) sont décrits dans [`commands.md`](commands.md) ;
ce fichier couvre le thread de rendu.

## Phase 1 — ce qui change dans la boucle de rendu (étape 2)
- `RenderEngine::spawn(device, queue, config, journal)` démarre aussi `vma-control` (et
  `vma-journal` si `Journal::Directory`). `commands()`, `state()`, `feedback()`,
  `control_stats()`, `journal_stats()`.
- À chaque tick, **avant** tout travail GPU (et même si le device est perdu ou si le tick est
  sauté pour contre-pression) : `Sequencer::tick(n)` lit le plan (seqlock, sans attente) et
  produit la `Composition` du tick. La progression d'une transition dépend de l'index d'image,
  pas du nombre de ticks traités : des ticks manqués ne la ralentissent pas.
- **Étape 3** : la `Composition` est dessinée par le `Mixer` du compositeur. Chaque input est
  connu du rendu par une table fixe (`sources.rs`, 64 entrées `Copy`) que `vma-control` tient à
  jour par différence avec l'état, via un canal borné, **avant** de publier le plan ; le rendu
  lit le plan **puis** vide le canal, si bien qu'un input est connu dès le premier tick où le
  plan le montre. Couleur → calque uni ; `TestSource` → mire ; autres inputs → noir jusqu'aux
  étapes 5–7. Sans input au Program : noir.
- **Étape 4** : `spawn` renvoie `Readers { program, preview, thumbnails }`, trois
  `FrameReader` (triple buffer, même invariant). À chaque tick, une seule soumission : Program
  (`Mixer`), Preview (second `Mixer`, l'input en Preview seul), et un tick sur deux l'atlas des
  vignettes. La cellule d'un input = son emplacement dans la table des sources (stable tant
  qu'il existe) ; `FrameReader::cell_of(id)` la donne pour l'image détenue.
- Les tests GPU du moteur mettent une mire à l'antenne (`spawn`) ; l'application ne le fait
  qu'en mode `--soak`.
- **Étape 6 (2026-10-27) : feeds d'images par input** (module `feed`). Un feed = 3
  `InputFrame` (textures par plan, cf. `compositor.md`) derrière un triple buffer d'indices,
  métadonnées par emplacement (n° d'image source, heure d'arrivée) et statistiques en
  atomiques (`FeedStats` : reçues, sautées, répétées, montrées, latence arrivée → tick).
  - **Producteur** : `FrameSink` (`push`, `push_packed` ; plans validés, jamais bloquant),
    détenu par le thread qui reçoit les images (streaming GStreamer à l'étape 7).
  - **Création** : `RenderEngine::feeds()` → `FeedRegistry::create(input, format, taille,
    colorimétrie, cadence)`, appelé hors du thread de rendu (il crée les textures). Le feed est
    envoyé à `vma-control` (canal borné) qui, si l'input existe et n'est pas généré, l'attache
    à la table des sources (avant le plan, comme le reste), passe l'input en `Live { source }`
    par l'événement journalisé `Event::InputLive`, et publie ses stats
    (`RenderEngine::input_stats()`). Un second feed pour le même input remplace le premier
    (changement de caps). *Écart avec `io-gst.md` (« textures créées par `vma-control` ») :
    elles sont créées par l'appelant, hors rendu lui aussi ; `vma-control` ne fait que les
    attacher.*
  - **Rendu** : à chaque tick, après le plan et la table, **chaque** feed prend sa dernière
    image publiée (`advance`), rendu ou non (stats comptées en ticks maîtres). Pas d'image
    encore : noir. Program, Preview et vignettes utilisent la même image du tick.
  - **Retrait** : un feed qui quitte la table (input retiré, feed remplacé) est **renvoyé** à
    `vma-control` par un second canal borné et libéré là ; abandon sur le thread de rendu
    seulement si ce canal est plein (journalisé `warn`). Le `FrameSink` d'un input retiré reste
    utilisable (images ignorées) jusqu'à ce que son propriétaire le lâche.

## Phase 0 — thread de rendu

## Comportement
- `RenderEngine::spawn(device, queue, config)` démarre le thread `vma-render` et
  renvoie le `ProgramReader` (consommateur unique des images Program).
- À chaque tick : `device.poll(Poll)` non bloquant, puis rendu de la mire dans la texture
  « back » du triple buffer, `submit`, publication.
- Contre-pression : si ≥ 2 images sont encore en vol sur le GPU, le tick est sauté (image
  répétée, `gpu_dropped += 1`, `warn!`). L'horloge n'est jamais bloquée par le GPU.
- `RenderStats` (atomiques) : images rendues, `gpu_dropped`, images en vol, temps CPU
  (dernier / moyenne glissante / max), cadence mesurée sur ~1 s.
- `ProgramReader::update()` prend la dernière image publiée sans attendre ;
  `current_slot()` / `current_frame_index()`.
- **Invariant du triple buffer** (ADR 0001) : le consommateur n'appelle `update()` qu'entre
  deux de ses frames, après avoir soumis tout ce qui lit le slot précédent ; consommateur et
  moteur soumettent sur la même `Queue`.
- `EngineConfig { rate, resolution, spin_margin }` (marge d'attente active de l'horloge).
- **Erreurs GPU** : le moteur installe le gestionnaire d'erreurs non capturées du device
  (journalisées, jamais de panique). wgpu l'appelle sur le thread fautif : celles du thread
  principal (la fenêtre et ses plein écrans, egui) sont comptées dans `window_gpu_errors`,
  les autres (rendu, inputs) dans `gpu_errors`. Sans ce partage, le soak hostile Windows du
  2026-11-11 comptait au Program 42 erreurs de la fenêtre (surface reconfigurée pendant un
  redimensionnement).
- **Démarrage** : l'horloge (créneau 0) est créée en dernier, juste avant le thread de
  rendu ; l'application ne démarre le moteur qu'après la première image de sa fenêtre
  (création de la chaîne d'images et première image sur le GPU partagé). Sous Windows, les
  deux faisaient manquer les premiers créneaux (soaks du 2026-11-11).
- **Perte du device** : callback `device_lost` → plus aucune commande GPU, l'horloge continue
  (`device_lost_ticks`), `device_lost` et `RenderStats::device_lost_reason()` exposés.
  L'application ferme alors la fenêtre et sort en erreur (ADR 0001, « Perte du GPU »).
- Drop de `RenderEngine` = arrêt + `join` du thread.

## Critères d'acceptation (étape 6)
| Critère | Test | Où |
|---|---|---|
| **A5** côté moteur : source à chacune des 6 cadences maîtres sur chacune des 6 (36 combinaisons, 40 s simulées) → l'image prise au tick *n* est toujours `floor(n · source / maître)` ; reçues = montrées + sautées, ticks = montrées + répétées ; latence < une période source | `feed::tests::every_source_rate_resamples_onto_every_master_rate` | CI |
| Comptes exacts : 25p sur 50p (chaque image 2 fois), 60p sur 50p (1 sur 6 sautée), 29,97p sur 30p (3 répétitions en 3003 ticks) | `feed::tests::rate_ratios_give_the_expected_counts` | CI |
| Producteur et rendu en parallèle : pas d'emplacement déchiré, pas de retour en arrière | `feed::tests::producer_and_reader_race_without_tearing` | CI |
| Image d'un feed au Program, en Preview et en vignette ; noir avant la première ; stats | `render_loop::feed_pictures_reach_program_preview_and_thumbnails` | CI (GPU logiciel) |
| **A12** : 20 inputs alimentés à 50p ajoutés, mis au Program, retirés pendant le rendu → chacun vu avec sa couleur, rien ne reste, 0 image manquée | `render_loop::hot_add_and_remove_without_missed_frames` | CI (GPU logiciel) |

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Triple buffer : jamais d'image plus ancienne, slots distincts (200 000 échanges concurrents) | `triple::tests::concurrent_stress` | CI |
| Le producteur n'écrit jamais dans le slot détenu, même longtemps (20 000 détentions aléatoires) | `triple::tests::producer_never_writes_the_held_slot` | CI |
| Invariant sur GPU : chaque lecture GPU du slot détenu contient l'image annoncée (moteur à 240 fps, ≥ 300 frames UI simulées) | `render_loop::triple_buffer_gpu_stress` | CI |
| Perte du device : pas de panique, rendu arrêté, horloge qui continue, raison exposée, arrêt propre | `render_loop::device_lost_stops_rendering_not_the_clock` | CI |
| 1 s réelle : ≥ 40 ticks, images publiées dans l'ordre | `render_loop::runs_one_second` | CI |
| **10 min à 50 fps 1080p : 0 manquée, 0 répétée** (acceptation Phase 0) | `render_loop::soak_render_loop` | `#[ignore]`, manuel — voir `tests/soak/reports/` |
| Pixels du Program : couleur à l'antenne, Cut, Fade à mi-course (somme des codes = 255 en gamma), retour à la couleur ; Preview = input en Preview ; vignettes = couleur de chaque input | `render_loop::program_pixels_follow_cut_and_fade` | CI |
| Commande → contrôle → plan → rendu avec les vrais threads ; Fade minuté qui bascule seul ; journal écrit, sans perte, et rejouable | `render_loop::commands_reach_the_render_thread_and_the_journal` | CI |
| Seqlock du plan : jamais de plan déchiré ni de version qui recule (3 lecteurs, 200 000 écritures) | `plan::tests::concurrent_reads_are_never_torn` | CI |
| **Soak hostile : 10 min, fenêtre malmenée, 0 manquée, 0 répétée** (amendement 3) | `vma --soak 600 --hostile` + [`docs/manual-tests/phase0-soak-hostile.md`](../manual-tests/phase0-soak-hostile.md) | manuel, Linux et Windows |

---

## Phase 2 — Sorties, côté moteur (étape 3, 2026-10-31)
Voir l'ADR 0006 (§§ 1–3, 5 et « Mise en œuvre côté moteur »).

- **Rendu** : si au moins une sortie est attachée (`OutputHub::active`), après le mixer du
  Program : `OutputStage` (FTB à 0 tant que la commande n'existe pas, étape 5 ; flash si
  `--sync-test`), puis copie des plans NV12 dans l'un des 4 tampons `MAP_READ` de l'anneau
  (`readback.rs`) et mappage à la soumission. Au tick suivant, après `device.poll`, les tampons
  mappés partent vers `vma-output` dans l'ordre de *n*. Aucun tampon libre → image non relue,
  `output_dropped`. Phase « étage de sortie » ajoutée à l'histogramme CPU.
- **`vma-output`** : copie le tampon dans une `OutputFrame` du pool (NV12 tassé : lignes de
  `width` octets, puis Cb Cr), `unmap`, PTS = `video_pts_ns(n)`, durée jusqu'à `n + 1`, audio
  `[audio_sample_at(n), audio_sample_at(n + 1))` en `f32` stéréo entrelacé à 48 kHz
  (silence, ou bip de synchro), puis `try_send` vers la file de chaque sortie.
- **Sorties** : `OutputHub::attach(id, Box<dyn OutputConsumer>)` / `detach(id)` ; compteurs
  par sortie (`delivered`, `dropped`, `queued`) et de distribution (`frames`,
  `pool_exhausted`, `read_errors`).
- `EngineConfig::sync_test` (ligne de commande à l'étape 5 ou 10).

- *(Étape 8)* **Plein écran** : `Readers::fullscreen`, un quatrième `FrameReader` (triple
  buffer `Rgba8Unorm`, même invariant). Quand le plan dit `fullscreen`, le rendu enregistre la
  passe FTB / flash (`OutputStage::encode_look`) puis la conversion `out16` → `Rgba8Unorm`
  (`encode_rgba8`) dans l'emplacement libre, dans la même soumission, et publie. Sans sortie
  attachée, ni NV12 ni relecture (`encode_nv12` et l'anneau seulement si `OutputHub::active`).

- *(Étape 9)* **Perte du GPU avec sorties actives** (ADR 0006 § 7 et « Mise en œuvre de la
  perte du GPU ») : à chaque tick perdu, `RenderFeedback::device_lost_at()` garde le premier
  *n* perdu et, si une sortie est attachée, le rendu envoie `Delivery::Black(n)` à
  `vma-output`, qui distribue une image noire NV12 (Y' 16, Cb Cr 128) avec le PTS et l'audio
  de *n* (compteur `black`). `EngineConfig::device_lost_grace` (10 s) : délai avant la
  finalisation des sorties par `vma-control` (`commands.md`).

### Critères d'acceptation (étape 3)
| Critère | Test | Où |
|---|---|---|
| **B3** (moteur) : PTS = f(*n*) et audio exact (blocs, cumul, 1 h, 29,97p = 1 601 / 1 602) aux 6 cadences | `outputs::tests::pts_and_audio_follow_n_at_every_master_rate` (temps simulé) | CI |
| **B3** (moteur) : trou d'une image non relue → PTS inchangés, audio continu (porte les images sautées), discontinuité au-delà d'1 s | `outputs::tests::audio_is_continuous_and_carries_the_skipped_frames` | CI |
| **B3** (moteur), vrais threads : à chaque cadence, PTS / durée / fin de l'audio = f(*n*), audio continu, barres de la mire à ±1 en Y' | `tests/outputs.rs::frames_carry_the_pts_and_audio_of_n_at_every_master_rate` | CI |
| Flash et bip sur le même échantillon, toutes les secondes, aux 6 cadences | `outputs::tests::sync_flash_and_beep_start_on_the_same_sample`, `tests/outputs.rs::sync_test_flashes_and_beeps_every_second` | CI |
| **B6** (moteur) : une sortie bloquée perd ses images (comptées), l'autre les reçoit toutes, 0 tick manqué, tampon de relecture toujours libre | `tests/outputs.rs::stalled_output_loses_only_its_own_frames` (exécuté seul) | CI |
| Pas de relecture sans sortie attachée ; arrêt après `detach` | `tests/outputs.rs::frames_are_read_back_only_while_an_output_is_attached` | CI |
| *(Étape 8)* **B12** (moteur) : images du plein écran publiées seulement en marche, Program après FTB (noir à l'écran, blanc dans la vue Program de l'UI), aucune relecture pour le plein écran seul | `render_loop::fullscreen_frames_show_the_program_after_ftb_while_on` (RTX 5090, Intel ARL) | CI |
| *(Étape 9)* **B14** (moteur) : GPU perdu (`Device::destroy`) avec une sortie attachée → une image par tick, noire, PTS / durée / audio de *n*, audio continu à travers la perte (≤ 5 images non relues à l'instant de la perte) ; démarrage refusé ; sorties finalisées après le délai, pas avant | `tests/outputs.rs::a_lost_gpu_sends_black_at_the_pts_of_n_then_finalises` (RTX 5090, Intel ARL) | CI |
| Coût en 1080p50 (une sortie) | `tests/outputs.rs::output_stage_cost_1080p50` (`#[ignore]`, manuel) : tick p99 0,62 ms, étage 0,011 ms (RTX 5090) | manuel |


## Phase 3 — Thread `vma-audio` (étape 3, 2026-11-03)
- **Rendu** : après `sources.advance`, un `AudioTick` (copie, sans allocation) par tick :
  index *n*, temps de flux du début idéal du tick (`EngineConfig::stream_clock`, converti par
  `vma_clock::StreamClock`), composition du Program (`from`, `to`, `mix`), et, par case de la
  table des sources, le temps de flux de l'image montrée (`SourceTable::stream_times`).
  `try_send` ; file pleine : compté (`AudioStats::queue_full`), le tick est reconstruit.
- **`vma-audio`** (`audio.rs`, `AudioMix`) : un `InputReader` (crate `audio`) par input qui a
  un anneau (`FeedRegistry::audio`, demandé par `io-gst` au démarrage de chaque pipeline ;
  le nouvel anneau remplace l'ancien) ; temps de lecture = temps du tick − latence lissée
  sur les images nouvelles (ADR 0007, précisions) ; image figée plus vieille de 100 ms que sa
  latence (fichier en pause, source perdue) : silence, et la tranche reste tant que la source
  tient son anneau (oubliée 500 ticks après que la source l'a lâché ou que l'input a quitté
  le plan). Un input sans vidéo, **et un input HTML**, sont lus à la latence de leur anneau
  (tick moins une période), image ou non. Audio-follow-video : part à l'antenne
  = parts en puissance constante de `from` et `to` selon `mix` (Cut : un bloc de rampe).
  Mixage du bloc *n* à l'arrivée du tick *n* + 3 ; ticks manquants (≤ 50) reconstruits avec
  la latence figée ; au-delà, silence écrit (`skipped_samples`). Les blocs des bus vont dans
  un anneau par bus, par index d'échantillon de sortie (`audio_sample_at(n)`).
- **`vma-output`** : l'audio d'une image vient de l'anneau du Master (sauf `--sync-test`,
  bip généré comme en Phase 2) ; l'image est retenue jusqu'à ce que son son soit écrit (8
  images ou 8 périodes au plus, puis silence compté : `DistributionSnapshot::audio_late`).
  `black` est compté à l'envoi.

### Critères (étape 3)
| Critère | Test | Statut |
|---|---|---|
| Le son d'un input est sur son image, au premier tick qui la montre (source à la cadence du maître et plus lente) | `audio::tests::the_sound_of_an_input_sits_on_its_picture` | CI |
| Ticks manquants reconstruits : mêmes débuts de bip | `audio::tests::missing_ticks_are_rebuilt` | CI |
| C6 (moteur) : hors antenne rien au Master, à l'antenne tout, à mi-mix la part en puissance constante | `audio::tests::{audio_follows_the_program, on_air_shares}` | CI |
| Image figée → silence ; nouvelle latence → lissage repris | `audio::tests::a_stale_picture_silences_its_input` | CI |
| C1 : source de synchro → Program → enregistrement relu, < 1 image, aux 6 cadences maîtres (source à une autre cadence) | `io-gst` : `outputs::tests::lip_sync_*` (6 tests) | CI |
| C2 (1re moitié) : son en avance de 80 ms mesuré −80 ms à une image près | `io-gst` : `outputs::tests::early_sound_is_measured_early` | CI |
| C3 (temps réel) : ± 100 ppm pendant 60 s, < 1 image, sans trou | `io-gst` : `outputs::tests::drifting_sources_stay_in_sync_for_a_minute` | CI |

## Phase 3 — Réglages audio et bus des sorties (étape 4, 2026-11-03)
- **`vma-audio`** lit le plan audio (`AudioPlanReader`, seqlock) avant chaque bloc : fader,
  mute, solo, envois de chaque input, fader et mute des bus, mode de solo ; temps de lecture
  d'un input = temps de l'image affichée − son **décalage** ; audio-follow-video désactivé →
  part à l'antenne 1. Un input sans réglages dans le plan n'est pas mixé.
- **Vumètres** : `RenderEngine::audio_meters()` → `AudioMeters::read(Level)` (input, bus,
  casque ; avant et après fader). Les vumètres du mixeur sont rangés par case ; la case de
  chaque input est publiée dans des atomiques, l'UI retrouve celle d'un input sans verrou.
- **`vma-output`** lit les **trois** anneaux de bus : `OutputFrame::buses` (Master, A, B ;
  `OutputFrame::audio(bus)`), chaque sortie prend le sien (`OutputFeed::with_bus`, gardé par
  les reconnexions d'un stream). L'image attend que le dernier bus écrit (B) ait son son.
  `--sync-test` : le bip sur les trois bus.

### Critères (étape 4)
| Critère | Test | Statut |
|---|---|---|
| Les réglages du plan façonnent le son : −6 dB, mute, envoi vers A seul (A ne suit pas l'image), audio-follow-video désactivé, décalage de 80 ms à 2 échantillons près | `audio::tests::the_audio_plan_shapes_the_sound` | CI |
| Input sans réglages : non mixé ; vumètres d'un input retrouvés par son identifiant, avant / après fader, et du Master | `audio::tests::meters_by_input_and_inputs_without_settings` | CI |
| **C2 (2de moitié)** : son en avance de 80 ms, input décalé de 80 ms au milieu d'un même enregistrement → < 1 image ensuite ; décalage appliqué à ± 1 ms (mesuré : 79,92 ms ; d'un enregistrement à l'autre, la chaîne de mesure varie jusqu'à 2 ms sur la CI, d'où la mesure dans un seul) | `io-gst` : `outputs::tests::a_delayed_input_is_back_in_sync` | CI (Linux) |
| Un enregistrement du bus A porte le son envoyé au bus A seul, synchro < 1 image | `io-gst` : `outputs::tests::a_recording_carries_its_bus` | CI (Linux) |

## Phase 3 — Son seul et casque (étape 5, 2026-11-03)
- **Input son seul** (plan audio : `PlanInput::video = false`) : pas d'image affichée ; il est
  lu au temps du tick **moins une période** (ADR 0007 § 3 : la latence de son anneau, plus
  une marge). Avec le mixage différé de 3 ticks, ses blocs (10 à 21 ms selon le pilote) sont
  arrivés quand le bloc est mixé. Le son d'un micro sort donc environ une image avant celui
  d'une caméra au même instant (latence de l'image) : le décalage par input les recale.
- **Casque** : `vma-audio` écrit le mix casque (`Mixer::headphones` : Master, ou les inputs en
  solo) dans un quatrième anneau, par index d'échantillon comme les bus ; `IoPort::headphones`
  le donne à `vma-io`.

### Critères (étape 5)
| Critère | Test | Statut |
|---|---|---|
| Son seul : lu une période derrière le tick, aucun échantillon manquant, bip à l'échantillon près ; casque = Master sans solo ; solo AFL d'un input muet : silence ; PFL : entendu au casque, absent du Master | `audio::tests::a_sound_only_input_and_the_headphones` | CI |

## Phase 4 — Overlay channels (étape 3, 2026-11-09)
- **Séquenceur** : une rampe d'opacité par channel (point fixe, comme le FTB, `ramp`), sans
  allocation (tableaux fixes) ; `Composition::overlays` donne, pour chaque channel visible,
  son input et son opacité.
- **Rendu** : le mixeur du Program est un `Mixer::with_keys` (ADR 0010) ; à chaque tick, chaque
  channel visible devient une `Key` (sa source, son opacité) dessinée sur le Program : vue
  Program de l'UI, sorties (le FTB de l'étage de sortie passe **par-dessus**) et plein écran.
  La Preview n'a pas d'overlay (Q5). Un input dont le feed n'a pas encore d'image n'est pas
  dessiné (pas de noir sur le Program). Sources avec alpha : feed créé avec `AlphaMode` (étape 2).
- Le son d'un input en overlay ne change pas (l'audio-follow-video ne suit que le Program).

### Critères (étape 3)
| Critère | Test | Où |
|---|---|---|
| **D1** Fondu d'entrée et de sortie linéaire au tick près, retour à mi-chemin, ticks manqués comptés | `sequencer::tests::an_overlay_fades_in_and_out_linearly`, `an_overlay_turned_back_returns_from_where_it_was` | CI |
| **D1** Cut et sortie immédiate à l'instant ; un autre input repart de 0 ; le mélange du Program inchangé | `sequencer::tests::a_cut_overlay_and_a_new_input_switch_at_once` | CI |
| **D1** De bout en bout sur GPU : image à alpha (feed `Straight`) en overlay 1 sur le Program (vue et sorties, ±1 code), overlay 2 en fondu par-dessus, FTB des sorties par-dessus les overlays, vue Program inchangée, Preview sans overlay, retour au Program seul | `render_loop::overlays_are_keyed_over_the_program_under_the_ftb` | CI |
| **D9** Règles : In / Out / Toggle / Off / AllOff, refus, input retiré, plan (durées, Cut, Off), indépendance des transitions du Program ; channel invalide refusé à la relecture | `state::tests::overlays_go_in_out_and_toggle`, `overlays_are_refused_without_pictures_or_out_of_range`, `a_removed_input_leaves_its_channel`, `the_plan_carries_each_channel_and_its_fade` | CI |
| Plan : 4 mots de plus, durée bornée à 65 535 ticks sans débordement | `plan::tests::encoding_round_trips` | CI |
