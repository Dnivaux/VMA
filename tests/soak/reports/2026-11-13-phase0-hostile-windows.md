# Rapport de soak (hostile) — 2026-11-13 10:05 UTC

**Verdict : ❌ ÉCHEC (images manquées ou répétées)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 600 --hostile --no-journal --report C:\Users\<utilisateur>\Downloads\VMA-windows (1)\VMA\Rapports\2026-11-13-phase0-hostile-windows.md` |
| Mode | hostile |
| OS | windows (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 3060 Laptop GPU · Dx12 · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 600 s · 600.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 30011 (attendu ≈ 30000) |
| **Images manquées (horloge)** | **1** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 30011 |
| Erreurs GPU | 81 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.014 ms · 0.172 ms · 9.478 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 30011 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 0.655 ms | 4.719 ms | 15.729 ms | 22.824 ms |
| `device.poll` | 0.074 ms | 1.180 ms | 14.680 ms | 21.762 ms |
| Encodage (uniforms compris) | 0.393 ms | 2.490 ms | 3.277 ms | 4.372 ms |
| `queue.submit` | 0.131 ms | 1.507 ms | 4.456 ms | 19.899 ms |

Sur 30011 ticks rendus.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 43076 |
| Rafraîchissement moyen | 71.8 Hz |
| Plus long blocage UI | 1663.317 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.

## Agressions automatiques
Cycle de 15 s : redimensionnement continu, maximiser, plein écran, minimiser, déplacement.

| Action | Commandes envoyées |
|---|---|
| redimensionnement (toutes les 50 ms) | 2400 |
| maximiser | 40 |
| dé-maximiser | 40 |
| plein écran | 40 |
| retour fenêtré | 40 |
| minimiser | 40 |
| restaurer | 40 |
| déplacement (toutes les 50 ms) | 2400 |

Sous Wayland, le déplacement (et parfois la minimisation) est ignoré par le compositeur.

## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
