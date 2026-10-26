# ADR 0005 — Chaîne couleur (Phase 1)

- **Statut** : accepté (mainteneur, 2026-10-26)
- **Date** : 2026-10-26
- **Contexte** : BRIEF §5 (couleur), ADR 0001 amendement 1 (gamma par défaut pour les
  transitions, linéaire pour l'échelle et le filtrage, `Rgba16Float` en interne)

## Décision
1. **Stockage interne** : textures `Rgba16Float` contenant des valeurs **R'G'B' Rec.709
   encodées** (pas de la lumière linéaire), pleine plage, 0–1. Le mixage par défaut se fait
   donc sans conversion ; la demi-précision (11 bits de mantisse) garde une marge confortable
   sur 8 et 10 bits.
2. **Entrée** : Y'CbCr → R'G'B' en shader, avec la matrice et la plage de l'input (601 / 709,
   limitée 16–235 / 16–240 ou pleine), lues dans les caps GStreamer ou déduites (spec `io-gst`).
   BT.2020 / HDR : hors périmètre.
3. **Linéarisation** (mise à l'échelle, Fade en domaine linéaire) : **EOTF BT.1886 simplifiée,
   puissance 2,4** (noir à 0) : `L = V^2,4`, et son inverse exact `V = L^(1/2,4)`. Le même
   couple sert dans les deux sens, donc un aller-retour sans traitement est l'identité. On
   travaille en lumière d'affichage (display-referred), comme un mélangeur broadcast.
4. **Sortie vers l'UI** : `Rgba16Float` → `Rgba8Unorm` par arrondi au plus proche, sans
   dithering en Phase 1. Les sorties Phase 2 convertiront vers Y'CbCr 709 plage limitée.
5. **Couleur unie** : saisie en R'G'B' 8 bits, stockée telle quelle.

## Conséquences
- Un Fade 50 % en domaine gamma donne la moyenne des codes : comportement attendu d'un
  mélangeur broadcast et vérifiable bit à bit en golden frame.
- Le filtrage d'échelle en lumière linéaire évite l'assombrissement des détails fins (un damier
  noir / blanc réduit de moitié donne 191 et non 128).
- Les inputs à la taille exacte du Program ne sont jamais linéarisés : pas de coût ni de perte.

## Alternatives écartées
- **Inverse de l'OETF Rec.709** (côté caméra) pour linéariser : non appariée à l'affichage, et
  la partie linéaire près du noir complique l'aller-retour pour un gain nul ici.
- **Stocker en lumière linéaire** : chaque Fade gamma (le cas par défaut) demanderait un
  ré-encodage et un décodage.
- **sRGB** (`*Srgb`, γ ≈ 2,2) : ce n'est pas la référence vidéo, et egui-wgpu exige du
  `Rgba8Unorm` pour les textures natives.
