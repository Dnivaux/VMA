# Phase 1 — Mélange de base : spec d'ensemble et plan

- **Statut** : **validé** par le mainteneur le 2026-10-26 (« oui pour tout »), avec la consigne
  de **se rapprocher le plus possible du comportement de la référence** → §7, ajouts validés
  le même jour
- **Date** : 2026-10-26
- **Références** : BRIEF §4 (Phase 1), §5, §8 ; ADR 0001 (amendé), ADR 0002 ; ADR acceptés
  0003 (état, commandes, threads), 0004 (entrées GStreamer), 0005 (chaîne couleur)
- **Specs de module** : [`commands.md`](commands.md) (engine), [`io-gst.md`](io-gst.md),
  [`compositor.md`](compositor.md) § Phase 1, [`ui.md`](ui.md)

## 1. Périmètre

### Dans la Phase 1
- **Inputs** : caméra / carte de capture (GStreamer), fichier vidéo (GStreamer ; lecture / pause,
  Loop), image fixe, couleur unie. Ajout / retrait à chaud.
- **Couche de commandes** : `Command` (enum) → canal borné → état unique versionné → snapshots.
  Toute action de l'UI passe par là. Journal des commandes (JSON Lines).
- **Compositeur** : Preview + Program, transitions **Cut** et **Fade** (4 boutons de transition, Quick Play, T-Bar — §7), cibles
  internes `Rgba16Float`, conversion YUV → RGB en shader, mise à l'échelle en lumière linéaire,
  mixage en domaine gamma par défaut (linéaire en option).
- **Multi-cadence** (ADR 0002) : cadence maître 25 / 29,97 / 30 / 50 / 59,94 / 60p, choisie au
  démarrage ; inputs de cadence quelconque rééchantillonnés sur l'horloge maître.
- **UI** : barre d'inputs avec vignettes live et boutons par input, vues Preview et Program avec
  tally, Cut, Quick Play, 4 boutons de transition, T-Bar, raccourcis clavier, panneau
  Statistiques étendu (par input).
- **Signal perdu** : une entrée qui tombe passe en « signal perdu » et se reconnecte seule ; le
  programme ne tombe jamais.

### Hors Phase 1 (rappel)
Audio (Phase 3 : les pistes audio des fichiers et captures sont ignorées), sorties (Phase 2),
overlays, Wipe / Stinger (Phase 4), multiview (Phase 5), FTB (Phase 2, avec les sorties), sauvegarde de
projet (Phase 5), changement de cadence **à chaud** (Phase 5, avec les réglages projet),
zero-copy GPU (plus tard), NDI / SRT (Phase 3).

## 2. Architecture (détail dans l'ADR 0003)

```
 UI egui ──Command──┐                          ┌──► ArcSwap<State>  (UI, futur API : lecture)
 (futur API, IA) ───┤ crossbeam borné          │
                    ▼                          │
            ┌─ thread vma-control ─────────────┴─┐
            │ applique les commandes, état       │──► journal commands.jsonl (non bloquant)
            │ versionné, cycle de vie des inputs │
            └──┬────────────────────────────┬────┘
   RenderPlan  │ (Copy, triple buffer)      │ ressources GPU des inputs
               ▼                            ▼ (attache / retrait, renvoi pour destruction)
 ┌─ thread vma-render (horloge maître) ───────────────────────────┐
 │ à chaque tick : lit le plan, prend la dernière image de chaque │
 │ input, compose Preview + Program, publie (triple buffers UI)   │
 └────────────────────────────────────────────────────────────────┘
               ▲ dernière image par input (triple buffer de textures, même invariant qu'ADR 0001)
 ┌─ threads GStreamer (un pipeline par input) ─┐
 │ appsink → upload `queue.write_texture`      │
 └─────────────────────────────────────────────┘
```

Règle temps réel conservée : le thread de rendu ne fait ni allocation applicative, ni verrou,
ni I/O ; tout ce qui alloue (création de textures, pipelines, journal) vit sur `vma-control`
ou sur les threads GStreamer.

## 3. Critères d'acceptation de la Phase 1

« Fait » = test vert en CI, ou rapport de soak / checklist manuelle commité si le matériel est
requis (et listé comme tel dans `ROADMAP.md`).

