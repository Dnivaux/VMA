# Rapport de soak (calme) — 2026-11-11 17:28 UTC

**Verdict : ❌ ÉCHEC (images manquées ou répétées)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 1800 --rate 50 --resolution 1080p --inputs 4 --no-journal --report C:\Users\<utilisateur>\Desktop\VMA\Rapports\2026-11-11-phase1-a1-windows.md` |
| Mode | calme |
| OS | windows (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 3060 Laptop GPU · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 1800 s · 1800.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 90000 (attendu ≈ 90000) |
| **Images manquées (horloge)** | **1** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 89999 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.022 ms · 0.623 ms · 9.460 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 90000 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 2.884 ms | 7.864 ms | 8.126 ms | 49.404 ms |
| `device.poll` | 1.507 ms | 5.243 ms | 7.078 ms | 48.521 ms |
| Encodage (uniforms compris) | 0.442 ms | 1.507 ms | 2.228 ms | 4.993 ms |
| `queue.submit` | 0.098 ms | 7.078 ms | 7.602 ms | 10.955 ms |

Sur 89999 ticks rendus.

## Inputs
449 transitions automatiques (une toutes les 4 s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Upload occupé | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|---|
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.87 (50p) | 81 | 82 | 0 | 13.107 ms · 14.156 ms · 15.386 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.87 (50p) | 62 | 62 | 0 | 13.107 ms · 14.156 ms · 15.153 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.87 (50p) | 36 | 36 | 0 | 12.583 ms · 13.631 ms · 15.566 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.87 (50p) | 61 | 61 | 0 | 13.107 ms · 14.156 ms · 15.949 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent. Upload occupé : images perdues parce que le tampon de transfert de leur emplacement était encore utilisé par le GPU (ADR 0004, amendement 1) ; doit rester à 0.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 259183 |
| Rafraîchissement moyen | 144.0 Hz |
| Plus long blocage UI | 120.882 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
