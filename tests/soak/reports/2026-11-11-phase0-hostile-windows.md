# Rapport de soak (hostile) — 2026-11-11 19:11 UTC

**Verdict : ❌ ÉCHEC (images manquées ou répétées)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 600 --hostile --no-journal --report C:\Users\<utilisateur>\Desktop\VMA\Rapports\2026-11-11-phase0-hostile-windows.md` |
| Mode | hostile |
| OS | windows (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 3060 Laptop GPU · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 600 s · 600.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 29993 (attendu ≈ 30000) |
| **Images manquées (horloge)** | **8** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 29993 |
| Erreurs GPU | 42 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.030 ms · 8.126 ms · 19.354 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 29993 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 1.376 ms | 12.583 ms | 27.263 ms | 95.156 ms |
| `device.poll` | 0.508 ms | 11.010 ms | 25.166 ms | 48.924 ms |
| Encodage (uniforms compris) | 0.279 ms | 0.819 ms | 1.638 ms | 16.623 ms |
| `queue.submit` | 0.090 ms | 6.029 ms | 12.059 ms | 94.947 ms |

Sur 29993 ticks rendus.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 73199 |
| Rafraîchissement moyen | 122.0 Hz |
| Plus long blocage UI | 1606.740 ms |

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

### Réponses de l'opératrice (lanceur du paquet Windows)
- Déplacée par la barre de titre : oui
- Redimensionnée par un bord : oui
- Réduite puis rouverte : oui
- Remarque : 
