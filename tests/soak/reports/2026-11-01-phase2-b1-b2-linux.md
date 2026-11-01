# Rapport de soak (calme) — 2026-11-01 20:08 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée ; inputs sans perte, à leur cadence, latence ≤ 2 images)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 3600 --no-journal --inputs 6 --record /tmp/tmp.y1oiwW1OHI/rec --stream rtmp://127.0.0.1:1935/live --sync-test --sync-check rtmp://127.0.0.1:1935/live/*** --report tests/soak/reports/2026-11-01-phase2-b1-b2-linux.md` |
| Mode | calme |
| OS | linux (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 5090 · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 3600 s · 3600.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 180006 (attendu ≈ 180000) |
| **Images manquées (horloge)** | **0** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 180006 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.000 ms · 0.001 ms · 3.013 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 180006 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 0.279 ms | 0.524 ms | 0.786 ms | 23.010 ms |
| `device.poll` | 0.023 ms | 0.043 ms | 0.172 ms | 0.260 ms |
| Encodage (uniforms compris) | 0.221 ms | 0.344 ms | 0.492 ms | 22.820 ms |
| `queue.submit` | 0.031 ms | 0.213 ms | 0.393 ms | 1.022 ms |
| dont étage de sortie (FTB, NV12, relecture) | 0.007 ms | 0.009 ms | 0.055 ms | 0.538 ms |

Sur 180006 ticks rendus.

## Inputs
899 transitions automatiques (une toutes les 4 s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Upload occupé | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|---|
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 0 | 17.826 ms · 17.826 ms · 19.723 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 0 | 17.826 ms · 17.826 ms · 19.715 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 0 | 16.777 ms · 16.777 ms · 19.626 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 0 | 15.729 ms · 16.253 ms · 18.654 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 1 | 1 | 0 | 18.053 ms · 18.053 ms · 18.053 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 0 | 14.680 ms · 14.680 ms · 17.440 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent. Upload occupé : images perdues parce que le tampon de transfert de leur emplacement était encore utilisé par le GPU (ADR 0004, amendement 1) ; doit rester à 0.

## Sorties
Images non relues par le rendu : 0 ; perdues faute d'image libre (pool) : 0 ; distribuées : 179994 (dont noires, GPU perdu : 0).

| Sortie | État | Encodeur | Envoyées | Perdues | File | Reconnexions | Fichier / écran | Taille |
|---|---|---|---|---|---|---|---|---|
| Enregistrement | en cours | nvh264enc + avenc_aac | 179994 | 0 | 0 | 0 | `/tmp/tmp.y1oiwW1OHI/rec/VMA 2026-11-01 21-08-08.mkv` | 208.8 Mo |
| Stream 1 | en cours | nvh264enc + avenc_aac | 179991 | 0 | 0 | 0 | `rtmp://127.0.0.1:1935/live` | — |

## Synchro (B1)
Flux relu : `rtmp://127.0.0.1:1935/live/***` (décodeurs logiciels). Décalage = début du bip − première image blanche ; positif : le son est en retard.

| Mesure | Valeur | |
|---|---|---|
| Flashs appariés à leur bip | 3595 (attendu ≥ 3585) | ✅ |
| Flashs · bips seuls | 0 · 0 | |
| **Pire décalage (valeur absolue)** | **0.58 ms** (limite : une image, 20.00 ms) | ✅ |
| Décalage min · moyen · max | -0.58 ms · -0.20 ms · +0.10 ms | |
| Dérive (dernier − premier) | +0.65 ms | |
| Images relues · trous | 179768 · 0 | ✅ |
| Interruptions du flux relu | 0 | ✅ |

Les 15 premières secondes ne comptent pas (démarrage du stream, analyse par le serveur, attente d'une image clé).

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 215978 |
| Rafraîchissement moyen | 60.0 Hz |
| Plus long blocage UI | 557.364 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
