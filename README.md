# VMA

Mélangeur vidéo live open source en Rust : switcher, streaming, enregistrement.
Linux et Windows en priorité, macOS ensuite. Nom provisoire (BRIEF §12 : « VMA » remplace le nom de code « Aiguillage »).

> **État** : Phases 0 à 2 closes sous Linux (mélange, entrées, enregistrement, streaming,
> plein écran), Phase 3 en cours (audio, SRT, NDI®). Voir [`ROADMAP.md`](ROADMAP.md).

## Démarrer
```sh
cargo run --release
```
Prérequis et installation (Arch Linux, Windows) : [`docs/dev-setup.md`](docs/dev-setup.md).

## NDI®
Les entrées et sorties NDI® passent par le plugin NDI de `gst-plugins-rs` (MPL-2.0), compilé
dans VMA. Le **runtime NDI n'est pas fourni** : l'installer à part (NDI Tools, ou `ndi-sdk`
sous Arch), ou indiquer son dossier par `NDI_RUNTIME_DIR_V6`, puis relancer VMA. Sans lui,
tout le reste fonctionne et les boutons NDI sont grisés.

NDI® is a registered trademark of Vizrt NDI AB.

## Documentation
- [`BRIEF.md`](BRIEF.md) — vision, périmètre, architecture cible
- [`ROADMAP.md`](ROADMAP.md) — état réel, phase par phase
- [`docs/adr/`](docs/adr/) — décisions d'architecture
- [`docs/specs/`](docs/specs/) — spécifications par module

## Licence
Au choix : [MIT](LICENSE-MIT) ou [Apache-2.0](LICENSE-APACHE).
