# Spec — `ui` (Phase 1)

- **Statut** : validé (2026-10-26), complété par l'alignement sur la référence
  ([`phase-1.md`](phase-1.md) §7, validé le 2026-10-26)

## Règle
L'UI **ne modifie jamais l'état** : chaque action envoie une `Command` (`CommandSender`) et
l'affichage relit le snapshot `State` (une fois par frame). Elle ne bloque jamais sur le
moteur : si une commande est refusée ou que le canal est plein, un message s'affiche.

On reproduit l'**organisation et les comportements** d'un mélangeur logiciel classique
(Preview à gauche, Program à droite, barre d'inputs en bas), **avec notre propre style
visuel** : couleurs, icônes, textes et proportions sont les nôtres (BRIEF §2.1–2.2).

## Disposition
```
┌─────────────────────────────┬────────────┬─────────────────────────────┬────────────┐
│ PREVIEW  (tally vert)       │ Quick Play │ PROGRAM  (tally rouge)      │ Statis-    │
│                             │ Cut        │                             │ tiques     │
│  ▶ ❚❚ ⟲  ──●────── 00:12    │ [1] Fade   │  ▶ ❚❚ ⟲  ────●──── 01:03    │            │
│  (contrôles si vidéo)       │ [2] Fade   │  (contrôles si vidéo)       │ (Phase 0   │
│                             │ [3] Fade   │                             │  + par     │
│                             │ [4] Cut    │                             │  input)    │
│                             │ ║ T-Bar ║  │                             │            │
├─────────────────────────────┴────────────┴─────────────────────────────┤            │
│ Inputs                                                    [+ Ajouter]  │            │
│ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐                            │            │
│ │vignette│ │vignette│ │vignette│ │ signal │                            │            │
│ │  live  │ │  live  │ │  live  │ │ perdu  │                            │            │
│ │1 Cam 1 │ │2 Clip  │ │3 Fond  │ │4 Cam 2 │                            │            │
│ │✕ QP Cut│ │✕ QP Cut│ │✕ QP Cut│ │✕ QP Cut│                            │            │
│ │        │ │  Loop  │ │        │ │        │                            │            │
│ └────────┘ └────────┘ └────────┘ └────────┘                            │            │
└────────────────────────────────────────────────────────────────────────┴────────────┘
```
- **Vignettes** : image live de chaque input (petite taille, cadence réduite), numéro, nom, état
  (« signal perdu », « tentative n »), mode (taille, cadence, format) au survol.
- **Tally** : cadre **rouge** = Program, **vert** = Preview ; pendant une transition, les deux
  inputs concernés sont rouges.
- **Souris** (comportement de la référence) : clic sur une vignette = `SetPreview` ;
  **double-clic sur le titre = réglages de l'input** (nom ; couleur pour un input couleur ;
  Loop et position pour une vidéo) ; boutons sous la vignette : Fermer (`RemoveInput`),
  Quick Play (`QuickPlay { input }`), Cut (`CutDirect`), Loop (`SetLoop`, vidéos).
- **Colonne centrale** : Quick Play (`QuickPlay { input: None }`), Cut, 4 boutons de transition
  (`Transition { button }`) avec un menu pour l'effet et la durée (`SetTransition`), T-Bar
  verticale (`SetFader`, effet du bouton 1). Pendant une transition minutée, une barre de
  progression avance sur le bouton actif.
- **Contrôles vidéo** sous Preview et Program quand l'input est une vidéo : lecture / pause,
  redémarrage, position (`Play`, `Pause`, `Restart`, `SetPosition`).
- **Ajouter** : Capture (liste des périphériques de `io-gst`), Vidéo, Image (sélecteur de
  fichier `rfd`), Couleur (sélecteur egui), Source de test.
- **Réglages** (fenêtre) : durée et effet de Quick Play, options d'auto-lecture, domaine de
  mixage.

## Raccourcis clavier
| Touche | Commande |
|---|---|
| `1`–`9` | `SetPreview` de l'input n° 1–9 |
| `Espace` | `Cut` |
| `Entrée` | `Transition { button: 1 }` |

