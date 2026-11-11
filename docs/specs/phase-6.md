# Phase 6 — Sport : spec d'ensemble et plan

- **Statut** : **proposé** (2026-11-11), à valider par le mainteneur : questions Q1–Q14 du § 7
- **Date** : 2026-11-11
- **Références** : BRIEF §4 (Phase 6), §2.3 (temps réel), §8 (budgets) ; ADR 0004 (entrées
  GStreamer), ADR 0006 (sorties, encodeurs), ADR 0007 (audio), ADR 0014 (MultiView) ;
  [`api-compat.md`](api-compat.md) ; guide public de la référence (« Instant Replay »,
  p. 164–172 ; « MultiCorder », p. 209–210), lu pour les gestes et la disposition, rien n'en
  est repris
- **Entrée en phase** : Phase 5 à clore (E9 : Companion réel ; raccourcis : liste de la
  partenaire) ; soaks Windows en cours chez la partenaire

## 1. Périmètre

### Dans la Phase 6 (BRIEF)
- **Enregistrement ISO** : chaque input choisi enregistré dans son propre fichier, en plus du
  Program (l'équivalent du MultiCorder de la référence).
- **Replay** : un enregistrement continu par caméra (durée réglable), marquage d'événements,
  lecture au ralenti (BRIEF : 25 %, 50 %, 75 %), lecture multi-angle, envoi à l'antenne.
- **Acceptation (BRIEF)** : marquer un événement et le rejouer au ralenti **en moins de
  3 actions**, **sans impacter le Program live**.

### Ce que fait la référence, pour les gestes (guide public)
- Deux **canaux de lecture**, « Replay A » et « Replay B », qui sont des **inputs** comme les
  autres : on les met en Preview, on les envoie au Program, ils passent dans la MultiView.
- **Un clic** crée un événement des **5 ou 10 dernières secondes** ; un autre clic le
  **rejoue** : transition vers le canal A, lecture, transition de retour vers la caméra.
- **Marques d'entrée / sortie** à la volée, en direct ou dans le passé (mode « enregistré »,
  en déplaçant la tête de lecture) ; liste d'événements, chacun avec ses angles et sa vitesse.
- Ralenti de **0 à 100 %**, **arrière**, **image par image** ; molette de la souris (Maj :
  vitesse ; Ctrl : image par image ; Alt : seconde par seconde) ; contrôleurs à molette.
- **Angle** changé en pleine lecture, à la même position dans le temps.
- Panneau dans les onglets de droite (à côté du mélangeur), et un mini-panneau à côté de
  l'input Replay A (enregistrer, −5 s / −10 s, rejouer le dernier, vitesse).
- Enregistrement sur **SSD dédié**, découpé toutes les 20 min, les plus anciennes effacées
  quand le disque se remplit ; caméras de même résolution ; « Output » enregistrable comme
  une caméra.

### Hors Phase 6
Listes de lecture « highlights » avec musique, export de clips en fichiers (plus tard dans la
phase si le temps le permet, Q11), 20 listes d'événements (une seule ici), canaux A et B
liés / indépendants avancés, interpolation d'images au ralenti, caméras à haute cadence
(120p), contrôleurs MIDI / ShuttlePro en direct (Companion les couvre), MultiView dédiée au
replay, la MultiView vers le NDI® (reportée en Phase 5).

## 2. Architecture
```
 caméras (vma-io) ─► feed de l'input (inchangé) ─► rendu (inchangé)
        │ tee : la même image, en mémoire système, horodatée sur l'horloge partagée
        ▼
 vma-record (un pipeline par input enregistré, hors du rendu)
   encodeur H.264 tout-intra ou GOP court (NVENC, sinon …) ─► segments de 2 s (MPEG-TS)
   + index : n° d'image du maître → (segment, décalage) ─► dossier de session (SSD)
        │                                               ▲ effacement des plus anciens
        ▼                                               │ (durée réglable)
 vma-replay : canal A (et B) = un input « Replay » du moteur
   lecteur : (événement, angle, vitesse, position) ─► décodeur ─► images au rythme du maître
   (ralenti : chaque image répétée selon la vitesse ; arrière : images lues à rebours)
        │
        ▼ feed ordinaire ─► Preview / Program / MultiView, transitions, overlays
 vma-control : commandes Replay* (marquer, jouer, vitesse, angle) journalisées comme les autres
```
- **Le rendu ne change pas** : un canal de replay est un input comme un autre, avec un feed ;
  l'enregistrement prend les images **avant** le rendu, sur les threads de `vma-io`.
