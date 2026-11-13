//! Command-line options (hand-parsed: a handful of flags do not justify a dependency).

use anyhow::{Context as _, bail};
use std::path::PathBuf;
use std::time::Duration;
use vma_core::{FrameRate, Resolution};
use vma_engine::Secret;

pub const HELP: &str = "\
VMA — mélangeur vidéo live

Usage : vma [options]

Options :
  --rate <cadence>       cadence maître : 25, 29.97, 30, 50, 59.94 ou 60 (défaut : 50)
  --resolution <taille>  résolution du Program : 1920x1080, 1280x720, 1080p, 720p…
                         (défaut : 1080p)
  --inputs <n>           ajoute n sources synthétiques (GStreamer videotestsrc) au démarrage
  --input-rates <liste>  cadences de ces sources, séparées par des virgules, réparties en
                         boucle (défaut : la cadence maître) ; ex. 50,25,29.97,60
  --input-resolution <taille>
                         résolution de ces sources (défaut : celle du Program)
  --sync-inputs          ces sources sont des sources synchro, avec son : une image blanche
                         et un bip par seconde de leur horloge ; le Master suit la vidéo
  --input-drifts <liste> avance de leurs horloges en ppm, réparties en boucle (défaut : 0) ;
                         ex. 0,0,0,0,100,-100 (exige --sync-inputs)
  --html <page>          ajoute un input HTML (page rendue hors écran, avec son alpha) au
                         démarrage : une adresse http(s):// ou file://, ou un fichier
                         (répétable ; exige vma-html à côté de vma, ou VMA_HTML)
  --overlay-image <png>  ajoute cette image (avec alpha) au démarrage
  --overlays             met en overlay 1 à 4, au démarrage, les inputs HTML puis l'image
                         de --overlay-image (soak D11 de la Phase 4)
  --stinger <fichier>    charge ce fichier dans le stinger 1 et le règle sur le bouton 2 ;
                         pendant le soak, une transition sur trois est ce stinger
  --html-data            pendant le soak, envoie une donnée par seconde au dernier input
                         HTML (événement « vma-data » : n, score)
  --spin-margin-us <µs>  marge d'attente active de l'horloge maître (défaut : 1000)
  --project <fichier>    ouvre ce projet (.vma) au démarrage, à sa cadence et à sa résolution
                         (elles priment sur --rate et --resolution)
  --no-autosave          n'enregistre pas l'émission automatiquement (coupé d'office pendant
                         un soak : « Dernier » garde l'émission d'avant)
  --no-api               n'ouvre pas les API de contrôle (sur 127.0.0.1 : native 8090,
                         compat HTTP 8088 et TCP 8099)
  --record <dossier>     enregistre le Program dès le démarrage, dans ce dossier
                         (sans l'option : bouton « Enregistrer », dossier Vidéos)
  --fullscreen <écran>   montre le Program (après FTB) en plein écran dès le démarrage, sur
                         cet écran : son nom (DP-2, HDMI-A-1… : voir le journal au
                         démarrage ou la fenêtre Sorties) ou son numéro (1 = le premier)
  --stream <url>         streame le Program dès le démarrage vers ce serveur RTMP(S)
                         (rtmp://hôte/app) ; clé : --stream-key, ou la variable
                         d'environnement VMA_STREAM_KEY (jamais écrite dans le rapport)
  --stream-key <clé>     clé de stream de --stream
  --ndi                  sortie NDI® du Program dès le démarrage (« VMA Program »)
  --sync-test            mode synchro des sorties : une image blanche et un bip par seconde
  --sync-check <url>     pendant le soak, relit ce flux (ex. rtmp://hôte/app/clé) et mesure
                         le décalage bip − flash (exige --soak, et --sync-test ou
                         --sync-inputs)
  --gpu-loss-grace <s>   GPU perdu : durée pendant laquelle les sorties reçoivent du noir
                         avant d'être finalisées et l'application fermée (défaut : 10)
  --soak <secondes>      soak : tourne N secondes, écrit un rapport, quitte
                         (code 0 = réussi, 2 = images manquées ou répétées)
  --api-load             pendant le soak, un client de l'API compat TCP (127.0.0.1:8099) :
                         une commande toutes les 100 ms, l'état XML toutes les 250 ms (E13)
  --replay-load <dossier>
                         pendant le soak, le replay au travail (F7) : les inputs enregistrés
                         pour le replay et en ISO dans ce dossier (un SSD de préférence),
                         « −5 s » puis « Rejouer » toutes les 30 s, un autre angle, l'arrière
  --fullscreen-source <source>
                         ce que montre le plein écran de --fullscreen : program (défaut),
                         preview ou multiview
  --hostile              pendant le soak, agresse la fenêtre en continu
                         (redimensionne, maximise, plein écran, minimise, déplace)
  --report <chemin>      fichier du rapport de soak (défaut : soak-report.md)
  --journal-dir <dossier>
                         dossier du journal des commandes (défaut : logs)
  --no-journal           n'écrit pas de journal des commandes
  --no-headphones        ne joue pas le casque (coupé d'office pendant un soak : rien sur
                         les haut-parleurs de la machine)
  -h, --help             affiche cette aide
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub rate: FrameRate,
    pub resolution: Resolution,
    /// Synthetic inputs added at start-up.
    pub inputs: Vec<Synthetic>,
    /// HTML inputs added at start-up (addresses).
    pub html: Vec<String>,
    /// An image added at start-up (`--overlay-image`).
    pub overlay_image: Option<PathBuf>,
    /// HTML inputs then the overlay image keyed in overlays 1–4 at start-up.
    pub overlays: bool,
    /// A stinger file for slot 1, on button 2.
    pub stinger: Option<PathBuf>,
    pub spin_margin: Option<Duration>,
    /// A project opened at start-up, at its format (ADR 0013).
    pub project: Option<PathBuf>,
    /// Autosave the show: not with `--no-autosave`, nor during a soak.
    pub autosave: bool,
    /// Open the control APIs (ADR 0012): not with `--no-api`.
    pub api: bool,
    /// Record from start-up into this folder.
    pub record: Option<PathBuf>,
    /// Full screen from start-up on this screen (name, or number from 1).
    pub fullscreen: Option<String>,
    /// What that full screen shows.
    pub fullscreen_source: vma_engine::ViewSource,
    /// Stream from start-up to this RTMP(S) server.
    pub stream: Option<StreamOption>,
    /// Sync test on the outputs (flash and beep every second).
    pub sync_test: bool,
    /// NDI® output of the Program from start-up.
    pub ndi: bool,
    /// After a GPU loss, black on the outputs for this long, then finalised.
    pub gpu_loss_grace: Option<Duration>,
    pub soak: Option<SoakOptions>,
    /// `None` = no journal.
    pub journal_dir: Option<PathBuf>,
    /// Play the headphones: not with `--no-headphones`, nor during a soak.
    pub headphones: bool,
    pub help: bool,
}

/// A synthetic input asked for on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Synthetic {
    pub rate: FrameRate,
    pub resolution: Resolution,
    /// `Some(ppm)`: a sync source (flash and beep), its clock this much faster than the
    /// shared one; `None`: `videotestsrc`, no sound.
    pub sync_drift_ppm: Option<i32>,
}

/// Default journal directory (spec `commands.md`).
pub const DEFAULT_JOURNAL_DIR: &str = "logs";

/// `--stream`: where, under which key (never written out: [`Secret`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOption {
    pub url: String,
    pub key: Secret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoakOptions {
    pub duration: Duration,
    pub hostile: bool,
    pub report: PathBuf,
    /// Read this stream back and measure beep − flash (B1).
    pub sync_check: Option<String>,
    /// Send data to the last HTML input every second (D11).
    pub html_data: bool,
    /// One transition in three is stinger 1 (button 2).
    pub stinger: bool,
    /// A client of the compat API runs during the soak (E13).
    pub api_load: bool,
    /// The replay at work during the soak, recording in that folder (F7).
    pub replay_load: Option<PathBuf>,
}

/// Environment variable holding the stream key, to keep it out of the command line.
pub const STREAM_KEY_ENV: &str = "VMA_STREAM_KEY";

/// The command line as reports show it: the value of `--stream-key`, and the key wherever
/// else it appears (a `--sync-check` URL…), are `***`.
pub fn redacted(args: impl IntoIterator<Item = String>, key: Option<&str>) -> String {
    let key = key.filter(|k| !k.is_empty());
    let mut out = Vec::new();
    let mut hide_next = false;
    for arg in args {
        let shown = if std::mem::take(&mut hide_next) {
            "***".to_owned()
        } else {
            key.map_or_else(|| arg.clone(), |k| arg.replace(k, "***"))
        };
        hide_next = arg == "--stream-key";
        out.push(shown);
    }
    out.join(" ")
}

pub fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Options> {
    let mut rate = FrameRate::DEFAULT;
    let mut resolution = Resolution::HD_1080;
    let mut inputs = 0usize;
    let mut html = Vec::new();
    let mut overlay_image = None;
    let mut overlays = false;
    let mut stinger = None;
    let mut html_data = false;
    let mut input_rates: Option<Vec<FrameRate>> = None;
    let mut input_resolution = None;
    let mut spin_margin = None;
    let mut record = None;
    let mut project = None;
    let mut autosave = true;
    let mut api = true;
    let mut fullscreen = None;
    let mut gpu_loss_grace = None;
    let mut stream_url = None;
    let mut stream_key: Option<String> = None;
    let mut sync_test = false;
    let mut sync_inputs = false;
    let mut input_drifts: Option<Vec<i32>> = None;
    let mut ndi = false;
    let mut sync_check = None;
    let mut soak_secs = None;
    let mut hostile = false;
    let mut api_load = false;
    let mut replay_load = None;
    let mut fullscreen_source = vma_engine::ViewSource::Program;
    let mut report = None;
    let mut help = false;
    let mut journal_dir = Some(PathBuf::from(DEFAULT_JOURNAL_DIR));
    let mut headphones = true;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rate" => {
                let raw: String = value(&mut args, &arg)?;
                rate = parse_rate(&raw)?;
                if !FrameRate::MASTER_RATES.contains(&rate) {
                    bail!(
                        "cadence maître non prise en charge : {raw} (25, 29.97, 30, 50, 59.94, 60)"
                    );
                }
            }
            "--resolution" => resolution = parse_resolution(&value::<String>(&mut args, &arg)?)?,
            "--inputs" => inputs = value(&mut args, &arg)?,
            "--html" => html.push(html_address(&value::<String>(&mut args, &arg)?)?),
            "--overlay-image" => {
                overlay_image = Some(PathBuf::from(value::<String>(&mut args, &arg)?));
            }
            "--overlays" => overlays = true,
            "--stinger" => stinger = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--html-data" => html_data = true,
            "--input-rates" => {
                let raw: String = value(&mut args, &arg)?;
                input_rates = Some(
                    raw.split(',')
                        .map(|r| parse_rate(r.trim()))
                        .collect::<anyhow::Result<_>>()?,
                );
            }
            "--sync-inputs" => sync_inputs = true,
            "--input-drifts" => {
                let raw: String = value(&mut args, &arg)?;
                input_drifts = Some(
                    raw.split(',')
                        .map(|d| {
                            d.trim()
                                .trim_start_matches('+')
                                .parse::<i32>()
                                .ok()
                                .filter(|ppm| ppm.abs() <= 1_000)
                                .with_context(|| format!("avance invalide : {d} (±1000 ppm)"))
                        })
                        .collect::<anyhow::Result<_>>()?,
                );
            }
            "--ndi" => ndi = true,
            "--input-resolution" => {
                input_resolution = Some(parse_resolution(&value::<String>(&mut args, &arg)?)?);
            }
            "--spin-margin-us" => {
                let us: u64 = value(&mut args, &arg)?;
                spin_margin = Some(Duration::from_micros(us));
            }
            "--record" => record = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--project" => project = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--no-autosave" => autosave = false,
            "--no-api" => api = false,
            "--fullscreen" => fullscreen = Some(value::<String>(&mut args, &arg)?),
            "--fullscreen-source" => {
                fullscreen_source = match value::<String>(&mut args, &arg)?.as_str() {
                    "program" => vma_engine::ViewSource::Program,
                    "preview" => vma_engine::ViewSource::Preview,
                    "multiview" => vma_engine::ViewSource::Multiview,
                    other => bail!("--fullscreen-source : {other} (program, preview ou multiview)"),
                }
            }
            "--api-load" => api_load = true,
            "--replay-load" => {
                replay_load = Some(PathBuf::from(value::<String>(&mut args, &arg)?));
            }
            "--stream" => stream_url = Some(value::<String>(&mut args, &arg)?),
            "--stream-key" => stream_key = Some(value::<String>(&mut args, &arg)?),
            "--sync-test" => sync_test = true,
            "--sync-check" => sync_check = Some(value::<String>(&mut args, &arg)?),
            "--gpu-loss-grace" => {
                gpu_loss_grace = Some(Duration::from_secs(value::<u64>(&mut args, &arg)?));
            }
            "--soak" => soak_secs = Some(value::<u64>(&mut args, &arg)?),
            "--hostile" => hostile = true,
            "--report" => report = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--journal-dir" => {
                journal_dir = Some(PathBuf::from(value::<String>(&mut args, &arg)?));
            }
            "--no-journal" => journal_dir = None,
            "--no-headphones" => headphones = false,
            "-h" | "--help" => help = true,
            other => bail!("option inconnue : {other} (voir --help)"),
        }
    }

    let soak = match soak_secs {
        Some(secs) => Some(SoakOptions {
            duration: Duration::from_secs(secs),
            hostile,
            report: report.unwrap_or_else(|| PathBuf::from("soak-report.md")),
            sync_check: sync_check.clone(),
            html_data,
            stinger: stinger.is_some(),
            api_load,
            replay_load: replay_load.clone(),
        }),
        None if hostile
            || report.is_some()
            || sync_check.is_some()
            || api_load
            || replay_load.is_some() =>
        {
            bail!("--hostile, --report, --sync-check, --api-load et --replay-load exigent --soak")
        }
        None => None,
    };
    if sync_check.is_some() && !sync_test && !sync_inputs {
        bail!(
            "--sync-check exige --sync-test ou --sync-inputs (sans flash ni bip, rien à mesurer)"
        );
    }
    if sync_test && sync_inputs {
        bail!("--sync-test et --sync-inputs : deux flashs par seconde, la mesure serait fausse");
    }
    if input_drifts.is_some() && !sync_inputs {
        bail!("--input-drifts exige --sync-inputs");
    }
    let stream = match (stream_url, stream_key) {
        (None, Some(_)) => bail!("--stream-key exige --stream"),
        (None, None) => None,
        (Some(url), key) => {
            let key = key
                .or_else(|| std::env::var(STREAM_KEY_ENV).ok())
                .filter(|k| !k.is_empty())
                .with_context(|| {
                    format!("--stream exige une clé : --stream-key ou {STREAM_KEY_ENV}")
                })?;
            Some(StreamOption {
                url,
                key: Secret::new(key),
            })
        }
    };
    if inputs > vma_engine::MAX_INPUTS {
        bail!("--inputs : au plus {}", vma_engine::MAX_INPUTS);
    }
    let rates = input_rates.unwrap_or_else(|| vec![rate]);
    let drifts = input_drifts.unwrap_or_else(|| vec![0]);
    let inputs = rates
        .iter()
        .cycle()
        .zip(drifts.iter().cycle())
        .take(inputs)
        .map(|(&rate, &drift)| Synthetic {
            rate,
            resolution: input_resolution.unwrap_or(resolution),
            sync_drift_ppm: sync_inputs.then_some(drift),
        })
        .collect();
    Ok(Options {
        rate,
        resolution,
        inputs,
        html,
        overlay_image,
        overlays,
        stinger,
        spin_margin,
        project,
        autosave: autosave && soak.is_none(),
        api,
        record,
        fullscreen,
        fullscreen_source,
        gpu_loss_grace,
        stream,
        sync_test,
        ndi,
        headphones: headphones && soak.is_none(),
        soak,
        journal_dir,
        help,
    })
}

