# Spec — `clock` (horloge maître)

## Comportement
- Cadence rationnelle `FrameRate` (défaut 50/1). Créneau *n* : `[origin + n/rate, origin + (n+1)/rate)`.
- L'échéance de *n* est calculée depuis *n* en arithmétique entière 128 bits, arrondie à la
  nanoseconde supérieure → aucune dérive quelle que soit la durée (testé sur 24 h en 59,94).
- `FrameClock::wait_next()` bloque jusqu'au début du créneau suivant et renvoie un `Tick`
  (`index`, `scheduled` = PTS idéal, `woke_at`, `missed_before`).
- Si le travail précédent a débordé :
  - dans la même période → tick **en retard** (`lateness`), pas manqué ;
  - au-delà → les créneaux écoulés sont **manqués** : comptés, sautés, pas rattrapés.
- Attente hybride : `thread::sleep` jusqu'à 1 ms avant l'échéance, puis attente active.
- Statistiques lock-free (`ClockStats`, atomiques) : ticks, manqués, dernier/max retard.
- Chaque créneau manqué émet un `tracing::warn!`.

## Interface publique
`FrameClock<T: TimeSource>`, `Tick`, `ClockStats`, `ClockStatsSnapshot`,
`TimeSource` (`SystemTime` réel, `ManualTime` pour les tests).

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Pas de dérive sur 10⁶ ticks | `clock::tests::ticks_follow_the_rate_without_drift` | CI |
| Débordement < 1 période = retard, pas manqué | `overrun_within_one_period_is_late_not_missed` | CI |
| Débordement > 1 période = créneaux manqués comptés | `overrun_beyond_one_period_skips_slots` | CI |
| 1 s réelle ≈ 50 ticks | `real_time_one_second` | CI (tolérance large) |
| 10 min à 50 fps, 0 manquée (avec rendu GPU) | `engine` : `soak_render_loop` | `#[ignore]`, manuel |
