# Test manuel — soak hostile Phase 0 (ADR 0001, amendement 3)

**But** : 10 min à 50 fps, **0 image manquée, 0 image répétée**, pendant qu'on malmène la
fenêtre en continu (redimensionnement, minimisation, déplacement, plein écran). Le Program
est cadencé par son propre thread : un blocage de l'UI ne doit pas le toucher.

Le mode `--hostile` automatise une partie des agressions (cycle de 15 s : redimensionnement
toutes les 50 ms, maximiser, plein écran, minimiser / restaurer, déplacement toutes les 50 ms).
Le **glisser à la souris** ne s'automatise pas : c'est l'opérateur qui le fait.

À faire sur **chaque OS** (Linux puis Windows), sur la machine de dev, en build release.

## Préparation
- [ ] Profil d'alimentation « performances » (Windows) / gouverneur CPU par défaut (Linux).
- [ ] Fermer les applications lourdes (jeux, encodages, navigateur chargé).
- [ ] `git pull`, puis `cargo build --release`.

## Linux (Hyprland / Wayland)
Sous Wayland, un client ne peut ni se positionner ni se dé-minimiser : ces deux agressions
automatiques sont sans effet (un avertissement winit s'affiche), d'où les actions manuelles.
Une fenêtre en mosaïque ignore aussi les demandes de taille.

1. [ ] Lancer :
   ```sh
   cargo run --release -- --soak 600 --hostile \
     --report tests/soak/reports/$(date +%F)-phase0-hostile-linux.md
   ```
2. [ ] Passer la fenêtre VMA en flottant (`hyprctl dispatch togglefloating`, ou votre raccourci)
   pour que les redimensionnements automatiques s'appliquent.
3. [ ] Pendant les 10 minutes, en continu, par périodes d'environ 30 s :
   - [ ] déplacer la fenêtre à la souris (SUPER + clic gauche glissé) sans relâcher ;
   - [ ] redimensionner à la souris (SUPER + clic droit glissé) ;
   - [ ] changer d'espace de travail et revenir ;
   - [ ] repasser en mosaïque puis en flottant ;
   - [ ] mettre une autre fenêtre plein écran par-dessus quelques secondes.
4. [ ] Ne pas fermer la fenêtre : elle se ferme seule à la fin. Code de sortie attendu : `0`
   (`echo $?`). `2` = images manquées ou répétées.

## Windows
Le cas le plus dur : pendant un déplacement ou un redimensionnement à la souris, Windows
fait tourner sa propre boucle modale et le thread UI est gelé.

1. [ ] Lancer (PowerShell) :
   ```powershell
   cargo run --release -- --soak 600 --hostile `
     --report "tests/soak/reports/$(Get-Date -Format yyyy-MM-dd)-phase0-hostile-windows.md"
   ```
2. [ ] Pendant les 10 minutes, en plus des agressions automatiques :
   - [ ] **maintenir** la barre de titre et déplacer la fenêtre en continu pendant ≥ 30 s,
     plusieurs fois (boucle modale de déplacement) ;
   - [ ] redimensionner par un bord en continu pendant ≥ 30 s ;
   - [ ] minimiser / restaurer par la barre des tâches, Win + ↓ / Win + ↑ ;
   - [ ] déplacer la fenêtre d'un écran à l'autre si un second écran est branché.
3. [ ] Code de sortie attendu : `0` (`$LASTEXITCODE`).

## Après chaque passage
- [ ] Ouvrir le rapport généré ; vérifier : **Images manquées = 0**, **Images répétées = 0**,
  GPU perdu = non, Ticks ≈ attendu (± 2).
- [ ] Compléter la section « Actions manuelles » du rapport (ce qui a été fait, combien de
  temps, observations : saccades visibles, avertissements dans le terminal).
- [ ] Noter le « Plus long blocage UI » : il montre que l'UI a bien été gelée, sans effet sur
  le Program.
- [ ] Commiter le rapport (`docs(soak): add Phase 0 hostile soak report (<os>)`).

## En cas d'échec
Garder le rapport et la sortie du terminal (avertissements `frame clock missed slots`,
`GPU backpressure`). Indiquer à quel moment c'est arrivé (quelle action). Pistes déjà connues
(ADR 0001) : priorité du thread de rendu, contention sur la `Queue` partagée, marge d'attente
active (`--spin-margin-us 2000` pour comparer).
