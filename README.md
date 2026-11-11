# VMA

Mélangeur vidéo live open source en Rust : switcher, streaming, enregistrement.
Linux et Windows en priorité, macOS ensuite. Nom provisoire (BRIEF §12 : « VMA » remplace le nom de code « Aiguillage »).

> **État** : Phases 0 à 3 closes sous Linux (mélange, entrées, enregistrement, streaming,
> plein écran, audio, SRT, NDI®), Phase 4 en revue (overlays, input HTML, stinger). Voir
> [`ROADMAP.md`](ROADMAP.md).

## Démarrer
```sh
cargo run --release
```
Prérequis et installation (Arch Linux, Windows) : [`docs/dev-setup.md`](docs/dev-setup.md).

## Machine de production : GPU en performances maximales
Avec la charge légère d'un mélange, le pilote du GPU peut rester en économie d'énergie et
ralentir son lien PCIe : les copies d'images des inputs vers le GPU deviennent alors 5 à
10 fois plus lentes (mesuré sur la machine de dev, NVIDIA, lien en Gen 1 :
[`docs/specs/io-gst.md`](docs/specs/io-gst.md), « Relâche avancée du temps de copie »).
VMA compense, mais avec beaucoup de sources live 1080p la marge reste mince. Sur une machine
de production, régler le mode d'alimentation du pilote sur **performances maximales** (NVIDIA
sous Windows : Panneau de configuration NVIDIA → Gérer les paramètres 3D → Mode de gestion de
l'alimentation). Réglage pas encore vérifié avec VMA sous Linux ni sous Windows.

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
