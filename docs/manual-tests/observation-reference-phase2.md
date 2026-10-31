# Observation de la référence — Phase 2 (sorties)

À remplir par le partenaire en manipulant le logiciel de référence. On ne relève que des
**comportements** (valeurs par défaut, réactions), jamais l'apparence ni les textes (BRIEF §2).
Noter la réponse sous chaque question ; « ? » si non vérifiable.

En attendant les réponses, VMA utilise la valeur entre parenthèses (`phase-2.md` § 6).

## FTB (fondu au noir)
1. Durée par défaut du FTB ? Est-elle réglable, et où ? (1 000 ms)
2. Un second appui ramène-t-il le Program avec la même durée ? (oui)
3. Le FTB agit-il sur l'enregistrement, le stream **et** la sortie plein écran ? Sur la vue
   Program de l'interface ? (sorties oui, vue Program non)
4. Une transition (Cut, Fade) pendant que le FTB est actif : le Program change-t-il « sous » le
   noir ? (oui, on le voit au retour)

## Enregistrement
5. Format, conteneur et qualité proposés par défaut ? (H.264, MKV, ≈ 40 Mb/s)
6. Nom et dossier des fichiers par défaut ? (date et heure, dossier Vidéos)
7. Que se passe-t-il si le disque est plein pendant un enregistrement ? (arrêt, message)

## Streaming
8. Nombre de destinations, et peut-on les démarrer une par une **et** toutes ensemble ? (3, oui)
9. Chaque destination a-t-elle sa propre qualité (débit, résolution) ? (oui, débit seulement en Phase 2)
10. Coupure réseau : reconnexion automatique ? Au bout de combien de temps, combien d'essais ?
    (oui : 1, 2, 5, 10 s puis toutes les 10 s, sans limite)
11. L'arrêt d'un stream demande-t-il une confirmation ? (non)

## Sortie plein écran (sortie externe)
12. Que peut-on y envoyer (Program, Preview, un input, multiview) ? (Program seulement en Phase 2)
13. Comment choisit-on l'écran ? (liste des écrans)

## Sorties en difficulté (étape 3 : comportement retenu à confirmer)
14. Un stream qui n'arrive plus à suivre (réseau saturé) : l'enregistrement et les autres
    streams continuent-ils sans perte ? (oui : seule la sortie en difficulté perd des images,
    comptées dans ses statistiques)
15. Machine surchargée pendant un enregistrement : dans le fichier, la vidéo saccade-t-elle
    (image précédente tenue) pendant que **le son reste continu**, ou entend-on des coupures ?
    (vidéo tenue, son continu, sans coupure)
