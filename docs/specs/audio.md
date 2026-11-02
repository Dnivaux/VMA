# Spec — `audio` (anneaux, lecture au temps de flux, mélangeur, vumètres)

Crate sans GStreamer (ADR 0007) : les pipelines de `io-gst` écrivent les anneaux, le thread
`vma-audio` d'`engine` (étape 3 de la Phase 3) appelle le reste une fois par tick. Rien
n'alloue après construction (test `no_alloc`). Format partout : 48 kHz, stéréo entrelacée,
`f32` (`SAMPLE_RATE`, `CHANNELS`) ; bloc le plus long d'un tick : 1 920 images (25p,
`MAX_BLOCK`).

## Comportement

### Anneau d'un input (`ring`, `RingWriter`, `RingReader`)
- Un producteur (thread de streaming de l'input), un consommateur (thread audio). Images
  stéréo rangées par **position** (images écrites depuis la création), capacité 2 s
  (`RING_FRAMES`).
- Chaque bloc poussé donne l'**ancre** `(position de sa première image, temps de flux)`,
  publiée par un seqlock ; le lecteur ne voit que la dernière.
- Le producteur n'attend jamais : il écrase les plus anciennes images. Le lecteur sait ce
  qu'il a lu : `missing` (pas encore écrites, ou avant le début), `overwritten` (écrasées) ;
  ces images valent 0. Un compteur de réservation, publié avant la réécriture des cases,
  garantit qu'une image rendue n'a jamais été réécrite pendant sa copie.
- `push` (`f32`) et `push_le_bytes` (tampon `F32LE` de GStreamer, sans contrainte
  d'alignement). Un bloc plus long que l'anneau garde sa fin (ancre décalée d'autant).

### Fréquence d'une source (`RateEstimator`)
- Boucle à verrouillage de délai du second ordre (bande 0,01 Hz, `BANDWIDTH_HZ`) sur les
  ancres : droite `position(t)` dont la pente est la fréquence réelle de la source vue par
  l'horloge maître. Départ : la première ancre, à 48 kHz.
- Une ancre à plus de 50 ms de la droite (`RESET_FRAMES` : source redémarrée, trou de
  réception) la fait repartir sur elle, pente conservée (compté). Pente bornée à ± 0,5 %.

### Lecture d'un input (`InputReader`)
- Pour chaque tick, le moteur demande le bloc qui **commence au temps de flux** `t` (image
  affichée moins le décalage de l'input). Un curseur parcourt la source sans discontinuité,
  interpolée par Catmull-Rom (cubique à 4 points, calculée à chaque échantillon) ; son pas est
  la pente de la DLL, corrigée d'au plus ± 0,5 % (`MAX_CORRECTION`) pour rattraper l'écart
  entre le curseur et `position(t)` (5 % de l'écart par bloc).
- Écart > 10 ms (`RESYNC_FRAMES`) : **saut** au temps demandé, fondu enchaîné de 5 ms
  (`FADE_FRAMES`) entre l'ancien chemin et le nouveau. Premier départ, reprise après des
  images manquantes : fondu d'entrée de 5 ms.
- Avant la première ancre : silence (`BlockKind::Silent`). Compteurs (`ReaderStats`) :
  images produites, manquantes, écrasées, sauts, redémarrages de la DLL, fréquence (ppm),
  écart courant (ms).

### Mélangeur (`Mixer`)
- Par input (`StripParams`) : fader (−∞ à +10 dB, `MAX_GAIN_DB` ; au-delà, borné), mute,
  solo, envois vers Master / A / B, part à l'antenne (`on_air`, audio-follow-video : 1 au
  Program, 0 hors Program, courbe de la transition entre les deux, 1 si l'option est
  désactivée). Par bus (`BusParams`) : fader, mute. Mode de solo (`SoloMode`) : AFL (défaut)
  ou PFL.
- Chemin d'un input : *vumètre avant fader* → fader × mute → *vumètre après fader* → envoi
  vers chaque bus, × `on_air` pour le **Master seulement**. Chemin d'un bus : somme →
  *vumètre avant fader* → fader × mute → borné à ± 1 (échantillons bornés comptés, jamais
  d'enroulement) → *vumètre après fader*.
- **Casque** : le Master ; dès qu'un input est en solo, la somme des inputs en solo (AFL :
  après fader et mute ; PFL : avant fader, donc un input muet s'écoute en PFL). Le solo ne
  change **pas** le Master.
- Chaque gain (fader, mute, envoi, part à l'antenne, solo, part du Master au casque) passe
  de sa valeur à la fin du bloc précédent à la nouvelle par une **rampe linéaire sur le
  bloc** : ni clic au mute, ni à la coupure d'audio-follow-video (Cut : un bloc de rampe).
- `equal_power(p)` : parts (sortant, entrant) d'une transition à l'avancement `p`
  (`cos`, `sin` de `p·π/2`), pour le Fade d'audio-follow-video.
- Un input absent d'un bloc est silencieux, ses vumètres remis à zéro ; de retour, il part
  du silence.

### Vumètres (`Meter`, `MeterCell`, `MixMeters`)
- Par canal : crête du bloc, crête maintenue 1,5 s (`HOLD_FRAMES`), RMS sur les 300 derniers
  ms (`RMS_FRAMES`, fenêtre glissante de blocs). Valeurs linéaires publiées dans des
  atomiques, lues par l'UI sans verrou ; `to_db` (−∞ pour le silence), `db_to_gain`.

## Critères d'acceptation (étape 1 de la Phase 3)
| Critère | Test | Statut |
|---|---|---|
| Anneau : lecture par position, manquantes / écrasées signalées, bloc trop long | `ring::tests` (6 tests) | CI |
| Anneau : jamais d'image réécrite rendue, producteur et lecteur concurrents | `ring::tests::concurrent_reads_never_return_a_rewritten_frame` | CI |
| C3 (DLL, temps simulé) : ± 100 ppm, gigue ± 1 ms, 1 h : fréquence à ± 5 ppm (mesuré ± 2,7), droite à 0,5 ms (mesuré 0,09), aucun redémarrage | `rate::tests::locks_on_a_drifting_source_for_an_hour` | CI |
| DLL : saut de temps → redémarrage, pente conservée ; fréquence fantaisiste bornée | `rate::tests` | CI |
| C3 (lecture, temps simulé) : ± 100 ppm, gigue ± 1 ms, 1 h : aucun saut, aucune image manquante, aucun clic, son à 0,1 ms de son temps | `reader::tests::a_drifting_source_plays_an_hour_without_a_jump_or_a_click` | CI |
| Lecture exacte au temps demandé ; petit écart rattrapé sans saut ; saut en fondu enchaîné ; source arrêtée → silence compté, reprise en fondu | `reader::tests` | CI |
| C4 : gains en dB, mute, envois, bus, somme ; rampe d'un bloc ; écrêtage borné et compté | `mixer::tests::{mixes_exactly, a_fader_move_ramps_over_one_block, clipping_is_clamped_and_counted}` | CI |
| C5 : solo au casque (AFL / PFL, plusieurs), Master identique à l'échantillon près ; PFL entend un input muet | `mixer::tests::{solo_goes_to_the_headphones_and_leaves_the_master_alone, pfl_hears_a_muted_input}` | CI |
| C6 (côté mixeur) : la part à l'antenne pèse sur le Master seul, rampe d'un bloc ; puissance constante | `mixer::tests::{audio_follow_video_weighs_the_master_only, equal_power_keeps_the_power}` | CI |
| C7 (calcul) : sinus connu → crête et RMS à ± 0,1 dB, silence → −∞ ; maintien 1,5 s ; fenêtre 300 ms ; vumètres avant / après fader publiés | `meter::tests`, `mixer::tests::meters_are_published_before_and_after_the_faders` | CI |
| C8 (allocation) : 500 ticks de 8 inputs (écriture, lecture, mixage, vumètres) sans allocation | `tests/no_alloc.rs` | CI |

La crate est compilée optimisée même en `dev` (`Cargo.toml` racine) : ses tests jouent une
heure de son simulé.
