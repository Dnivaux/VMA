# Rapport de soak (calme) — 2026-10-27 17:09 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée ; inputs sans perte, à leur cadence, latence ≤ 2 images)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 1800 --no-journal --rate 50 --resolution 1080p --inputs 4 --report tests/soak/reports/2026-10-27-phase1-a1-linux.md` |
| Mode | calme |
| OS | linux (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 5090 · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 1800 s · 1800.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 90001 (attendu ≈ 90000) |
| **Images manquées (horloge)** | **0** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 90000 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Rendu CPU moyen · max | 7.364 ms · 10.789 ms |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.006 ms · 0.156 ms · 1.311 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 90001 réveils.

## Inputs
449 transitions automatiques (une toutes les 4 s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 16.253 ms · 16.777 ms · 19.714 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 14.156 ms · 16.777 ms · 19.777 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 11.534 ms · 13.107 ms · 18.162 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 11.010 ms · 13.107 ms · 18.316 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 100071 |
| Rafraîchissement moyen | 55.6 Hz |
| Plus long blocage UI | 35.307 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._

Aucune action manuelle (soak calme, machine non utilisée pendant les 30 min).

## Note (relecture du rapport)
« Rendu CPU moyen » est une moyenne glissante sur ≈ 50 images, donc la dernière seconde du run,
pas la moyenne des 30 min (libellé corrigé dans le code après ce run). Le **max du run, 10,8 ms**,
est à comparer aux 2,8 ms de la Phase 0 (mire seule, sans upload) et aux 6,5–10,3 ms des soaks A6 :
le surcoût suit les uploads d'images des inputs (ici 4 × 1080p NV12 à 50 i/s). Hypothèse non
vérifiée : contention sur les verrous internes de wgpu entre les `write_texture` des threads
GStreamer et la soumission du thread de rendu (dette notée dans `ROADMAP.md`). Le budget (20 ms)
est tenu avec 46 % de marge au pire tick ; elle sera plus faible en 59,94 / 60p (16,7 ms).