- **Un seul enregistrement par input** sert à l'ISO **et** au replay (Q8) : la référence ne
  peut pas faire les deux à la fois ; ici l'ISO est le même fichier gardé en entier.
- **Temps commun** : chaque image enregistrée porte son instant sur l'horloge partagée
  (celui qui sert déjà à la synchro audio, ADR 0007) ; un événement est un intervalle de cet
  instant, pour **tous** les angles à la fois : changer d'angle garde la position.
- **Ralenti** : à vitesse *v*, le lecteur avance de *v* images de la source par tick du
  maître ; à 50 % chaque image est montrée deux fois, à 25 % quatre fois (pas d'interpolation).
  Le décodage est fait **à l'avance** (quelques images) pour ne jamais retarder un tick.
- **Son** : à 100 %, le son de la caméra (ou le Master de l'instant) ; au ralenti, coupé
  (Q12).

## 3. Critères d'acceptation de la Phase 6
| # | Critère | Comment | Où |
|---|---|---|---|
| F1 | **ISO** : chaque input choisi dans son propre fichier, avec son son, fichier lisible même après un arrêt brutal ; 4 inputs 1080p50 pendant 1 h : 0 image perdue dans les fichiers, 0 image manquée au Program | `io-gst` + soak | CI (courte) + machine de dev |
| F2 | **Enregistrement continu** : segments et index par caméra, horodatés sur l'horloge partagée ; effacement des plus anciens au-delà de la durée réglée ; disque plein → l'enregistrement s'arrête proprement, l'émission continue, alerte dans l'UI | `replay` | CI |
| F3 | **Marquer** : « −5 s », « −10 s » (un clic, événement immédiat) ; « Entrée » / « Sortie » ; en direct ou dans le passé ; jamais d'image hors de l'enregistrement | `engine` + `replay` | CI |
| F4 | **Rejouer** : un événement joué dans le canal A à 100 %, 75 %, 50 %, 25 % et à une vitesse quelconque : **chaque image source montrée exactement** 1, 4/3, 2, 4 fois en moyenne, dans l'ordre, sans saut ; arrière ; image par image ; disponible au plus **1 s** après sa sortie | `replay` (temps simulé + relecture GPU) | CI |
| F5 | **Multi-angle** : changer d'angle en pleine lecture garde la position à l'image près | `replay` | CI |
| F6 | **Acceptation BRIEF** : « −5 s » puis « Rejouer » (2 actions) : transition vers le canal A, lecture au ralenti réglé, transition de retour vers la caméra d'avant | moteur réel | CI + checklist |
| F7 | **Le direct n'est pas touché** : soak 20 min, 4 caméras 1080p50 enregistrées, un replay toutes les 30 s (ralenti, arrière, changement d'angle), enregistrement + stream : **0 image manquée, 0 répétée** au Program ; disque et CPU mesurés | soak | machine de dev |
| F8 | **API** : fonctions replay de l'API compat (`api-compat.md`, à vérifier dans la doc : `ReplayMarkIn`, `ReplayMarkOut`, `ReplayMarkInOut`, `ReplayPlayLastEvent`, vitesses, angles…) et commandes natives ; Companion les pilote | `api` | CI |
| F9 | **UI** : panneau Replay (onglet à côté du mélangeur, comme la référence) et mini-panneau près de l'input Replay A ; molette : vitesse / image par image | `ui` | CI (egui_kittest) |

## 4. Plan (une étape = un commit testé)
| Étape | Contenu | Critères |
|---|---|---|
| 1 | ADR 0015 (stockage et codec du replay, dossier de session), ADR 0016 (temps commun, lecteur) ; **validation** | — |
| 2 | `io-gst` : branche d'enregistrement par input (tee avant le feed), encodeur, segments, index horodaté | F1, F2 |
| 3 | ISO : réglages par input (dossier, format, découpage), démarrer / arrêter, UI façon MultiCorder | F1 |
| 4 | `replay` : session, effacement, événements (marques, liste) ; commandes moteur | F2, F3 |
| 5 | `replay` : lecteur (décodage d'avance, vitesse, arrière, image par image) ; input « Replay A » | F4 |
| 6 | Multi-angle ; « rejouer » automatique (transition aller, lecture, transition retour) | F5, F6 |
| 7 | UI : panneau Replay, mini-panneau, molette | F9 |
| 8 | API compat et native : fonctions replay | F8 |
| 9 | Soak d'acceptation, checklist sur matériel réel (caméras), **arrêt pour revue** | F7 |

## 5. Dépendances prévues
Aucune nouvelle crate lourde attendue : GStreamer (encodeurs, `splitmuxsink` ou segments
écrits à la main, `mpegtsmux`, décodeurs) est déjà là ; index en binaire ou en JSON simple.
Décodeurs matériels (`nvh264dec`, `d3d11h264dec`, `vah264dec`) quand ils sont présents.

## 6. Risques
- **Disque** : 4 caméras 1080p50 tout-intra à ~100 Mb/s = 50 Mo/s ; 1 h ≈ 180 Go. Un SSD
  dédié est nécessaire (comme pour la référence) ; le débit et l'espace sont surveillés.
- **Charge** : 4 encodages + 1 à 2 décodages en plus du Program, de l'enregistrement et du
  stream : NVENC a un nombre de sessions limité sur les cartes grand public (8 sur les
  récentes) ; à mesurer.
- **Lire un fichier en cours d'écriture** : d'où les segments courts et l'index tenu par
  l'enregistreur (pas de relecture d'un MP4 ouvert).
- **Ralenti saccadé** : sans caméra à haute cadence, 25 % répète chaque image 4 fois ; c'est
  le comportement attendu (interpolation hors phase).

## 7. Questions au mainteneur (recommandation en premier)
- **Q1 — Où vit l'enregistrement du replay.** Le BRIEF dit « tampon circulaire en mémoire » ;
  en 1080p50 non compressé, c'est 150 Mo/s par caméra (une minute ≈ 9 Go). **Recommandé** :
  comme la référence, des **segments compressés sur disque** (SSD dédié), en boucle sur une
  durée réglable (défaut : 2 h), seul l'index en mémoire. Alternative : un anneau compressé en
  mémoire (quelques minutes par caméra, plusieurs Go de RAM).
- **Q2 — Codec.** **Recommandé** : H.264 **tout-intra** (ou GOP de 0,2 s) par l'encodeur
  matériel quand il existe, ~100 Mb/s en 1080p50 : recherche à l'image près, arrière et image
  par image sans coût ; sinon un encodeur logiciel non GPL (openh264), à débit plus bas.
- **Q3 — Combien de caméras.** **Recommandé** : **4** à la fin de la phase (mesurées sans
  perte), 8 visées ensuite si la machine suit.
- **Q4 — Vitesses.** **Recommandé** : de 0 à 100 % en continu (préréglages 25 / 50 / 75 /
  100 % comme le BRIEF), **arrière** et **image par image** (le tout-intra les rend simples).
- **Q5 — Canaux de lecture.** **Recommandé** : **A** dans la phase, **B** ensuite (même code,
  un second input) ; la référence en a deux.
- **Q6 — Formats des caméras.** **Recommandé** : chaque caméra enregistrée à sa taille et à
  sa cadence, remise à la cadence du maître à la lecture (comme les inputs aujourd'hui) ;
  la référence exige la même résolution partout.
- **Q7 — ISO.** **Recommandé** : une fenêtre façon MultiCorder (cocher les inputs, dossier,
  format MKV par défaut — résiste aux coupures —, nouveau fichier toutes les *n* minutes, son
  de l'input ou Master).
- **Q8 — ISO et replay en même temps.** **Recommandé** : oui, **un seul enregistrement par
  input** sert aux deux (la référence ne peut pas faire les deux).
- **Q9 — Où dans l'interface.** **Recommandé** : comme la référence : onglet « Replay » à
  côté du mélangeur (même zone), mini-panneau près de l'input Replay A.
- **Q10 — Raccourcis et Companion.** **Recommandé** : les fonctions replay de l'API compat
  (vérifiées dans la doc) dès l'étape 8, pour Companion et les Stream Deck.
- **Q11 — Export de clips.** **Recommandé** : plus tard (fin de phase si le temps le permet) :
  un événement exporté en MP4 pour les réseaux sociaux.
- **Q12 — Son au ralenti.** **Recommandé** : son de la caméra à 100 %, **coupé** au ralenti
  et en arrière ; option « Master de l'instant ».
- **Q13 — Listes d'événements.** **Recommandé** : **une** liste dans la phase (la référence en
  a 20), chaque événement avec ses angles, sa vitesse et un commentaire.
- **Q14 — Le « −5 s » et le « Rejouer » par défaut.** **Recommandé** : « Rejouer » joue le
  **dernier** événement dans le canal A, à **50 %**, avec la transition du bouton 1 à
  l'aller et au retour, puis revient à la caméra qui était à l'antenne ; tout réglable.
