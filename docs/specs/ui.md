# Spec — `ui` (Phase 1)

- **Statut** : proposé, à valider avec [`phase-1.md`](phase-1.md)

## Règle
L'UI **ne modifie jamais l'état** : chaque action envoie une `Command` (`CommandSender`) et
l'affichage relit le snapshot `State` (une fois par frame). Elle ne bloque jamais sur le
moteur : si une commande est refusée ou que le canal est plein, un message s'affiche.

## Disposition
```
┌──────────────────────────────────────────────────────────────┬──────────────┐
│  PREVIEW (bord vert)            │  PROGRAM (bord rouge)       │ Statistiques │
│                                 │                             │  (existant,  │
│                                 │                             │  + par input)│
├─────────────────────────────────┴─────────────────────────────┤              │
│ [ CUT ]  [ FADE ]  durée [ 1000 ] ms   domaine (gamma|linéaire)│              │
├────────────────────────────────────────────────────────────────┤              │
│ Inputs                                              [+ Ajouter]│              │
│  1 ● Caméra 1   1920×1080 50p NV12   Live          (tally)  ✕  │              │
│  2 ● Fichier    1920×1080 25p I420   Live                   ✕  │              │
│  3 ● Couleur    #202020                                     ✕  │              │
│  4 ○ Caméra 2   signal perdu (tentative 3)                  ✕  │              │
└────────────────────────────────────────────────────────────────┴──────────────┘
```
- **Tally** : pastille et bord **rouge** = Program, **vert** = Preview ; pendant un Fade, les
  deux inputs concernés sont rouges (convention courante).
- Clic sur un input = `SetPreview` ; double-clic = `SetProgram`.
- **Ajouter** : menu Capture (liste des périphériques de `io-gst`), Fichier vidéo, Image (sélecteur
  de fichier, *question 6*), Couleur (sélecteur de couleur egui), Source de test.
- Pendant un Fade, la barre de progression du bouton FADE avance ; SetPreview / SetProgram
  sont grisés.
- Pas de vignettes live en Phase 1 (*question 7*).

## Raccourcis clavier (*question 4*)
| Touche | Commande |
|---|---|
| `1`–`9` | `SetPreview` de l'input n° 1–9 |
| `Espace` | `Cut` |
| `Entrée` | `Fade { duration_ms: None }` |

Les raccourcis sont inactifs quand un champ texte a le focus.

## Statistiques (ajouts au panneau Phase 0)
Par input : état, mode (taille, cadence, format), images reçues / s, répétées, sautées,
latence arrivée → tick p50 / p99, conversions `videoconvert`. Global : commandes reçues /
refusées, `journal_dropped`.

## Critères d'acceptation
| Critère | Test |
|---|---|
| Chaque bouton, clic, double-clic et raccourci émet exactement la `Command` attendue (A11) | test egui sans fenêtre (`egui::Context` + événements simulés), `CommandSender` factice |
| Raccourcis ignorés quand un champ texte a le focus | idem |
| L'UI reflète un nouvel état au plus une frame UI après sa publication | idem, snapshot injecté |
| Tally conforme au tableau ci-dessus, y compris pendant un Fade | idem (couleurs lues dans la sortie de la frame) |
| Utilisation réelle : ajout de chaque type d'input, Cut / Fade à la souris et au clavier | checklist `docs/manual-tests/phase1-inputs.md` |
