# Observation de la référence — Phase 4 (overlays, input HTML, stinger)

À remplir par le partenaire en manipulant le logiciel de référence. On ne relève que des
**comportements** (valeurs par défaut, réactions), jamais l'apparence ni les textes (BRIEF §2).
Noter la réponse sous chaque question ; « ? » si non vérifiable.

En attendant les réponses, VMA utilise la valeur entre parenthèses (`phase-4.md` § 6).

## Overlay channels
1. Transition d'entrée et de sortie par défaut d'un overlay channel, et sa durée ? (Fade,
   500 ms)
2. Peut-on régler une transition d'entrée différente de celle de sortie ? (non en Phase 4)
3. Quels effets propose-t-on pour un overlay (fondu, glissé, zoom…) ? (Cut et Fade en Phase 4)
4. Deux overlays actifs : lequel passe au-dessus ? (le channel 4 au-dessus du 1)
5. Un overlay est-il visible sur la Preview ? Y a-t-il un réglage pour cela ? (non)
6. Le Fade to Black couvre-t-il aussi les overlays ? (oui)
7. Pendant un Cut ou un Fade du Program, un overlay actif reste-t-il fixe à l'écran ? (oui)
8. On clique le bouton d'un channel pour un input déjà dans ce channel : il sort ? Pour un
   **autre** input : il remplace l'ancien tout de suite, ou après une sortie puis une entrée ?
   (il sort ; il remplace avec la transition d'entrée)
9. Un input en overlay qu'on prend aussi au Program : visible deux fois, ou autre chose ?
   (deux fois)
10. Un input retiré de la liste alors qu'il est en overlay : que devient le channel ? (vidé
    sans transition)
11. Raccourcis clavier des overlays par défaut, s'il y en a ? (aucun)

## Input HTML
12. Taille de la page par défaut (celle du Program ?) et cadence de rendu par défaut ? (taille
    et cadence du Program)
13. Fond transparent par défaut, ou faut-il le demander ? (transparent)
14. La page continue-t-elle de s'animer quand l'input n'est ni au Program ni en overlay ?
    (oui)
15. Le son d'une page est-il dans le mélangeur ? Muet par défaut ? (oui ; comme un input vidéo)
16. Peut-on interagir avec la page à la souris (clic, défilement) depuis la référence ? (non
    en Phase 4)
17. Y a-t-il un bouton « recharger » ? La page se recharge-t-elle seule après un plantage ?
    (oui ; oui)

## Stinger
18. Combien d'emplacements de stinger ? Le stinger est-il un input de la liste ou un réglage
    de transition ? (2 ; réglage de transition)
19. Comment règle-t-on le point de bascule (ms, image, % ) et quelle valeur par défaut ?
    (ms ; moitié du fichier)
20. Le son du stinger est-il joué ? Sur quels bus ? (oui, Master)
21. Le stinger passe-t-il au-dessus ou au-dessous des overlays actifs ? (au-dessous)
22. Que se passe-t-il si on relance une transition pendant un stinger ? (refusée jusqu'à la
    fin du stinger)
23. Formats de fichier acceptés avec alpha (WebM, MOV ProRes 4444, séquence PNG, autres) ?
    (WebM VP8 / VP9 alpha, ProRes 4444, QuickTime Animation / PNG)

## Compléments (étape 3)
24. Le tally d'un input en overlay (à l'antenne) : rouge comme au Program, une autre couleur,
    ou rien ? (rouge)
25. Une vidéo mise en overlay démarre-t-elle seule (comme au Program avec la lecture
    automatique) ? (non en Phase 4 : elle joue si on l'a lancée)
26. Le son d'un input en overlay passe-t-il au Master (audio-follow-video) ? (non : seul le
    Program le fait suivre)
