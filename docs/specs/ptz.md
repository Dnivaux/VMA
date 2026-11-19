# Spec — `ptz` (caméras PTZ, Phase 7A)

Décisions : [ADR 0018](../adr/0018-cameras-ptz.md) ; spec de phase :
[`phase-7.md`](phase-7.md) (7A, critères G1–G5).

## Comportement
- **VISCA** (`visca`) : les commandes envoyées par VMA, codées à l'octet près dans un tampon
  fixe (aucune allocation par commande), et les réponses d'une caméra (accusé, fin de
  commande, réponse à une demande, erreur). Caméra d'adresse 1 (`0x81`, réponses `0x90`).
  - Mouvement pan / tilt dans 8 directions et arrêt ; zoom et mise au point (vers tele / far,
    vers wide / near, arrêt) ; mise au point auto / manuelle, à la demande ; retour à
    l'origine ; préréglages 1 à 16 (mémoires 0 à 15), enregistrer et rappeler ; demande
    d'état de l'alimentation (la sonde).
  - Vitesse `Speed` de 1 à 100, ramenée à l'échelle de chaque moteur : pan 1–24, tilt 1–20,
    zoom et mise au point 0–7.
- **VISCA over IP** (`ip`) : en-tête de 8 octets (type, longueur, séquence ; gros-boutiste),
  remise à zéro de la séquence, erreurs de séquence et de message.
- **Client** (`client::PtzClient`), un par caméra, sur son thread :
  - UDP (VISCA over IP, port 52381 par défaut) ou TCP (VISCA brut, port 5678 par défaut) ;
  - une commande à la fois ; accusé attendu au plus 300 ms ; sans réponse, remise à zéro de
    la séquence et un renvoi ; un second silence rend la caméra injoignable ;
  - sonde toutes les 5 s au repos ; injoignable : sondes à 1, 2, 5, 10 s puis toutes les
    10 s ; les commandes reçues pendant ce temps sont écartées (`LinkEvent::Dropped`) ;
  - un mouvement en attente est remplacé par le suivant du même genre (pan / tilt, zoom,
    mise au point) ;
  - les fins de commande tardives (un rappel finit à l'arrivée de la caméra) ne passent
    jamais pour l'accusé d'une commande suivante (UDP : numéro de séquence ; TCP : sockets
    de la caméra) ;
  - événements : `Reachable`, `Unreachable { reason }`, `Refused { command, error }`,
    `Dropped { command }`.
- **Caméra simulée** (`sim::SimCamera`), en UDP ou TCP sur 127.0.0.1 : accusé puis fin dans
  les sockets 1 et 2, réponse à la sonde, erreur de syntaxe pour une commande inconnue,
  séquence vérifiée. Elle peut se taire, refuser, perdre des messages, se désynchroniser,
  finir ses commandes en retard.

## Interface publique
`visca::{ViscaCommand, Direction, LensMove, FocusMode, Speed, Preset, PRESETS, Reply,
CameraError, MoveKind, MAX_MESSAGE}`, `ip::{encode, decode, PayloadType, Datagram}`,
`client::{PtzClient, CameraAddress, Transport, Timing, LinkEvent, ClientError}`,
`sim::SimCamera`.

## Critères d'acceptation
| Critère | Test | Statut |
|---|---|---|
| G1 : chaque commande codée à l'octet près, et relue | `visca::tests::{pan_tilt_moves_are_encoded_to_the_byte, lens_commands_are_encoded_to_the_byte, presets_are_numbered_from_one_and_stored_from_zero}` | CI |
| G1 : vitesses sur l'échelle de chaque moteur, croissantes | `visca::tests::speeds_span_each_motor_scale` | CI |
| G1 : réponses (accusé, fin, réponse, erreurs) lues et écrites | `visca::tests::replies_are_read_and_written` | CI |
| G1 : en-tête VISCA over IP, remise à zéro, messages tronqués refusés | `ip::tests::*` | CI |
| G1 : UDP et TCP, chaque commande arrive une fois, dans l'ordre, à l'octet près | `tests/client.rs` : `udp_commands_reach_the_camera_in_order`, `tcp_commands_reach_the_camera_in_order` | CI |
| G1 : erreur de la caméra signalée, pas renvoyée, caméra toujours joignable | `a_refused_command_is_reported_and_the_camera_stays_reachable` | CI |
| G1 : datagramme perdu → remise à zéro et renvoi ; erreur de séquence → idem | `a_lost_datagram_is_resent_after_a_reset`, `a_sequence_error_resets_the_sequence_and_resends` | CI |
| G1 : fins de commande tardives sans confusion | `late_completions_do_not_stand_for_an_accuse` | CI |
| G1 : mouvements en attente remplacés par le dernier | `moves_waiting_are_replaced_by_the_latest` | CI |
| G2 : caméra muette → injoignable, gestes écartés, reprise | `a_silent_camera_is_unreachable_then_found_again`, `nothing_listening_is_unreachable`, `an_idle_camera_is_probed` | CI |
| G2 : le Program n'en souffre pas | étape 7A.3 (moteur) | — |
