# ADR 0004 — Entrées GStreamer et dépendances de la Phase 1

- **Statut** : proposé (à valider)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 (upload GPU), §6 (stack), §2.6 (licences) ; spec
  [`docs/specs/io-gst.md`](../specs/io-gst.md)

## Décision

### GStreamer via gstreamer-rs, lié dynamiquement
- Crates `gstreamer`, `gstreamer-app`, `gstreamer-video` **0.25** (MIT OR Apache-2.0, MSRV 1.92
  ≤ notre 1.95). Elles se lient aux **bibliothèques GStreamer du système** (≥ 1.24).
- GStreamer (cœur, base, good, bad, libav) est **LGPL-2.1-or-later**, chargé **dynamiquement** :
  compatible avec un binaire MIT/Apache. `cargo deny` ne voit que les crates Rust ; la règle est
  donc écrite ici et dans `docs/dev-setup.md`.
- **Aucun plugin GPL** n'est requis ni demandé par nom (pas de `x264enc`, pas de
  `gst-plugins-ugly`). Le jour où l'on embarque GStreamer dans un installeur Windows, la liste des
  plugins embarqués fera l'objet d'un ADR de packaging (exclusion des plugins GPL).
- `io-gst` est une dépendance **obligatoire** (pas une feature) : sans GStreamer, pas de
  capture ni de fichier. L'application démarre quand même si l'initialisation de GStreamer
  échoue : seuls les inputs « couleur » et « image » restent disponibles, avec un message.

### Un thread `vma-io` possède les pipelines
Création, changements d'état, seeks et redémarrages des pipelines peuvent bloquer : ils vivent
sur un thread `vma-io` piloté par `vma-control` (ADR 0003) via un canal. Les messages du bus
sont relayés par un *sync handler* (sur les threads GStreamer, sans traitement lourd) vers
`vma-io`, qui en fait des événements pour `vma-control` (`Live`, `SignalLost`, …).

### Upload : sur le thread de streaming, dans un triple buffer de textures
Le callback `new-sample` de l'appsink (thread de streaming GStreamer) écrit les plans de l'image
dans la texture « back » de l'input (`queue.write_texture`, même `Queue` que le rendu) puis
publie. **Même invariant que le triple buffer Program (ADR 0001)**, avec le rendu comme
consommateur : il ne change d'image qu'en début de tick, avant d'encoder. Le zero-copy
(interop CUDA / Vulkan / D3D11) reste une optimisation ultérieure, derrière un trait.

### Autres dépendances
| Crate | Licence | Usage |
|---|---|---|
| gstreamer, gstreamer-app, gstreamer-video 0.25 | MIT OR Apache-2.0 | entrées (`io-gst`) |
| serde 1, serde_json 1 | MIT OR Apache-2.0 | état, commandes, journal (BRIEF §6) |
| image 0.25 (PNG, JPEG) | MIT OR Apache-2.0 | input « image fixe » (déjà en dev-dépendance) |
| rfd 0.17 | MIT | sélecteur de fichier natif (portail XDG sous Linux) — *question 6* ; features choisies pour ne pas tirer de runtime async lourd, `cargo deny` fera foi |

### CI
- Ubuntu : `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`,
  `gstreamer1.0-plugins-{base,good}`, `gstreamer1.0-libav` par apt.
- Windows : installeurs MSVC officiels runtime + development (msiexec silencieux), mis en cache.
- Les tests `io-gst` utilisent uniquement `videotestsrc` et des fichiers générés pendant le test
  (`videotestsrc ! … ! matroskamux`, plugins base / good) : rien de propriétaire, rien de GPL.

## Conséquences
- Temps de CI en hausse (installation de GStreamer sous Windows).
- Prérequis de dev : GStreamer dev + `pkg-config` (déjà documentés dans `docs/dev-setup.md`).
- Le crate `io-gst` porte aussi l'input « image fixe » (décodée par `image`, pas par
  GStreamer) pour garder un seul propriétaire du thread `vma-io` et des producteurs d'images.

## Alternatives écartées
- **FFmpeg direct** (bindings `ffmpeg-next`) : capture multiplateforme à réécrire, et le BRIEF
  impose GStreamer.
- **Upload sur le thread de rendu** : copie de 3 Mo par input et par tick dans le chemin chaud.
- **Conversion systématique par `videoconvert` en RGBA** : coût CPU élevé à 4 × 1080p50 ; la
  conversion YUV → RGB se fait en shader (BRIEF §5), `videoconvert` ne sert que pour les
  formats non pris en charge.