Inactifs quand un champ texte a le focus. Raccourcis configurables (comme dans la référence,
où l'utilisateur les définit lui-même) : Phase 5.

## Statistiques (ajouts au panneau Phase 0)
Par input : état, mode (taille, cadence, format), images reçues / s, répétées, sautées,
latence arrivée → tick p50 / p99, conversions `videoconvert`. Global : commandes reçues /
refusées, `journal_dropped`.

## Mise en œuvre (étape 4 du plan, 2026-10-26)
- `vma_ui::Switcher` : toute la surface de mélange. Il ne lit qu'une `View` (snapshot `State`,
  `CommandSender`, identifiants de textures egui, dernier tick rendu, dernier refus) et ne
  possède aucun objet GPU : les tests le font tourner sans fenêtre ni moteur.
  `MainUi` le relie au moteur (trois `FrameReader` : Program, Preview, atlas des vignettes,
  mis à jour en début de frame, invariant ADR 0001).
- Ajouter : **Couleur** (6 couleurs prêtes + sélecteur) et **Mire** ; Caméra, Vidéo, Image
  sont grisés jusqu'aux étapes 5–8. Une session normale démarre **vide**, comme la référence
  (le mode `--soak` met une mire à l'antenne).
- **Contrôles vidéo** sous Preview / Program (lecture, pause, position) : reportés à
  l'étape 8, avec les vidéos ; le bouton Loop des vignettes et des réglages est déjà là.
- T-Bar : barre verticale, **haut = Program seul, bas = transition terminée**. Arrivée en bas,
  la transition se termine (échange) et le reste du geste est ignoré ; au relâchement, la
  T-Bar repart en haut (observé sur la référence). Chaque position n'est envoyée qu'une fois.
- La barre de progression d'une transition minutée s'affiche sous le bouton qui l'a lancée
  (bouton 1 pour `Entrée`) ; Quick Play n'en a pas encore.
- Menu d'un bouton de transition : **clic droit** (effet Fade / Cut, durée en ms).
- Refus du moteur (`ControlStats::last_rejection`) et canal plein : message dans la barre du
  haut pendant 4 s.
- Barre du haut : Réglages, et un interrupteur **Statistiques** pour libérer de la place.
- Panneau Statistiques : + commandes acceptées / refusées, lignes de journal écrites / perdues.
- Le glyphe « ✕ » manque dans les polices d'egui : le bouton Fermer affiche « × ».
- Journal activé par défaut dans `logs/` (`--journal-dir <dossier>`, `--no-journal`).
- *(Étape 6)* Panneau Statistiques, section **Inputs** : une ligne par input doté d'un feed
  (format source, reçues / s sur ~1 s, répétées, sautées, latence p50 / p99 / max). Le nombre
  de conversions `videoconvert` arrivera avec `io-gst` (étape 7).

## Critères d'acceptation
| Critère | Test |
|---|---|
| Chaque clic, double-clic, bouton, menu, T-Bar et raccourci émet exactement la `Command` attendue (A11) | `crates/ui/tests/switcher.rs` (10 tests, `egui_kittest` piloté par AccessKit, `CommandSender::probe`) |
| Raccourcis ignorés quand un champ texte a le focus | `switcher::double_click_opens_input_settings_and_typing_does_not_trigger_shortcuts` (vérifié : échoue si la protection est retirée) |
| L'UI reflète un nouvel état au plus une frame UI après sa publication | `switcher::ui_follows_the_new_state_on_the_next_frame` |
| Tally conforme, y compris pendant une transition et à la T-Bar | `tally::tests::red_green_and_both_red_during_a_transition` |
| Chaque widget a un nom accessible | contrôle automatique d'`egui_kittest` dans chaque test |
| Utilisation réelle : chaque type d'input, Quick Play, Cut, 4 boutons, T-Bar, contrôles vidéo, à la souris et au clavier | checklist `docs/manual-tests/phase1-inputs.md` |
