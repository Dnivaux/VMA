# ADR 0001 — Architecture générale

- **Statut** : **accepté avec amendements** (validation du mainteneur, 2026-10-26 — texte
  d'origine en fin de document)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 et §6, Phase 0

## Décision

### Workspace
Workspace Cargo unique, édition 2024, resolver 3, **MSRV 1.95** (imposée par `eframe 0.37`).
Licence du projet : **MIT OR Apache-2.0**. Paquets nommés `vma-<crate>` (nom provisoire
« VMA », ADR 0002) ; découpage conforme au BRIEF §5 (`core`, `clock`, `engine`, `compositor`,
`io-gst`, `io-ndi`, `io-html`, `audio`, `replay`, `api`, `ui`, `app`).

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
| `vma-render` | horloge maître + rendu GPU du Program | pas d'alloc applicative, pas de lock, pas d'I/O |
| writer de logs | `tracing-appender` non bloquant | absorbe l'I/O des logs |
| `vma-soak` (option `--soak`) | agressions de la fenêtre, rapport de soak | hors chemin chaud |

### Horloge maître
Cadence rationnelle (`FrameRate { num, den }`), échéance de l'image *n* calculée depuis *n*
(pas d'accumulation, donc pas de dérive). Attente hybride : `sleep` jusqu'à une **marge
d'attente active** avant l'échéance (1 ms par défaut, **configurable** :
`EngineConfig::spin_margin`, option `--spin-margin-us`), puis attente active. Une échéance
dépassée de plus d'une période = **image manquée** : elle est comptée et sautée (pas de
rattrapage), le Program reste calé sur l'heure.

Le **retard au réveil** de chaque tick alimente un histogramme sans verrou (classes
log-linéaires, précision ≈ 6 %, taille fixe) ; p50 / p99 / p99,9 / max sont affichés dans le
panneau Statistiques et écrits dans les rapports de soak.

### Horodatage des sorties (PTS)
**Le PTS de toute sortie (enregistrement, stream, NDI, SRT…) dérive de l'index d'image *n*
(`n × den / num`), jamais de l'heure murale** ni de `Tick::woke_at`. Une image manquée laisse
un trou d'une période dans les PTS, elle ne décale pas les suivantes. La correspondance avec
une horloge externe (audio, genlock) est traitée à part (points différés).

### Partage GPU UI ↔ rendu
Un seul `wgpu::Device` (celui créé par eframe), cloné vers le thread de rendu. Le rendu écrit
dans 3 textures Program échangées par un **triple buffer sans verrou** (un `AtomicU8`) :
le producteur n'écrit jamais dans la texture que l'UI détient.

**Invariant** (documenté dans `crates/engine/src/triple.rs` et sur `ProgramReader::update`) :
1. l'UI ne change de texture (`update`) **qu'entre deux de ses frames, après avoir soumis**
   tout ce qui échantillonne la texture précédente ;
2. UI et rendu soumettent sur **la même `Queue`**, qui exécute dans l'ordre de soumission.

La texture rendue par l'UI ne peut donc être réécrite qu'après la soumission qui la lit, et
le GPU exécute la lecture d'abord. Tests : `triple::concurrent_stress`,
`triple::producer_never_writes_the_held_slot` (indices) et
`render_loop::triple_buffer_gpu_stress` (lecture GPU réelle pendant que le moteur tourne à
240 fps ; vérifié en échec si l'invariant est violé).

**Contre-pression GPU** : au plus 2 images Program en vol. Au-delà, le tick est sauté (image
répétée, compteur `gpu_dropped`) au lieu de bloquer l'horloge.

### Perte du GPU (device lost)
Le moteur s'approprie le callback `device_lost` et le gestionnaire d'erreurs non capturées
du device (celui de wgpu par défaut panique) :
- **erreur GPU non capturée** : journalisée et comptée (`gpu_errors`), jamais fatale ;
- **device perdu** : le thread de rendu n'émet plus aucune commande GPU mais **l'horloge
  continue** (ticks comptés dans `device_lost_ticks`) ; raison exposée dans les stats ;
- **application (Phase 0)** : l'UI partageant le device, la fenêtre ne peut plus être rendue.
  Elle se ferme et le processus sort **en erreur** (message + code ≠ 0) pour qu'un opérateur
  ou un superviseur le relance. Aucune tentative de recréation en place.
- **Plus tard (Phase 2, ADR dédié)** : avec des sorties actives, une perte GPU ne doit pas
  couper le stream. Piste : les sorties continuent avec la dernière image valide ou un noir
  généré côté CPU, pendant que le moteur recrée device, ressources et UI.

Test : `render_loop::device_lost_stops_rendering_not_the_clock` (perte forcée par
`Device::destroy`).

### Couleur
- **Phase 0** : textures Program `Rgba8Unorm` contenant du R'G'B' Rec.709 encodé, pleine plage
  (format exigé par egui-wgpu pour les textures natives).
- **Phase 1** : cibles internes **`Rgba16Float`**, passe de sortie vers `Rgba8Unorm` pour l'UI
  et vers YUV pour les sorties, conversion YUV → RGB en shader à l'entrée.
- **Domaine des transitions (amendement 1)** : le mode de mixage des transitions est
  **configurable** ; par défaut, mixage en **domaine gamma** (R'G'B' encodé : comportement des
  mélangeurs broadcast, un fondu à 50 % donne la moyenne des valeurs encodées). Le domaine
  **linéaire** est réservé au **redimensionnement et au filtrage** (et disponible en option
  pour les transitions). Le stockage en `Rgba16Float` sert les deux sans perte de précision.

## Limites connues (assumées en Phase 0)
- **Allocations internes à wgpu** : `create_command_encoder`, `queue.submit` et le callback
  `on_submitted_work_done` allouent à l'intérieur de wgpu. Aucune allocation *applicative* dans
  la boucle. À mesurer en Phase 1 (compteur d'allocations dans le soak). Les chemins
  exceptionnels (erreur GPU, perte du device) allouent pour journaliser.
- **Verrous internes à wgpu** : la `Queue` est partagée avec l'UI ; wgpu-core sérialise les
  soumissions par des mutex internes. Une contention UI ↔ rendu est possible mais courte ;
  les soaks calme et hostile la couvrent.
- **Priorité du thread de rendu** : priorité OS par défaut. Une élévation (SCHED_FIFO /
  `THREAD_PRIORITY_TIME_CRITICAL`) demandera de l'`unsafe` ou une dépendance → ADR dédié si
  les soaks montrent des retards (l'histogramme du réveil sert à le décider).
- **Attente active** : ≈ la marge par image (1 ms ≈ 5 % d'un cœur à 50 fps), pour la précision
  du réveil.
- **Temps GPU** : seul le temps CPU de rendu est mesuré ; le temps GPU (timestamp queries) et
  l'utilisation GPU (NVML) viendront plus tard.
- **Soak hostile sous Wayland** : un client ne peut ni se positionner ni se dé-minimiser ;
  ces agressions automatiques sont sans effet sous Hyprland et sont faites à la main
  (`docs/manual-tests/phase0-soak-hostile.md`).

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
- **Encodeur de secours CI (Phase 2)** : **`openh264enc`** (BSD-2) à la place de x264 (GPL,
  `gst-plugins-ugly`) — accord du mainteneur ; ADR à rédiger en Phase 2.
- **Horloge maître vs horloge audio (Phase 3)** : une carte son a son propre quartz et dérive
  par rapport à l'horloge monotone du système (quelques dizaines de ppm, soit une image toutes
  les quelques dizaines de minutes). À trancher en Phase 3 : quelle horloge est maître (vidéo,
  audio, ou référence externe) et où se fait le rééchantillonnage (audio asservi vs saut /
  répétition d'images). ADR dédié.
- **Recréation à chaud après perte du GPU** : Phase 2 (voir « Perte du GPU »).
- **`insta`** (BRIEF §6) sera ajouté quand un premier snapshot textuel en aura besoin.

## Alternatives écartées
- **Rendu dans le callback de peinture egui** : l'UI cadencerait le Program (dépend du vsync
  écran, 60 Hz ≠ 50 Hz). Rejeté : l'horloge maître doit être indépendante de l'affichage.
- **Device GPU séparé pour le rendu** : pas de partage de textures avec l'UI sans copie, et
  l'invariant du triple buffer (même `Queue`) ne tiendrait plus.

## Validation du mainteneur (texte d'origine, 2026-10-26)

> ADR 0001 validé avec amendements :
>
> 1. Transitions : mode de mixage configurable, domaine gamma par défaut (comportement broadcast), linéaire réservé au scaling et au filtrage. Garder Rgba16Float en interne.
> 2. Documenter dans le code l'invariant du triple buffer (swap du front uniquement entre deux frames UI, après soumission) et ajouter un test de stress.
> 3. Ajouter un soak « hostile » (resize, minimise, drag de fenêtre en continu, 10 min, 0 image manquée) et définir le comportement en cas de device lost.
> 4. Ajouter un histogramme du retard au réveil (p50, p99, max) au panneau stats et aux rapports de soak ; rendre la marge de 1 ms configurable.
> 5. Inscrire dans l'ADR que les PTS des sorties dérivent de l'index d'image n, jamais de l'heure murale.
> 6. Ajouter aux points différés : choix de l'horloge maître vis-à-vis de la dérive de l'horloge audio (Phase 3).
> 7. OK pour openh264enc en CI, ADR en Phase 2.
>
> Ensuite, tu peux implémenter la Phase 0.

| Amendement | Où |
|---|---|
| 1 | « Couleur » ci-dessus (implémentation : Phase 1) |
| 2 | `triple.rs`, `ProgramReader::update` ; tests `producer_never_writes_the_held_slot`, `triple_buffer_gpu_stress` |
| 3 | `vma --soak 600 --hostile` + `docs/manual-tests/phase0-soak-hostile.md` ; « Perte du GPU » ci-dessus |
| 4 | `vma_clock::LatencyHistogram`, panneau Statistiques, rapports ; `--spin-margin-us` |
| 5 | « Horodatage des sorties » ci-dessus |
| 6, 7 | « Points différés » ci-dessus |
