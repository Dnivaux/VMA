# Phase 1 — Mélange de base : spec d'ensemble et plan

- **Statut** : proposé, **à valider** (BRIEF §10 : pas de code avant validation)
- **Date** : 2026-10-26
- **Références** : BRIEF §4 (Phase 1), §5, §8 ; ADR 0001 (amendé), ADR 0002 ; ADR proposés
  0003 (état, commandes, threads), 0004 (entrées GStreamer), 0005 (chaîne couleur)
- **Specs de module** : [`commands.md`](commands.md) (engine), [`io-gst.md`](io-gst.md),
  [`compositor.md`](compositor.md) § Phase 1, [`ui.md`](ui.md)

## 1. Périmètre

### Dans la Phase 1
- **Inputs** : caméra / carte de capture (GStreamer), fichier vidéo en boucle (GStreamer), image
  fixe, couleur unie. Ajout / retrait à chaud.
- **Couche de commandes** : `Command` (enum) → canal borné → état unique versionné → snapshots.
  Toute action de l'UI passe par là. Journal des commandes (JSON Lines).
- **Compositeur** : Preview + Program, transitions **Cut** et **Fade** (durée réglable), cibles
  internes `Rgba16Float`, conversion YUV → RGB en shader, mise à l'échelle en lumière linéaire,
  mixage en domaine gamma par défaut (linéaire en option).
- **Multi-cadence** (ADR 0002) : cadence maître 25 / 29,97 / 30 / 50 / 59,94 / 60p, choisie au
  démarrage ; inputs de cadence quelconque rééchantillonnés sur l'horloge maître.
- **UI** : liste des inputs (avec état), vues Preview et Program avec tally, boutons Cut / Fade,
  durée du Fade, raccourcis clavier, panneau Statistiques étendu (par input).
- **Signal perdu** : une entrée qui tombe passe en « signal perdu » et se reconnecte seule ; le
  programme ne tombe jamais.

### Hors Phase 1 (rappel)
Audio (Phase 3 : les pistes audio des fichiers et captures sont ignorées), sorties (Phase 2),
overlays, Wipe / Stinger (Phase 4), multiview et vignettes des inputs (Phase 5), sauvegarde de
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
Ajouts uniquement, aucun type existant modifié :
- `PixelFormat` (`Nv12`, `I420`, `Yuy2`, `Uyvy`, `Rgba`, `Bgra`) ;
- `ColorInfo { matrix: Matrix (Bt601 | Bt709), range: Range (Limited | Full) }` ;
- `Rgb8 { r, g, b }` (couleur unie, R'G'B' encodé) ;
- `InputKind` (`Capture`, `File`, `Image`, `Color`) — description sérialisable d'un input.

## 5. Plan d'implémentation (un commit = une fonctionnalité testée)
Ordre choisi pour avoir un Program qui change le plus tôt possible, puis brancher le vrai
GStreamer.

1. **ADR 0003–0005 validés**, `core` : nouveaux types + tests.
2. `engine` : `Command`, `State` versionné, thread `vma-control`, journal JSONL, `RenderPlan` vers
   le rendu. Inputs « couleur » seulement. Tests A3, A11 (partiel).
3. `compositor` : cibles `Rgba16Float`, passe de sortie `Rgba8Unorm`, couleur unie, Cut / Fade
   (gamma par défaut, linéaire en option). Golden A2, A4.
4. `engine` + `ui` : Preview + Program dans l'UI, tally, boutons Cut / Fade, raccourcis.
   *Premier jalon visible : deux couleurs, Cut / Fade à la main.*
5. `compositor` : upload d'images CPU (NV12, I420, YUY2, UYVY, RGBA, BGRA), YUV → RGB, mise à
   l'échelle linéaire, letterbox. Golden A7. Input « image fixe » (crate `image`).
6. `engine` : triple buffer de textures par input, sélection de l'image par tick
   (rééchantillonnage), stats par input, ajout / retrait à chaud. Tests A5, A12 (source
   factice).
7. `io-gst` : pipelines `videotestsrc` → appsink → upload ; état et reconnexion ; CI avec
   GStreamer (Ubuntu + Windows). Tests A8, A9 (fichier généré).
8. `io-gst` : capture (DeviceMonitor : V4L2 / Media Foundation), fichier en boucle
   (`uridecodebin`, seek de segment), UI d'ajout d'input (liste des périphériques, sélecteur de
   fichier).
9. Multi-cadence de bout en bout : `--rate`, `--resolution`, sources synthétiques à toutes les
   cadences, soaks courts A6.
10. Soak d'acceptation A1 (Linux, puis Windows avec A13), rapport, `ROADMAP.md`, arrêt pour revue.

Parallélisation possible après l'étape 1 (types de `core` figés) : `compositor` (3, 5) et
`io-gst` (7) en worktrees séparés.

## 6. Questions au mainteneur (avec ma proposition par défaut)

1. **Après une transition, Preview et Program s'échangent-ils** (l'ancien Program revient en
   Preview, comportement « flip-flop » de la plupart des mélangeurs) ? *Proposition : oui.*
2. **Fade demandé pendant un Fade** : ignoré ? **Cut pendant un Fade** : termine le Fade
   immédiatement (Program = destination) ? *Proposition : Fade ignoré, Cut termine.*
3. **Image « signal perdu »** dans le Program : noir (sûr à l'antenne) ou mire ? Une mire avec
   texte demande un rendu de texte, absent en Phase 1. *Proposition : noir dans le Program,
   état bien visible dans l'UI.*
4. **Raccourcis** : `1`–`9` = input en Preview, `Espace` = Cut, `Entrée` = Fade (durée
   courante) ? *Proposition : oui, non configurables en Phase 1.*
5. **Cadence et résolution** choisies **au démarrage** (`--rate`, `--resolution`, et plus tard le
   projet) en Phase 1, changement à chaud en Phase 5 ? *Proposition : oui.*
6. **Sélecteur de fichier natif** via `rfd` (MIT, portail XDG sous Linux) plutôt qu'un champ
   texte ? *Proposition : oui (ADR 0004).*
7. **Vignettes live des inputs** dans la liste : hors Phase 1 (elles relèvent du multiview,
   Phase 5) ? *Proposition : oui, liste textuelle avec état et tally en Phase 1.*
8. **Courbe du Fade** : linéaire ? *Proposition : linéaire (une option de courbe plus tard).*
9. **Ajustement des sources de format différent** : « fit » avec bandes noires, sans
   recadrage ? *Proposition : oui, fit uniquement en Phase 1.*
10. **Linéarisation** pour la mise à l'échelle : EOTF BT.1886 (γ 2,4) et son inverse
    (ADR 0005) ? *Proposition : oui.*
