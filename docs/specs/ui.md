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
  sont grisés jusqu'aux étapes 5–8 (actifs depuis l'étape 8c). Une session normale démarre **vide**, comme la référence
  (le mode `--soak` met une mire à l'antenne).
- **Contrôles vidéo** sous Preview / Program (lecture, pause, position) : reportés à
  l'étape 8, avec les vidéos (faits à l'étape 8c) ; le bouton Loop des vignettes et des
  réglages est déjà là.
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

## Mise en œuvre (étape 8c, 2026-10-27) : capture, fichiers, contrôles vidéo
- L'UI ne dépend pas de GStreamer : `MainUi` reçoit un `IoStatus` (implémenté par l'app
  au-dessus de `vma_io_gst::IoRuntime`) : GStreamer indisponible, périphériques connectés,
  mode de capture et conversion par input. `MainUi` possède `vma-io` et le libère après le
  moteur. Le `Switcher` lit ces infos dans `View::io` (`IoView`), plus `Timelines` pour la
  position des vidéos.
- **Ajouter** :
  - **Caméra / capture** : sous-menu des périphériques connectés, nom affiché, identifiant au
    survol, « Aucun périphérique détecté » sinon ; `AddInput { Capture, name: nom du
    périphérique }` ;
  - **Vidéo…** et **Image…** : le `Switcher` ne fait que noter la demande
    (`take_file_request`) ; `MainUi` ouvre le sélecteur natif `rfd` sur un thread à part
    (`vma-file-dialog` : l'UI continue de s'afficher), puis envoie `AddInput { File | Image }`.
    Filtres : vidéos (mp4, mov, mkv, webm, avi, m4v, mxf, ts, mts), images (png, jpg, jpeg),
    et « Tous les fichiers ». Linux : portail XDG (pas de GTK lié) ;
  - **GStreamer introuvable** : Caméra et Vidéo grisés avec un message dans le menu, et un
    avertissement permanent dans la barre du haut (raison au survol) ; les images restent
    possibles.
