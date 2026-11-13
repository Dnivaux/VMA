# Rapport de soak (calme) — 2026-11-13 13:53 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée ; inputs sans perte, à leur cadence, latence ≤ 2 images)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 1200 --no-journal --no-autosave --rate 50 --resolution 1080p --inputs 4 --sync-inputs --replay-load /home/dnivaux/.cache/vma-soak-f7 --record /tmp/tmp.E2ZlYk2tah/rec --stream rtmp://127.0.0.1:1935/live --report tests/soak/reports/2026-11-13-phase6-f7-linux.md` |
| Mode | calme |
| OS | linux (x86_64) |
| Adaptateur GPU | NVIDIA GeForce RTX 5090 · Vulkan · DiscreteGpu |
| Cadence · résolution | 50p · 1920x1080 |
| Marge d'attente active | 1.000 ms |
| Durée demandée · réelle | 1200 s · 1200.0 s |

## Program (thread de rendu)
| Mesure | Valeur |
|---|---|
| Ticks | 60002 (attendu ≈ 60000) |
| **Images manquées (horloge)** | **0** |
| **Images répétées (GPU saturé)** | **0** |
| Images rendues | 60002 |
| Erreurs GPU | 0 |
| GPU perdu | non |
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.000 ms · 0.001 ms · 0.092 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 60002 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 0.328 ms | 0.590 ms | 0.950 ms | 9.551 ms |
| `device.poll` | 0.031 ms | 0.074 ms | 0.139 ms | 9.168 ms |
| Encodage (uniforms compris) | 0.254 ms | 0.377 ms | 0.426 ms | 1.335 ms |
| `queue.submit` | 0.037 ms | 0.229 ms | 0.557 ms | 2.194 ms |
| dont étage de sortie (FTB, NV12, relecture) | 0.009 ms | 0.012 ms | 0.025 ms | 0.600 ms |

Sur 60002 ticks rendus.

## Inputs
0 transitions automatiques, une toutes les 4 s environ, de chaque input au suivant (Fade : 0, Cut : 0) : placées entre les flashs des deux sources, un Fade quand il y tient, sinon un Cut. Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Upload occupé | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|---|
| Synchro 50p | 1920x1080 NV12 | 49.99 (50p) | 2 | 2 | 0 | 11.010 ms · 14.680 ms · 14.704 ms | 0 |
| Synchro 50p | 1920x1080 NV12 | 49.99 (50p) | 2 | 2 | 0 | 11.010 ms · 13.107 ms · 13.186 ms | 0 |
| Synchro 50p | 1920x1080 NV12 | 49.99 (50p) | 2 | 2 | 0 | 11.010 ms · 13.107 ms · 13.180 ms | 0 |
| Synchro 50p | 1920x1080 NV12 | 49.99 (50p) | 2 | 2 | 0 | 11.010 ms · 13.070 ms · 13.070 ms | 0 |
| Replay A | 1920x1080 NV12 | 9.58 | 0 | 47989 | 0 | 9.437 ms · 9.961 ms · 18.940 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent. Upload occupé : images perdues parce que le tampon de transfert de leur emplacement était encore utilisé par le GPU (ADR 0004, amendement 1) ; doit rester à 0.

## Sorties
Images non relues par le rendu : 0 ; perdues faute d'image libre (pool) : 0 ; distribuées : 59988 (dont noires, GPU perdu : 0).

| Sortie | État | Encodeur | Envoyées | Perdues | File | Reconnexions | Fichier / écran | Taille |
|---|---|---|---|---|---|---|---|---|
| Enregistrement | en cours | nvh264enc + avenc_aac | 59988 | 0 | 0 | 0 | `/tmp/tmp.E2ZlYk2tah/rec/VMA 2026-11-13 14-53-25.mkv` | 32.1 Mo |
| Stream 1 | en cours | nvh264enc + avenc_aac | 59986 | 0 | 0 | 0 | `rtmp://127.0.0.1:1935/live` | — |

## Audio (C14)
Mélange (`vma-audio`) : 59999 blocs, 0 ticks reconstruits, 0 ticks non transmis (file pleine), 0 échantillons de silence sur un trou ; échantillons envoyés en silence aux sorties faute de mélange à temps : 0.

| Input | Lu | Manquant depuis la chauffe | Écrasé | Reprises · sauts de latence | Cadence mesurée (avance attendue) |
|---|---|---|---|---|---|
| Synchro 50p | 1199.8 s | 0 | 0 | 4 · 4 | +0.0 ppm (+0 ppm) |
| Synchro 50p | 1199.7 s | 0 | 0 | 4 · 4 | +0.0 ppm (+0 ppm) |
| Synchro 50p | 1199.7 s | 0 | 0 | 4 · 4 | +0.0 ppm (+0 ppm) |
| Synchro 50p | 1199.7 s | 0 | 0 | 4 · 4 | +0.0 ppm (+0 ppm) |

Manquant : échantillons de la source pas encore arrivés quand le mélange les lit (lus comme du silence), comptés depuis la fin des 15 s de chauffe ; écrasé : arrivés mais remplacés avant d'être lus ; reprises : sauts de lecture de plus de 10 ms, chacun attendu avec un saut de la latence des images que le son suit (une image sautée ou répétée par une source qui dérive, un tampon de gigue recalé), pas au-delà. Cadence : celle de la source mesurée par le lecteur, rapportée à 48 kHz sur l'horloge commune.

## Replay (F7)
4 caméras enregistrées pour le replay (5 min gardées, 100 Mb/s) et en ISO ; toutes les 30 s : « −5 s » puis « Rejouer » (vitesses 50, 25, 75 % tour à tour, transition du bouton 1), un autre angle 2 s après, l'arrière pendant 1,5 s un cycle sur trois.

| Mesure | Valeur |
|---|---|
| Cycles (« −5 s » + « Rejouer ») | 40 |
| **Revenus à la caméra d'avant**, sur les cycles vérifiés (pas coupés par la fin) | **39 sur 39** |
| Changements d'angle · passages en arrière | 40 · 13 |
| **Commandes refusées** | **0** |
| Lecteur : images montrées · décodées | 11496 · 11364 |
| **Lecteur : en retard · ticks manqués · illisibles** | **13 · 0 · 0** |
| Dossier du replay à la fin | 501 Mo (les segments de plus de 5 min sont effacés) |
| ISO | 1990 Mo en 4 fichiers, 1.7 Mo/s |
| | Le débit suit l'image : des sources de test simples se compressent bien plus qu'une caméra (au plus 100 Mb/s par caméra ici) |
| Processeur (tout le processus) | 74 % d'un cœur en moyenne (24 cœurs) |

| Caméra | Encodeur | Offertes | Écrites | **Perdues** | Son perdu | Arrêt |
|---|---|---|---|---|---|---|
| Synchro 50p | nvh264enc | 59985 | 59985 | **0** | 0 | non |
| Synchro 50p | nvh264enc | 59985 | 59985 | **0** | 0 | non |
| Synchro 50p | nvh264enc | 59985 | 59985 | **0** | 0 | non |
| Synchro 50p | nvh264enc | 59985 | 59985 | **0** | 0 | non |

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 64218 |
| Rafraîchissement moyen | 53.5 Hz |
| Plus long blocage UI | 71479.892 ms |
| Erreurs GPU des fenêtres | 0 |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program. Les erreurs GPU des fenêtres (thread principal) sont comptées à part de celles du Program, sur le GPU qu'ils partagent.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._
