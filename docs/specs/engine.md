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
  (journalisées, comptées dans `gpu_errors`, jamais de panique).
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
