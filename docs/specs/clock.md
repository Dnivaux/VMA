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
- Attente hybride : `thread::sleep` jusqu'à la marge d'attente active avant l'échéance
  (1 ms par défaut, configurable : `FrameClock::with_spin_margin`), puis attente active.
- Statistiques lock-free (`ClockStats`, atomiques) : ticks, manqués, dernier/max retard, et
  **histogramme du retard au réveil** (`LatencyHistogram` : classes log-linéaires, 16 par
  puissance de 2, précision ≈ 6 %, taille fixe, sans allocation) → p50 / p99 / p99,9 / max.
- Chaque créneau manqué émet un `tracing::warn!`.
- **Priorité des threads** (`priority`, [ADR 0017](../adr/0017-priorite-des-threads.md)) :
  `raise_current_thread(ThreadRole::Render | Timing)`, appelé au départ du rendu et des threads
  de cadence. Sous Windows, `TIME_CRITICAL` / `HIGHEST` sans bridage d'énergie (et, au premier
  appel, pour le processus) ; sous Linux, `SCHED_FIFO` 12 / 10 si l'utilisateur en a le droit.
  Un refus laisse la priorité par défaut, ce n'est jamais une erreur. `summary()` donne ce que
  chaque rôle a obtenu (panneau Statistiques, rapport de soak).

## Interface publique
`FrameClock<T: TimeSource>`, `Tick`, `ClockStats`, `ClockStatsSnapshot`,
`LatencyHistogram`, `LatencySummary`,
`TimeSource` (`SystemTime` réel, `ManualTime` pour les tests),
`priority::{raise_current_thread, summary, ThreadRole, Priority, PrioritySummary}`.

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| Pas de dérive sur 10⁶ ticks | `clock::tests::ticks_follow_the_rate_without_drift` | CI |
| Débordement < 1 période = retard, pas manqué | `overrun_within_one_period_is_late_not_missed` | CI |
| Débordement > 1 période = créneaux manqués comptés | `overrun_beyond_one_period_skips_slots` | CI |
| Histogramme : classes contiguës, erreur relative ≤ 1/16 | `histogram::tests::{buckets_are_contiguous_and_cover_their_values, relative_error_is_bounded}` | CI |
| Percentiles p50 / p99 / p99,9 / max | `histogram::tests::percentiles` | CI |
| Priorité : chaque thread compté, élevé ou non, avec sa raison | `priority::tests::a_raised_or_refused_thread_is_counted` | CI |
| Windows : rendu `TIME_CRITICAL`, cadence `HIGHEST`, bridage du processus levé | `priority::tests::windows_raises_both_roles_and_the_process` | CI Windows |
| Linux : la politique du thread est celle annoncée (`SCHED_FIFO` ou inchangée) | `priority::tests::linux_reports_the_policy_the_thread_has` | CI Ubuntu |
| 1 s réelle ≈ 50 ticks | `real_time_one_second` | CI (tolérance large) |
| 10 min à 50 fps, 0 manquée (avec rendu GPU) | `engine` : `soak_render_loop` | `#[ignore]`, manuel |
