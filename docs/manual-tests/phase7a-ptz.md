# Test manuel — Caméras PTZ sur du vrai matériel (Phase 7A, étape 6)

**But** : vérifier sur une vraie caméra PTZ ce que la CI et le soak G11 couvrent avec la
caméra simulée : la connexion, les mouvements, les préréglages, les inputs virtuels, la
confirmation à l'antenne, les pannes, l'API. Critères G1–G5 de
[`phase-7.md`](../specs/phase-7.md), ADR 0018. **À faire à la fin de la phase** (décision du
mainteneur, 2026-11-19), quand une caméra est disponible.

À faire en build release (`cargo run --release`, ou le paquet Windows). Noter pour chaque
case ce qui a été vu, surtout quand ça ne va pas : les écarts des fabricants ne se voient
qu'ici.

## Matériel
- [ ] Caméra : marque et modèle `…`, version du micrologiciel `…`
- [ ] Protocole annoncé par sa documentation : VISCA over IP (UDP 52381) / VISCA sur TCP
  (port `…`) ; son adresse IP : `…`
- [ ] Son image arrive dans VMA par : NDI® / SRT / carte de capture / USB (`…`)
- [ ] Machine : `…` ; même réseau que la caméra (câble de préférence)

## Connexion
- [ ] Ajouter l'input de la caméra (son image), ouvrir ses réglages (double-clic).
- [ ] « Caméra PTZ » : protocole, adresse, port ; « Connecter ». L'état passe à
  « Connectée » en moins d'une seconde, « PTZ » en vert sur la tranche.
- [ ] Mauvaise adresse : « Injoignable : … » en moins de 2 s, « PTZ » orange.
- [ ] Débrancher le câble réseau de la caméra : « Injoignable » au plus 6 s après (sonde
  toutes les 5 s), ou dès le geste suivant ; l'image de l'input n'est pas touchée par le
  PTZ. Rebrancher : « Connectée » de nouveau en 10 s au plus.
- [ ] Une commande pendant la panne : « commande écartée : caméra injoignable », la caméra
  ne bouge pas en retard au retour.

## Mouvements
- [ ] Pavé : chaque flèche, **maintenue**, fait bouger la caméra dans ce sens ; elle s'arrête
  au relâchement. Diagonales.
- [ ] « Vitesse » à 10, 50, 100 : la caméra bouge plus ou moins vite ; noter si 100 est
  trop rapide ou 1 trop lent pour ce modèle.
- [ ] « Zoom avant / arrière » maintenus ; arrêt au relâchement.
- [ ] « Manuelle », puis « Plus loin / Plus près » maintenus ; « Une fois » ; « Auto ».
  Noter si la caméra refuse un geste (message « commande refusée : … »).
- [ ] « Origine » : la caméra revient à sa position d'origine.
- [ ] Fermer la fenêtre de réglages pendant un mouvement maintenu : la caméra s'arrête.

## Préréglages et inputs virtuels
- [ ] Cadrer, « Enregistrer la position », « 1 » ; cadrer ailleurs, « 2 ». « 1 » puis « 2 » :
  la caméra y retourne (le numéro est en gras une fois enregistré).
- [ ] La numérotation correspond à celle de la caméra (préréglage 1 de VMA = mémoire 0 du
  VISCA ; certains fabricants numérotent autrement : noter ce que montre la télécommande
  ou la page web de la caméra).
- [ ] « Nouvel input virtuel » deux fois, à deux cadrages : deux inputs « *caméra* 1 »,
  « *caméra* 2 » ; leur vignette montre l'image de la caméra.
- [ ] Une autre caméra au Program. Clic sur un input virtuel (Preview) : la caméra part sur son
  préréglage, **une fois** ; la Preview montre le mouvement. Transition : il passe au Program.
- [ ] L'autre input virtuel en Preview pendant que la caméra est à l'antenne : la caméra
  **ne bouge pas** ; sous la Preview, « Caméra à l'antenne » et « Déplacer la caméra ».
  « Déplacer la caméra » : elle bouge (à l'antenne).
- [ ] Dans les préréglages de la caméra à l'antenne : « Caméra à l'antenne : aller au
  préréglage n ? », « Annuler » ne fait rien, « Déplacer quand même » la bouge.
- [ ] Réglages d'un input virtuel : « Mettre à jour la position » après un recadrage ; le
  rappel suivant va au nouveau cadrage.
- [ ] Enregistrer le projet, le rouvrir : la caméra se reconnecte, ses inputs virtuels sont là.
- [ ] « Retirer la PTZ » : les inputs virtuels disparaissent.

## Le direct n'en souffre pas
- [ ] Pendant tout le test, Statistiques : 0 image manquée, 0 répétée au Program.

## API et Companion
- [ ] `curl "http://127.0.0.1:8088/api/?Function=PTZMoveLeft&Input=<n>&Value=0.5"` puis
  `PTZMoveStop` : la caméra bouge puis s'arrête.
- [ ] `PTZMoveToVirtualInputPosition&Input=<input virtuel>` : la caméra va au préréglage
  sans changer la Preview.
- [ ] Companion, module de la référence : ses actions PTZ (s'il en a) pilotent la caméra.
  Noter celles qui manquent ou échouent.
