# API compat : fonctions supportées, partielles, non supportées

- **Statut** : **proposé** (2026-11-11), avec la [spec de la Phase 5](phase-5.md) ; à compléter
  à chaque fonction ajoutée (BRIEF §7 : « lister les fonctions supportées, partiellement
  supportées et non supportées »)
- **Sources** (lues le 2026-11-11) : documentation publique officielle de la référence :
  pages « TCP API » (édition 26), « HTTP Web API » (éditions 26 et 28) et « Shortcut
  Function Reference » (édition 28) ; code du **module Companion** de la référence (dépôt
  public de Bitfocus, MIT, v5.0.6), lu pour savoir ce qu'un serveur doit fournir. Aucun code
  repris ; aucun binaire de la référence inspecté (BRIEF §2.1).
- **Colonne « Vu »** : **D** = texte de la documentation lu mot à mot ; **d** = la page a
  été lue en partie seulement, l'entrée est **à revérifier** avant l'implémentation (règle
  BRIEF §7 : ne rien inventer de mémoire) ; **M** = envoyé ou lu par le module Companion.

## 0. Mise en œuvre (2026-11-11)
- **HTTP** (étape 6) : `GET /api` (et `/api/`, `/API`, `/API/`) : XML d'état ;
  `GET /api/?Function=…` : 200 « Function completed successfully. », 500 avec la raison. Toutes
  les fonctions « oui » et « partiel » des tableaux du § 4 sont faites, **sauf** `OpenPreset` /
  `SavePreset` (plus tard dans la phase). Un test par fonction : `crates/api/tests/compat_http.rs`.
- **TCP** (étape 7) : à venir.
- **Choix à confirmer par l'observation du XML réel (Q3)**, constantes de `api::compat` :
  version `26.0.0.0`, édition `HD` ; types d'input (`Capture`, `Video`, `Image`, `Colour` pour
  les couleurs et les sources de test, `Audio`, `SRT`, `NDI`, `Browser`) ; `state` `Running` /
  `Paused` ; enfants de `audio` `master`, `busA`, `busB` ; `position`, `duration`, vumètres et
  `gainDb` à 0 (pas encore lus par l'API) ; **loi du volume 0–100** : amplitude
  (volume / 100)², soit 100 = 0 dB, 50 ≈ −12 dB, 0 = silence (un fader au-dessus de 0 dB vaut
  100).
- `Fade` / `Cut` avec `Input` : cet input passe au Program directement, la Preview reste
  (comme Quick Play) ; à vérifier sur la référence.

## 1. Transports
**TCP** (port 8099 par défaut, réglable chez VMA, Q5) : texte UTF-8, une commande par ligne
terminée par `\r\n`. Réponse `<COMMANDE> <OK|ER|longueur> [texte]\r\n`, puis, avec une
longueur, exactement ce nombre d'**octets** de données (fin de ligne comprise pour le texte
multiligne). « OK, ER et longueur sont les trois seuls statuts » (D). Les événements arrivent
à tout moment, même avant la réponse à `SUBSCRIBE` (D).

