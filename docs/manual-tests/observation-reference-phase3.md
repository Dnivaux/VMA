# Observation de la référence — Phase 3 (audio, NDI®, SRT en entrée)

À remplir par le partenaire en manipulant le logiciel de référence. On ne relève que des
**comportements** (valeurs par défaut, réactions), jamais l'apparence ni les textes (BRIEF §2).
Noter la réponse sous chaque question ; « ? » si non vérifiable.

En attendant les réponses, VMA utilise la valeur entre parenthèses (`phase-3.md` § 6).

## Mélangeur
1. Un input qu'on vient d'ajouter (vidéo avec son) : son fader, est-il muet, envoyé à quels
   bus ? (0 dB, non muet, Master)
2. Plage du fader et loi (dB) : jusqu'où monte-t-il au-dessus de 0 dB ? (+10 dB)
3. Le mute d'un input coupe-t-il aussi ses envois vers les bus auxiliaires ? (oui, tous les bus)
4. Un bus auxiliaire peut-il être envoyé dans le Master ? Est-ce le cas par défaut ? (non en
   Phase 3)
5. Le fader d'un bus auxiliaire agit-il sur le niveau envoyé aux sorties qui l'utilisent ? (oui)

## Solo et casque
6. Le solo d'un input : entend-on seulement cet input au casque, ou le Master + l'input ?
   (seulement les inputs en solo)
7. Plusieurs solos à la fois : comment, et entend-on la somme ? (oui, la somme)
8. Sans solo, qu'entend-on au casque ? (le Master)
9. AFL / PFL : lequel par défaut, où le change-t-on ? (AFL, réglage audio)

## Audio-follow-video
10. Activé par défaut pour un input vidéo ? Pour une source audio seule (micro) ? (oui ; non)
11. Pendant un Fade, le son passe-t-il en fondu croisé de la même durée ? Pendant un Cut,
    franc ? (oui ; oui)
12. Un input en audio-follow-video mais **muet** : passe-t-il à l'antenne quand on le prend au
    Program ? (non : le mute l'emporte)
13. Agit-il sur les bus auxiliaires, ou seulement sur le Master ? (Master seulement)

## Vumètres
14. Crête, RMS, ou les deux ? Maintien de la crête, et combien de temps ? (les deux ; 1,5 s)
15. Le vumètre d'un input est-il avant ou après son fader ? (après ; avant en option)

## Décalage audio
16. Existe-t-il un décalage **par input** ? Bornes et unité ? (oui, 0 à 1 000 ms)
17. Peut-on avancer le son (décalage négatif) ? (non en Phase 3)

## NDI®
18. Nom sous lequel la sortie Program est annoncée sur le réseau par défaut ? (« VMA Program »
    côté VMA ; relever seulement s'il contient le nom de la machine, et la forme)
19. Une source NDI qui disparaît du réseau : l'input garde-t-il la dernière image, ou noir ?
    Se reconnecte-t-il seul quand elle revient ? (signal perdu comme les autres inputs ;
    oui)
20. La sortie NDI porte-t-elle le son du Master, ou d'un bus choisi ? (Master, bus au choix)

## SRT en entrée
21. Modes proposés (caller, listener, rendezvous) et latence par défaut ? (caller et listener,
    120 ms)
22. Un émetteur qui s'arrête puis revient : reconnexion automatique ? (oui)

NDI® est une marque déposée de Vizrt NDI AB.
