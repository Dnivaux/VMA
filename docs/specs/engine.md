# Spec — `engine` (Phase 0 : thread de rendu)

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
| **Soak hostile : 10 min, fenêtre malmenée, 0 manquée, 0 répétée** (amendement 3) | `vma --soak 600 --hostile` + [`docs/manual-tests/phase0-soak-hostile.md`](../manual-tests/phase0-soak-hostile.md) | manuel, Linux et Windows |
