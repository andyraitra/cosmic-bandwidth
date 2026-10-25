# cosmic-bandwidth

Applet pour le panneau COSMIC, équivalent de Bandwidth+ sur macOS : débit en direct et
consommation data du mois par rapport à un plafond.

Panneau : `↓1.2 Mo/s ↑40.0 Ko/s · 12.4/50 Go`. Au clic : reçu / envoyé, jauge, date de remise à
zéro, plafond réglable, remise à zéro manuelle. Notification à 80 % et à 100 % du plafond.

## Installation (sans sudo)

Prérequis de compilation (une fois) :

    sudo apt install libxkbcommon-dev libudev-dev libinput-dev libfontconfig-dev just

Puis :

    just install

Le binaire va dans `~/.local/bin`, le `.desktop` dans `~/.local/share/applications`. Ajouter
l'applet : Paramètres → Bureau → Panneau → Applets → Bandwidth. Désinstaller : `just uninstall`.

## Fonctionnement

- Compte les octets de `/sys/class/net/<interface>/statistics/{rx,tx}_bytes` toutes les 2 s.
- Cumul du mois : `~/.local/state/cosmic-bandwidth/usage.json` (écriture atomique, au plus toutes
  les 30 s, et à l'arrêt). Un fichier invalide ou illisible est mis de côté en
  `usage.json.corrupt-<horodatage>` (jamais écrasé) ; s'il ne peut pas l'être, la sauvegarde est
  désactivée plutôt que d'écraser des données.
- Plusieurs instances (panneau sur plusieurs écrans, panneau + dock) : une seule compte et
  sauvegarde (verrou `usage.json.lock`), les autres affichent le même cumul. La remise à zéro
  depuis n'importe laquelle passe par la config et s'applique partout.
- Remise à zéro le 1er du mois à 00:00 (heure locale). Si l'horloge recule de plus d'un mois
  (horloge matérielle déréglée), le cumul est conservé et rattaché au mois courant.
- Unités décimales (1 Go = 10⁹ octets), comme macOS et les opérateurs.

Limites connues :

- le trafic qui passe pendant que l'applet ne tourne pas (session fermée, machine éteinte) n'est
  pas compté ;
- quand une instance reprend le comptage d'une autre (redémarrage du panneau), au plus un tick
  (2 s) de trafic peut manquer — jamais compté deux fois ;
- si le fichier d'état n'a pas pu être lu, la sauvegarde reste désactivée jusqu'au redémarrage
  de l'applet (le comptage et les alertes continuent).
- une remise à zéro demandée depuis une instance secondaire est perdue si l'instance qui compte
  s'arrête dans les quelques millisecondes qui suivent, avant de l'avoir reçue.

## Réglages

`~/.config/cosmic/io.github.andyraitra.CosmicBandwidth/v1/` :

- `interface` : interface comptée, `"wlp2s0f0"` par défaut ;
- `cap_bytes` : plafond en octets, `0` = aucun (aussi réglable depuis l'applet) ;
- `reset_requests` : compteur interne des demandes de remise à zéro.

Exemple pour compter l'Ethernet :

    echo '"enp59s0"' > ~/.config/cosmic/io.github.andyraitra.CosmicBandwidth/v1/interface