/// `50`, `50p`, `29.97` (or `29,97`), `30000/1001`. A decimal is one of the master rates.
/// An address as given, or a local file as a `file://` address.
fn html_address(raw: &str) -> anyhow::Result<String> {
    if raw.contains("://") {
        return Ok(raw.to_owned());
    }
    let path = std::fs::canonicalize(raw)
        .map_err(|e| anyhow::anyhow!("--html : page introuvable ({raw}) : {e}"))?;
    let p = path.to_string_lossy().replace('\\', "/");
    // Windows: `\\?\C:\…` once canonical.
    let p = p.trim_start_matches("//?/");
    Ok(if p.starts_with('/') {
        format!("file://{p}")
    } else {
        format!("file:///{p}")
    })
}

pub fn parse_rate(raw: &str) -> anyhow::Result<FrameRate> {
    let text = raw.trim().trim_end_matches('p').replace(',', ".");
    let bad = || anyhow::anyhow!("cadence invalide : {raw}");
    if let Some((num, den)) = text.split_once('/') {
        let (num, den) = (
            num.parse().map_err(|_| bad())?,
            den.parse().map_err(|_| bad())?,
        );
        return FrameRate::new(num, den).map_err(|_| bad());
    }
    if let Ok(n) = text.parse::<u32>() {
        return FrameRate::new(n, 1).map_err(|_| bad());
    }
    let value: f64 = text.parse().map_err(|_| bad())?;
    FrameRate::MASTER_RATES
        .into_iter()
        .find(|r| (r.as_f64() - value).abs() < 0.01)
        .ok_or_else(bad)
}

