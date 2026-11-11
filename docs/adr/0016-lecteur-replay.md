# ADR 0016 — Lecteur de replay

- **Statut** : **accepté** le 2026-11-11 (recommandations Q4, Q5, Q12, Q13, Q14 de
  [`phase-6.md`](../specs/phase-6.md) validées par le mainteneur)
- **Date** : 2026-11-11
- **Contexte** : ADR 0015 (enregistrement), ADR 0004 (feeds), BRIEF §4 (Phase 6)

## Décision
1. **Canal de lecture = input** : « Replay A » (puis B) est un input du moteur, de type
   replay, avec un feed ordinaire : Preview, Program, MultiView, transitions, overlays le
   traitent comme une caméra. Le rendu ne change pas.
2. **Lecteur** : un thread par canal ; état (événement, angle, vitesse, position sur
   l'horloge partagée, sens) fixé par des commandes de `vma-control`. À chaque tick du maître,
   la position avance de *vitesse* × période ; l'image publiée est celle de l'angle dont
   l'instant est le plus proche, décodée **d'avance** (quelques images) : un tick n'attend
   jamais un décodage.
3. **Vitesse** : de 0 à 100 % (préréglages 25 / 50 / 75 / 100), arrière, image par image ;
   à 50 % chaque image est montrée deux fois (pas d'interpolation).
4. **Événements** : une liste (instants d'entrée et de sortie sur l'horloge partagée, angles,
   vitesse, commentaire), dans l'état du moteur (journalisée, rejouable, gardée dans le
   projet) ; « −5 s » / « −10 s » créent un événement fini à l'instant présent.
5. **Rejouer** : le dernier événement dans le canal A, à 50 %, transition du bouton 1 à
   l'aller et au retour vers l'input qui était à l'antenne ; réglable.
6. **Son** : à 100 %, le son de l'angle ; coupé au ralenti et en arrière.

## Conséquences
- Un décodeur par canal (matériel quand il existe) ; l'image décodée passe par le chemin des
  feeds (tampons de transfert, ADR 0004 amendement 1).
