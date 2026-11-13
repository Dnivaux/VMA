# ADR 0015 — Enregistrement par input (replay et ISO)

- **Statut** : **accepté** le 2026-11-11 (recommandations Q1, Q2, Q3, Q6, Q8 de
  [`phase-6.md`](../specs/phase-6.md) validées par le mainteneur)
- **Date** : 2026-11-11
- **Contexte** : BRIEF §4 (Phase 6), §2.3 ; ADR 0004 (entrées), ADR 0006 § 6 (échelle
  d'encodeurs), ADR 0007 § 1 (horloge partagée)

## Décision
1. **Où** : sur disque, dans un **dossier de session** (SSD dédié conseillé), en boucle sur une
   durée réglable (2 h par défaut) ; seul l'index est en mémoire chez le lecteur. Pas d'anneau
   d'images en mémoire (1080p50 non compressé : 150 Mo/s par caméra).
2. **Prise des images** : dans `vma-io`, au moment où une image part vers le feed de son input
   (`deliver`), une référence à la même image est passée à l'enregistreur de l'input par une
   file bornée (jamais d'attente ; file pleine → image comptée perdue pour l'enregistrement,
   jamais pour le direct). Le rendu n'est pas touché.
3. **Codec** : H.264 **tout-intra** (GOP 1) par l'échelle d'encodeurs de l'ADR 0006 (NVENC,
   QSV, VA, Media Foundation, puis openh264 en logiciel), ~100 Mb/s en 1080p50, réglable.
   Toute image se décode seule : recherche à l'image près, arrière et image par image sans
   coût de décodage en plus.
4. **Format sur disque** (le nôtre, pour lire ce qui s'écrit) : par input, un dossier ; des
   **segments de 2 s** : `seg-NNNNNN.h264` (unités d'accès Annex-B bout à bout) et
   `seg-NNNNNN.idx` (une entrée de 24 octets par image : instant sur l'horloge partagée,
   décalage, longueur). Écrits en ajout : un lecteur ne lit que les entrées complètes.
   Les segments plus vieux que la durée gardée sont effacés.
5. **Temps** : l'instant de chaque image est son instant sur l'horloge partagée (celui des
   feeds et du son), pas un numéro d'image : les angles se recoupent par le temps.
6. **ISO** (Q8) : le même enregistrement sert à l'ISO ; le fichier lisible (MKV avec le son)
   en est tiré par l'étape 3 (sans réencoder la vidéo).
7. **Erreurs** : disque plein ou écriture impossible → l'enregistrement de l'input s'arrête,
   la raison est publiée, le direct continue (BRIEF §2.4).

## Alternatives écartées
- **Anneau en mémoire** (BRIEF) : quelques minutes coûtent plusieurs Go par caméra.
- **MP4 / MKV en cours d'écriture** : leurs index ne sont écrits qu'à la fin (MP4) ou
  difficiles à suivre pendant l'écriture ; les segments et l'index maison se lisent à tout
  moment.
- **GOP long** : moins de débit, mais l'arrière et l'image par image demandent de décoder
  depuis la dernière image clé.

## Conséquences
- Débit disque : ~12,5 Mo/s par caméra 1080p50 ; 4 caméras pendant 2 h ≈ 360 Go.
- NVENC limite le nombre de sessions sur les cartes grand public : 4 enregistrements + les
  sorties, à mesurer (F7).

## Mise en œuvre : le son dans la session (2026-11-13)
Pour le son du replay (ADR 0016 § 6), l'enregistreur garde aussi le son de l'input, à côté de
ses images : dans le sous-dossier `sound` du dossier de l'input, des segments de 2 s
`seg-NNNNNN.f32` (les blocs de son tels qu'ils arrivent : `f32` stéréo entrelacé à 48 kHz,
petit-boutiste) et `seg-NNNNNN.idx` (même entrée de 24 octets : instant de la première trame
du bloc sur l'horloge partagée, décalage, longueur). Même durée gardée, même effacement, même
lecture pendant l'écriture (`SegmentWriter::sound`, `SegmentReader::open_sound`). Ce sont les
blocs donnés à l'anneau audio de l'input, avec les mêmes dates : rien n'est rééchantillonné
ni encodé. Coût : 384 ko/s par caméra (1,4 Go par heure), 3 % du débit des images à
100 Mb/s. Un input sans son n'a pas de dossier `sound` : son replay est muet.
