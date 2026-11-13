# API native

- **Statut** : mise en œuvre le 2026-11-11 (Phase 5 étape 5, ADR 0012) ; critères E10, E11 de
  [`phase-5.md`](phase-5.md)
- **Adresse** : `http://127.0.0.1:8090` par défaut (cette machine seulement, ADR 0012 § 3).
  Réglages → API de contrôle : « Accessible depuis le réseau » (0.0.0.0, avec un
  avertissement), les trois ports, l'état de chaque serveur, « Appliquer » (les API sont
  fermées puis rouvertes) ; gardés dans `settings.json` du dossier de configuration. Un port
  qui ne s'ouvre pas est signalé dans la barre du haut ; `--no-api` les ferme toutes.

L'API native est la porte des outils maison, du futur agent IA et d'une future UI web. Elle
ne touche jamais l'état : elle envoie des `Command`, exactement celles de l'UI, journalisées
avec la source `api`, et lit l'état que `vma-control` publie.

## `POST /commands`
Corps : **une** `Command` en JSON, sous la forme du journal (`docs/specs/commands.md`) :
`"Cut"`, `{"Transition":{"button":1}}`, `{"SetPreview":{"input":3}}`,
`{"AddInput":{"spec":{"Color":{"color":{"r":200,"g":0,"b":0}}},"name":"Rouge"}}`…
Les inputs sont désignés par leur `id` (champ `id` de `GET /state`).

Replay (Phase 6, les commandes de l'UI) : `"StartReplayRecording"`,
`{"ReplayMark":{"mark":{"Last":{"seconds":5}}}}` (`"In"`, `"Out"`, `"Cancel"` ; l'instant est
celui où `vma-control` la reçoit), `{"ReplayPlayEvent":{"id":null}}` (le dernier),
`{"ReplaySpeed":{"percent":50}}`, `{"ReplayAngle":{"input":4}}`, `{"ReplayAuto":{"id":null}}`
(« Rejouer »), `"StartIso"`… ; l'état du replay est dans `replay` de `GET /state`.

Son d'une caméra : `{"SetCaptureAudio":{"input":2,"device":"alsa:alsa_input.usb-Mic"}}`
(`"device":null` : aucun) ; les périphériques audio ne sont pas listés par l'API (réglages de
l'input dans l'interface).

| Code | Corps | Quand |
|---|---|---|
| 200 | `{"id": 17, "result": "ok"}` | appliquée |
| 422 | `{"id": 17, "result": "rejected", "error": "…"}` | refusée par le moteur (raison en anglais, comme dans le journal) |
| 400 | `{"error": "not a command: …"}` | ce n'est pas une `Command` |
| 503 | `{"error": "the command queue is full"}` | file pleine (1 024 commandes en attente) ou moteur arrêté |
| 504 | `{"error": "the engine did not answer in time"}` | pas de réponse en 2 s |

Corps de 1 Mio au plus (un `LoadProject` complet y tient).

## `GET /state`
L'état complet en JSON (`vma_engine::State` sérialisé, `version` comprise). Les secrets
(clés de stream, phrases SRT) y sont écrits `***`. Ses champs suivent ceux du moteur : un
client doit ignorer ceux qu'il ne connaît pas.

## `GET /tally`
Un objet par input, dans l'ordre d'affichage :
`{"number": 2, "key": "…", "name": "Caméra 2", "program": true, "preview": false}`.
`program` : vu à l'antenne, c'est-à-dire au Program, dans un overlay entré, ou entrant par
une transition (T-Bar : dès qu'elle a bougé) ; `preview` : en Preview (`phase-5.md` Q11).

## `GET /events` (WebSocket)
Messages texte JSON, du serveur vers le client :
- `{"type": "state", "state": {…}}` : à la connexion, puis à chaque nouvelle version de
  l'état ;
- `{"type": "tally", "tally": […]}` : à la connexion, puis quand le tally change (après le
  `state` qui le change).

Seul l'état le plus récent part : un client lent saute des versions, rien ne s'accumule ;
un client qui ne lit plus pendant 5 s est déconnecté. Les messages du client sont ignorés
(les pings reçoivent leur pong).