- **Contrôles vidéo** sous Preview et Program quand l'input affiché est une vidéo : Lecture
  / Pause (selon `playing` de l'état), Début (`Restart`), temps « 00:12 / 01:03 » (« · fin »
  à la fin sans Loop), curseur de position : `SetPosition` envoyé une fois, au relâchement (ou
  à une valeur posée au clavier / par l'accessibilité), le curseur suit la souris pendant le
  glissé. Pas de glyphes ▶ / ⏸ : les polices d'egui ne les ont pas toutes, libellés texte.
- **Vignettes** : au survol, le mode (« Mode 1280x720 30p MJPEG → 1280x720 I420 30p » pour une
  capture, le format livré sinon).
- **Statistiques**, par input : mode de capture, conversion (« aucune » ou « videoconvert
  (CPU) »).

## Phase 2 — Sorties (étape 5, 2026-10-31)
- **Barre du haut** : **Enregistrer** (toutes les sorties d'enregistrement ; pendant
  l'enregistrement, bouton rouge « REC hh:mm:ss », un clic l'arrête), **Streamer** (toutes les
  destinations ; « EN DIRECT hh:mm:ss » ; grisé sans destination), interrupteur **Sorties**
  (« Sorties (erreur) » quand une sortie est en erreur).
- **FTB** dans la colonne de transition, sous les 4 boutons : rouge tant que les sorties sont
  au noir ou en fondu, barre de progression pendant le fondu, durée au **clic droit** (ms).
  La vue Program de l'UI ne change pas (seules les sorties passent au noir).
- **Fenêtre Sorties** : une carte par sortie (nom, type, état, encodeur retenu et fichier quand
  elle tourne, raison d'une erreur). À l'arrêt, les réglages se modifient dans un brouillon
  local : enregistrement (dossier, avec « Parcourir… » ouvert par `MainUi` sur un thread à
  part ; codec H.264 / HEVC ; conteneur MKV / MP4 ; débit automatique ou en kb/s), stream
  (RTMP : adresse, clé ; SRT : mode appelant / écoute, hôte, port, latence, phrase de passe ;
  débit). **Appliquer** envoie `UpdateOutput` ; **Démarrer** envoie d'abord les réglages non
  appliqués, puis `StartOutput` ; **Annuler**, **Supprimer**. En marche : un résumé des réglages
  et **Arrêter**. « + Destination de stream » (3 au plus) ouvre un formulaire, « Ajouter »
  envoie `AddOutput`.
- **Secrets** : clé de stream et phrase de passe dans des champs mot de passe, jamais affichées
  ni dans un libellé ; relues d'un journal, le champ est vide (« à saisir de nouveau »).
- **Statistiques**, section Sorties (`vma_ui::output_stats`, testable sans moteur) : pour chaque
  sortie qui n'est pas à l'arrêt, état, encodeur, fichier, durée, débit sur la dernière
  seconde, taille, images envoyées / perdues (rouge si non nul), file d'attente, reconnexions
  (streams). En dessous : images non relues par le rendu et images perdues faute d'image libre
  (pool), affichées dès qu'une sortie est attachée. Durée, débit et taille viennent de
  `vma-io` par `IoStatus::output_activity`.
- *(Étape 6)* Stream en reconnexion : « reconnexion (essai n) » et la raison, dans la fenêtre
  Sorties et les Statistiques ; nombre de connexions perdues depuis le démarrage
  (`OutputActivity::reconnections`).
- Glyphes « ● » et « ⚠ » absents des polices d'egui : libellés texte (« REC », « (erreur) »).

## Phase 2 — Sortie plein écran (étape 8, 2026-11-01)
- **Fenêtre Sorties** : « + Plein écran » (une seule) ajoute la sortie sur l'écran proposé par
  défaut : le **deuxième** de la liste s'il y en a plusieurs (le premier est souvent celui de
  l'opérateur), sinon le seul. Sa carte : liste déroulante **Écran** (« DP-2 · 1920 × 1080 ·
  50 Hz »), puis Démarrer / Appliquer / Annuler / Supprimer comme les autres sorties ; en
  marche, l'écran et **Arrêter**. Avertissements sous la carte : écran **non branché** (« la
  sortie n'est pas affichée » si elle est en marche), **un seul écran** (« la sortie couvrira
  cette fenêtre »), fréquence de l'écran qui n'est pas un multiple de la cadence maître
  (« saccades » : 60 Hz pour du 50p ; 59,94p sur 60 Hz n'est pas signalé, une image répétée
  toutes les 17 s).
- **Liste des écrans** : egui ne la donne pas. L'application fait tourner eframe sur **sa
  propre boucle winit** (`eframe::create_native`, enveloppée par `app/src/screens.rs`) et
  interroge le système au démarrage puis au plus une fois par seconde
  (`ActiveEventLoop::available_monitors`, liste tenue par winit : peu coûteux) ; l'UI la lit
  dans `vma_ui::Screens`. Nom de l'écran donné par le système (sinon « Écran n »), taille en
  pixels, fréquence ; l'index dans la liste est celui que prend `ViewportBuilder::with_monitor`.
  Liste journalisée au démarrage et à chaque changement (`screens=[…]`).
- **Fenêtre plein écran** (`fullscreen.rs`) : un *viewport* egui **différé** (sa fonction de
  dessin tourne à son propre rythme, même quand la fenêtre principale ne se redessine pas),
  ouvert tant que la sortie est en marche **et** son écran branché, plein écran sans bordure
  sur cet écran (`with_monitor` : `xdg_toplevel.set_fullscreen(output)` sous Wayland), sans
  barre des tâches, **sans prendre le focus** (le clavier reste à la fenêtre principale),
  titre « VMA — Program », identifiant d'application **`vma-program`** (règles du
  compositeur). Image du Program **après FTB** (lecteur `Readers::fullscreen`), proportions
  conservées, bandes noires, curseur masqué. Elle prend l'image la plus récente au début de
  chacune de ses frames (invariant ADR 0001 : sa frame précédente est soumise avant que sa
  fonction ne soit rappelée).
