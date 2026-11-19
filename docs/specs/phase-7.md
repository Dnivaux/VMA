# Phase 7 — Caméras PTZ, invités à distance, automatisation : spec d'ensemble et plan

- **Statut** : **proposé** le 2026-11-19, à valider (questions du § 7)
- **Date** : 2026-11-19
- **Références** : BRIEF §4 (« Phase 7 et au-delà », hors MVP, sur demande : demandée par le
  mainteneur le 2026-11-19) et §1 (régie automatisée par IA pour le sport, le handball
  d'abord) ; ADR 0003 (couche de commandes), ADR 0004 (inputs GStreamer), ADR 0007 (audio),
  ADR 0012 (API) ; [`api-native.md`](api-native.md), [`api-compat.md`](api-compat.md) ; guide
  public de la référence (PTZ, appels à distance), lu pour les gestes seulement
- **Entrée en phase** : restent ouverts la Phase 5 (E9 : Companion réel ; raccourcis : liste
  de la partenaire) et la revue de la Phase 6. Les essais avec de vraies caméras se feront à
  la fin (décision du mainteneur, 2026-11-19).

## 1. Périmètre

Le BRIEF place trois sujets après le MVP. Ils sont indépendants et de taille très
différente. **Recommandé : trois sous-phases, chacune avec sa revue**, dans cet ordre
(question Q1) :

| Sous-phase | Sujet | Pourquoi à cette place |
|---|---|---|
| **7A** | **Caméras PTZ** (VISCA over IP) | Le plus petit et le mieux défini (protocole public, aucune dépendance) ; utile au sport ; il donne à l'automatisation le cadrage qu'elle pilotera |
| **7B** | **Invités à distance** (WebRTC) | Gros morceau : réseau, NAT, chiffrement, navigateur ; il faut d'abord trancher l'accès depuis Internet (Q8) |
| **7C** | **Automatisation** (plugins) | La couche de commandes existe déjà ; il manque aux plugins des images et des niveaux à analyser, et un garde-fou pour l'opérateur |

### 7A — Caméras PTZ
- **Protocole** : VISCA over IP (UDP 52381, en-tête de 8 octets, numéros de séquence, accusé
  de réception puis fin de commande), d'après sa documentation publique. Variante « VISCA
  brut » sur TCP (port 5678 ou réglable) pour les caméras qui ne parlent que celle-là (Q3).
- **Gestes** (ceux de la référence, dessin propre à VMA) :
  - Une caméra PTZ est un input de capture, NDI® ou réseau auquel on associe une adresse
    PTZ (réglages de l'input, onglet « PTZ »).
  - Panneau de contrôle : pavé directionnel avec vitesse, zoom, mise au point (auto /
    manuelle), retour à la position d'origine.
  - **Préréglages** : enregistrer la position courante, la rappeler.
  - **Input virtuel par préréglage** : un input qui montre l'image de la caméra et qui, mis en
    Preview, envoie la caméra sur son préréglage. On prépare un plan en Preview pendant que
    l'autre est à l'antenne.
  - Si l'input de la caméra est **à l'antenne**, rappeler un préréglage demande une
    confirmation (Q5).
- **Commandes** : tout passe par `Command` (journal, rejeu, API native) ; fonctions PTZ de
  l'API compat, à vérifier dans la documentation publique ; Companion les pilote.
- **Pannes** : une caméra qui ne répond pas est signalée (tranche d'input, Statistiques),
  avec une reprise 1 / 2 / 5 / 10 s ; l'image de l'input n'est pas touchée.

### 7B — Invités à distance
- **Standards** : l'invité envoie sa caméra et son micro en WebRTC par **WHIP** (RFC 9725) ;
  il reçoit le **retour** (Program ou une autre vue, et un son *mix-minus* : le Master sans sa
  propre voix) par **WHEP**.
  - Côté invité, une **page web servie par VMA** (aucune installation), ou n'importe quel
    client WHIP (logiciel d'émission récent).
- **Dans VMA** : un invité est un input (image et son, synchro comme les autres inputs live,
  calé sur les ticks). La référence propose plusieurs invités : jusqu'à 4 dans la
  sous-phase (Q7).
- **Lien d'invitation** : un lien par invité, avec un jeton à usage unique ; on le révoque d'un
  clic.
- **Accès depuis Internet** : c'est la vraie difficulté (Q8). Un navigateur n'ouvre la caméra
  que sur une page sécurisée (HTTPS), et les NAT demandent STUN, voire TURN.

### 7C — Automatisation
- **Plugins = programmes à part** : ils parlent à l'API native, comme une IA le fera
  (commandes, état, événements). Ils ne sont jamais chargés dans le processus : un plugin qui
  tombe ne fait pas tomber VMA.
- **Ce qui leur manque** :
  - des **images à analyser** : vignettes en basse résolution des inputs et du Program, en
    mémoire partagée, au rythme choisi ;
  - des **niveaux audio** par input dans les événements ;
  - une **commande d'origine « automate »**, journalisée comme telle.
- **Garde-fou** : un bouton « Automate » (marche / arrêt) dans l'UI. Un geste de
  l'opérateur coupe l'automate pour un temps réglable : l'opérateur garde toujours la main
  (Q10).
- **Automate d'exemple fourni** : le **réalisateur au son**, qui met à l'antenne la caméra
  liée au micro qui parle (podcast). Il a une hystérésis, des durées de plan minimum et
  maximum, et une vue large quand plusieurs personnes parlent. C'est un algorithme simple,
  sans IA, et il sert de référence de plugin.
- **Hors 7C** : la détection d'actions de jeu (handball) par apprentissage, qui est un projet
  à part. 7C lui prépare l'accès aux images et aux commandes.

### Hors Phase 7
Contrôle PTZ par NDI® ou par d'autres protocoles propriétaires ; suivi automatique de sujet
par la caméra ; plus de 4 invités ; un serveur relais hébergé par le projet ; IA de
détection d'actions.

## 2. Architecture
- **7A** : crate `vma-ptz` (sans GStreamer) :
  - codage et décodage VISCA, sans allocation par commande ;
  - un client par caméra sur un thread `vma-ptz` (UDP / TCP non bloquant, délais, reprise),
    piloté par `vma-control` comme `vma-io`.
  - L'état PTZ (adresse, préréglages, joignable ou non) vit dans `State` et dans le projet
    (ADR 0013). Les inputs virtuels sont des inputs ordinaires dont le feed est celui de la
    caméra, sans second pipeline.
- **7B** : `vma-io-gst` :
  - `webrtcbin` (gst-plugins-bad, LGPL, lié dynamiquement) ou `whipserversrc` /
    `whepserversink` de gst-plugins-rs (MPL-2.0), à trancher par un ADR après un essai ;
  - signalisation WHIP / WHEP sur le runtime de `vma-api` (axum, déjà là) ; page de
    l'invité embarquée ; HTTPS par un certificat fourni ou par un tunnel (Q8).
  - Le *mix-minus* est un bus de `vma-audio` par invité (ADR 0007, amendement).
- **7C** :
  - les vignettes de l'atlas (déjà rendues pour l'UI et la MultiView) sont relues à basse
    cadence vers une mémoire partagée, sur le même modèle que `vma-html` ;
  - l'origine `Automate` s'ajoute à `Origin` (journal) ;
  - les niveaux audio sont publiés par `/events`. Le réalisateur au son est un binaire
    `vma-auto` qui n'utilise que l'API publique.

## 3. Critères d'acceptation
| # | Critère | Où |
|---|---|---|
| G1 | VISCA : chaque commande (mouvements, zoom, mise au point, préréglages, retour) codée à l'octet près ; numéros de séquence, accusé puis fin, erreur de la caméra, délai sans réponse, remise à zéro de séquence | CI (caméra simulée en UDP et TCP) |
| G2 | Une caméra injoignable est signalée, reprise 1 / 2 / 5 / 10 s ; le Program n'en souffre pas (0 image manquée) | CI |
| G3 | Input virtuel : mis en Preview, la caméra reçoit le rappel de son préréglage **une fois** ; à l'antenne, rien n'est envoyé sans confirmation | CI |
| G4 | UI PTZ (pavé, vitesse, zoom, préréglages, inputs virtuels) : chaque geste émet sa commande | CI (egui_kittest) |
| G5 | API compat (fonctions PTZ vérifiées dans la doc) et native ; Companion les pilote | CI + checklist |
| G6 | Invité WHIP : image et son reçus en input, synchro ≤ 1 image (flash + bip depuis un client de test), perte et retour du réseau → signal perdu puis repris | CI (client GStreamer local) |
| G7 | Retour WHEP : l'invité reçoit le Program et un *mix-minus* sans sa propre voix (corrélation mesurée) | CI |
| G8 | Lien d'invitation : jeton à usage unique, révocation immédiate, jamais écrit en clair (journal, logs, état, projet) | CI |
| G9 | Automate : l'origine est journalisée ; un geste de l'opérateur le coupe pour la durée réglée ; un automate qui tombe ne touche pas VMA | CI |
| G10 | Réalisateur au son : sur 3 voix enregistrées, le bon plan à l'antenne ≥ 90 % du temps parlé, aucun plan plus court que le minimum | CI (sons de test) |
| G11 | Soak 20 min par sous-phase : 0 image manquée, 0 répétée (7A : rappels de préréglages toutes les 10 s ; 7B : 2 invités + retour ; 7C : réalisateur au son actif) | machine de dev |

## 4. Plan (une étape = un commit testé ; arrêt pour revue après chaque sous-phase)
| Étape | Contenu | Critères |
|---|---|---|
| 7A.1 | Questions Q1–Q5, ADR 0018 (PTZ : protocole, threads, état), **validation** | — |
| 7A.2 | `vma-ptz` : codage VISCA, client UDP / TCP, caméra simulée | G1, G2 |
| 7A.3 | Commandes, état, projet ; inputs virtuels | G3 |
| 7A.4 | UI PTZ | G4 |
| 7A.5 | API compat et native | G5 |
| 7A.6 | Soak, checklist sur vraie caméra (à la fin, avec la partenaire), **revue** | G11 |
| 7B.1 | Q6–Q8, essai `webrtcbin` contre gst-plugins-rs, ADR 0019, **validation** | — |
| 7B.2–6 | Input invité (WHIP), page de l'invité, retour (WHEP + *mix-minus*), liens, UI, soak, **revue** | G6–G8, G11 |
| 7C.1 | Q9–Q10, ADR 0020 (plugins), **validation** | — |
| 7C.2–5 | Origine « automate » et garde-fou, vignettes partagées, niveaux dans `/events`, `vma-auto`, soak, **revue** | G9–G11 |

## 5. Dépendances prévues
- **7A** : aucune (UDP / TCP de la bibliothèque standard).
- **7B** :
  - `webrtcbin` + libnice (LGPL-2.1 / MPL-1.1, GStreamer lié dynamiquement comme
    aujourd'hui), ou les éléments WHIP / WHEP de gst-plugins-rs (MPL-2.0) ;
  - TLS : `rustls` (Apache-2.0 / ISC / MIT) ;
  - aucun serveur TURN embarqué (coturn est BSD, mais c'est un service à part).
- **7C** : aucune pour l'API ; `vma-auto` réutilise les vumètres de `vma-audio`.

## 6. Risques
- **PTZ sans caméra** : la caméra simulée suit la documentation, pas une caméra réelle. Les
  écarts des fabricants (réponses en retard, préréglages numérotés autrement) ne se verront
  qu'à la checklist finale.
- **WebRTC depuis Internet** : HTTPS obligatoire pour la caméra du navigateur, NAT, débit
  variable. Sans relais ni tunnel, seul le réseau local marche à coup sûr.
- **Latence des invités** : WebRTC vise moins de 300 ms, mais le retour vers l'invité s'y
  ajoute ; c'est à mesurer.
- **Automatisation** : un automate qui coupe trop souvent gêne plus qu'il n'aide. D'où les
  durées de plan, le garde-fou, et un critère mesuré (G10).

## 7. Questions au mainteneur (recommandation en premier)
- **Q1 — Découpage et ordre.** **Recommandé** : trois sous-phases revues séparément,
  **7A PTZ → 7B invités → 7C automatisation**. Alternative : commencer par 7C si la régie
  automatique du handball presse plus que les caméras.
- **Q2 — Matériel PTZ de la partenaire.** Quelles caméras PTZ (ou aucune pour l'instant) ? Le
  protocole et la variante (UDP 52381 ou TCP) en dépendent ; **recommandé** : VISCA over IP
  (UDP) d'abord, la variante TCP ensuite si une caméra l'exige.
- **Q3 — Variante TCP dès 7A.** **Recommandé** : oui, si une caméra de la partenaire la
  demande ; sinon plus tard (le codage est le même, seul le transport change).
- **Q4 — Nombre de préréglages par caméra.** **Recommandé** : 16 (la plupart des caméras en
  ont plus ; l'UI reste lisible).
- **Q5 — Rappel d'un préréglage quand la caméra est à l'antenne.** **Recommandé** :
  confirmation demandée (pour qu'un mouvement de caméra ne parte pas à l'antenne par
  mégarde) ; alternative : refus net.
- **Q6 — Côté invité.** **Recommandé** : la page web servie par VMA (rien à installer), et
  tout client WHIP accepté.
- **Q7 — Nombre d'invités.** **Recommandé** : 4 dans 7B (comme le podcast le demande au
  plus), mesurés sans perte.
- **Q8 — Accès depuis Internet.** **Recommandé** : réseau local d'abord ; pour Internet, un
  **tunnel HTTPS** fourni par l'utilisateur (documenté) ou un certificat qu'il fournit, et
  un serveur TURN qu'il fournit s'il en faut. Pas de service hébergé par le projet.
  Alternative : un relais hébergé (coût, vie privée, exploitation).
- **Q9 — Images pour les plugins.** **Recommandé** : vignettes 320×180 (ou 640×360) en
  mémoire partagée, 10 images/s au plus par défaut ; alternative : un flux NDI® du Program
  (plus lourd, dépend du runtime).
- **Q10 — Garde-fou.** **Recommandé** : un geste de l'opérateur coupe l'automate pour 30 s
  (réglable), et le bouton « Automate » est toujours visible.
