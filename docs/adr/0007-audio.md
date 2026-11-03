# ADR 0007 — Audio : horloge, alignement sur l'image, mélangeur, périphériques

- **Statut** : **accepté** le 2026-11-02 par le mainteneur (avec la spec de la Phase 3)
- **Date** : 2026-11-02
- **Contexte** : BRIEF §4 (Phase 3 : mélangeur audio, synchro lèvres < 1 image), §5 (thread
  audio temps réel), §6 (`cpal` pour les périphériques), §8 (dérive A/V < 1 image sur 1 h) ;
  ADR 0003 (état, commandes, threads), ADR 0004 (pipelines d'entrée), ADR 0006 § 3 et § 5
  (audio des sorties dérivé de *n*, piste générée en Phase 2) ; spec
  [`docs/specs/phase-3.md`](../specs/phase-3.md)

## Problème
En Phase 2, l'audio des sorties est **généré** : le bloc de l'image *n* compte
`audio_sample_at(n + 1) − audio_sample_at(n)` échantillons à 48 kHz (960 à 50p), silence ou
bip. En Phase 3, il vient des inputs, et chaque input a **sa propre horloge** : carte son ou de
capture, émetteur NDI, appelant SRT, fichier lu en temps réel. Trois exigences :
1. **dérive** : une source à 48 000 Hz « selon elle » en fournit 48 000 × (1 ± ε) par seconde de
   l'horloge maître (ε ≈ 10 à 100 ppm, soit jusqu'à 0,36 s par heure) : il faut l'absorber sans
   trou ni accumulation ;
2. **synchro lèvres par input** : le son d'un input doit sortir avec **son** image, qui arrive
   au Program avec une latence (décodage, triple buffer, rééchantillonnage à la cadence maître :
   ≤ 2 images, A10) ;
3. **temps réel** : ni allocation, ni verrou bloquant, ni I/O sur le chemin du mixage ; une
   source qui tombe donne du silence, jamais un blocage.

## Décision

### 1. Une seule horloge : l'horloge système monotone
Tous les pipelines GStreamer (entrées et sorties) utilisent l'horloge système **monotone**
(`gst::SystemClock`, `ClockType::Monotonic`), la même que l'horloge maître (`Instant` :
`CLOCK_MONOTONIC` sous Linux, `QueryPerformanceCounter` sous Windows ; à vérifier par un test
sur les deux OS, étape 2). Toute image et tout bloc audio d'un input reçoit un **temps de flux**
`base_time + running_time(pts)`, comparable aux instants des ticks. Les éléments sources
asservissent déjà leur horloge matérielle à celle du pipeline (`slave-method`) : les temps de
flux sont dans le domaine maître, seul le **nombre d'échantillons par seconde** dérive.

### 2. Par input : un anneau audio horodaté
Branche audio de chaque pipeline d'entrée (aujourd'hui vers un `fakesink`) :
`audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=48000,channels=2,layout=interleaved
! appsink`. Mono dupliqué, plus de 2 canaux : les 2 premiers (Q9). Le rappel de l'`appsink`
écrit dans un **anneau SPSC préalloué** (2 s), chaque bloc avec son temps de flux. Débordement
→ le plus ancien est écrasé (compté) ; jamais d'attente.

### 3. Lecture alignée sur l'image affichée
Le rendu note, à chaque tick, le **temps de flux de l'image affichée** de chaque input (déjà
connue du `FeedReader`). Pour le tick *n*, le mixeur lit, pour l'input *i*, la fenêtre
`[t_i(n), t_i(n) + T)` de son anneau, où `T` est la durée de l'image *n* et
`t_i(n) = image_i(n) − d_i`, avec :
- `image_i(n)` : temps de flux de l'image affichée, **lissé** (moyenne glissante de l'écart
  tick − image : sinon, une source 25p dans un maître 50p ferait sauter le son d'une image une
  fois sur deux) ; l'erreur instantanée reste sous une demi-période de la source ;