- **Fermeture** : par le système (Alt+F4, raccourci du compositeur) → `StopOutput`, journalisé.
  egui ne ferme la fenêtre que quand la fenêtre principale cesse de la montrer ; sur un seul
  écran, la fenêtre principale qu'elle couvre n'est plus redessinée (Wayland) : la fenêtre
  plein écran lit donc l'état elle-même et **se masque** dès que la sortie n'est plus en marche
  (et se réaffiche si elle repart avant d'avoir été fermée). Échap ne l'arrête **pas** (écran
  du public ; à confirmer, question 19 de la checklist d'observation).
- **Statistiques** : section plein écran (dès la première image affichée) — images du Program
  affichées et rafraîchissements de la fenêtre, images **sautées** (publiées par le rendu,
  jamais montrées : rouge si non nul), plus long écart entre deux rafraîchissements. Mêmes
  chiffres dans le rapport de soak (section « Plein écran (B12) », informative).
- **Ligne de commande** : `--fullscreen <écran>` (nom, ou numéro à partir de 1) ajoute et
  démarre la sortie au lancement (commandes `Script`, journalisées).

## Critères d'acceptation
| Critère | Test |
|---|---|
| Chaque clic, double-clic, bouton, menu, T-Bar et raccourci émet exactement la `Command` attendue (A11) | `crates/ui/tests/switcher.rs` (10 tests, `egui_kittest` piloté par AccessKit, `CommandSender::probe`) |
| Ajouter : caméra listée → `AddInput { Capture }` nommé ; Vidéo… / Image… → demande de fichier, aucune commande avant le choix | `switcher::add_menu_capture_devices_and_files` |
| GStreamer absent : avertissement, Caméra et Vidéo désactivés, Image possible | `switcher::without_gstreamer_capture_and_video_are_disabled` |
| Contrôles vidéo : Lecture / Pause selon l'état, Début, position (une `SetPosition`), temps affiché | `switcher::video_controls` |
| Raccourcis ignorés quand un champ texte a le focus | `switcher::double_click_opens_input_settings_and_typing_does_not_trigger_shortcuts` (vérifié : échoue si la protection est retirée) |
| L'UI reflète un nouvel état au plus une frame UI après sa publication | `switcher::ui_follows_the_new_state_on_the_next_frame` |
| Tally conforme, y compris pendant une transition et à la T-Bar | `tally::tests::red_green_and_both_red_during_a_transition` |
| Chaque widget a un nom accessible | contrôle automatique d'`egui_kittest` dans chaque test |
| *(Phase 2)* Enregistrer, Streamer, FTB, Sorties (Démarrer / Appliquer / Arrêter / Supprimer, dossier, destination ajoutée) émettent la bonne `Command` (A11) | `switcher::record_and_stream_buttons`, `ftb_button`, `output_settings_start_stop_and_remove`, `a_stream_destination_is_added_with_its_key_hidden` |
| *(Phase 2)* **B13** côté UI : la clé tapée n'apparaît ni en libellé ni en valeur | `switcher::a_stream_destination_is_added_with_its_key_hidden` |
| *(Phase 2)* **B15** côté UI : état, encodeur, durée, débit, taille, envoyées / perdues, file, reconnexions | `switcher::output_statistics_show_every_figure` |
| *(Phase 2, étape 8)* Plein écran : ajout sur le 2ᵉ écran, choix de l'écran, Démarrer (réglages d'abord), Arrêter ; avertissements écran absent, écran unique, fréquence | `switcher::fullscreen_output_screen_choice_start_and_stop`, `fullscreen_output_warns_about_its_screen`, `fullscreen::tests` (4 tests : écran par nom, saccades, images sautées, proportions) |
| *(Phase 2, étape 8)* **B12** : plein écran réel sur un écran secondaire | checklist `docs/manual-tests/phase2-outputs.md` |
| Utilisation réelle : chaque type d'input, Quick Play, Cut, 4 boutons, T-Bar, contrôles vidéo, à la souris et au clavier ; sélecteur de fichier natif (Linux, Windows) | checklist `docs/manual-tests/phase1-inputs.md` |
