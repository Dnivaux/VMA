# ADR 0018 — Caméras PTZ (VISCA)

- **Statut** : **accepté** le 2026-11-19 (choix délégués par le mainteneur : « choisis le
  mieux », réponse à la spec de la Phase 7)
- **Date** : 2026-11-19
- **Contexte** : [`phase-7.md`](../specs/phase-7.md) (7A, décisions Q2–Q5) ; ADR 0003 (état,
  commandes, threads), ADR 0004 (`vma-io`), ADR 0013 (projet) ; documentation publique de
  VISCA et de VISCA over IP

## Constat
La partenaire n'a pas de caméra PTZ pour l'instant (Q2). Rien ne dit donc quel modèle elle
achètera : il faut le protocole le plus répandu, sous ses deux transports courants, et une
caméra simulée qui suit la documentation publique pour tout tester sans matériel.

## Décision
1. **Protocole : VISCA**, l'adresse de caméra 1 (`0x81`, réponses `0x90`), comme le veut
   VISCA sur IP. Deux transports, au choix de l'opérateur pour chaque caméra :
   - **VISCA over IP** (UDP, port 52381 par défaut). Chaque message porte un en-tête de
     8 octets : type (2 octets : commande `0x0100`, demande `0x0110`, réponse `0x0111`,
     contrôle `0x0200`, réponse de contrôle `0x0201`), longueur (2), numéro de séquence (4),
     en gros-boutiste. La remise à zéro de la séquence est le message de contrôle `0x01`.
   - **VISCA brut sur TCP** (port 5678 par défaut) : les mêmes octets VISCA, sans en-tête.
2. **Commandes** utilisées, toutes **idempotentes** (les renvoyer ne fait pas bouger deux
   fois la caméra) :
   - mouvement pan / tilt à vitesse donnée (huit directions, arrêt) ;
   - zoom et mise au point (plus près / plus loin à vitesse donnée, arrêt), mise au point
     auto / manuelle, mise au point à la demande ;
   - retour à l'origine ;
   - préréglages : enregistrer, rappeler (mémoires 0 à 15 : 16 préréglages, Q4) ;
   - demande d'état de l'alimentation, qui sert de **sonde** (la caméra répond-elle ?).
3. **Échange.** Une commande à la fois par caméra :
   - On attend l'accusé (ou l'erreur) au plus **300 ms**, mais pas la fin de la commande : un
     rappel de préréglage ne finit qu'à l'arrivée de la caméra, et le pavé doit rester
     réactif. Les fins de commande arrivées entre-temps sont lues et ignorées.
   - Sans réponse, la séquence est remise à zéro (UDP), et la commande est renvoyée une
     fois ; un second silence rend la caméra **injoignable**.
   - Une erreur de la caméra (syntaxe, commande impossible, tampon plein) est signalée, sans
     renvoi.
   - Les mouvements se remplacent : dans la file d'une caméra, un mouvement du même genre
     (pan / tilt, zoom, mise au point) remplace celui qui attend, pour que la caméra suive le
     dernier geste et pas une file de gestes passés. Un arrêt n'est jamais perdu, puisqu'il
     est le dernier.
4. **Joignable ou non.**
   - Au repos, une sonde part toutes les 5 s.
   - Une caméra injoignable est sondée à 1, 2, 5 puis 10 s, et ensuite toutes les 10 s (la
     reprise des inputs). Les commandes qui arrivent pendant ce temps sont écartées et
     comptées : un mouvement envoyé en retard serait pire que pas de mouvement.
   - L'état (joignable, injoignable avec la raison, commandes écartées) remonte à
     `vma-control` (`IoEvent`). L'image de l'input n'est jamais touchée.
5. **Threads.** La crate `vma-ptz` (sans GStreamer ni dépendance nouvelle) code et décode
   VISCA sans allocation par commande. Elle fournit un client par caméra, sur son thread
   `vma-ptz-<n>`, qui attend en bloquant, avec des délais, jamais en boucle active ; il
   garde la priorité par défaut, comme tout thread de contrôle. `vma-io` crée et arrête ces
   clients sur requête de `vma-control` (`IoRequest`), comme ses pipelines.
6. **État et projet.**
   - Une caméra PTZ est un input de capture, NDI® ou réseau, avec des réglages PTZ :
     transport, hôte, port, et les noms de ses 16 préréglages. Ils sont dans `State` et dans
     le projet (ADR 0013). Ce qui vient de la caméra (joignable ou non) n'est pas écrit dans
     le projet.
   - Toute action PTZ est une `Command`, journalisée, rejouable, et accessible par l'API.
7. **Inputs virtuels.**
   - Un input virtuel est un input ordinaire de type « vue PTZ » : la caméra et un
     préréglage. Il **n'a pas de pipeline** : le rendu montre l'image de l'input caméra (un
     alias dans la table des sources).
   - Mis en Preview, il envoie le rappel de son préréglage, **une fois**. Si la caméra est
     déjà à l'antenne, par elle-même ou par un autre de ses inputs virtuels, rien n'est
     envoyé : l'UI demande une confirmation (Q5), qui envoie le rappel.
   - Supprimer la caméra supprime ses inputs virtuels.

## Conséquences
- Les écarts des fabricants (réponses tardives, préréglages numérotés autrement, variantes
  de commandes) ne se verront qu'à la checklist sur vraie caméra, à la fin de la phase.
- Un rappel de préréglage n'est pas suivi jusqu'à son terme : l'UI ne sait pas quand la
  caméra est arrivée. Suffisant pour préparer un plan en Preview ; à revoir si l'automatisation
  (7C) en a besoin.
- **Écart connu avec la référence** : son aide (« PTZ control virtual inputs ») dit qu'un
  input virtuel ni en Preview ni au Program montre une image fixe, prise à sa création. Chez
  VMA, il montre toujours l'image vivante de sa caméra (aucune image à garder, rien de plus à
  rendre). **Gardé** par le mainteneur le 2026-11-19.
- Les fonctions PTZ de l'API compat sont celles de la référence (`api-compat.md` § 4) ;
  `PTZMoveToVirtualInputPosition` est une demande explicite : elle déplace la caméra même à
  l'antenne, sans confirmation.
- Pas de contrôle PTZ par NDI® ni par des protocoles propriétaires (hors Phase 7).
