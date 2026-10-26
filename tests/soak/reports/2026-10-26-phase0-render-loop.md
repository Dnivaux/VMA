# Soak — Phase 0, boucle de rendu (2026-10-26)

**Résultat : ✅ réussi** — 0 image manquée, 0 image répétée sur 10 min.

| | |
|---|---|
| Test | `aiguillage-engine::render_loop::soak_render_loop` (`--release`) |
| Machine | Intel Core Ultra 9 285K, RTX 5090 (pilote 610.57.04), Arch Linux (Omarchy), noyau 7.2.5 |
| Adaptateur | NVIDIA GeForce RTX 5090, Vulkan |
| Configuration | 1920×1080, 50p, mire, consommateur simulé à 60 Hz |
| Priorité du thread de rendu | normale (aucune élévation) |
| Charge parallèle | session de bureau active ; compilations `cargo` lancées en parallèle pendant les premières minutes |

## Mesures
| Métrique | Valeur |
|---|---|
| Durée | 600,01 s |
| Ticks | 30 002 (attendu ≈ 30 001) |
| Images manquées (horloge) | **0** |
| Images répétées (contre-pression GPU) | **0** |
| Images rendues | 30 002 |
| Temps CPU de rendu, moyen | 0,122 ms |
| Temps CPU de rendu, max | 7,51 ms |
| Retard de réveil max | 14,83 ms |
| Cadence mesurée (par minute) | 49,999 – 50,001 fps |

## Observations
- Le retard max de réveil (14,8 ms) et le temps CPU max (7,5 ms) ont été atteints pendant la
  première minute, quand des compilations `cargo` saturaient le CPU ; les deux sont restés
  inchangés ensuite. 14,8 ms < 20 ms : l'image a été servie en retard dans son propre créneau,
  donc pas manquée. **Marge restante dans ce pire cas : ~5 ms.** C'est l'argument principal pour
  un ADR « priorité temps réel du thread de rendu » si les phases suivantes réduisent la marge.
- Journal brut : sortie du test, une ligne par minute (ticks, manquées, cadence, temps CPU).
