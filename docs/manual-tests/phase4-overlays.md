# Checklist manuelle — Phase 4, overlays (étape 3)

Sur la machine de dev, dans l'application réelle (`cargo run --release`). Cocher, noter ce qui
diffère. Il faut une image PNG avec transparence (un lower third) et, si possible, une vidéo
avec alpha (WebM VP9 alpha ou ProRes 4444) ; les clips de `crates/io-gst/tests/data` sont
trop petits pour juger de l'aspect mais suffisent pour le comportement.

## Entrée et sortie
- [ ] Ajouter → Image… : le PNG transparent ; l'infobulle de sa vignette dit « RGBA avec alpha ».
- [ ] Bouton **1** de sa vignette : le lower third entre en fondu (0,5 s) **au Program
      seulement** ; la Preview ne le montre pas ; le bouton 1 reste enfoncé ; la vignette est
      rouge (tally).
- [ ] Le Program sous le lower third se voit à travers les parties transparentes, sans liseré
      sombre autour des lettres.
- [ ] Cut, puis Fade (boutons de transition) du Program : le lower third ne bouge pas.
- [ ] Bouton **1** à nouveau : il sort en fondu ; le tally s'éteint.
- [ ] Réglages → Overlays : overlay 1 en Cut ; entrée et sortie à l'instant.
- [ ] Un deuxième input (vidéo avec alpha, ou une couleur) en overlay **2** pendant que le 1 est
      présent : le 2 passe **au-dessus** du 1.
- [ ] Mettre un autre input dans le channel 1 occupé : l'ancien disparaît, le nouveau entre.

## Sorties
- [ ] Enregistrer quelques secondes avec un overlay : il est dans le fichier.
- [ ] FTB : les sorties passent au noir **overlays compris** ; la vue Program de l'UI les garde.
- [ ] Retirer l'input de la liste (×) pendant qu'il est en overlay : il disparaît, rien d'autre
      ne change.

## Notes
_À compléter : versions, écran, remarques._
