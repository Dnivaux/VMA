# Test manuel — Phase 3 : son (périphériques audio, casque, mélangeur)

**But** : vérifier sur du vrai matériel ce que la CI ne voit pas (faux périphériques en CI) :
**C12** (micro / ligne, casque, solo audible) et l'usage réel du mélangeur.
**Matériel** : la machine de dev, un micro USB (le « Nor-Tec streaming mic » convient), un
casque ou des haut-parleurs. Windows : ⏸ (aucun test possible pour l'instant).

Cocher chaque case ; noter tout ce qui surprend sous « Remarques ».

## Préparation
- [x] Lancer `cargo run --release` (sans `--soak` ni `--no-headphones` : le casque joue).
- [x] Le journal liste les périphériques audio (`audio device kind=source …`,
      `kind=sink …`), une ligne par périphérique (pas de doublon PulseAudio / ALSA).

## Micro / entrée ligne
- [x] Ajouter → Son (micro, ligne) : le micro est listé sous son nom ; le choisir. Un input
      « n Nom du micro » apparaît, vignette « Son seul », **sans** QP ni Cut ; il n'est mis ni
      au Program ni en Preview.
- [x] Sa tranche dans le Mélangeur : **Muet** allumé, « Suit l'image » décoché (pas de son
      surprise à l'antenne). Parler : son vumètre reste à zéro tant qu'il est muet (vumètre
      après fader).
- [x] Désactiver Muet : le vumètre suit la voix, le Master aussi ; le casque entend la voix
      (avec un léger retard, ≈ 100 ms : normal). *(voir Remarques 1)*
- [x] Fader : de −∞ à +10 dB, le niveau suit ; double-clic → 0 dB.
- [x] **Débrancher** le micro : en 2 s, la vignette passe à « Signal perdu (tentative n) »,
      le vumètre tombe à zéro, rien d'autre ne bouge (Program, sorties).
- [x] **Rebrancher** (même port ou un autre) : il revient seul en quelques secondes, avec
      ses réglages (fader, muet…).
- [x] Enregistrer quelques secondes en parlant : le fichier contient la voix.

## Casque
- [x] Tranche Casque, liste **Sortie** : « Par défaut » et les sorties audio de la machine.
      Choisir le casque : le son y passe.
- [x] Choisir une autre sortie, puis revenir : le son suit, sans bloquer l'interface.
- [x] Débrancher la sortie choisie (si elle est USB) : « Casque muet » (raison au survol) ;
      la rebrancher : le son revient seul.

## Solo (C5, C12 « solo audible ») *(voir Remarques 2)*
- [x] Avec deux sources (micro + une vidéo avec son au Program) : **Solo** sur le micro →
      le casque n'entend plus que le micro, l'étiquette passe à « Solo » ; le **Master** (son
      vumètre, un enregistrement) ne change pas.
- [x] Deux solos : le casque entend la somme des deux.
- [ ] AFL (défaut) : un input en solo **muet** est silencieux au casque ; **PFL** : on
      l'entend (avant fader), toujours absent du Master.

## Décalage et synchro (ressenti) — non passé : pas de caméra
- [ ] Une caméra (ou une vidéo) et le micro : le son du micro arrive environ une image avant
      l'image ; régler le **décalage** du micro (ms) jusqu'à ce que lèvres et voix coïncident.
      Noter la valeur trouvée : __________ ms.

## Remarques
1. *(2026-11-03, première passe)* Micro reconnu, perdu et retrouvé au débranchement, mais
   **aucun son**, quel que soit l'input : vumètre immobile une fois le Muet désactivé, rien à
   enregistrer, casque impossible à tester. Cause : l'application ne donnait pas son horloge
   de flux au moteur (défaut présent depuis l'étape 3, que les tests ne voyaient pas : ils la
   donnent eux-mêmes). Corrigé (`6e599cf`) ; seconde passe : micro, fader, enregistrement,
   casque et solo validés.
2. Pas de vidéo avec du son adaptée sous la main pour le test du solo. AFL / PFL non coché.
3. Machine : Arch Linux (PipeWire), micro USB Nor-Tec, sorties USB.
