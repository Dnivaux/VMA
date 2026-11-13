# Rapport de soak (calme) — 2026-11-13 09:35 UTC

**Verdict : ❌ ÉCHEC (input perdu, hors cadence, upload en attente du GPU ou latence > 2 images)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 1800 --rate 50 --resolution 1080p --inputs 4 --no-journal --report C:\Users\<utilisateur>\Downloads\VMA-windows (1)\VMA\Rapports\2026-11-13-phase1-a1-windows.md` |
| Mode | calme |
| OS | windows (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 3060 Laptop GPU · Dx12 · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 1800 s · 1800.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 90004 (attendu ≈ 90000) |
| **Images manquées (horloge)** | **9** |
| **Images répétées (GPU saturé)** | **1** |
| Images rendues | 90003 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.035 ms · 2.884 ms · 18.934 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 90004 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 0.754 ms | 4.456 ms | 9.437 ms | 45.492 ms |
| `device.poll` | 0.070 ms | 0.754 ms | 2.359 ms | 44.097 ms |
| Encodage (uniforms compris) | 0.508 ms | 2.753 ms | 4.719 ms | 26.107 ms |
| `queue.submit` | 0.147 ms | 1.376 ms | 4.719 ms | 14.308 ms |

Sur 90003 ticks rendus.

## Inputs
449 transitions automatiques (une toutes les 4 s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Upload occupé | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|---|
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.86 (50p) | 34 | 32 | 1 ❌ | 13.631 ms · 14.680 ms · 51.941 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.86 (50p) | 50 | 47 | 0 | 12.583 ms · 14.680 ms · 29.448 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.86 (50p) | 43 | 39 | 0 | 13.107 ms · 14.680 ms · 42.889 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.86 (50p) | 52 | 48 | 0 | 12.583 ms · 14.680 ms · 39.516 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent. Upload occupé : images perdues parce que le tampon de transfert de leur emplacement était encore utilisé par le GPU (ADR 0004, amendement 1) ; doit rester à 0.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 99731 |
| Rafraîchissement moyen | 55.4 Hz |
| Plus long blocage UI | 151.184 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