| Commande | Requête → réponse | Vu | VMA |
|---|---|---|---|
| `FUNCTION` | `FUNCTION <Nom> [requête]` → `FUNCTION OK <Nom>` ou `FUNCTION ER <message>` ; la requête a la syntaxe de l'HTTP, encodée URL | D, M | oui |
| `TALLY` | → `TALLY OK 0121…` : un chiffre par input, dans l'ordre (0 éteint, 1 Program, 2 Preview), jusqu'à 1 000 | D | oui |
| `XML` | → `XML <octets>\r\n` puis le document d'état | D, M | oui |
| `XMLTEXT <xpath>` | → `XMLTEXT OK <valeur>` (une ligne) ou longueur + texte multiligne | D | sous-ensemble (Q14) |
| `ACTS <activateur> [input]` | → `ACTS OK <activateur> [input] <valeur 0–1>` ; `ER No Input` si l'activateur n'a pas d'input | D, M | oui (§ 5) |
| `SUBSCRIBE` / `UNSUBSCRIBE` `TALLY` ou `ACTS` | → `SUBSCRIBE OK TALLY` ; événements jusqu'au désabonnement ou à la fermeture | D, M | oui |
| `QUIT` | ferme la connexion | D | oui |
| `PING` | → `PING OK PONG` (absent de la doc ; le module l'envoie toutes les 3 s et attend cette ligne) | M | oui |

Le module ouvre **trois connexions** (fonctions, activateurs, XML), découpe les lignes sans
tampon entre deux paquets : chaque ligne d'événement doit partir d'un seul envoi (M).

**HTTP** (port 8088 par défaut, Q5) : `GET /api` = XML d'état ; `GET /api/?Function=<Nom>&…`
= appel ; « code 200 » en cas de succès, « code 500 » en cas d'erreur (D). Pas
d'authentification documentée (D).

## 2. Paramètres communs
- **Input** (D) : numéro à partir de 1 (`0` = la Preview, `-1` = le Program) ; ou le **nom**
  (sensible à la casse, complet) ; ou la **clé** (GUID de l'attribut `key` du XML).
- **Mix** (D) : `0` = le mélange principal ; plusieurs, séparés par des virgules. VMA n'a qu'un
  mélange : `0` accepté, autre chose → erreur.
- **Duration** : millisecondes, transitions seulement (D) ; **Value** : selon la fonction (D).
- **Nom de fonction** : la doc ne dit rien de la casse ; le module envoie `fadeToBlack`,
  `quickPlay` (M) : VMA compare **sans tenir compte de la casse**.

## 3. XML d'état
Élément racine : le nom de la référence en minuscules (question Q1 de la Phase 5).
L'exemple de la doc (D) a : `version`, `inputs/input` (`key`, `number`, `type`, `title`,
`state`, `position`, `duration`, `muted`, `loop`, `selectedIndex`, texte = titre),
`overlays/overlay` (`number`, texte = numéro de l'input), `preview`, `active`, `fadeToBlack`,
`transitions/transition` (`number`, `effect`, `duration`), `recording`, `external`,
`streaming`, `playList`, `multiCorder` ; booléens `True` / `False`.

Le module (M) ignore **tout le document** s'il manque l'un de : `version`, `edition`,
`preview`, `active`, `audio`, `fadeToBlack`, `external`, `recording`, `streaming`, `playList`,
`multiCorder`, `fullscreen` ; il lit aussi, s'ils sont là : `inputs` (au moins un `input`
s'il est présent), `overlays`, `transitions`, `outputs`, `preset`, `mix`, `dynamic` ; par
input : `shortTitle`, `volume`, `muted`, `solo`, `balance`, `gainDb`, `audiobusses` (`M,A,B`),
`meterF1`, `meterF2` ; dans `audio` : un enfant par bus (`volume`, `muted`, `meterF1`,
`meterF2`, `solo`, `sendToMaster`, `headphonesVolume`) ; `recording` : attribut `duration`.
Au-delà de la version 27, il lit des champs que VMA n'a pas (Q2).

**À confirmer par l'observation** (Q3) : noms des types d'input (`type`), valeurs de `state`,
nom des enfants de `audio`, échelle de `volume` et des vumètres, attributs des overlays.

## 4. Fonctions
Toutes les autres fonctions de la référence (plusieurs centaines) répondent `ER` / 500 avec
« fonction non prise en charge par VMA ». Effets de transition de VMA : Fade, Cut et stingers.

### Transitions
| Fonction | Paramètres (doc) | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `Cut` | Input, Duration | Preview → Program sans transition ; avec Input : cet input | oui | D (nom de transition), M |
| `Fade` | Input, Duration | Fondu Preview → Program sur Duration (défaut : celle du bouton 1) ; avec Input : cet input | oui (commande à ajouter : transition avec effet, durée et input donnés) | D, M |
| Autres effets (`Wipe`, `Zoom`, `Slide`, `Merge`…) | Input, Duration | — | non : effets absents | D (principe), M |
| `CutDirect` | Input | Input au Program sans changer la Preview | oui | D, M |
| `Transition1`–`Transition4` | — | bouton de transition *n* | oui | D, M |
| `Stinger1`–`Stinger2` | Input, Mix | stinger *n* | oui | D, M |
| `Stinger3`–`Stinger4` | Input, Mix | — | non : 2 stingers | D |
| `PreviewInput` | Input, Mix | Preview | oui | D |
| `PreviewInputNext` / `Previous` | — | Preview suivante / précédente | oui | D |
| `ActiveInput` | Input, Mix | « envoie l'input à la sortie » : comme `CutDirect` | partiel : effet exact à observer | D, M |
| `QuickPlay` | Input | Quick Play de l'input | oui | D, M |
| `SetFader` | Value 0–255 | T-Bar ; 255 termine | oui | D, M |
| `SetTransitionEffect1`–`4` | Value | effet du bouton | partiel : Fade, Cut, Stinger1–2 | D, M |
| `SetTransitionDuration1`–`4` | Value (ms) | durée du bouton | oui | D, M |
| `FadeToBlack` | — | FTB (bascule) | oui | D, M |

### Overlays (entrées **d** : à revérifier, la page n'a été lue qu'en partie)
| Fonction | Paramètres | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `OverlayInput1`–`4` | Input, Mix | bascule de l'overlay *n* avec cet input | oui | d, M |
| `OverlayInput1In`–`4In` | Input, Mix | entrée de l'overlay | oui | d, M |
| `OverlayInput1Out`–`4Out` | — | sortie (avec sa transition) | oui | d, M |
| `OverlayInput1Off`–`4Off` | — | sortie immédiate | oui | d, M |
| `OverlayInput1Last`–`4Last` | Mix | rentre le dernier input du channel | oui | d, M |
| `OverlayInput1Zoom`–`4Zoom` | — | — | non | d, M |
| `OverlayInput5`–`8…` | | — | non : 4 channels | d, M |
| `OverlayInputAllOff` | — | tous les overlays sortent | oui | d, M |
| `PreviewOverlayInput1`–`4` | Input | — | non : pas d'overlay en Preview | d, M |

### Sorties
| Fonction | Paramètres | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `StartRecording` / `StopRecording` / `StartStopRecording` | — | enregistrement | oui | D, M |
| `StartStreaming` / `StopStreaming` / `StartStopStreaming` | Value = numéro de stream à partir de 0, vide = tous | destinations RTMP / SRT, dans l'ordre de la fenêtre Sorties | oui | D, M |
| `Fullscreen` / `FullscreenOn` / `FullscreenOff` | — | sortie plein écran (si elle est configurée) | oui | D, M |
| `StartExternal`, `StartMultiCorder`, `SetOutput2`… | | — | non (sortie externe, ISO : Phase 6) | D |

### Son
| Fonction | Paramètres | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `Audio` / `AudioOn` / `AudioOff` | Input | muet de l'input | oui | D |
| `AudioAuto` / `AudioAutoOn` / `AudioAutoOff` | Input | son qui suit la vidéo | oui | D |
| `Solo` / `SoloOn` / `SoloOff` | Input | solo | oui | D |
| `AudioBus` / `AudioBusOn` / `AudioBusOff` | Value = M, A, B…G ; Input | envois Master, A, B | partiel : C–G refusés | D |
| `SetVolume` | Value 0–100 ; Input | fader de l'input | partiel : loi 0–100 → dB à observer | D |
| `SetMasterVolume`, `SetBusAVolume`, `SetBusBVolume` | Value 0–100 | gain du bus | partiel : même loi | D |
| `MasterAudio…`, `BusAAudio…`, `BusBAudio…` (+ `On` / `Off`) | — | muet du bus | oui | D |
| `SetHeadphonesVolume` | Value 0–100 | volume du casque | partiel : même loi | D |
| `SetVolumeFade`, `SetGain` | | — | non (pas de fondu de fader, pas d'étage de gain) | D |

### Inputs
| Fonction | Paramètres | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `Play` / `Pause` / `PlayPause` / `Restart` | Input | lecture | oui | D |
| `Loop` / `LoopOn` / `LoopOff` | Input | boucle | oui | D |
| `SetPosition` | Value (ms) ; Input | position | oui | D |
| `SetInputName` | Value ; Input | renomme | oui | D |
| `RemoveInput` | Input | retire | oui | D |
| `AddInput` | Value = `Type\|Fichier` (`Video`, `Image`, `Colour`) | ajoute | partiel : ces trois types | D |
| `SetText`, `SelectIndex`, `NextItem`… | | — | non (titres natifs, listes : absents) | D |

### Général
| Fonction | Paramètres | Effet dans VMA | État | Vu |
|---|---|---|---|---|
| `OpenPreset` / `SavePreset` | Filename | ouvre / enregistre un **projet VMA** (pas un preset de la référence) | partiel | d |
| `KeyPress`, `ScriptStart` | | — | non | D, d |

## 5. Activateurs (`ACTS`, `SUBSCRIBE ACTS`)
Forme (M) : `ACTS OK <événement> <input> <valeur>`, ou `ACTS OK <événement> <valeur>` sans
input ; valeurs de 0 à 1 (booléens → 1 / 0, volumes en flottants) (D).
Émis par VMA : `Input`, `InputPreview`, `Overlay1`–`4`, `InputPlaying`, `InputVolume`,
`InputAudio`, `InputSolo`, `InputAudioAuto`, `InputMasterAudio`, `InputBusAAudio`,
`InputBusBAudio`, `MasterVolume`, `MasterAudio`, `MasterHeadphones`, `BusAVolume`,
`BusAAudio`, `BusASolo`, `BusBVolume`, `BusBAudio`, `BusBSolo`, `FadeToBlack`, `Recording`,
`Streaming`, `Fullscreen`. Demandés par le module et absents de VMA (`BusC`–`G…`,
`Replay…`, `InputMix2…`, `InputVolumeChannelMixer…`) → `ER`.
