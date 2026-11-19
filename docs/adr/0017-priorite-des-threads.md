# ADR 0017 — Priorité des threads de temps réel

- **Statut** : **accepté** le 2026-11-19 (choix interne délégué par le mainteneur ; dette de
  l'ADR 0001, « Priorité du thread de rendu »)
- **Date** : 2026-11-19
- **Contexte** : ADR 0001 (limites connues : priorité OS par défaut, élévation « si les soaks
  montrent des retards »), soaks Windows de la partenaire (2026-11-11 et 2026-11-13)

## Constat
L'ADR 0001 laissait le thread de rendu à la priorité par défaut, à revoir d'après
l'histogramme du retard au réveil. Sous Linux, ce retard reste faible : au plus 0,2 ms
(A6, 2026-11-19). **Sous Windows, il ne l'est pas**. Sur le portable de la partenaire (RTX 3060,
Intel de 11ᵉ ou 12ᵉ génération), le retard p99,9 va jusqu'à 8,1 ms et le maximum de 9,5 à
19,4 ms, sur les 4 soaks. Au 2ᵉ passage d'A1 (sur batterie), 75 des 86 alertes des inputs
viennent de réveils tardifs des threads de cadence (`vma-pace-*`). Une image manquée vient d'un
réveil en retard de plus d'une demi-période (10 ms à 50p).

## Décision
1. **Deux rôles.** Le **rendu** (`vma-render`). Les threads **de cadence**, réveillés à
   l'heure d'un tick ou d'un bloc : `vma-audio`, `vma-pace-*` (inputs calés sur les ticks),
   `vma-replay-*` (lecteurs), `vma-html-ticks` (relais des ticks vers les pages), `vma-phones`
   (casque). Les autres threads (contrôle, sorties, enregistreurs, UI, GStreamer) gardent la
   priorité par défaut : ils ont des files d'attente.
2. **Windows.**
   - Rendu : `THREAD_PRIORITY_TIME_CRITICAL`. Cadence : `THREAD_PRIORITY_HIGHEST`. Aucun
     droit n'est requis, et la classe du processus reste normale (GStreamer et l'UI ne
     changent pas).
   - Pour ces threads, le bridage d'énergie est levé (`ThreadPowerThrottling`, *EcoQoS*
     désactivé) : Windows ne les envoie pas sur les cœurs économes ni à fréquence réduite.
   - Pour le processus, les mêmes réglages sont levés une fois (`ProcessPowerThrottling` :
     vitesse d'exécution, et résolution des minuteurs gardée quand la fenêtre est cachée).
3. **Linux.**
   - `SCHED_FIFO` (rendu 12, cadence 10 ; `SCHED_RESET_ON_FORK`, pour qu'un processus lancé
     depuis ces threads ne l'hérite pas). Il n'est accordé que si l'utilisateur en a le droit
     (`RLIMIT_RTPRIO`, par exemple le groupe `realtime` sous Arch).
   - Sans ce droit, le thread reste à la priorité normale, ce qui est noté une fois dans le
     journal. Rien n'est demandé à `rtkit` : il faudrait un client D-Bus, et Linux n'en a pas
     besoin d'après les mesures.
4. **Jamais d'échec.** Une élévation refusée laisse le thread tel quel. Le résultat de chaque
   rôle est publié (`vma_clock::priority::summary`) dans le panneau Statistiques et dans le
   rapport de soak.
5. **Code.** Le module `vma_clock::priority` est le seul endroit où le `unsafe` est
   autorisé, avec un commentaire `// SAFETY:` pour chaque appel système (les appels de
   `libc` et de `windows-sys` sont `unsafe` en Rust). Aucune nouvelle crate : `libc` (MIT OR
   Apache-2.0) et `windows-sys` (MIT OR Apache-2.0) sont déjà dans l'arbre des dépendances.
   La crate `thread-priority` a été écartée, car elle ne lève pas le bridage d'énergie de
   Windows.

## Conséquences
- Un thread de cadence qui boucle sans dormir pourrait affamer d'autres threads. Le rendu
  attend en actif au plus 1 ms par tick (marge d'attente), les autres dorment entre deux
  réveils. Sous Linux, le noyau garde 5 % du temps aux threads ordinaires
  (`sched_rt_runtime_us`).
- Sous Windows, le portable sur batterie consomme un peu plus.
- **À vérifier sur la machine de la partenaire** : le 3ᵉ passage Windows doit montrer le
  retard au réveil (p99,9, max) et la priorité obtenue dans le rapport.
