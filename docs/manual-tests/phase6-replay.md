# Test manuel — Replay et ISO sur du vrai matériel (Phase 6, étape 9)

**But** : vérifier sur de vraies caméras ce que la CI et le soak F7 couvrent avec des sources
de test : l'enregistrement continu, le marquage, le ralenti, les angles, « Rejouer », l'ISO,
et que le direct n'en souffre pas. Critères F1–F9 de [`phase-6.md`](../specs/phase-6.md).

À faire en build release (`cargo run --release`, ou le paquet Windows). Noter pour chaque
case ce qui a été vu, surtout quand ça ne va pas.

## Matériel
- [ ] Machine : `…` (processeur, GPU, mémoire ; **sur secteur** pour un portable)
- [ ] Disque du replay : `…` (un SSD à part de préférence ; place libre : `…`)
- [ ] Caméras (2 au moins, 4 si possible) : `…` (webcam, carte de capture, NDI®, SRT ; leur
  résolution et cadence). **Au moins une sans son** (une webcam sans micro, une vidéo muette) :
  son ISO et son replay doivent marcher aussi.
- [ ] Companion (pour la dernière partie), avec le module de la référence : version `…`

## Préparation
- [ ] Ajouter les caméras (« Ajouter un input »), et « Replay A » (« Ajouter → Replay A »).
- [ ] « Réglages du replay » (roue dentée du panneau Replay) : dossier sur le SSD, 10 min
  gardées, cocher les caméras ; « Rejouer » à 50 %, bouton 1 (Fade 500 ms).
- [ ] « Enregistrement ISO » (bouton « ISO » de la barre du bas, sa roue dentée) : cocher les
  caméras, un dossier, « nouveau fichier toutes les 5 min ».
- [ ] Le Program enregistré (« Enregistrer ») et, si possible, streamé.

## Enregistrer, marquer, rejouer
- [ ] « Enregistrer le replay » : le bouton passe au rouge, le dossier se remplit (un
  sous-dossier par caméra, des segments de 2 s).
- [ ] « −5 s » : un événement dans la liste, tout de suite ; « −10 s » de même.
- [ ] « Entrée », quelques secondes, « Sortie » : un événement de cette durée. « Entrée » puis
  « Annuler » : rien.
- [ ] **F6, en 2 actions** : « −5 s » puis « Rejouer » : transition vers Replay A, l'événement
  au ralenti, transition de retour vers la caméra qui était à l'antenne. Recommencer 5 fois,
  avec une autre caméra à l'antenne chaque fois.
- [ ] Le mini-panneau à côté de la carte de Replay A fait de même (« −5 s », « Rejouer », sa
  vitesse).
- [ ] Pendant une lecture : 25 / 50 / 75 / 100 %, le curseur ; l'arrière ; pause et image par
  image (« Image précédente » / « Image suivante ») ; le mouvement est régulier, sans saut.
- [ ] Molette sur le panneau : Maj = vitesse, Ctrl = image par image, Alt = seconde par seconde.
- [ ] **F5** : changer d'angle (boutons des caméras) en pleine lecture : la nouvelle caméra
  reprend au même instant (un geste, un objet qui passe : à l'image près).
- [ ] Un événement : son angle, sa vitesse, un commentaire ; « Lire », « Rejouer »,
  « Supprimer ».
- [ ] Un événement marqué juste avant : il se lit au plus 1 s après sa sortie (F4).
- [ ] **Son** : Replay A au Program, un événement lu à **100 %** : on entend le son de la
  caméra, calé sur l'image (une parole, un claquement de mains) ; à 50 %, en arrière, en
  pause : silence, sans clic. Changer d'angle à 100 % : le son passe à l'autre caméra.
  Replay A a sa tranche dans le mélangeur (fader, mute). Une caméra sans son : replay muet.

## Le direct n'est pas touché (F7)
- [ ] Pendant 20 min de replays (un toutes les 30 s environ), l'enregistrement du Program et
  le stream : le Program reste fluide, le panneau Statistiques ne montre **ni image manquée ni
  image répétée** ; noter le processeur et le disque (gestionnaire des tâches, `htop`, `iotop`).
- [ ] Panneau Statistiques, « replay » : images montrées, décodées en retard, ticks manqués,
  illisibles (ces trois-là à 0 de préférence) : `…`

## ISO (F1)
- [ ] Un fichier MKV par caméra cochée, nommé d'après l'input (deux inputs du même nom :
  « (2) » après le second), découpé toutes les 5 min, chaque morceau lisible (VLC, mpv), **avec
  le son** de la caméra (une caméra sans son : une piste muette).
- [ ] Arrêt brutal pendant l'ISO (tuer le processus : `kill -9`, ou le gestionnaire des
  tâches) : les fichiers déjà écrits se lisent, le dernier jusqu'à l'arrêt ou presque.
- [ ] « Arrêter l'ISO » puis « Démarrer l'ISO » : de nouveaux fichiers, rien d'écrasé.

## Disque plein (F2)
- [ ] Le dossier du replay sur une petite clé USB ou une petite partition : quand elle est
  pleine, l'enregistrement s'arrête proprement, une alerte apparaît dans le panneau Replay,
  **l'émission continue** (Program, enregistrement, stream ailleurs).
- [ ] Avec 2 min gardées : au bout de quelques minutes, les plus anciens segments sont
  effacés (le dossier ne grossit plus).

## Companion (F8)
- [ ] Boutons de Companion : Replay Start / Stop Recording, Mark In / Mark Out, Mark In-Out
  (5 s), Play Last Event, Play Last Event To Output (« Rejouer »), Set Speed 0,5, Camera 1–4,
  Play / Pause. Chacun fait son effet dans VMA.
- [ ] Les retours de Companion (enregistrement du replay, caméra montrée) suivent VMA, y
  compris quand on agit dans VMA à la souris.
- [ ] Un bouton d'une fonction que VMA n'a pas (canal B, « Quad mode ») : VMA la refuse sans
  que rien d'autre ne casse.

## Remarques
`…`
