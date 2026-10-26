# VMA

Mélangeur vidéo live open source en Rust : switcher, streaming, enregistrement.
Linux et Windows en priorité, macOS ensuite. Nom provisoire (BRIEF §12 : « VMA » remplace le nom de code « Aiguillage »).

> **État** : Phase 0 (fondations). L'application ouvre une fenêtre qui affiche une mire
> générée sur le GPU à 50 images/s, avec un panneau de statistiques. Aucune entrée vidéo,
> aucune sortie pour l'instant. Voir [`ROADMAP.md`](ROADMAP.md).

## Démarrer
```sh
cargo run --release
```
Prérequis et installation (Arch Linux, Windows) : [`docs/dev-setup.md`](docs/dev-setup.md).

## Documentation
- [`BRIEF.md`](BRIEF.md) — vision, périmètre, architecture cible
- [`ROADMAP.md`](ROADMAP.md) — état réel, phase par phase
- [`docs/adr/`](docs/adr/) — décisions d'architecture
- [`docs/specs/`](docs/specs/) — spécifications par module

## Licence
Au choix : [MIT](LICENSE-MIT) ou [Apache-2.0](LICENSE-APACHE).
