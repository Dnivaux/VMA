# Rapport de soak (hostile) — 2026-10-26 18:39 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 600 --hostile --report tests/soak/reports/2026-10-26-phase0-hostile-linux.md` |
| Mode | hostile |
| OS | linux (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 5090 · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 600 s · 600.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 30001 (attendu ≈ 30000) |
| **Images manquées (horloge)** | **0** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 30001 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Rendu CPU moyen · max | 0.069 ms · 2.826 ms |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.003 ms · 0.074 ms · 1.398 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 30001 réveils.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 34287 |
| Rafraîchissement moyen | 57.1 Hz |
| Plus long blocage UI | 26900.075 ms |

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
Rien a signalé tout etait bon.