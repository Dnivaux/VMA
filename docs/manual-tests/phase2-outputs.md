# Test manuel — Phase 2 : sorties

**But** : vérifier sur du vrai matériel ce que la CI ne voit pas. Complété au fil des étapes de
la Phase 2 ; pour l'instant, **B12** (sortie plein écran, étape 8).
**Matériel** : la machine de dev **avec un deuxième écran** (sans lui, seule la partie
« un seul écran » est possible). Windows : ⏸ (aucun test possible pour l'instant).

Cocher chaque case ; noter tout ce qui surprend sous « Remarques ».

## B12 — plein écran sur un écran secondaire

### Préparation
- [ ] Brancher le deuxième écran. Si possible, le régler à la cadence maître (50 Hz pour du
      50p, ou 100 Hz) : un écran à 60 Hz montre du 50p avec une image répétée sur cinq.
- [ ] Lancer `cargo run --release -- --inputs 4` ; le journal affiche
      `screens=["…", "…"]` : les deux écrans, avec leur taille et leur fréquence.

### Depuis l'interface
- [ ] Sorties → « + Plein écran » : une carte « Plein écran » apparaît, l'écran proposé est le
      **deuxième** (pas celui de la fenêtre principale ; sinon le choisir dans « Écran »).
- [ ] Avertissement « saccades » présent si la fréquence de l'écran n'est pas un multiple de
      la cadence maître, absent sinon.
- [ ] **Démarrer** : le Program s'affiche **en plein écran sur le deuxième écran**, sans
      bordure, proportions respectées (bandes noires si l'écran n'est pas en 16:9), sans
      curseur. La fenêtre principale **garde le clavier** (les raccourcis marchent sans
      cliquer).
- [ ] Cut, Fade, T-Bar : l'écran suit le Program, sans saccade visible (écran réglé à la
      cadence maître) ni déchirure.
- [ ] **FTB** : l'écran passe au noir avec le fondu réglé, **la vue Program de l'interface
      ne change pas** ; second FTB : retour.
- [ ] L'interface reste fluide (panneau Statistiques : « Rafraîchissement UI » proche de la
      fréquence de l'écran principal), « Images manquées (horloge) » reste à 0.
- [ ] Statistiques, section plein écran : images affichées ≈ cadence maître par seconde,
      **0 image sautée** (écran à la cadence maître ou plus).
- [ ] **Minimiser la fenêtre principale** : le plein écran **continue** de bouger. La
      rétablir.
- [ ] **Arrêter** (carte Plein écran) : la fenêtre plein écran disparaît. **Démarrer** de
      nouveau : elle revient sur le bon écran.
- [ ] Fermer la fenêtre plein écran par le compositeur (raccourci de fermeture, en la visant) :
      elle disparaît, la carte repasse « à l'arrêt », le journal contient `StopOutput`.
- [ ] Changer d'écran dans la carte (à l'arrêt), Démarrer : le Program va sur l'autre écran.
- [ ] **Débrancher** le deuxième écran pendant que la sortie tourne : la fenêtre plein écran
      disparaît (et ne vient **pas** couvrir la fenêtre principale), la carte affiche « Écran
      « … » non branché : la sortie n'est pas affichée ». **Rebrancher** : elle revient d'elle-même
      sur cet écran.

### Depuis la ligne de commande (rapport)
- [ ] `cargo run --release -- --soak 600 --inputs 4 --fullscreen 2 --report tests/soak/reports/$(date +%F)-phase2-b12-linux.md`
      (`2` = deuxième écran de la liste, ou son nom) : verdict **RÉUSSI**, section « Plein écran
      (B12) » : rafraîchissements ≈ fréquence de l'écran, images affichées ≈ cadence maître,
      0 sautée ; section « UI » : rafraîchissement moyen proche de celui de l'écran principal.
      Commiter le rapport.

### Un seul écran (si pas de deuxième écran)
- [ ] La carte affiche « Un seul écran : la sortie couvrira cette fenêtre ».
- [ ] Démarrer : le Program couvre l'écran. Le fermer par le raccourci du compositeur : la
      fenêtre principale revient, la sortie est « à l'arrêt ».

### Si la fenêtre ne passe pas en plein écran (Wayland)
Le compositeur peut refuser la demande (constaté avec Hyprland quand une autre fenêtre était
déjà en plein écran sur le même espace de travail : « VMA — Program » juxtaposée à la fenêtre
principale). Noter le compositeur et sa version sous « Remarques », puis essayer une règle sur
l'identifiant d'application `vma-program` (par exemple, sous Hyprland : plein écran et écran
voulu pour la classe `vma-program`).

## Remarques
_À compléter._
