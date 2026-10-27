# Test manuel — Phase 1 : utilisation réelle

**But** : vérifier à la souris et au clavier ce que les tests automatiques ne voient pas (rendu
réel, fluidité, ergonomie). Complété au fil des étapes de la Phase 1.
**Durée** : environ 10 minutes. **Lancer** : `cargo run --release`.

Cocher chaque case ; noter tout ce qui surprend sous « Remarques ».

## Étape 4 — couleurs, mire, transitions, T-Bar
### Démarrage et inputs
- [ ] La fenêtre s'ouvre **vide** : Preview et Program noirs, aucune vignette.
- [ ] « + Ajouter → Couleur → Rouge » : l'input 1 apparaît, **au Program** (cadre rouge), le
      Program devient rouge.
- [ ] « + Ajouter → Couleur → Bleu » : l'input 2 apparaît **en Preview** (cadre vert).
- [ ] « + Ajouter → Mire » : l'input 3 apparaît, ni Preview ni Program ; sa vignette montre la
      mire et le **repère blanc bouge** en bas.
- [ ] « + Ajouter → Couleur → sélecteur + Ajouter cette couleur » : la couleur choisie arrive.

### Preview, Cut, transitions
- [ ] Clic sur une vignette : elle passe en Preview (cadre vert, vue Preview à gauche).
- [ ] Touches `1`, `2`, `3` : même effet pour les inputs 1 à 3.
- [ ] **Cut** (bouton ou `Espace`) : Preview et Program s'échangent instantanément.
- [ ] Bouton **1 · Fade 0,5 s** (ou `Entrée`) : fondu fluide d'une demi-seconde, puis échange ;
      une barre de progression avance sous le bouton ; les deux vignettes sont **rouges**
      pendant le fondu.
- [ ] Boutons 2 et 3 : fondus de 1 s et 1,5 s. Bouton 4 : Cut.
- [ ] Pendant un fondu de 1,5 s, cliquer **Cut** ou un autre bouton : **rien**, le fondu va au
      bout (comportement observé sur la référence).
- [ ] **Quick Play** : fondu d'une demi-seconde vers la Preview, puis échange.
- [ ] Clic droit sur un bouton de transition : choisir Fade / Cut et une durée ; le libellé du
      bouton change et la transition suit le réglage.

### T-Bar
- [ ] Descendre la T-Bar à mi-course et lâcher : l'image reste **mélangée**.
- [ ] La remonter tout en haut : retour au Program seul, rien n'a changé.
- [ ] La descendre jusqu'en bas : Preview et Program s'échangent, la T-Bar **remonte** au
      relâchement.

### Vignettes et réglages
- [ ] Sous une vignette : **QP** (fondu de cet input vers le Program, la Preview ne change pas),
      **Cut** (cet input au Program, la Preview ne change pas), **×** (ferme l'input ; s'il
      était au Program, le Program devient noir).
- [ ] **Double-clic sur le nom** d'un input : la fenêtre de réglages s'ouvre ; renommer (le nom
      change partout) ; pour une couleur, changer la couleur (Program / Preview / vignette
      suivent). Taper des chiffres et des espaces dans le nom ne déclenche **aucun** raccourci.
- [ ] **Réglages** (en haut) : changer la durée de Quick Play ; « Lumière linéaire » : un fondu
      noir ↔ blanc paraît plus clair à mi-course qu'en « Gamma ».
- [ ] **Statistiques** (en haut) : masque / affiche le panneau ; pendant tout le test,
      « Images manquées » et « Images répétées » restent à **0**.

### Journal
- [ ] Après la session : `logs/commands-AAAA-MM-JJ.jsonl` contient une ligne par action
      (`"source":"ui"`), et des lignes `"source":"render"` pour les fondus.

## Étape 8 — vidéos et images
Préparer une vidéo courte (quelques secondes, avec du son) et une image PNG ou JPEG.
- [ ] « + Ajouter → Image… » : le sélecteur de fichier natif s'ouvre ; **pendant qu'il est
      ouvert, le Program continue de bouger** dans la fenêtre ; choisir l'image : elle arrive.
- [ ] « + Ajouter → Vidéo… » : choisir la vidéo ; l'input apparaît **en pause sur sa première
      image** ; le son n'est pas joué (Phase 3).
- [ ] Mettre la vidéo en Preview : sous la Preview, **Lecture** la lance ; le temps avance
      (« 00:03 / 00:10 ») ; **Pause** la fige sur l'image courante ; **Début** revient au
      début.
- [ ] Glisser le curseur de position : l'image suit **au relâchement**.
- [ ] Sans Loop, laisser finir : la **dernière image reste** (« · fin »), pas de noir ;
      Lecture repart du début.
- [ ] Loop (vignette) activé : la boucle est **invisible** (pas de noir, pas d'à-coup).
- [ ] Auto-lecture (Réglages, valeurs par défaut) : une vidéo en pause qui passe au Program
      par un Fade **démarre** ; celle qui quitte le Program **se met en pause**.
- [ ] Statistiques : la vidéo affiche son format, « conversion : aucune » pour du H.264 / MJPEG
      courant.

## Étape 8 — capture
Il faut une webcam USB (et, sous Windows, la même checklist).
- [ ] « + Ajouter → Capture » : la webcam est listée **une seule fois** (pas de doublon
      PipeWire sous Linux) ; l'ajouter : l'image arrive en moins de 2 s.
- [ ] Le mode choisi (résolution, cadence, YUY2 / MJPEG) est affiché et cohérent avec la
      cadence maître (journal : ligne `capture mode`).
- [ ] Mouvement fluide, pas de saccade visible ; « Images manquées » reste à **0**.
- [ ] **Débrancher** la webcam alors qu'elle est au Program : l'input passe en « signal perdu »
      (noir) en moins d'une seconde, le Program continue (horloge, autres inputs).
- [ ] La **rebrancher** (même port) : elle revient seule en quelques secondes, sans rien faire.
- [ ] La rebrancher sur **un autre port** : noter le comportement (l'identifiant suit le port :
      elle est vue comme un autre périphérique).
- [ ] Windows : mêmes cases avec Media Foundation.

### Remarques
…
