# ADR 0014 — MultiView

- **Statut** : **accepté** le 2026-11-11 (« ok pour la MultiView ») ; Q9 et Q10 de
  [`phase-5.md`](../specs/phase-5.md) : « au plus proche de la référence »
- **Date** : 2026-11-11
- **Contexte** : BRIEF §4 (Phase 5 : « grille, labels, bordures tally »), §2.1 (clean room :
  le guide public de la référence décrit ses dispositions par leur nom, leur géométrie n'y est
  qu'en images, qu'on ne reproduit pas) ; ADR 0004 amendement 1 (téléversements par le rendu)

## Ce que dit le guide public de la référence (texte, pages 18–19 et 37–39)
- La MultiView est l'une des **sources** d'une sortie (plein écran 1 et 2, sorties
  supplémentaires) : Program, Preview, MultiView ou un input donné. Deux plein écran au plus.
- **Dispositions** : « Preview / Output + 8 inputs », « + 14 inputs », « + 2 inputs », toutes
  avec **titres** et **vumètres** ; « Legacy » : Preview / Output + 8 inputs **sans** titres ni
  vumètres, Preview et Output repérés par une **bordure de couleur**.
- **Ordre des cases** réglable (« Customise Layout ») : par numéro d'input, ou par type (les deux
  premières caméras dans les deux premières cases, où qu'elles soient dans l'émission).
- **Titres** au-dessus ou au-dessous de chaque image ; titres de Preview / Output : « Preview »
  et « Output », ou le titre de l'input qu'elles montrent.

## Décision
1. **Dispositions** (géométrie propre à VMA, image 16:9 à la résolution du maître) :
   - **Preview / Program + 8** : Preview et Program côte à côte sur la moitié haute ; 8 inputs
     en 2 rangées de 4 sur la moitié basse.
   - **+ 14** : Preview et Program côte à côte sur la moitié haute ; 14 inputs en 2 rangées de
     7 dessous.
   - **+ 2** : Preview et Program sur la moitié haute, 2 inputs côte à côte dessous.
   - **Sans titres** (l'équivalent de « Legacy ») : + 8, sans titres ni vumètres.
2. **Cases** : chaque case montre l'image de son input (vignette 320 × 180 agrandie ou réduite),
   une **bordure rouge** si l'input est à l'antenne (Program, overlay entré, transition :
   même règle que le tally), **verte** s'il est en Preview ; son **titre** (numéro et nom) dans
   un bandeau au-dessus ou au-dessous (réglage) ; un **vumètre** stéréo vertical au bord de
   l'image pour les inputs qui ont du son. Preview et Program : bordure verte et rouge, titre
   « Preview » / « Program » ou le nom de l'input (réglage).
3. **Ordre** : par défaut, les inputs dans l'ordre d'affichage ; « Personnaliser » choisit
   l'input de chaque case (ou « suivant », ou vide).
4. **Sources des sorties** : le plein écran et le NDI® prennent une source : Program
   (défaut), Preview ou MultiView ; **deux plein écran** possibles, un par écran.
5. **Rendu** : une passe à part du thread de rendu, faite **seulement si une sortie la
   montre** : des quadrilatères texturés (vignettes de l'atlas, peint à chaque tick tant que la
   MultiView est montrée ; Preview, Program), des rectangles pleins (bandeaux, bordures,
   vumètres lus sans verrou) et les titres. Les titres sont rastérisés **hors du rendu et hors
   de `vma-control`**, sur un thread à part (`vma-titles`), quand ils changent ; le rendu les
   copie dans sa texture de titres quand leur version change (rare : un nom, la disposition).
   Mise en œuvre (2026-11-11) : une première version rastérisait sur `vma-control` et retardait
   ses commandes de plusieurs secondes en build de test ; d'où le thread à part.
6. **Police des titres** : celle d'egui (Ubuntu, Ubuntu Font Licence 1.0, déjà embarquée),
   contours lus par `skrifa` (comme egui, MIT ou Apache-2.0) et rastérisés par
   `ab_glyph_rasterizer` (Apache-2.0) ; `ab_glyph` écarté : il tire `ttf-parser`, signalé non
   maintenu (`cargo deny`, 2026-11-11). Licence de la police dans « À propos ».

## Alternatives écartées
- **Dessiner la MultiView avec egui dans la fenêtre plein écran** : pas de MultiView sur le NDI
  ni dans un enregistrement ; et le plein écran suit l'UI, pas le maître.
- **Un moteur de texte complet (`cosmic-text`, `glyphon`)** : des titres d'une ligne n'en ont
  pas besoin ; il viendra peut-être avec les titres natifs (ADR 0002).

## Conséquences
- `OutputSpec::Fullscreen` et `OutputSpec::Ndi` gagnent une source ; la règle « un seul plein
  écran » devient « un par écran, deux au plus ».
- Le coût est mesuré (E12 : ≤ 2 ms GPU à 1080p, 10 cases, machine de dev).
