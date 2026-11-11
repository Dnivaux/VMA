# ADR 0011 — Identité visuelle de l'interface

- **Statut** : **accepté pour les icônes** le 2026-11-11 (demande du mainteneur : « ajoute des
  icônes comme ceux de react-icons ») ; palette et typographie **ouvertes**, à trancher après
  les retours de la partenaire
- **Date** : 2026-11-11
- **Contexte** : chantier « Interface familière » (`ROADMAP.md`) ; BRIEF §2.1–2.2 (clean room,
  aucune marque ni aucun asset tiers) ; inventaire de la référence
  [`ui-inventaire-reference.md`](../specs/ui-inventaire-reference.md) ; disposition
  [`ui.md`](../specs/ui.md), « Interface familière »

## Constat
L'interface doit être **familière** à une opératrice de la référence (mêmes zones, mêmes
gestes) sans **ressembler** à la référence : apparence propre à VMA. Les polices fournies par
egui 0.37 n'ont presque aucun symbole (ni flèche, ni haut-parleur, ni rouage) : sans jeu
d'icônes, les boutons restent en texte (« QP », « … », « Son »), moins lisibles.

## Décision
1. **Icônes Lucide** (le jeu « lu » de react-icons ; ISC, sous-ensemble dérivé de Feather en
   MIT), par la crate `egui-lucide` 0.2 (MIT) : les icônes sont des SVG compilés dans le
   binaire, rendus par le chargeur SVG d'`egui_extras` 0.37 (feature `svg` seule : `resvg`,
   sans texte ni images incluses), installé par le `Switcher` à sa première frame.
2. **Usage** (`crates/ui/src/switcher/icons.rs`) : 16 points, trait 2, blanches et teintées
   par la couleur du texte du bouton (survol, désactivé, sélection suivent le thème).
   - Commandes principales (barres, colonne centrale, menu Ajouter) : **icône + texte**.
   - Tuiles d'input et contrôles vidéo, où la place manque : **icône seule**, infobulle qui dit
     ce que fait le bouton, **nom accessible** donné à part (`icons::named`), si bien que les
     tests et les lecteurs d'écran trouvent chaque bouton par son nom.
3. **Correspondances** : Ajouter `plus`, Enregistrer `circle-dot` (rouge), Streamer
   `radio-tower`, Plein écran `monitor`, réglages d'une sortie `settings`, Sorties `cable`,
   Mélangeur `sliders-vertical`, Statistiques `chart-column`, Réglages `settings`, À propos
   `info`, Quick Play `play`, Cut `scissors`, réglage d'un bouton de transition
   `chevron-down`, FTB `sunset`, overlays `layers`, fermer `x`, boucle `repeat`, son
   `volume-2` / `volume-x`, lecture / pause / début `play` / `pause` / `skip-back` ; menu
   Ajouter : `palette`, `tv`, `camera`, `mic`, `film`, `cable`, `cast`, `globe`, `image`.
4. **Couleurs d'état** inchangées (spec `ui.md`) : rouge = Program, à l'antenne, enregistrement,
   muet ; vert = Preview. Thème sombre d'egui et police par défaut gardés **pour l'instant**.

## Alternatives écartées
- **Material Symbols** (`egui_material_icons`, police de 3,6 Mio) : complet, mais une police
  d'icônes met un caractère privé dans le nom accessible de chaque bouton, et son style est
  celui d'une plateforme tierce reconnaissable.
- **Phosphor** (`egui-phosphor`) : la crate n'était pas encore à egui 0.37 (un fork l'est).
- **Dessiner nos icônes** : long, et sans gain de lisibilité.

## Conséquences
- Dépendances : `egui-lucide`, `egui_extras` (svg) et ce qu'il tire (`resvg`, `usvg`,
  `tiny-skia`…) ; licences vérifiées par `cargo deny` (MIT, ISC, Apache-2.0, BSD-3-Clause) et
  notées dans le `Cargo.toml` racine ; Lucide et resvg cités dans « À propos ».
- Chaque icône est rastérisée au premier affichage puis gardée dans le cache de textures
  d'egui (thread de l'UI, jamais celui du rendu).
- Les tests sans fenêtre affichent les mêmes icônes ; la capture de relecture
  (`screenshot_of_the_main_window`) les montre.

## Reste ouvert
Palette propre à VMA, police (taille et graisse pour la lecture à distance), densité, thème
clair éventuel : après les retours de la partenaire (checklist
[`observation-reference-interface.md`](../manual-tests/observation-reference-interface.md)).
