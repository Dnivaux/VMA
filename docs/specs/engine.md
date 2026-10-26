# Spec — `engine` (Phase 0 : thread de rendu)

## Comportement
- `RenderEngine::spawn(device, queue, config)` démarre le thread `aiguillage-render` et
  renvoie le `ProgramReader` (consommateur unique des images Program).
- À chaque tick : `device.poll(Poll)` non bloquant, puis rendu de la mire dans la texture
  « back » du triple buffer, `submit`, publication.
- Contre-pression : si ≥ 2 images sont encore en vol sur le GPU, le tick est sauté (image
  répétée, `gpu_dropped += 1`, `warn!`). L'horloge n'est jamais bloquée par le GPU.
- `RenderStats` (atomiques) : images rendues, `gpu_dropped`, images en vol, temps CPU
  (dernier / moyenne glissante / max), cadence mesurée sur ~1 s.
- `ProgramReader::update()` prend la dernière image publiée sans attendre ;
  `current_slot()` / `current_frame_index()`.
- Drop de `RenderEngine` = arrêt + `join` du thread.

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Triple buffer : jamais d'image plus ancienne, slots distincts (200 000 échanges concurrents) | `triple::tests::concurrent_stress` | CI |
| 1 s réelle : ≥ 40 ticks, images publiées dans l'ordre | `render_loop::runs_one_second` | CI |
| **10 min à 50 fps 1080p : 0 manquée, 0 répétée** (acceptation Phase 0) | `render_loop::soak_render_loop` | `#[ignore]`, manuel — voir `tests/soak/reports/` |