/// `1920x1080` (or `×`), `1080p`, `720p`, `2160p`.
pub fn parse_resolution(raw: &str) -> anyhow::Result<Resolution> {
    let bad = || anyhow::anyhow!("résolution invalide : {raw}");
    let text = raw.trim().to_lowercase();
    let (w, h) = match text.as_str() {
        "720p" => (1280, 720),
        "1080p" => (1920, 1080),
        "1440p" => (2560, 1440),
        "2160p" | "4k" => (3840, 2160),
        _ => {
            let (w, h) = text.split_once(['x', '×']).ok_or_else(bad)?;
            (w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?)
        }
    };
    Resolution::new(w, h).map_err(|_| bad())
}

fn value<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = args
        .next()
        .with_context(|| format!("{flag} attend une valeur"))?;
    raw.parse()
        .with_context(|| format!("valeur invalide pour {flag} : {raw}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> anyhow::Result<Options> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn defaults() {
        let o = parse_str("").unwrap();
        assert_eq!(o.spin_margin, None);
        assert_eq!(o.soak, None);
        assert_eq!(
            (o.rate, o.resolution),
            (FrameRate::FPS_50, Resolution::HD_1080)
        );
        assert!(o.inputs.is_empty());
        assert_eq!(o.journal_dir, Some(PathBuf::from("logs")));
        assert!(!o.help);
        assert_eq!(
            parse_str("--journal-dir /tmp/j").unwrap().journal_dir,
            Some(PathBuf::from("/tmp/j"))
        );
        assert_eq!(parse_str("--no-journal").unwrap().journal_dir, None);
        assert!(o.headphones);
        assert!(!parse_str("--no-headphones").unwrap().headphones);
        assert!(o.autosave);
        assert!(o.api);
        assert!(!parse_str("--no-api").unwrap().api);
        assert!(!parse_str("--no-autosave").unwrap().autosave);
        assert!(
            !parse_str("--soak 10").unwrap().autosave,
            "a soak keeps « Dernier »"
        );
        assert_eq!(o.project, None);
        assert_eq!(
            parse_str("--project show.vma").unwrap().project,
            Some(PathBuf::from("show.vma"))
        );
        assert!(parse_str("--project").is_err());
        assert!(
            !parse_str("--soak 10").unwrap().headphones,
            "never during a soak"
        );
        assert_eq!(o.record, None);
        assert_eq!(
            parse_str("--record /media/rec").unwrap().record,
            Some(PathBuf::from("/media/rec"))
        );
        assert!(parse_str("--record").is_err());
        assert_eq!(o.fullscreen, None);
        assert_eq!(
            parse_str("--fullscreen DP-2")
                .unwrap()
                .fullscreen
                .as_deref(),
            Some("DP-2")
        );
        assert!(parse_str("--fullscreen").is_err());
        assert_eq!(o.gpu_loss_grace, None);
        assert_eq!(
            parse_str("--gpu-loss-grace 3").unwrap().gpu_loss_grace,
            Some(Duration::from_secs(3))
        );
        assert!(parse_str("--gpu-loss-grace 1.5").is_err());
    }

    #[test]
    fn stream_and_sync_options() {
        let o = parse_str(
            "--stream rtmp://localhost/live --stream-key s3cr3t --sync-test --soak 60 \
             --sync-check rtmp://localhost/live/s3cr3t",
        )
        .unwrap();
        let stream = o.stream.unwrap();
        assert_eq!(stream.url, "rtmp://localhost/live");
        assert_eq!(stream.key.expose(), Some("s3cr3t"));
        assert!(!format!("{stream:?}").contains("s3cr3t"));
        assert!(o.sync_test);
        assert_eq!(
            o.soak.unwrap().sync_check.as_deref(),
            Some("rtmp://localhost/live/s3cr3t")
        );
        assert!(!parse_str("").unwrap().sync_test);
        if std::env::var_os(STREAM_KEY_ENV).is_none() {
            assert!(
                parse_str("--stream rtmp://localhost/live").is_err(),
                "no key"
            );
        }
        assert!(parse_str("--stream-key k").is_err());
        assert!(
            parse_str("--soak 9 --sync-check rtmp://h/a/k").is_err(),
            "no --sync-test"
        );
        assert!(
            parse_str("--sync-test --sync-check rtmp://h/a/k").is_err(),
            "no --soak"
        );
    }

    #[test]
    fn the_stream_key_never_reaches_the_report() {
        let args = "--stream rtmp://h/live --stream-key s3cr3t --sync-check rtmp://h/live/s3cr3t";
        let line = redacted(args.split(' ').map(String::from), Some("s3cr3t"));
        assert_eq!(
            line,
            "--stream rtmp://h/live --stream-key *** --sync-check rtmp://h/live/***"
        );
        // Even without knowing the key (taken from the environment, say).
        let line = redacted(["--stream-key".into(), "abc".into()], None);
        assert_eq!(line, "--stream-key ***");
    }

    #[test]
    fn soak_options() {
        let o = parse_str("--soak 600 --hostile --report r.md --spin-margin-us 500").unwrap();
        assert_eq!(o.spin_margin, Some(Duration::from_micros(500)));
        assert_eq!(
            o.soak,
            Some(SoakOptions {
                duration: Duration::from_secs(600),
                hostile: true,
                report: PathBuf::from("r.md"),
                sync_check: None,
                html_data: false,
                stinger: false,
                api_load: false,
                replay_load: None,
            })
        );
        let o = parse_str("--soak 60 --replay-load /ssd/f7").unwrap();
        assert_eq!(o.soak.unwrap().replay_load, Some(PathBuf::from("/ssd/f7")));
        assert!(parse_str("--replay-load /ssd/f7").is_err());
        let o =
            parse_str("--soak 60 --api-load --fullscreen 2 --fullscreen-source multiview").unwrap();
        assert!(o.soak.unwrap().api_load);
        assert_eq!(o.fullscreen_source, vma_engine::ViewSource::Multiview);
        assert!(parse_str("--api-load").is_err());
        assert!(parse_str("--fullscreen-source mosaic").is_err());
        let o = parse_str(
            "--soak 60 --html https://x.example/a.html --overlay-image o.png --overlays \
             --stinger s.mov --html-data",
        )
        .unwrap();
        assert!(o.overlays && o.soak.as_ref().is_some_and(|s| s.html_data && s.stinger));
        assert_eq!(o.overlay_image, Some(PathBuf::from("o.png")));
        assert_eq!(o.stinger, Some(PathBuf::from("s.mov")));
    }

    #[test]
    fn master_format_and_synthetic_inputs() {
        let o = parse_str("--rate 59.94 --resolution 720p --inputs 5 --input-rates 50,25,29.97")
            .unwrap();
        assert_eq!(
            (o.rate, o.resolution),
            (FrameRate::FPS_59_94, Resolution::HD_720)
        );
        let rates: Vec<_> = o.inputs.iter().map(|i| i.rate).collect();
        assert_eq!(
            rates,
            [
                FrameRate::FPS_50,
                FrameRate::FPS_25,
                FrameRate::FPS_29_97,
                FrameRate::FPS_50,
                FrameRate::FPS_25
            ]
        );
        assert!(o.inputs.iter().all(|i| i.resolution == Resolution::HD_720));
        let o = parse_str("--rate 30000/1001 --inputs 2 --input-resolution 640x360").unwrap();
        assert_eq!(o.rate, FrameRate::FPS_29_97);
        assert_eq!(
            o.inputs[1].rate,
            FrameRate::FPS_29_97,
            "master rate by default"
        );
        assert_eq!(o.inputs[0].resolution, Resolution::new(640, 360).unwrap());
        for (raw, rate) in [
            ("25", FrameRate::FPS_25),
            ("25p", FrameRate::FPS_25),
            ("29,97", FrameRate::FPS_29_97),
            ("60000/1001", FrameRate::FPS_59_94),
            ("24", FrameRate::new(24, 1).unwrap()),
        ] {
            assert_eq!(parse_rate(raw).unwrap(), rate, "{raw}");
        }
        assert_eq!(parse_resolution("1920×1080").unwrap(), Resolution::HD_1080);
    }

    #[test]
    fn sync_inputs_with_drifts_and_ndi() {
        let o =
            parse_str("--inputs 6 --sync-inputs --input-drifts 0,0,0,0,+100,-100 --ndi").unwrap();
        let drifts: Vec<_> = o.inputs.iter().map(|i| i.sync_drift_ppm).collect();
        assert_eq!(
            drifts,
            [Some(0), Some(0), Some(0), Some(0), Some(100), Some(-100)]
        );
        assert!(o.ndi);
        assert!(o.inputs.iter().all(|i| i.resolution == Resolution::HD_1080));
        let o = parse_str("--inputs 2 --sync-inputs").unwrap();
        assert!(o.inputs.iter().all(|i| i.sync_drift_ppm == Some(0)));
        assert!(
            parse_str("--inputs 2").unwrap().inputs[0]
                .sync_drift_ppm
                .is_none()
        );
        assert!(!parse_str("").unwrap().ndi);
        // The analyser needs flashes; one kind of them.
        assert!(parse_str("--inputs 2 --sync-inputs --soak 9 --sync-check rtmp://h/a/k").is_ok());
        assert!(parse_str("--inputs 2 --sync-inputs --sync-test").is_err());
        assert!(parse_str("--inputs 2 --input-drifts 100").is_err());
        assert!(parse_str("--inputs 2 --sync-inputs --input-drifts 5000").is_err());
        assert!(parse_str("--inputs 2 --sync-inputs --input-drifts abc").is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_str("--rate 24").is_err(), "not a master rate");
        assert!(parse_str("--rate 23.976").is_err());
        assert!(parse_str("--resolution 0x0").is_err());
        assert!(parse_str("--resolution big").is_err());
        assert!(parse_str("--inputs 65").is_err());
        assert!(parse_str("--input-rates 50,abc").is_err());
        assert!(parse_str("--soak").is_err());
        assert!(parse_str("--soak abc").is_err());
        assert!(parse_str("--hostile").is_err());
        assert!(parse_str("--frobnicate").is_err());
    }

    #[test]
    fn html_pages_are_addresses_or_local_files() {
        let o = parse_str("--html https://scores.example/board.html --html file:///tmp/a.html")
            .unwrap();
        assert_eq!(
            o.html,
            ["https://scores.example/board.html", "file:///tmp/a.html"]
        );
        let page = std::env::temp_dir().join(format!("vma-cli-{}.html", std::process::id()));
        std::fs::write(&page, "<html></html>").unwrap();
        let o = parse_str(&format!("--html {}", page.display())).unwrap();
        assert!(o.html[0].starts_with("file:///"), "{:?}", o.html);
        assert!(o.html[0].ends_with(&format!("vma-cli-{}.html", std::process::id())));
        assert!(parse_str("--html /no/such/page.html").is_err());
        let _ = std::fs::remove_file(page);
    }
}
