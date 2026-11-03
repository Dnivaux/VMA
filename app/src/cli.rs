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
  --spin-margin-us <µs>  marge d'attente active de l'horloge maître (défaut : 1000)
  --record <dossier>     enregistre le Program dès le démarrage, dans ce dossier
                         (sans l'option : bouton « Enregistrer », dossier Vidéos)
  --fullscreen <écran>   montre le Program (après FTB) en plein écran dès le démarrage, sur
                         cet écran : son nom (DP-2, HDMI-A-1… : voir le journal au
                         démarrage ou la fenêtre Sorties) ou son numéro (1 = le premier)
  --stream <url>         streame le Program dès le démarrage vers ce serveur RTMP(S)
                         (rtmp://hôte/app) ; clé : --stream-key, ou la variable
                         d'environnement VMA_STREAM_KEY (jamais écrite dans le rapport)
  --stream-key <clé>     clé de stream de --stream
  --sync-test            mode synchro des sorties : une image blanche et un bip par seconde
  --sync-check <url>     pendant le soak, relit ce flux (ex. rtmp://hôte/app/clé) et mesure
                         le décalage bip − flash (exige --soak et --sync-test)
  --gpu-loss-grace <s>   GPU perdu : durée pendant laquelle les sorties reçoivent du noir
                         avant d'être finalisées et l'application fermée (défaut : 10)
  --soak <secondes>      soak : tourne N secondes, écrit un rapport, quitte
                         (code 0 = réussi, 2 = images manquées ou répétées)
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
    pub spin_margin: Option<Duration>,
    /// Record from start-up into this folder.
    pub record: Option<PathBuf>,
    /// Full screen from start-up on this screen (name, or number from 1).
    pub fullscreen: Option<String>,
    /// Stream from start-up to this RTMP(S) server.
    pub stream: Option<StreamOption>,
    /// Sync test on the outputs (flash and beep every second).
    pub sync_test: bool,
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
    let mut input_rates: Option<Vec<FrameRate>> = None;
    let mut input_resolution = None;
    let mut spin_margin = None;
    let mut record = None;
    let mut fullscreen = None;
    let mut gpu_loss_grace = None;
    let mut stream_url = None;
    let mut stream_key: Option<String> = None;
    let mut sync_test = false;
    let mut sync_check = None;
    let mut soak_secs = None;
    let mut hostile = false;
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
            "--input-rates" => {
                let raw: String = value(&mut args, &arg)?;
                input_rates = Some(
                    raw.split(',')
                        .map(|r| parse_rate(r.trim()))
                        .collect::<anyhow::Result<_>>()?,
                );
            }
            "--input-resolution" => {
                input_resolution = Some(parse_resolution(&value::<String>(&mut args, &arg)?)?);
            }
            "--spin-margin-us" => {
                let us: u64 = value(&mut args, &arg)?;
                spin_margin = Some(Duration::from_micros(us));
            }
            "--record" => record = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--fullscreen" => fullscreen = Some(value::<String>(&mut args, &arg)?),
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
        }),
        None if hostile || report.is_some() || sync_check.is_some() => {
            bail!("--hostile, --report et --sync-check exigent --soak")
        }
        None => None,
    };
    if sync_check.is_some() && !sync_test {
        bail!("--sync-check exige --sync-test (sans flash ni bip, rien à mesurer)");
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
    let inputs = rates
        .iter()
        .cycle()
        .take(inputs)
        .map(|&rate| Synthetic {
            rate,
            resolution: input_resolution.unwrap_or(resolution),
        })
        .collect();
    Ok(Options {
        rate,
        resolution,
        inputs,
        spin_margin,
        record,
        fullscreen,
        gpu_loss_grace,
        stream,
        sync_test,
        headphones: headphones && soak.is_none(),
        soak,
        journal_dir,
        help,
    })
}

/// `50`, `50p`, `29.97` (or `29,97`), `30000/1001`. A decimal is one of the master rates.
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
            })
        );
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
}
