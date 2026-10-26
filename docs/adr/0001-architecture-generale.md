# ADR 0001 — Architecture générale

- **Statut** : proposé (à valider par le mainteneur)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 et §6, Phase 0

## Décision

### Workspace
Workspace Cargo unique, édition 2024, resolver 3, **MSRV 1.95** (imposée par `eframe 0.37`).
Licence du projet : **MIT OR Apache-2.0**. Paquets nommés `aiguillage-<crate>` ; découpage
conforme au BRIEF §5 (`core`, `clock`, `engine`, `compositor`, `io-gst`, `io-ndi`, `io-html`,
`audio`, `replay`, `api`, `ui`, `app`).

Graphe de dépendances (Phase 0) :
```
app ──► ui ──► engine ──► compositor ──► core
  │             │  └────► clock ───────► core
  └────────────►┘
```
Règle : `core` ne dépend de rien d'interne ; aucun crate ne dépend de `app` ou `ui`.

### Threads
| Thread | Rôle | Contraintes |
|---|---|---|
| UI (principal) | boucle eframe/winit, egui, présentation fenêtre | peut bloquer brièvement ; jamais attendu par le rendu |
| `aiguillage-render` | horloge maître + rendu GPU du Program | pas d'alloc applicative, pas de lock, pas d'I/O |
| writer de logs | `tracing-appender` non bloquant | absorbe l'I/O des logs |

### Horloge maître
Cadence rationnelle (`FrameRate { num, den }`), échéance de l'image *n* calculée depuis *n*
(pas d'accumulation, donc pas de dérive). Attente hybride : `sleep` jusqu'à 1 ms avant
l'échéance, puis attente active. Une échéance dépassée de plus d'une période = **image
manquée** : elle est comptée et sautée (pas de rattrapage), le Program reste calé sur l'heure.

### Partage GPU UI ↔ rendu
Un seul `wgpu::Device` (celui créé par eframe), cloné vers le thread de rendu. Le rendu écrit
dans 3 textures Program échangées par un **triple buffer sans verrou** (un `AtomicU8`) :
le producteur n'écrit jamais dans la texture que l'UI affiche. L'ordre des soumissions sur la
`Queue` unique garantit que l'UI a fini d'échantillonner une texture avant sa réécriture.

**Contre-pression GPU** : au plus 2 images Program en vol. Au-delà, le tick est sauté (image
répétée, compteur `gpu_dropped`) au lieu de bloquer l'horloge.

### Couleur (Phase 0)
Textures Program `Rgba8Unorm` contenant du R'G'B' Rec.709 encodé, pleine plage (format exigé
par egui-wgpu pour les textures natives). Le passage au compositing en lumière linéaire
(`Rgba16Float` interne + passe de sortie) est prévu en Phase 1 avec la conversion YUV → RGB.

## Limites connues (assumées en Phase 0)
- **Allocations internes à wgpu** : `create_command_encoder`, `queue.submit` et le callback
  `on_submitted_work_done` allouent à l'intérieur de wgpu. Aucune allocation *applicative* dans
  la boucle. À mesurer en Phase 1 (compteur d'allocations dans le soak).
- **Verrous internes à wgpu** : la `Queue` est partagée avec l'UI ; wgpu-core sérialise les
  soumissions par des mutex internes. Une contention UI ↔ rendu est possible mais courte ;
  le soak de 10 min la couvre.
- **Priorité du thread de rendu** : priorité OS par défaut. Une élévation (SCHED_FIFO /
  `THREAD_PRIORITY_TIME_CRITICAL`) demandera de l'`unsafe` ou une dépendance → ADR dédié si
  les soaks montrent des retards.
- **Attente active** : ≈ 1 ms par image (≈ 5 % d'un cœur à 50 fps), pour la précision du réveil.
- **Temps GPU** : seul le temps CPU de rendu est mesuré ; le temps GPU (timestamp queries) et
  l'utilisation GPU (NVML) viendront plus tard.

## Dépendances (Phase 0)
| Crate | Licence | Usage |
|---|---|---|
| wgpu 30 | MIT OR Apache-2.0 | rendu |
| egui / eframe / egui-wgpu 0.37 | MIT OR Apache-2.0 | UI |
| thiserror 2 | MIT OR Apache-2.0 | erreurs typées |
| anyhow 1 | MIT OR Apache-2.0 | `app/` uniquement |
| tracing / tracing-subscriber / tracing-appender | MIT | logs, métriques |
| bytemuck | MIT OR Apache-2.0 OR Zlib | uniformes GPU |
| crossbeam-channel, arc-swap | MIT OR Apache-2.0 | déclarées, utilisées à partir de la Phase 1 |
| pollster (dev) | MIT OR Apache-2.0 | tests GPU sans fenêtre |
| image (dev, PNG seul) | MIT OR Apache-2.0 | golden frames |

La politique de licences est appliquée par `cargo deny` (`deny.toml`) en CI.

## Points différés
- **Encodeur de secours CI (Phase 2)** : le BRIEF cite x264, qui est GPL (plugin
  `gst-plugins-ugly`). Il n'est pas lié au binaire, mais pour rester simple juridiquement je
  proposerai un ADR pour `openh264enc` (BSD-2) à la place.
- **`insta`** (BRIEF §6) sera ajouté quand un premier snapshot textuel en aura besoin.

## Alternatives écartées
- **Rendu dans le callback de peinture egui** : l'UI cadencerait le Program (dépend du vsync
  écran, 60 Hz ≠ 50 Hz). Rejeté : l'horloge maître doit être indépendante de l'affichage.
- **Device GPU séparé pour le rendu** : pas de partage de textures avec l'UI sans copie.