| # | Critère | Test | Où |
|---|---|---|---|
| A1 | **4 inputs 1080p50 simultanés, 0 image perdue sur 30 min** (BRIEF) | soak `vma --soak 1800` avec 4 inputs synthétiques 1080p50 + rapport | manuel (machine de dev), Linux **et Windows** |
| A2 | **Golden frame du Fade à 50 %** (BRIEF) | `compositor` : deux couleurs unies, valeur exacte attendue en domaine gamma ; mire ↔ couleur en PSNR | CI |
| A3 | Cut effectif au tick suivant la commande (BRIEF §8) | `engine` : temps simulé, commande entre deux ticks → Program changé au tick suivant | CI |
| A4 | Fade de *d* ms = `round(d × cadence)` images exactement, progression linéaire, swap Preview/Program à la fin | `engine` + golden à 0 %, 50 %, 100 % | CI |
| A5 | **Chaque cadence maître** (25, 29,97, 30, 50, 59,94, 60p) : horloge sans dérive, rééchantillonnage correct d'une source à chacune de ces cadences | tests déterministes (temps simulé, source factice) : 36 combinaisons source × maître | CI |
| A6 | Chaque cadence maître en réel : 2 min, 4 inputs, 0 image perdue | soaks courts, un rapport par cadence | manuel |
| A7 | Conversion YUV → RGB : NV12, I420, YUY2, UYVY, matrices 601 / 709, plages limitée / pleine | golden sur barres de couleur synthétiques (valeurs connues à ±1) | CI |
| A8 | Signal perdu : pipeline en erreur / source débranchée → input « signal perdu » (noir), Program non interrompu, reconnexion automatique | `io-gst` : pipeline factice forcé en erreur ; checklist manuelle (débrancher la webcam) | CI + manuel |
| A9 | Fichier en boucle sans trou (0 image noire à la boucle) | `io-gst` : fichier court généré en CI, ≥ 3 boucles, continuité des index | CI |
| A10 | Latence entrée → Program ≤ 2 images (BRIEF §8), mesurée et affichée | stats par input (arrivée → tick qui l'utilise) ; vérifiée dans A1 | manuel |
| A11 | Toute action de l'UI passe par `Command` et est journalisée | test : chaque bouton / raccourci émet la `Command` attendue ; journal relu | CI |
| A12 | Ajout / retrait d'input à chaud sans image manquée | `engine` (GPU logiciel) : 20 ajouts / retraits pendant le rendu | CI |
| A13 | Soak hostile Windows reporté de la Phase 0 | `vma --soak 600 --hostile` sous Windows | manuel (échéance fixée) |
| A14 | CI verte Ubuntu + Windows, GStreamer installé en CI | workflow | CI |

## 4. Changements dans `core` (à signaler, BRIEF §10)
Ajouts uniquement, aucun type existant modifié dans sa forme (fait à l'étape 1) :
- `PixelFormat` (`Nv12`, `I420`, `Yuy2`, `Uyvy`, `Rgba`, `Bgra`), avec la géométrie des plans
  (`plane`, `PlaneSize`, `frame_bytes` ; dimensions de chroma arrondies au supérieur) partagée
  par `io-gst` (copie) et `compositor` (textures) ;
- `ColorInfo { matrix: Matrix (Bt601 | Bt709), range: Range (Limited | Full) }`, avec
  `ColorInfo::assumed` (règle par défaut de `io-gst.md`) et `ycbcr_to_rgb` (matrice affine
  3 × 4 sur les codes normalisés, utilisée par le shader et par les références CPU des tests) ;
- `Rgb8 { r, g, b }` (couleur unie, R'G'B' encodé), texte `#rrggbb` ;
- `InputKind` (`Capture`, `File`, `Image`, `Color`, `TestSource`) : étiquette du type d'input
  (la description complète reste `InputSpec`, dans `engine`) ; `TestSource` ajouté pour suivre
  `InputSpec` ;
- `FrameRate::FPS_29_97` et `FrameRate::MASTER_RATES` (les 6 cadences maîtres, ADR 0002) ;
- `serde` (`Serialize` / `Deserialize`) sur tous les types de `core`, pour l'état et le journal.
  `FrameRate` et `Resolution` sont **validés à la lecture** (dénominateur nul, dimension nulle
  ou > 8192 refusés ; cadence réduite).

## 5. Plan d'implémentation (un commit = une fonctionnalité testée)
Ordre choisi pour avoir un Program qui change le plus tôt possible, puis brancher le vrai
GStreamer.

1. **ADR 0003–0005 validés**, `core` : nouveaux types + tests.
2. `engine` : `Command`, `State` versionné, thread `vma-control`, journal JSONL, `RenderPlan` vers
   le rendu. Inputs « couleur » seulement. `Cut`, `CutDirect`, `QuickPlay`, 4 transitions,
   `SetFader`. Tests A3, A11 (partiel), A15–A18.
3. `compositor` : cibles `Rgba16Float`, passe de sortie `Rgba8Unorm`, couleur unie, Cut / Fade
   (gamma par défaut, linéaire en option). Golden A2, A4.
4. `engine` + `ui` : Preview + Program dans l'UI, tally, barre d'inputs (vignettes), Cut,
   Quick Play, 4 boutons de transition, T-Bar, raccourcis.
   *Premier jalon visible : des couleurs, Cut / Fade / T-Bar à la main.*
5. `compositor` : upload d'images CPU (NV12, I420, YUY2, UYVY, RGBA, BGRA), YUV → RGB, mise à
   l'échelle linéaire, letterbox. Golden A7. Input « image fixe » (crate `image`).
6. `engine` : triple buffer de textures par input, sélection de l'image par tick
   (rééchantillonnage), stats par input, ajout / retrait à chaud. Tests A5, A12 (source
   factice).
7. `io-gst` : pipelines `videotestsrc` → appsink → upload ; état et reconnexion ; CI avec
   GStreamer (Ubuntu + Windows). Tests A8, A9 (fichier généré).
8. `io-gst` : capture (DeviceMonitor : V4L2 / Media Foundation), fichier en boucle
   (`uridecodebin`, seek de segment), lecture / pause / redémarrage / Loop et options
   d'auto-lecture (A20), UI d'ajout d'input (liste des périphériques, sélecteur de fichier).
9. Multi-cadence de bout en bout : `--rate`, `--resolution`, sources synthétiques à toutes les
   cadences, soaks courts A6.
10. Soak d'acceptation A1 (Linux, puis Windows avec A13), rapport, `ROADMAP.md`, arrêt pour revue.

Parallélisation possible après l'étape 1 (types de `core` figés) : `compositor` (3, 5) et
`io-gst` (7) en worktrees séparés.

## 6. Décisions (questions validées le 2026-10-26)
Les dix propositions ont été acceptées ; §7 en corrige trois pour coller à la référence
(double-clic, vignettes, fichiers vidéo).

| # | Décision |
|---|---|
| 1 | Après Cut / Fade / transition : Preview et Program **s'échangent** (confirmé par la doc de la référence) |
| 2 | Fade pendant un Fade : ignoré ; Cut pendant un Fade : **ignoré aussi, le Fade continue** (observé sur la référence le 2026-10-26, §7.3 ; d'abord prévu « le termine ») |
| 3 | Input en signal perdu à l'antenne : noir, état visible dans l'UI |
| 4 | Raccourcis `1`–`9` Preview, `Espace` Cut, `Entrée` transition 1 (la référence n'a pas de raccourcis imposés : ils sont définis par l'utilisateur, ce qu'on fera en Phase 5) |
| 5 | Cadence et résolution choisies au démarrage ; changement à chaud en Phase 5 |
| 6 | Sélecteur de fichier natif `rfd` |
| 7 | ~~Pas de vignettes~~ → **vignettes live en Phase 1** (§7.1) |
| 8 | Fade linéaire |
| 9 | Fit avec bandes noires |
| 10 | Linéarisation BT.1886 (γ 2,4) |

## 7. Alignement sur la référence (consigne du mainteneur)
Règles inchangées (BRIEF §2) : on reproduit des **comportements** décrits dans la documentation
publique ou observés, jamais l'apparence (pas de copie de l'interface, des couleurs du thème, des
icônes ni des textes) et jamais le nom. Source : guide utilisateur public de la référence
(version 28, sections *Getting Started*, *Transitions*, *Inputs*, *Settings – Options*,
*Shortcut Function Reference*), consulté le 2026-10-26.

### 7.1 Ajouts et corrections à la Phase 1 (validés le 2026-10-26)
| Comportement documenté | Avant | Désormais |
|---|---|---|
| Barre d'inputs avec une **petite vue live** de chaque input | liste textuelle | **vignettes live** (rendues à cadence réduite, voir `compositor.md`) |
| Clic sur un input = Preview ; **double-clic = réglages de l'input** | double-clic = Program | double-clic = fenêtre de réglages (nom, couleur, boucle) |
| Boutons sous chaque input : **Fermer, Quick Play, Cut, Loop** (+ overlays 1–4) | ✕ seulement | Fermer, Quick Play, Cut, Loop ; overlays en Phase 4 |
| **Quick Play** : bascule Preview → Program avec une transition dédiée, **500 ms de Fade par défaut** (réglable) ; aussi par input | absent | commande `QuickPlay`, durée réglable (défaut 500 ms) |
| **Cut d'un input** : envoie directement au Program **sans changer la Preview** | `SetProgram` | renommé `CutDirect`, même comportement |
| **Quatre boutons de transition** configurables (effet + durée), en plus de Cut | un bouton Fade + durée | 4 boutons ; effets disponibles en Phase 1 : Fade, Cut (Wipe, Stinger… en Phase 4) |
| **T-Bar** : transition manuelle avec l'effet du bouton 1 ; valeur 0–255, 255 = bascule terminée | absent | T-Bar dans l'UI + commande `SetFader` |
| Vidéo : **lecture / pause, redémarrage, position**, bouton **Loop** ; options « démarrer la vidéo quand elle passe à l'antenne », « la redémarrer », « mettre en pause l'input qui quitte l'antenne » | fichier toujours en lecture, en boucle | contrôles de lecture + Loop par input + les 3 options globales |
| **Cadence maître** : tous les inputs sont convertis à cette cadence | — | déjà prévu (ADR 0002) |
| Disposition Preview à gauche, Program à droite | idem | inchangé (convention des mélangeurs ; notre propre style visuel) |

Reporté : **FTB** (fondu au noir) agit sur les sorties (enregistrement, sortie externe, plein
écran) et pas sur la vue Program de l'UI → Phase 2, avec les sorties. Overlays 1–4 → Phase 4.
Catégories d'inputs, glisser-déposer pour réordonner → Phase 5 (projet).

### 7.2 Critères d'acceptation ajoutés
| # | Critère | Test | Où |
|---|---|---|---|
| A15 | Quick Play global et par input : Fade de la durée Quick Play, puis échange | `engine`, temps simulé | CI |
| A16 | `CutDirect` : Program changé au tick suivant, Preview inchangée | `engine` | CI |
| A17 | 4 boutons de transition : effet et durée propres, `Entrée` = bouton 1 | `engine` + `ui` | CI |
| A18 | T-Bar : progression = valeur / 255, 255 termine la transition (échange), retour à 0 sans effet ensuite | `engine` + golden à 128 | CI |
| A19 | Vignettes : 4 inputs 1080p50 + vignettes, 0 image perdue (dans le soak A1) | soak | manuel |
| A20 | Vidéo : lecture / pause / redémarrage / Loop ; options d'auto-lecture appliquées aux transitions | `io-gst` (fichier généré) + `engine` | CI |

### 7.3 À vérifier par observation sur la référence
Valeurs par défaut et cas limites **non décrits** dans la documentation, à observer dans le
logiciel (checklist [`docs/manual-tests/observation-reference-phase1.md`](../manual-tests/observation-reference-phase1.md)).
En attendant, la valeur entre parenthèses est utilisée.

**Observé le 2026-10-26** : tout est confirmé sauf deux points.
- **Cut pendant une transition : ignoré** (le Fade continue), et non « la termine ». Corrigé.
- Durées par défaut des 4 boutons : **pas encore relevées** (question de suivi) ; Fade 500 /
  1000 / 1500 ms et Cut restent provisoires.

Liste d'origine :
- Effets et durées par défaut des 4 boutons de transition (Fade 500 ms, Fade 1000 ms, Fade
  1500 ms, Cut).
- Quick Play échange-t-il aussi Preview et Program ? (oui)
- Transition demandée pendant une transition (ignorée) ; Cut pendant une transition (la termine).
- Loop activé par défaut sur un nouvel input vidéo ? (non) Et les trois options d'auto-lecture
  (démarrer : oui, redémarrer : non, pause en sortie : oui) ?
- T-Bar relâchée à mi-course : l'image reste mixée (oui).
