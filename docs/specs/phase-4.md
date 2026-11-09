# Phase 4 — Habillage : spec d'ensemble et plan

- **Statut** : **à valider** par le mainteneur (questions Q1–Q10 du § 7)
- **Date** : 2026-11-09
- **Références** : BRIEF §2, §4 (Phase 4), §5, §6, §8 ; ADR 0002 (titres HTML **et** natifs),
  ADR 0004 (entrées), ADR 0005 (couleur), ADR 0007 (audio) ;
  **ADR 0009 (input HTML) et ADR 0010 (alpha, overlays, stinger), à valider**
- **Entrée en phase** : Phase 3 close le 2026-11-09 sous Linux (C14 : 4ᵉ soak d'1 h réussi) ;
  soaks Windows et checklists manuelles reportés (voir `ROADMAP.md`)

## 1. Périmètre

### Dans la Phase 4 (BRIEF)
- **4 overlay channels indépendants**, chacun avec une transition d'entrée et de sortie.
- **Input HTML** : page web rendue hors écran (CEF) **avec canal alpha, à la cadence du
  maître** (50 fps à 1080p50) ; affiche des overlays HTML existants (lower thirds, scores) ;
  reçoit des données **sans recharger la page** (mise à jour d'un score).
- **Transition Stinger** : vidéo avec alpha jouée par-dessus le Program, bascule A → B à un
  point configurable.
- **Acceptation (BRIEF)** : un lower third HTML animé passe en overlay **sans saccade** ;
  golden-frame sur l'alpha.

### Ajouts nécessaires au périmètre
- **Chaîne alpha de bout en bout** : aujourd'hui l'alpha des sources est jeté (le shader de
  conversion rend α = 1). Les overlays n'ont de sens qu'avec des sources qui en portent :
  **images PNG avec alpha**, **vidéos avec alpha** (WebM VP8 / VP9 alpha, ProRes 4444,
  QuickTime Animation / PNG), **NDI® avec alpha** (reporté de la Phase 3), et l'input HTML.
  Les images et vidéos avec alpha permettent aussi de tester les overlays en CI sans CEF.
- **Données vers une page** : commandes internes (`HtmlData`, `HtmlScript`, rechargement,
  changement d'URL), journalisées ; l'API HTTP / TCP qui les exposera vient en Phase 5.
- **Audio de l'input HTML** (reporté de la Phase 3) et **audio du stinger** (Q7, Q9).
- **Licences CEF / Chromium** dans la fenêtre « À propos ».

### Hors Phase 4 (rappel)
Moteur de titres natif (Q8), position / zoom / recadrage d'un input dans un overlay, effets
d'overlay autres que Cut et Fade (glissé, zoom : Q6), incrustation chroma, sortie avec alpha
(NDI ou fichier), multiview, API, sauvegarde du projet (Phase 5), virtual sets.

## 2. Architecture (détail dans les ADR 0009 et 0010)
```
 sources avec alpha (vma-io) : PNG · vidéo alpha · NDI UYVA ─┐
 processus vma-html (CEF hors écran) ─ mémoire partagée BGRA ┤─► feed de l'input (inchangé)
        ▲ commandes (URL, données, script, BeginFrame)       │      α droit → prémultiplié
        └──────────── thread vma-html (app) ◄─ tick n ─┐     ▼        (shader de conversion)
 rendu, tick n :                                       │  calques Rgba16Float prémultipliés
   Program = mélange A / B (Cut, Fade, T-Bar)  ────────┘
           ▸ stinger (image k du fichier, bascule A → B à l'image de bascule)
           ▸ overlay 1 ▸ 2 ▸ 3 ▸ 4   (« over » prémultiplié, domaine gamma, opacité de la
                                      transition de chaque channel)
           ▸ FTB ▸ sorties (NV12, opaque) · vue Program · plein écran
```
- **Overlays** : composés **après** le mélange du Program et **avant** l'étage de sortie (FTB
  par-dessus tout, comme aujourd'hui). Channel 1 en bas, 4 en haut. Une transition d'overlay
  ne change que son opacité (Cut : au tick ; Fade : linéaire sur sa durée). Un channel garde
  son input pendant les transitions du Program.
- **Alpha** : prémultiplié dans tous les calques ; les sources à alpha droit (PNG, ProRes,
  VP9) sont prémultipliées par le shader de conversion ; composition sur les valeurs encodées
  (domaine gamma, Q4), comme un keyer de régie. Une source sans alpha a α = 1 : rien ne change
  pour les Phases 1 à 3 (golden frames inchangés).
- **Input HTML** : CEF tourne dans un **processus auxiliaire `vma-html`** (ADR 0009) : le
  binaire principal ne se lie pas à CEF, démarre sans lui, et un plantage de Chromium ne fait
  tomber que les inputs HTML (« signal perdu », relance). À chaque tick, le thread `vma-html` de
  l'application demande une image (`SendExternalBeginFrame`) : la page avance d'exactement une
  période du maître par tick, sans battement entre deux horloges (critère « sans saccade »).
- **Stinger** : un pipeline par stinger, préchargé (pré-roll) ; au déclenchement, l'image *k*
  du fichier est montrée au tick *t₀ + k*, **dans l'ordre et sans saut** (contrairement aux
  inputs, qui donnent leur dernière image) ; au tick de bascule, le Program passe de A à B sous
  le stinger. Image en retard au décodage : répétée et comptée, le Program ne l'attend pas.

## 3. Critères d'acceptation de la Phase 4
| # | Critère | Comment | Où |
|---|---|---|---|
| D1 | **4 overlay channels** : entrée / sortie en Cut (au tick suivant la commande) et en Fade (durée réglable, opacité linéaire au tick près, aux 6 cadences), ordre 1 → 4, indépendants (un channel qui entre pendant qu'un autre sort), inchangés pendant un Cut / Fade / T-Bar du Program, FTB par-dessus ; tous les overlays sur les sorties, la vue Program et le plein écran | `compositor` (golden) + `engine` (temps simulé, GPU) | CI |
| D2 | **Golden-frame sur l'alpha** (BRIEF) : sources RGBA connues (α = 0, ¼, ½, 1 ; alpha droit et prémultiplié) sur un fond connu : résultat au code près ; overlay en Fade à 50 % ; PNG avec alpha de bout en bout ; une source opaque reste identique aux golden des Phases 1–3 | `compositor` | CI |
| D3 | **Sources avec alpha** : PNG, WebM VP9 alpha, ProRes 4444 (ou QuickTime Animation), relus jusqu'au Program avec leur alpha ; NDI® avec alpha (boucle locale) | `io-gst`, `io-ndi` | CI ; NDI conditionnel (`VMA_REQUIRE_NDI`) |
| D4 | **Input HTML** : page locale à fond transparent → alpha correct dans le Program ; 1080p à la cadence du maître ; taille de l'input = Program par défaut | `io-html` + moteur réel | CI (si le job CEF passe, voir § 8) + machine de dev |
| D5 | **Sans saccade** (BRIEF) : page de test animée (position avancée à chaque `requestAnimationFrame`) en overlay 60 s : relue à chaque tick, **chaque image du Program montre une étape nouvelle et une seule** passée la chauffe ; 0 image répétée ou sautée comptée pour l'input HTML ; latence p99 ≤ 2 images (A10) | id., relecture GPU | CI + machine de dev |
| D6 | **Données sans rechargement** : `HtmlData` (JSON) → la page change sous 2 images, compteur de chargements de la page inchangé ; `HtmlScript`, `HtmlReload`, `SetHtmlUrl` ; journalisés, rejouables | id. | CI |
| D7 | **CEF absent ou en panne** : processus auxiliaire introuvable → inputs HTML refusés avec raison, UI grisée avec explication ; processus de rendu de la page tué → « signal perdu » puis rechargement ; `vma-html` tué → tous les inputs HTML perdus, relance ; **jamais d'effet sur le Program** (0 tick manqué) | `io-html` + `engine` | CI |
| D8 | **Stinger** : image *k* du fichier au tick *t₀ + k* (aucune sautée ni répétée sur un disque local), bascule A → B **au tick de l'image de bascule**, Program = B à la fin ; audio du stinger au Master (Q7) ; golden aux images de bascule ; stinger pas encore préchargé → transition refusée avec raison | `io-gst` + `compositor` + `engine` | CI |
| D9 | **Commandes, journal, UI** : overlays, HTML, stinger passent par `Command`, journalisés ; rejeu → même état ; boutons 1–4 par input, réglages des channels, ajout d'un input HTML, envoi de données, réglages du stinger (A11 : chaque contrôle émet la bonne `Command`) | `engine` + `ui` (sans fenêtre) | CI |
| D10 | **Temps réel** : rendu sans allocation ni verrou bloquant avec 4 overlays et un stinger ; page HTML lente ou figée → images répétées comptées, **0 tick manqué** ; coût des 4 overlays mesuré dans le rapport de soak | `engine` | CI + soak |
| D11 | **Soak d'acceptation** (budget BRIEF §8) : 1 h, 6 inputs 1080p50 + **4 overlays** (dont 2 HTML animés et 1 qui reçoit des données toutes les secondes), stingers et transitions automatiques, 1 stream + 1 enregistrement : 0 image manquée, 0 perdue par sortie, input HTML sans saccade (D5 sur 1 h), synchro < 1 image (C14 inchangé) | soak + rapport | manuel (Linux) |
| D12 | **Licences** : CEF et Chromium dans « À propos » (avis de Chromium joints au paquet) ; `cargo deny` vert avec `vma-html` | `ui` + CI | CI |
| D13 | CI verte Ubuntu + Windows, job CEF compris | workflow | CI |

## 4. Changements dans `core` (à signaler, BRIEF §10)
Ajouts uniquement :
- `InputKind::Html` (`has_video()` vrai, `has_playback()` faux) ;
- `OverlayChannel` (1 à 4, constructeur vérifié) : sérialisable, utilisé par l'état, le
  journal, l'UI et l'API de la Phase 5 ;
- `AlphaMode` (`Opaque`, `Straight`, `Premultiplied`) : porté par la description des images
  des feeds, à côté de `PixelFormat` ; `Opaque` pour toutes les sources actuelles.

## 5. Commandes et état (complète `commands.md`)
```rust
// Ajouts à InputSpec
Html { url: String, size: Option<Resolution> },   // http(s):// ou file:// ; None = taille du Program

// Ajouts à Command
OverlayIn { channel, input: InputId },            // entre avec la transition du channel (remplace son input)
OverlayOut { channel },                           // sort avec la transition du channel
OverlayOff { channel },                           // retire tout de suite
OverlayToggle { channel, input: InputId },        // entre, ou sort si cet input y est déjà
OverlayAllOff,
SetOverlayTransition { channel, effect: Effect, duration_ms: u32 }, // Cut | Fade
HtmlData { input, json: String },                 // événement DOM « vma-data » (detail = JSON)
HtmlScript { input, script: String },             // JavaScript exécuté dans la page
HtmlReload { input },
SetHtmlUrl { input, url: String },
SetStinger { slot: StingerSlot, file: Option<PathBuf>, cut_point_ms: u32 },
// Effect gagne Stinger(StingerSlot) : Transition / SetTransition l'utilisent comme Cut et Fade
```
- État par channel : input, phase (absent, entrée, présent, sortie) et départ de la
  transition, effet et durée ; par stinger : fichier, point de bascule, état du préchargement
  (prêt, en cours, erreur avec raison). Versionné et journalisé comme le reste.
- Un input retiré alors qu'il est dans un overlay : retiré du channel (sans transition).
- Une transition du Program demandée pendant un stinger : refusée avec raison jusqu'à la fin
  du stinger (à confirmer, checklist q. 22).
- Hors bornes (channel 0 ou 5, durée > 10 s, point de bascule au-delà de la durée du fichier,
  JSON invalide, URL d'un autre schéma) : refusés avec raison.
- Le contenu de `HtmlData` et `HtmlScript` est journalisé en entier (il peut contenir un score,
  pas un secret) ; une URL qui contient des identifiants (`user:pass@`) est écrite masquée
  (comme `Secret`).

## 6. Réglages par défaut proposés (modifiables dans l'UI)
| Réglage | Défaut (à confirmer sur la référence, checklist `observation-reference-phase4.md`) |
|---|---|
| Transition d'overlay | Fade, 500 ms, la même à l'entrée et à la sortie |
| Ordre des channels | 1 en bas, 4 en haut |
| Overlays sur la Preview | non (Q5) |
| Input HTML | taille du Program, fond transparent, cadence du maître, échelle 1 |
| Page qui plante | « signal perdu », rechargée après 1 s (au plus une fois toutes les 5 s) |
| Stingers | 2 emplacements, point de bascule à la moitié du fichier, audio au Master |
| Ordre des couches | Program ▸ stinger ▸ overlays 1–4 ▸ FTB (Q7) |
| Sauvegarde | aucune (projet en Phase 5) |

## 7. Questions au mainteneur
| # | Question | Recommandation |
|---|---|---|
| Q1 | Input HTML par **CEF** (crate `cef`, Apache-2.0 OR MIT, CEF sous BSD-3-Clause) **dans un processus auxiliaire `vma-html`** plutôt que lié au binaire (ADR 0009) ? | **Oui** : le binaire démarre sans CEF, un plantage de Chromium ne fait pas tomber le Program |
| Q2 | Cadence de la page **pilotée par le tick du maître** (`SendExternalBeginFrame`) plutôt que par la minuterie propre de CEF ? | **Oui** : seule façon d'avoir exactement une image de la page par tick (D5) ; vérifié à l'étape 4 |
| Q3 | Données : événement DOM `vma-data` (JSON), script JS libre, rechargement, URL. **Comment tes overlays existants (app Electron / React) reçoivent-ils leurs données aujourd'hui** (WebSocket vers ton app, paramètres d'URL, autre) ? Peux-tu fournir une page d'exemple pour les tests ? | Le format ci-dessus ; une page réelle pour D4 à D6 |
| Q4 | Composition de l'alpha **sur les valeurs encodées (gamma), prémultiplié**, sans option linéaire en Phase 4 (ADR 0010) ? | **Oui** : comportement d'un keyer, conforme à ce que les pages web produisent |
| Q5 | Overlays **sur le Program seulement** (sorties, vue Program, plein écran), pas sur la Preview, FTB par-dessus ? | **Oui**, à confirmer sur la référence (checklist) |
| Q6 | Transitions d'overlay : **Cut et Fade** seulement en Phase 4 (glissé, zoom plus tard) ? | **Oui** |
| Q7 | Stinger : **2 emplacements** réglés dans les transitions (fichier + point de bascule en ms), pas un input de la liste ; **son du fichier au Master** ; stinger **sous** les overlays (le lower third reste lisible) ? | **Oui**, à confirmer sur la référence |
| Q8 | **Moteur de titres natif** : l'ADR 0002 le place en Phase 4 « sauf demande contraire ». Le reporter à une sous-phase après la revue de la Phase 4, avec sa spec et son ADR (rendu de texte) ? | **Reporter** : le critère du BRIEF ne le demande pas, l'input HTML couvre les lower thirds et les scores |
| Q9 | **Audio de l'input HTML** dans la Phase 4 (étape 9, horodaté à l'arrivée) ? | **Oui** (reporté de la Phase 3), en dernier |
| Q10 | Binaires CEF **téléchargés à la compilation de `vma-html`** (builds automatiques publiques de CEF), livrés à côté du binaire ; **sans codecs propriétaires** (pas de H.264 dans une `<video>` de page) ; **sans le bac à sable de Chromium** au départ (pages choisies par l'opérateur) ? | **Oui** ; le bac à sable sera repris avant toute page venue d'Internet |

## 8. Risques et limites connus
- **CEF en CI** : le job télécharge et compile le wrapper C++ de CEF (centaines de Mo, `cmake`,
  cache) sur les 2 OS ; rendu hors écran sans écran sous Linux (plateforme *headless* de
  Chromium ou Xvfb) : vérifié à l'étape 4 ; s'il ne tourne pas en CI, D4 à D7 deviennent
  manuels sur la machine de dev (listés comme tels).
- **Copies mémoire** : en rendu logiciel (`OnPaint`), une page 1080p50 = 8,3 Mo par image,
  ≈ 415 Mo/s par input HTML entre les processus, puis l'upload : 4 overlays HTML ≈ 1,7 Go/s.
  Supportable sur la machine de dev ; le partage de texture (`OnAcceleratedPaint`, DMA-BUF /
  D3D11, déjà importé vers wgpu par la crate `cef`) est l'optimisation suivante, comme le
  zero-copy du BRIEF §5.
- **GPU partagé** : Chromium a son propre processus GPU ; sa charge s'ajoute au rendu.
  Mesurée au soak D11.
- **Mémoire** : CEF ≈ quelques centaines de Mo par navigateur ; à mesurer.
- **Pages** : une page qui fixe un fond opaque (`body { background: … }`) masque le Program :
  c'est la page qu'il faut corriger (signalé dans l'UI à l'ajout ? à décider à l'étape 5).
- **Stinger** : précharger un fichier 1080p avec alpha coûte du décodage CPU (alpha droit
  converti en RGBA par GStreamer en Phase 4) ; si un fichier ne suit pas la cadence, ses images
  sont répétées et comptées.
- **Windows** : CEF, alpha et stinger testés en CI seulement (pas de test manuel, comme les
  phases précédentes).
- **Version de Chromium** : chaque mise à jour de la crate `cef` change de Chromium (sécurité,
  comportement des pages) ; épinglée, mise à jour par un commit dédié.

## 9. Plan d'implémentation (un commit = une fonctionnalité testée)
1. **ADR 0009 et 0010 validés** ; `core` : `InputKind::Html`, `OverlayChannel`, `AlphaMode`.
   `compositor` : alpha dans le shader de conversion (droit → prémultiplié), passe d'overlays
   (4 calques, opacité par channel), stinger dans l'ordre des couches ; **D2**, D1 (côté
   compositeur).
2. `io-gst` : sources avec alpha (PNG, WebM VP8 / VP9 alpha, ProRes 4444 / QuickTime
   Animation), NDI® avec alpha ; **D3**.
3. `engine` + `ui` : état et commandes des overlays, transitions au tick, boutons 1–4 par input,
   réglages des channels ; D1 de bout en bout, D9 (overlays), D10 (sans HTML).
   *Jalon visible : un lower third PNG ou vidéo avec alpha en overlay.*
4. **Essai CEF** (ADR 0009, « à vérifier ») : crate `io-html`, binaire `vma-html` hors des
   `default-members`, job CI sur les 2 OS ; mesurés et notés dans l'ADR : rendu hors écran sans
   écran, alpha (prémultiplié ou non), une image par `SendExternalBeginFrame`, coût CPU / mémoire.
   **Point d'arrêt** si une hypothèse tombe : retour au mainteneur avant l'étape 5.
5. Input HTML : protocole de commande + mémoire partagée, démarrage / arrêt de `vma-html`,
   `InputSpec::Html`, absent → désactivé, plantages → signal perdu et relance ; **D4, D7**.
   *Jalon visible : une page HTML en overlay.*
6. Cadence pilotée par le tick ; page de test animée ; **D5**.
7. Données : `HtmlData`, `HtmlScript`, `HtmlReload`, `SetHtmlUrl` ; UI (ajout d'un input HTML,
   champ d'envoi de données) ; **D6**, D9 (HTML).
8. Stinger : préchargement, images dans l'ordre, bascule à l'image près, audio ; UI des
   réglages ; **D8**, D9 (stinger).
9. Audio de l'input HTML (Q9) ; « À propos » : licences CEF / Chromium ; **D12**.
10. Soak d'acceptation **D11** (1 h), rapport, `ROADMAP.md`, arrêt pour revue.

Parallélisation possible après l'étape 1 (types de `core` figés) : `io-gst` (2, 8),
`io-html` (4) et `engine` / `ui` (3) en worktrees séparés.
