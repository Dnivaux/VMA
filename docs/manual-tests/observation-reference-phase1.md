# Observation de la référence — Phase 1

**Pour qui** : une personne qui utilise déjà le logiciel de référence.
**But** : noter ce que fait le logiciel dans des cas que sa documentation ne décrit pas, pour
que VMA se comporte pareil.
**Règle** (BRIEF §2.1) : on **observe** et on **décrit avec ses mots**. Pas de captures d'écran,
pas de copie de textes ni d'icônes, pas d'outil qui inspecte le programme.

Durée : environ 20 minutes, avec deux ou trois inputs quelconques (une couleur, une image, une
vidéo courte, une webcam si possible), dans une configuration neuve (paramètres par défaut).

Pour chaque question : cocher la case et écrire la réponse à la place de `…`. La valeur entre
parenthèses est celle que VMA utilise en attendant.

## Boutons de transition
- [ ] Sur une installation neuve, quels **effets** et quelles **durées** ont les 4 boutons de
  transition ? (Fade 500 ms, Fade 1000 ms, Fade 1500 ms, Cut)
  → …
- [ ] Après un **Quick Play**, l'ancien Program revient-il en Preview ? (oui)
  → …
- [ ] Pendant un Fade de 3 s, on clique **encore** sur un bouton de transition : que se passe-t-il ?
  (rien)
  → …
- [ ] Pendant un Fade de 3 s, on clique sur **Cut** : que se passe-t-il ? (le Fade se termine tout
  de suite)
  → …
- [ ] Pendant un Fade, on clique sur un **autre input** de la barre : la Preview change-t-elle ?
  (non)
  → …

## T-Bar
- [ ] On pousse la T-Bar à mi-course et on la lâche : l'image reste-t-elle mélangée ? (oui)
  → …
- [ ] On la pousse jusqu'au bout : Preview et Program s'échangent-ils ? La T-Bar revient-elle
  à sa position de départ, ou reste-t-elle en bas (et il faut la remonter pour la prochaine
  transition) ? (échange ; elle repart de zéro)
  → …

## Vidéos
- [ ] Une vidéo qu'on vient d'ajouter : **Loop** est-il activé ? Est-elle en lecture ou en pause ?
  (Loop non, en pause sur la première image)
  → …
- [ ] Dans Réglages → Options, les trois cases « démarrer / redémarrer / mettre en pause avec
  la transition » sont-elles cochées par défaut ? (démarrer : oui, redémarrer : non, pause : oui)
  → …
- [ ] Une vidéo sans Loop arrive à la fin pendant qu'elle est au Program : dernière image figée,
  noir, ou autre ? (dernière image)
  → …

## Inputs
- [ ] On ajoute un **premier** input dans une session vide : va-t-il en Preview, au Program, ou
  nulle part ? Et le deuxième ? (premier au Program, deuxième en Preview)
  → …
- [ ] On **ferme** l'input qui est au Program : qu'affiche le Program ? (noir)
  → …
- [ ] On **débranche la webcam** pendant qu'elle est au Program : qu'affiche le Program, et que
  montre la vignette ? Que se passe-t-il quand on la rebranche ? (noir ; vignette « signal
  perdu » ; elle revient seule)
  → …

## Couleurs des cadres (tally)
- [ ] De quelle couleur est le cadre d'un input au Program, en Preview, et pendant une
  transition ? (rouge, vert ; les deux en rouge pendant une transition)
  → …

## Après l'observation
Rendre ce fichier rempli. Je mets à jour la spec (`docs/specs/phase-1.md` §7.3) et les valeurs
par défaut, puis les tests correspondants.