- `d_i` : le **décalage réglé** de l'input (ms, ≥ 0 : retarde le son ; Q7).
Un input sans vidéo (périphérique audio, Phase 3) prend la latence de son anneau seule
(au plus juste, plus une marge d'un bloc).

### 4. Dérive : rééchantillonnage adaptatif borné
La fenêtre lue contient `T × r_i` échantillons de la source, où `r_i` est sa fréquence réelle
dans le domaine maître, estimée par une **boucle à verrouillage de délai** (DLL du second ordre
sur les couples temps de flux / nombre d'échantillons). Un rééchantillonneur fractionnaire
(interpolation cubique de Catmull-Rom, calculée à chaque échantillon ; précisé à
l'implémentation) produit exactement les échantillons du bloc *n*. Correction bornée à
±0,5 % : au-delà (écart de plus de 10 ms : source fantaisiste, reprise après coupure,
décalage changé), le mixeur **se recale** (saut, compté comme discontinuité,
fondu de 5 ms pour éviter le clic). Un trou dans l'anneau (source tombée) → silence, compté.

### 5. Thread `vma-audio`, cadencé par les ticks
Un thread dédié reçoit de l'horloge maître le top de chaque tick (le rendu ne fait rien de
plus qu'un `try_send` du temps de tick et des temps de flux des images affichées) et, pour le
bloc *n* :
1. lit et rééchantillonne chaque input actif (§ 3–4) ;
2. applique **gain** (fader en dB, rampe sur le bloc pour éviter les clics), **mute**,
   **audio-follow-video** (§ 6), et répartit vers les **bus** Master, A, B selon les envois de
   l'input ;
3. applique le fader et le mute de chaque bus ; calcule les **vumètres** (crête et RMS par
   canal, avant et après fader, entrées et bus) ;
4. publie les blocs des bus pour *n* dans un anneau lu par `vma-output` (qui y prend le bus de
   chaque sortie au lieu de générer l'audio) et le **mix casque** vers la sortie casque.
Paramètres lus comme le plan du rendu (seqlock, ADR 0003), vumètres publiés par atomiques
(lus par l'UI à sa cadence). Aucune allocation : blocs et anneaux préalloués à la taille
maximale (60p → 800 échantillons, 25p → 1 920). Si le bloc *n* n'est pas prêt quand
`vma-output` en a besoin (thread audio en retard), `vma-output` met du silence et compte
(`audio_late`) : la vidéo ne l'attend jamais. Le mode `--sync-test` remplace le Master par le
bip (comportement Phase 2 conservé).

### 6. Audio-follow-video
Option par input. Activée, la contribution de l'input au Master suit son passage à l'antenne :
gain 1 quand l'input est au Program, 0 sinon ; pendant une transition, la même courbe que la
vidéo (Fade : fondu croisé en puissance constante, Cut : au tick, rampe d'un bloc). Les bus A
et B n'en dépendent pas (comportement de la référence à confirmer, Q6 et checklist).

### 7. Périphériques par GStreamer, pas par `cpal` (écart au BRIEF §6)
Entrées audio (micro, entrée ligne, audio d'une carte de capture vue comme périphérique séparé)
et sortie casque passent par **GStreamer** (`DeviceMonitor` classe `Audio/Source` et
`Audio/Sink` ; `pipewiresrc` / `pulsesrc` sous Linux, `wasapi2src` / `wasapi2sink` sous
Windows), pas par `cpal` :
- **même horodatage** que la vidéo (§ 1) : un micro et une caméra se calent par le même
  mécanisme, sans pont entre deux mondes d'horloges ;
- **un seul inventaire** de périphériques, identifiants stables et reconnexion déjà faits pour
  la vidéo (ADR 0004, Phase 1 étape 8b) ;
- **pas de dépendance de plus** (`cpal` : Apache-2.0, mais ALSA / WASAPI en direct, sans
  rééchantillonnage ni asservissement d'horloge).
La sortie casque est un pipeline `appsrc ! audioconvert ! audioresample ! <sink>` dont le sink
s'asservit à l'horloge du pipeline (rééchantillonnage interne si la carte dérive) ; sa latence
n'importe pas pour la synchro des sorties.

## Précisions de mise en œuvre (étape 3, 2026-11-03)
Constatées en mesurant la synchro de bout en bout ; elles ne changent pas la décision.
- **Lissage sur les images nouvelles** (§ 3) : la latence lissée est celle des ticks qui
  montrent une image *nouvelle* (tick − image) ; une image montrée de nouveau garde la
  latence. Lissée sur tous les ticks, une source plus lente que le maître avait un biais
  d'une demi-période du maître (+10 ms à 50p ← 25p), et une source à la cadence du maître,
  dont les images arrivent tantôt juste avant, tantôt juste après le tick, un biais allant
  jusqu'à une image (−20 ms mesurés à 50p ← 50p).
- **Mixage différé de 3 ticks** (§ 5) : le son d'une image n'est capté qu'au fil de sa
  durée, après l'image. Le bloc *n* est mixé à l'arrivée du tick *n* + 3 (60 ms à 50p),
  avec les données du tick *n*, et `vma-output` **retient** l'image *n* jusqu'à ce que le
  son du bloc *n* soit écrit (au plus 8 images ou 8 périodes ; au-delà : silence compté
  `audio_late`). Le rendu n'attend toujours rien ; les sorties prennent 3 à 4 images de
  latence de plus, sans effet sur leurs PTS.
- **Datation à travers `audioresample`** (§ 2) : `audioresample` date sa sortie en comptant
  les échantillons depuis le premier temps reçu ; une source à 44,1 kHz qui dérive était
  donc datée par sa propre horloge (+6 ms en 60 s à +100 ppm). Une sonde sur son entrée note
  le temps de flux de chaque tampon d'entrée ; un échantillon de sortie est daté par
  l'échantillon d'entrée dont il vient. À 48 kHz, passage direct, temps des tampons.
- **Artefact de mesure** : une source plus rapide que le maître perd des images ; si la
  première image blanche d'un flash est sautée, le Program montre la suivante, et l'analyseur
  mesure le bip une période de source avant le flash (−20 ms à 25p ← 50p), alors que le son
  suit bien l'image montrée. Reste sous une image du maître (C1).
- **Limite connue — image sautée** : quand une source plus rapide que le maître ou à sa
  cadence (dérive) perd une image, la latence de ses images baisse d'une période de source
  d'un coup ; le son, qui ne saute pas, la rattrape à 0,5 % au plus (≈ 4 s pour 20 ms).
  Pendant ce temps l'écart son − image va jusqu'à une période de source (16,9 ms mesurés à
  50p ← 50p + 100 ppm, une fois en 60 s). **Acceptée** par le mainteneur le 2026-11-03 ; à
  reprendre si le partenaire le remarque (pistes : viser le milieu de la bande de phase,
  rattrapage plus rapide).

## Conséquences
- `vma-output` ne génère plus l'audio (sauf `--sync-test`) : il assemble l'image *n* et le bloc
  *n* du bus de chaque sortie. PTS inchangés (ADR 0006 § 3), compensation de l'amorçage AAC
  inchangée.
- La latence audio des sorties suit celle de l'image : aucune ligne à retard globale.
- Un input dont la vidéo est figée (image fixe, couleur) n'a pas d'audio ; un fichier en pause
  donne du silence ; un fichier en boucle enchaîne son audio par son temps de flux (A9).
- Coût : mixage de 6 à 10 inputs stéréo × 3 bus à 48 kHz, quelques dizaines de µs par bloc ;
  mesuré par l'histogramme du thread audio (nouvelle section du rapport de soak).
- Nouveaux compteurs par input : échantillons reçus, débordements, trous, recalages, `r_i`
  estimé (ppm), décalage appliqué ; par sortie : `audio_late`.

## Alternatives examinées
- **`audiomixer` GStreamer** pour tout le mélange : asservi à l'horloge des pipelines, mais
  hors de notre maîtrise pour le temps réel (allocations, verrous, latence par agrégation),
  vumètres et solo à reconstruire autour ; et chaque changement de réglage devient une
  propriété GStreamer modifiée depuis un autre thread. Écarté.
- **Lire l'audio « au plus tôt »** (dernier échantillon arrivé) au lieu du temps de flux de
  l'image : la synchro dépendrait de la latence vidéo de chaque input (décodage, rééchantillon-
  nage), jusqu'à 2 images d'écart. Écarté (critère du BRIEF).
- **Retarder la vidéo** pour un son en retard (décalage négatif) : demande une file d'images
  par input sur le GPU. Hors Phase 3 (Q7).
- **`cpal`** (BRIEF §6) : voir § 7.
