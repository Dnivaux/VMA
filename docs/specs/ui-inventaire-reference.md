# Interface de la référence : inventaire d'après sa documentation publique

Base du chantier « Interface familière » (`ROADMAP.md`). Source unique : le **guide utilisateur
public** de la référence (PDF de 286 pages, téléchargé depuis son site le 2026-11-11), plus deux
pages d'aide en ligne de la même documentation. Les numéros de page renvoient à ce PDF.
Clean room (BRIEF §2) : on décrit des **zones, fonctions et gestes** avec nos mots ; aucune
capture, aucune icône, aucun libellé repris tel quel. Le guide montre beaucoup de choses en
images seulement : ce qui n'est pas dans son texte est marqué « ? » et passe dans la checklist
de la partenaire ([`observation-reference-interface.md`](../manual-tests/observation-reference-interface.md)).

Colonne **VMA** : l'UI après la passe de disposition et les icônes (2026-11-11,
`crates/ui/src/switcher.rs` ; spec [`ui.md`](ui.md), « Interface familière »). **Phase** : ce que BRIEF
place plus tard ; le chantier ne fait que la disposition et les gestes de ce qui existe déjà.

## 1. Fenêtre principale : les zones
| Zone (référence) | Où | Contenu, gestes (p.) | VMA aujourd'hui | Écart |
|---|---|---|---|---|
| Preview | en haut à gauche | titre de couleur (orange par défaut, réglable) ; commandes vidéo dessous : position, retour au début, lecture / pause (p. 11, 17) ; zones de sécurité par une icône du titre (p. 269) ; clic sur la Preview = Quick Play si l'option est cochée (p. 41) | en haut à gauche, commandes vidéo dessous | zones de sécurité, clic = Quick Play |
| Program (« Output ») | en haut à droite | titre de couleur (vert par défaut, réglable) ; mêmes commandes vidéo (p. 11, 17) ; titre « Preview » / « Output » ou le nom de l'input, au choix (p. 39) | en haut à droite | nom de l'input dans le titre (option) |
| Colonne des transitions | au centre, entre les deux | Quick Play, Cut, 4 boutons de transition (une flèche à côté de chacun règle effet et durée), FTB, T-Bar **sous** le FTB, puis les boutons des overlay channels 1–4 (montrent les channels actifs ; un clic rentre le dernier input du channel), puis 2 horloges de production (désactivées par défaut) (p. 20–25, 30 ; forum de la référence) | Quick Play, Cut, 4 boutons (petite flèche à côté, ou clic droit, pour régler), FTB, T-Bar, boutons des overlay channels 1–4 sous la T-Bar | horloges (hors périmètre) |
| Barre des inputs | en bas, sur toute la largeur gauche | une tuile par input, vignette en direct ; plusieurs rangées (le séparateur se tire ; jusqu'à 3 en 1080p), barre de défilement au-delà (p. 11, 52–53) | plusieurs rangées en bas à gauche (le séparateur se tire), défilement au-delà ; taille des vignettes dans Réglages → Affichage | — |
| Catégories | à gauche de la barre des inputs | « Tout », 6 couleurs + 16 catégories nommées, recherche par titre ; on y glisse la vignette d'un input ; trait de couleur sous la catégorie active (p. 26–28) | — | à faire (petit) |
| Onglets de droite | bord droit | ancrent le mélangeur audio (et le replay) ; le mélangeur se détache dans sa fenêtre (épingle) (p. 11, 97) | mélangeur en bas à droite, à côté des inputs, ouvert par « Mélangeur » (barre du bas, à droite) | ? emplacement exact (checklist) ; détacher |
| Barre de presets | en haut à gauche | Nouveau, Ouvrir (+ récents), Enregistrer, Enregistrer sous (+ paquet avec les médias), Dernier (sauvegarde auto) ; Annuler apparaît à côté après la fermeture d'un input (p. 13–16, 270) | — | **Phase 5** (projet) ; Annuler : petit |
| Réglages | en haut à droite | un bouton ouvre la fenêtre de réglages (p. 11) | « Réglages » en haut à droite | — |
| Barre du bas | en bas | « Ajouter un input » à gauche ; puis Enregistrer, Stream, Sortie externe, MultiCorder, PlayList, Plein écran : **un clic démarre / arrête, un rouage à côté règle**, la flèche du Plein écran choisit sa source (p. 11, 18, 121, 124, 207, 209) ; durée et images perdues à côté d'Enregistrer (p. 121) | « Ajouter un input » à gauche ; Enregistrer (durée à côté), Streamer, Plein écran : un clic démarre / arrête, un rouage à côté règle ; Sorties | Sortie externe, MultiCorder, PlayList (fonctions à venir) ; choix de la source du plein écran |
| Coin en bas à droite | en bas à droite | statistiques (icône), cadenas, réglages des overlays, instantané du Program (p. 32, 215, 217–218) | Statistiques et Mélangeur en bas à droite ; réglages des overlays dans Réglages | cadenas, instantané à faire (petits) ; réglages des overlays dans ce coin |
| Mode avancé | ? | un bouton « Avancé » montre plus de commandes (le rouage des inputs, notamment) ; option « démarrer en mode avancé » (p. 39, 78) | tout est toujours visible | ? ce qu'il cache (checklist) |

## 2. Tuile d'un input
| Élément (référence) | Geste (p.) | VMA | Écart |
|---|---|---|---|
| Titre | double-clic : réglages de l'input ; clic droit : replier la tuile (p. 52) | en haut de la tuile ; double-clic : réglages | replier |
| Vignette | clic : en Preview (action réglable par input) ; double-clic : réglages ; glisser sur une autre tuile : réordonner ; glisser sur une catégorie : la changer ; clic droit : menu propre au type (p. 52, 78–79, 34) | clic : en Preview ; double-clic : réglages | réordonner, menu clic droit |
| Fermer | retire l'input (p. 76) ; annulable (p. 270) ; bloqué par le cadenas (p. 218) | × | Annuler, cadenas |
| Quick Play, Cut | au Program en fondu / sans transition, la vidéo démarre (p. 76) | QP, Cut | — |
| Loop | boucle (p. 76) | Loop (vidéos) | — |
| 1 2 3 4 | overlay au Program ; re-clic : sortie (transition à l'envers) ; **clic droit : overlay sur la Preview**, envoyé au Program à la transition suivante ; clic droit en PIP : plein cadre (p. 76) | 1 2 3 4 au Program | overlay en Preview (fonction nouvelle, à décider) |
| Son | muet / pas muet (p. 76) | bouton haut-parleur sur la tuile (rouge et barré si muet), et dans le mélangeur | — |
| Grand aperçu (icône écran) | grand aperçu, clic pour fermer (p. 77) | — | petit |
| Rouage | réglages (mode avancé) ; clic droit : instantané de l'input, input virtuel (p. 35, 77, 217) | — | rouage ; le reste plus tard |
| Pause live | fige une caméra / NDI sans couper le flux (p. 77) | — | petit (moteur) |
| Tally | ? couleurs de la tuile au Program / en Preview / en overlay | rouge Program, vert Preview, rouge overlay | ? (checklist) |

## 3. Réglages d'un input (fenêtre)
Onglets (p. 78) : Général (changer la source en gardant les réglages, nom, action du clic,
rapport d'image, catégorie, désentrelacement, netteté, miroir, son automatique,
lecture / redémarrage / pause avec la transition, input virtuel), Couleur (noirs, blancs, R V B,
alpha), Incrustation couleur / chroma, Correction colorimétrique, Effets, Position (zoom,
recadrage, rotation), Calques / MultiView, Déclencheurs, Tally, PTZ, Avancé, Copier depuis.
VMA : nom, boucle, réglages HTML / SRT / NDI. **Hors chantier** : seuls l'ordre et les noms des
onglets qui existeront serviront de repère ; incrustation, position, calques, etc. sont des
fonctions à part (phases suivantes, à décider).

## 4. Mélangeur audio
Référence (p. 97–100) : une tranche par input qui a du son : fader, vumètre G / D, muet
(haut-parleur, coupe **tous** les bus), solo S (casque, AFL par défaut, PFL en option ; clic
droit : plusieurs solos), son qui suit la vidéo (bouton à deux flèches), bus M / A / B sous la
tranche (C–G par clic droit sur A ou B), rouage → réglages audio de l'input (gain, EQ,
compresseur, porte de bruit, volume casque par input, canaux, vumètres avant / après effets).
Section des sorties : Master, A, B, bouton rotatif du casque. Tranches repliables (clic droit
sur le titre), mélangeur détachable.
VMA (Phase 3) : fader, vumètres, muet, solo AFL / PFL, suivi de la vidéo, envois Master / A / B,
casque. Écarts : disposition à vérifier, gain / EQ / compresseur absents (fonctions, hors
chantier), détacher.

## 5. Raccourcis clavier
La référence **n'a plus de raccourcis par défaut** : tout se crée dans Réglages → Raccourcis
(touche, MIDI, X-Keys, Stream Deck, contrôleur web), un modèle « clavier par défaut » reprend
ceux d'anciennes versions, liste non publiée ; raccourcis globaux ou propres au preset ;
plusieurs fonctions sur une touche (p. 142–147). Les fonctions d'un raccourci : bouton de
transition 1–4, Cut, Quick Play, FTB, T-Bar, stingers, overlays 1–4 (entrée, sortie,
tout couper), enregistrement, stream, plein écran, son… (p. 229 et suivantes).
VMA : 1–9 → Preview, Espace → Cut, Entrée → bouton 1 (choix de VMA, Phase 1).
**Conséquence** : les raccourcis de la partenaire sont **les siens** ; il faut leur liste
(checklist), et VMA doit pouvoir les reproduire (éditeur de raccourcis : Phase 5, avec
l'API / les contrôleurs, ou plus tôt si elle en dépend).

## 6. Réglages généraux (fenêtre)
Onglets (p. 36) : Import / export, Affichage (thème, couleurs Preview / Program, cadence et
taille maître, écran du plein écran, **taille des vignettes**), Sorties / NDI (source de chaque
sortie, overlays par sortie = « clean feed », MultiView), Options (démarrages, confirmation
avant d'arrêter un enregistrement, lecture / redémarrage / pause auto avec la transition,
**masquer** Quick Play / transitions 2–4 / FTB / boutons d'overlay / T-Bar, Quick Play :
effet et durée 500 ms, clic sur la Preview = Quick Play), Performances, Décodeurs,
Enregistrement, Sortie externe, Audio, Sorties audio, Contrôleur web, Tally, Raccourcis,
Scripts, Alertes, À propos (p. 36–49).
VMA : Réglages (transitions, Quick Play, overlays, stingers, options de lecture), Sorties.
Écarts utiles au chantier : taille des vignettes (faite, Réglages → Affichage) ; masquer des
éléments de la colonne, confirmation avant d'arrêter un enregistrement / un stream (à faire).

## 7. Fonctions absentes de VMA (hors chantier)
Pour mémoire, avec leur phase BRIEF : presets / projet, MultiView (Phase 5) ; enregistrement
ISO (MultiCorder), replay (Phase 6) ; PlayList, titres natifs, calques / PIP, incrustation
chroma, PTZ, sources de données, appels distants, scripts, contrôleur web (Phase 5 pour l'API,
le reste Phase 7 ou à décider). Le chantier leur **réserve la place** (même zone que chez la
référence) sans les faire.

## 8. Ce que le chantier peut faire sans nouvelle fonction
État au 2026-11-11 (passe de disposition) : 1, 3 et 5 faits ; 2 fait sauf les réglages des
overlays ; 4 : bouton son fait, repli et glisser à faire ; 6 attend la checklist.

1. Barre du bas : « Ajouter un input », puis Enregistrer / Streamer / Plein écran (+ Sorties)
   en bouton qui démarre + rouage qui règle, durée à côté.
2. Réglages en haut à droite ; statistiques et réglages des overlays en bas à droite.
3. Colonne centrale : flèche de réglage sur chaque bouton de transition (le clic droit reste),
   boutons d'overlay channels 1–4 sous la T-Bar.
4. Tuile : bouton son, ordre des boutons, repli, glisser pour réordonner.
5. Barre d'inputs sur plusieurs rangées, taille des vignettes réglable.
6. Mélangeur : emplacement et détachement d'après la checklist.
Le reste (catégories, cadenas, annuler, grand aperçu, instantané, pause live, overlay en
Preview) : petites fonctions à proposer une par une, pas dans la passe de disposition.
