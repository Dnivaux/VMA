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
- Jusqu'à l'étape 3, la mire reste affichée ; la `Composition` n'est visible que par
  `feedback().on_air()`.

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

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Triple buffer : jamais d'image plus ancienne, slots distincts (200 000 échanges concurrents) | `triple::tests::concurrent_stress` | CI |
| Le producteur n'écrit jamais dans le slot détenu, même longtemps (20 000 détentions aléatoires) | `triple::tests::producer_never_writes_the_held_slot` | CI |
| Invariant sur GPU : chaque lecture GPU du slot détenu contient l'image annoncée (moteur à 240 fps, ≥ 300 frames UI simulées) | `render_loop::triple_buffer_gpu_stress` | CI |
| Perte du device : pas de panique, rendu arrêté, horloge qui continue, raison exposée, arrêt propre | `render_loop::device_lost_stops_rendering_not_the_clock` | CI |
| 1 s réelle : ≥ 40 ticks, images publiées dans l'ordre | `render_loop::runs_one_second` | CI |
| **10 min à 50 fps 1080p : 0 manquée, 0 répétée** (acceptation Phase 0) | `render_loop::soak_render_loop` | `#[ignore]`, manuel — voir `tests/soak/reports/` |
| Commande → contrôle → plan → rendu avec les vrais threads ; Fade minuté qui bascule seul ; journal écrit, sans perte, et rejouable | `render_loop::commands_reach_the_render_thread_and_the_journal` | CI |
| Seqlock du plan : jamais de plan déchiré ni de version qui recule (3 lecteurs, 200 000 écritures) | `plan::tests::concurrent_reads_are_never_torn` | CI |
| **Soak hostile : 10 min, fenêtre malmenée, 0 manquée, 0 répétée** (amendement 3) | `vma --soak 600 --hostile` + [`docs/manual-tests/phase0-soak-hostile.md`](../manual-tests/phase0-soak-hostile.md) | manuel, Linux et Windows |
