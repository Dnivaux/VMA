# Rapport de soak (calme) — 2026-10-27 18:55 UTC

**Verdict : ✅ RÉUSSI (0 image manquée, 0 image répétée ; inputs sans perte, à leur cadence, latence ≤ 2 images)**

## Configuration
| Paramètre | Valeur |
|---|---|
| Commande | `vma --soak 120 --inputs 4 --no-journal --report /tmp/claude-1000/-home-dnivaux-Projects-VMA/f8a03cc5-820e-47fc-aa75-a88bd2ce8cdd/scratchpad/cpu-4in.md` |
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
| Retard au réveil p50 · p99 · p99,9 · max | 0.000 ms · 0.005 ms · 0.009 ms · 0.958 ms |

Percentiles : borne haute de leur classe d'histogramme (≈ 6 %), sur 6001 réveils.

### Temps CPU de rendu par tick
Thread de rendu, hors exécution GPU ; budget d'un tick : 20.000 ms. Le tick complet compte aussi le séquenceur et la prise des images des inputs.

| Phase | p50 | p99 | p99,9 | max |
|---|---|---|---|---|
| **Tick complet** | 7.864 ms | 10.486 ms | 10.694 ms | 10.694 ms |
| `device.poll` | 0.106 ms | 0.410 ms | 0.524 ms | 0.689 ms |
| Encodage (uniforms compris) | 3.146 ms | 9.961 ms | 10.438 ms | 10.438 ms |
| `queue.submit` | 4.063 ms | 7.078 ms | 7.602 ms | 8.273 ms |

Sur 6001 ticks rendus.

## Inputs
29 transitions automatiques (une toutes les 4 s, Fade 0,5 s puis Cut). Latence : publication de l'image → tick qui la prend ; limite A10 : 2 images (40.0 ms).

| Input | Format | Reçues / s (nominal) | Sautées | Répétées | Latence p50 · p99 · max | Pertes de signal |
|---|---|---|---|---|---|---|
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 50.00 (50p) | 0 | 0 | 14.680 ms · 19.663 ms · 19.663 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.99 (50p) | 0 | 0 | 14.680 ms · 19.881 ms · 19.881 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.99 (50p) | 0 | 0 | 11.534 ms · 18.874 ms · 18.947 ms | 0 |
| Synthétique 1920x1080 50p | 1920x1080 NV12 | 49.99 (50p) | 0 | 0 | 11.010 ms · 17.826 ms · 17.912 ms | 0 |

Sautées : images remplacées avant d'être prises (source plus rapide que le maître) ; répétées : ticks sans image nouvelle (source plus lente). Les deux sont attendues quand les cadences diffèrent.

## UI (thread principal)
| Mesure | Valeur |
|---|---|
| Frames UI | 6890 |
| Rafraîchissement moyen | 57.4 Hz |
| Plus long blocage UI | 34.390 ms |

Un blocage de l'UI (déplacement, minimisation) ne doit pas affecter le Program.


## Actions manuelles
_À compléter par l'opérateur (voir `docs/manual-tests/phase0-soak-hostile.md`) : déplacement à la souris, redimensionnement par le bord, autres._

## Note (analyse)
Comparé au run sans input ([`2026-10-27-phase1-render-cpu-0-input.md`](2026-10-27-phase1-render-cpu-0-input.md),
tick complet p50 0,094 ms), le tick médian passe à 7,9 ms : c'est le coût courant, pas un pic
rare. Il se répartit entre l'encodage (qui fait des `queue.write_buffer` pour les uniforms) et
`queue.submit`.

**Le thread de rendu attend, il ne calcule pas.** Mesure complémentaire, run de 40 s identique, temps
CPU par thread lu dans `/proc/<pid>/task/<tid>/schedstat` sur 10 s (500 ticks) :

| Thread | Temps CPU sur 10 s |
|---|---|
| `vma-render` | 710 ms, soit ≈ 1,4 ms par tick, attente active de l'horloge (≤ 1 ms) comprise |
| 4 threads `videotestsrc` (génération 1080p + `write_texture`) | 4,0 à 4,7 s chacun |

Le tick complet de ce run : p50 5,0 ms, p99 10,0 ms (temps mural). Le thread de rendu est donc
bloqué la majeure partie du tick. C'est cohérent avec une contention sur les verrous internes
de la `Queue` wgpu, tenus par les `write_texture` des threads GStreamer (copie de ≈ 3 Mo par
image 1080p NV12, 200 fois par seconde). Le verrou précis n'est pas identifié.
