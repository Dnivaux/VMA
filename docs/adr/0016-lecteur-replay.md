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

## Mise en œuvre du § 6 : le son du replay (2026-11-13)
- **Entendu** : en lecture, vers l'avant, à 100 % ; muet au ralenti, en arrière, en pause et
  image par image (Q12).
- **Comment** : pendant un tel passage, le lecteur **date** les images qu'il remet au feed
  (instant enregistré, décalé à maintenant : une période avant leur tick, comme une source
  live) et écrit dans l'anneau audio de l'input le son enregistré avec elles (ADR 0015), aux
  mêmes dates, deux périodes en avance sur l'image montrée. `vma-audio` le lit à l'heure de
  l'image montrée, comme pour tout input (ADR 0007 § 3) : la synchro vient des dates
  d'enregistrement, communes aux images et au son. Hors de ces passages les images ne sont
  pas datées : l'input est silencieux, sans rien lire.
- **Fin d'un passage** (fin de l'événement, pause, autre vitesse, autre événement) : 125 ms
  de silence après le dernier son, car la dernière image reste montrée et `vma-audio` lit
  encore à son heure jusqu'à la trouver figée (100 ms) ; sans cela ces échantillons étaient
  comptés manquants.
- **Autre angle en pleine lecture** : le son de la nouvelle caméra prend la suite au même
  instant (les blocs des deux caméras ne tombent pas aux mêmes dates : le début déjà entendu
  d'un bloc est coupé, un trou jusqu'à 100 ms est comblé de silence, l'anneau garde son
  temps).
- **Tranche** : « Replay A » a une tranche dans le mélangeur, comme un input avec image
  (suit la vidéo : au Master quand il est au Program).
- **Mesuré** (moteur réel, source synchro 50p enregistrée, 4 s rejouées à 100 % au Program,
  enregistrement relu) : chaque bip sur son flash, écart de −0,25 à +0,42 ms ; à 50 % :
  aucun bip ; aucun échantillon manquant. Lecteur seul : son à la trame près sur ses images,
  y compris après un changement d'angle.
- **Non fait** à cette date : l'option « Master de l'instant » (Q12, faite le 2026-11-18,
  ci-dessous) ; le son d'une caméra de capture (fait le 2026-11-13, ADR 0007 amendement).

## Mise en œuvre de l'option « Master de l'instant » (Q12, 2026-11-18)
- **Réglage** : Réglages du replay → « Son du replay » : « Caméra de l'angle » (par défaut) ou
  « Master de l'instant » ; commande `SetReplaySound`, gardée dans le projet. Il vaut pour
  tous les événements, y compris celui en cours de lecture. Mêmes règles qu'avec le son de la
  caméra : entendu à 100 % vers l'avant, muet au ralenti, en arrière, en pause.
- **Enregistrement** : tant que le replay enregistre, le Master est écrit dans la session
  (`<session>/master/sound`, même format que le son d'une caméra, même durée gardée,
  384 ko/s), quel que soit le réglage : on peut changer d'avis pour un événement déjà marqué.
  `vma-audio` pousse chaque bloc du Master dans un anneau à part, que lit un thread de
  `vma-io` (`vma-replay-master`) : le disque ne retient ni l'un ni l'autre.
- **Datation** : le bloc du tick *n* du Master est le son des images montrées au tick *n*,
  donc capté plus tôt, à leur instant d'enregistrement. Il est daté au **temps de lecture de
  l'input le plus à l'antenne qui a une image** (le temps de tick moins sa latence lissée,
  celui auquel `vma-audio` lit son son), puis, quand plus rien d'animé n'est à l'antenne, au
  temps de tick moins la dernière latence : la datation reste continue. Il tombe ainsi sur les
  instants enregistrés des caméras, comme le son d'une caméra ; le lecteur le joue sans
  traitement particulier, et le garde en changeant d'angle. Une caméra d'une autre latence que
  celle qui était à l'antenne voit le Master décalé de leur différence (quelques ms entre des
  caméras synchronisées) ; un changement d'input à l'antenne décale la datation d'autant, ce
  que le lecteur absorbe comme la gigue d'une caméra (blocs coupés ou trous ≤ 100 ms comblés).
- **Mesuré** (moteur réel, source synchro 50p à l'antenne pendant l'enregistrement, 4 s
  rejouées à 100 % au Program, enregistrement relu) : chaque bip sur son flash, de −0,25 à
  +0,42 ms ; daté à l'heure du tick, le son arrivait 17,6 ms en retard.
- **Panne** : une écriture impossible (disque plein) arrête l'enregistrement du Master seul,
  écrite dans le journal et dans ses compteurs (`IoProbe::master_recording`) ; elle ne remonte
  pas dans l'état comme celle d'une caméra (`replay.failures` désigne un input), mais les
  caméras enregistrées sur le même disque signalent la leur.
