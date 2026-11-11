# Test manuel — Companion pilote VMA (Phase 5, E9)

**But** (acceptation BRIEF, Phase 5) : Bitfocus Companion pilote Cut / Fade / overlays par
l'API compat **sans modification de son module**. Le trafic exact du module est déjà rejoué
en CI (`crates/api/tests/compat_tcp.rs`, E8) ; ce test-ci le vérifie avec le vrai logiciel.

À faire sur la machine de dev, en build release, Companion sur la même machine (l'API écoute
sur 127.0.0.1, ADR 0012).

## Préparation
- [ ] Companion installé (site de Bitfocus). Version : `…`
- [ ] Module de la référence (dans Companion : « Add connection », chercher le nom de la
  référence). Version du module : `…` (le rejeu de la CI suit la v5.0.6)
- [ ] Lancer VMA avec quelques inputs :
  ```sh
  cargo run --release -- --inputs 4
  ```
  Au démarrage, le journal montre `compat TCP API listening bound=127.0.0.1:8099` (et 8088,
  8090).
- [ ] Dans Companion, la connexion : hôte `127.0.0.1`, port TCP `8099`, intervalle de
  sondage par défaut (250 ms). Elle passe au vert.

## Boutons (une page de Companion)
Créer un bouton par action, avec le retour (feedback) de tally quand il existe :
- [ ] Preview input 2 (retour : input 2 en Preview → vert)
- [ ] Send to Program input 3 (`CutDirect` ; retour : input 3 au Program → rouge)
- [ ] Cut ; Fade (Transition Mix, Fade, 1000 ms)
- [ ] Transition 1, 2, 3, 4 (les boutons de VMA)
- [ ] Quick Play input 4
- [ ] Overlay 1 : In (input 4), Out, Off ; Overlay All Off
- [ ] Fade To Black
- [ ] Start / Stop Recording
- [ ] T-Bar (Set Fader) à 128 puis 255

## Pendant 20 minutes
- [ ] Chaque bouton fait son effet dans VMA, au tick suivant (Cut) ou avec sa durée (Fade,
  boutons).
- [ ] Les couleurs des boutons de Companion suivent la Preview et le Program, y compris quand
  on agit dans VMA à la souris.
- [ ] Les variables de Companion (nom de l'input au Program, en Preview) sont justes.
- [ ] Le journal de VMA (`logs/commands-…jsonl`) montre ces commandes avec `"source":"api"`.
- [ ] Le journal de Companion (onglet Log) n'a ni « Unknown TCP message », ni « Message
  prefix issue », ni erreur de lecture du XML.
- [ ] Fermer VMA puis le relancer : Companion se reconnecte seul.

## Résultat
- Date, versions de Companion et du module :
- Ce qui ne marche pas (bouton, message du journal de Companion) :
