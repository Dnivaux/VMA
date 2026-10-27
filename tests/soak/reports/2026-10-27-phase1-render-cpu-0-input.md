# Rapport de soak (calme) — 2026-10-27 18:53 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 120 --no-journal --report /tmp/claude-1000/-home-dnivaux-Projects-VMA/f8a03cc5-820e-47fc-aa75-a88bd2ce8cdd/scratchpad/cpu-0in.md` |
| Mode | calme |
| OS | linux (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 5090 · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 120 s · 120.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 6001 (attendu ≈ 6000) |
| **Images manquées (horloge)** | **0** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 6001 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.000 ms · 0.000 ms · 1.097 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 6001 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 0.094 ms | 0.156 ms | 0.238 ms | 0.492 ms |
| `device.poll` | 0.013 ms | 0.023 ms | 0.035 ms | 0.050 ms |
| Encodage (uniforms compris) | 0.066 ms | 0.106 ms | 0.164 ms | 0.364 ms |
| `queue.submit` | 0.016 ms | 0.022 ms | 0.035 ms | 0.287 ms |

Sur 6001 ticks rendus.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 7203 |
| Rafraîchissement moyen | 60.0 Hz |
| Plus long blocage UI | 18.661 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
