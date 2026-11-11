# Paquet Windows d'essai

Demande du mainteneur (2026-11-11) : une version que la partenaire lance **par un double-clic**,
sans rien installer, pour à la fois regarder l'interface (checklist
[`observation-reference-interface.md`](../manual-tests/observation-reference-interface.md)) et
passer les soaks Windows reportés depuis la Phase 0 (A1 et A13 :
[`phase0-soak-hostile.md`](../manual-tests/phase0-soak-hostile.md), section Windows).
Ce n'est pas l'installeur d'une version (Phase 7) : pas de signature, pas de mise à jour.

## Fabrication
Workflow `windows-package.yml`, à la demande : `gh workflow run windows-package.yml`, puis
l'artefact **VMA-windows** du run (un zip, gardé 30 jours) est ce qu'on envoie.
1. GStreamer 1.26.11 officiel (action `.github/actions/gstreamer-windows`, la même que la CI).
2. `cargo build --release -p vma`.
3. [`packaging/windows/bundle.ps1`](../../packaging/windows/bundle.ps1) assemble le dossier
   dans la disposition de GStreamer (`bin`, `lib\gstreamer-1.0`, `libexec`), pour qu'il trouve
   ses plugins à côté de ses DLL :
   - plugins : tous ceux qui se chargent et dont la licence déclarée (`gst-inspect-1.0`) n'est
     **pas GPL** (BRIEF §2.6) ; la liste gardée et la liste écartée sont dans
     `Licences\LICENCES.txt` ;
   - DLL : la fermeture des imports (`dumpbin /dependents`) de `vma.exe`, du scanner et des
     plugins, prise dans `bin` de GStreamer ; le runtime Visual C++ (redistribuable) copié de
     System32 ; le reste est à Windows. Le paquet échoue si une DLL d'une bibliothèque GPL
     connue (x264, x265…) y entre ;
   - lanceurs, `LISEZMOI.txt`, licences (VMA, LGPL-2.1, MPL-2.0), la checklist et son image.
4. Contrôles : le GStreamer du paquet, **seul** (l'installé hors du PATH), a les éléments dont
   VMA se sert et n'a ni `x264enc` ni `x265enc` ; un soak de 5 s depuis le paquet (720p25, un
   input) doit aller au bout, input compris (images manquées tolérées : pas de GPU sur le
   runner).

## Ce que la testeuse a
| Fichier | Effet |
|---|---|
| `Ouvrir VMA.cmd` | VMA avec 4 mires en inputs ; journal des commandes et sortie de VMA dans `Rapports\` |
| `Test Windows - 40 min.cmd` | A1 (30 min, 1080p50, 4 sources, sans toucher), puis A13 (10 min, `--hostile`, actions à la souris décrites à l'écran) ; un rapport chacun, aux noms de `tests/soak/reports/` ; trois questions sur les actions faites, ajoutées au rapport A13 ; `machine-*.txt` (Windows, CPU, GPU et pilote, écran, secteur ou batterie, mode d'alimentation) ; pas de mise en veille pendant le test |
| `LISEZMOI.txt` | débloquer le zip, extraire, SmartScreen, les deux usages, quoi renvoyer |

Le lanceur (`bin\launch.ps1`) ne prend que le GStreamer du paquet : ses plugins, son scanner
et un registre à part (`%LOCALAPPDATA%\VMA`), même si un GStreamer est installé.
Pas d'input HTML dans le paquet (`vma-html` : démarrage de CEF sous Windows non résolu,
Phase 4 étape 4).
