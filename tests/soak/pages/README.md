# Pages du soak D11 (Phase 4)

Pages HTML mises en overlay par `tests/soak/phase4-d11.sh`. Toutes sont **claires** (blanc,
gris clair, translucides ou non) : l'analyseur de synchro ne reconnaît un flash que si aucun
pixel de l'image réduite n'est sous 170 de luma ; un overlay sombre empêcherait la mesure.
Chacune change à chaque `requestAnimationFrame` (une image par tick : D5 sur toute la durée).

- `bar.html` : une barre qui traverse le bas de l'image ;
- `clock.html` : une horloge au centième, en haut à droite ;
- `score.html` : un score mis à jour par les données (`vma-data`, une fois par seconde).
