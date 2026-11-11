# Checklist manuelle — Phase 4, overlays (étape 3)

Sur la machine de dev, dans l'application réelle (`cargo run --release`). Cocher, noter ce qui
diffère. Une image PNG avec transparence est fournie :
[`images/lower-third.png`](images/lower-third.png) (1920 × 1080 : une ligne de texte posée
directement sur la transparence, un bandeau à 60 % avec du texte lissé). Si possible, une
vidéo avec alpha (WebM VP9 alpha ou ProRes 4444) ; les clips de `crates/io-gst/tests/data`
sont trop petits pour juger de l'aspect mais suffisent pour le comportement.

**Préparation** (ajoutée le 2026-11-10, après le premier passage) : il faut **deux inputs
ordinaires différents** sous l'overlay, sinon un Cut ne change rien de visible et rien ne se
voit à travers le PNG. Ajouter → Couleur → **Rouge**, puis Ajouter → Couleur → **Bleu** ;
mettre le Rouge au Program et le Bleu en Preview. Le PNG s'ajoute ensuite comme troisième
input ; il ne va **pas** au Program, seulement en overlay (boutons 1–4 de sa vignette).

## Entrée et sortie
- [x] Ajouter → Image… : le PNG transparent ; l'infobulle de sa vignette dit « RGBA avec alpha ».
- [x] Bouton **1** de sa vignette : le lower third entre en fondu (0,5 s) **au Program
      seulement** ; la Preview ne le montre pas ; le bouton 1 reste enfoncé ; la vignette est
      rouge (tally).
- [x] Le Program (rouge) sous le lower third se voit à travers le bandeau et autour du texte.
      Regarder de près les bords des lettres de « Hors bandeau : bords sans liseré ? » : ils
      passent du blanc au rouge **sans trait sombre** autour (un fin contour gris ou noir =
      défaut). Premier passage : « Je ne trouve pas » — sans doute faute d'un input
      différent sous l'overlay (voir Préparation).
- [x] Cut, puis Fade (boutons de transition) du Program : le lower third ne bouge pas.
      Premier passage : « ça ne fait rien quand je clique », la même image étant en Preview et
      au Program ; à refaire avec Rouge / Bleu : le fond passe du rouge au bleu, le lower
      third reste en place, sans clignoter.
- [x] Bouton **1** de la vignette du PNG à nouveau (infobulle « Retirer de l'overlay 1 ») : il
      sort en fondu ; le bouton 1 n'est plus enfoncé ; le tally rouge de la vignette s'éteint.
      Premier passage : « je ne comprends pas, j'ai dû mal faire » — noter ce qui se passe
      (rien, il ressort puis rentre, autre ?). ca marche ça s'affiche avec un fondu
- [x] Réglages → Overlays : overlay 1 en Cut ; entrée et sortie à l'instant.
- [x] Un deuxième input (vidéo avec alpha, ou une couleur) en overlay **2** pendant que le 1 est
      présent : le 2 passe **au-dessus** du 1.
- [x] Mettre un autre input dans le channel 1 occupé : l'ancien disparaît, le nouveau entre.

## Sorties
- [x] Enregistrer quelques secondes avec un overlay : il est dans le fichier.
- [x] FTB : les sorties passent au noir **overlays compris** ; la vue Program de l'UI les garde.
      Oui, il faut une sortie pour le voir (la vue Program de l'UI n'est volontairement pas
      concernée par le FTB) : lancer **Enregistrer**, mettre un overlay, cliquer **FTB**,
      attendre 2 s, re-cliquer FTB, arrêter l'enregistrement ; à la relecture, l'image *et*
      l'overlay fondent au noir puis reviennent. ca fait fondu au noir
- [x] Retirer l'input de la liste (×) pendant qu'il est en overlay : il disparaît, rien d'autre
      ne change.

## Input HTML (étape 5)
Compiler `vma-html` une fois (télécharge CEF, ≈ 300 Mo) :
`CEF_PATH=~/.cache/vma-cef cargo build --release -p vma-io-html --features cef --bin vma-html`,
puis `cargo run --release -- --html docs/manual-tests/html/lower-third.html` (ou une page à
toi : `--html https://…`).
- [x] La vignette de l'input « lower-third » montre le bas de titre sur fond noir (vignette) ;
      l'infobulle dit « BGRA avec alpha ».
- [x] Bouton **1** : le bas de titre et l'horloge en haut à droite apparaissent sur le Program,
      le Program se voit autour et à travers le bandeau semi-transparent, sans liseré.
- [x] L'horloge défile régulièrement (centièmes), sans à-coup visible.
- [x] Sans `vma-html` (renommé) : l'input est « signal perdu » avec la raison
      « vma-html introuvable… », le reste de VMA fonctionne.
- [x] Pendant que la page est en overlay, tuer le processus de la page **dans un autre
      terminal**, VMA restant ouvert :
      `pgrep -af vma-connect` doit afficher une ligne `…/vma-html --vma-connect=127.0.0.1:…` ;
      puis `pkill -9 -f vma-connect`. L'input est perdu une à quelques secondes puis revient
      tout seul (nouvelle ligne dans `pgrep -af vma-connect`) ; le Program ne saute pas.
      Premier passage : « pas réussi » — noter ce que répondent les deux commandes.

## Notes
_À compléter : versions, écran, remarques._
- 2026-11-10, premier passage (mainteneur) : 11 points sur 16 cochés ; les 5 autres sont
  précisés ci-dessus, à refaire.
- 2026-11-11, second passage (mainteneur) : les 5 points refaits, tous ✓ (sortie au bouton 1
  en fondu, FTB vu sur l'enregistrement) : **16 / 16**, aucun défaut relevé.
