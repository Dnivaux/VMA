//! Command-line options (hand-parsed: four flags do not justify a dependency).

use anyhow::{Context as _, bail};
use std::path::PathBuf;
use std::time::Duration;

pub const HELP: &str = "\
VMA — mélangeur vidéo live

Usage : vma [options]

Options :
  --spin-margin-us <µs>  marge d'attente active de l'horloge maître (défaut : 1000)
  --soak <secondes>      soak : tourne N secondes, écrit un rapport, quitte
                         (code 0 = réussi, 2 = images manquées ou répétées)
  --hostile              pendant le soak, agresse la fenêtre en continu
                         (redimensionne, maximise, plein écran, minimise, déplace)
  --report <chemin>      fichier du rapport de soak (défaut : soak-report.md)
  --journal-dir <dossier>
                         dossier du journal des commandes (défaut : logs)
  --no-journal           n'écrit pas de journal des commandes
  -h, --help             affiche cette aide
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub spin_margin: Option<Duration>,
    pub soak: Option<SoakOptions>,
    /// `None` = no journal.
    pub journal_dir: Option<PathBuf>,
    pub help: bool,
}

/// Default journal directory (spec `commands.md`).
pub const DEFAULT_JOURNAL_DIR: &str = "logs";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoakOptions {
    pub duration: Duration,
    pub hostile: bool,
    pub report: PathBuf,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> anyhow::Result<Options> {
    let mut spin_margin = None;
    let mut soak_secs = None;
    let mut hostile = false;
    let mut report = None;
    let mut help = false;
    let mut journal_dir = Some(PathBuf::from(DEFAULT_JOURNAL_DIR));

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--spin-margin-us" => {
                let us: u64 = value(&mut args, &arg)?;
                spin_margin = Some(Duration::from_micros(us));
            }
            "--soak" => soak_secs = Some(value::<u64>(&mut args, &arg)?),
            "--hostile" => hostile = true,
            "--report" => report = Some(PathBuf::from(value::<String>(&mut args, &arg)?)),
            "--journal-dir" => {
                journal_dir = Some(PathBuf::from(value::<String>(&mut args, &arg)?));
            }
            "--no-journal" => journal_dir = None,
            "-h" | "--help" => help = true,
            other => bail!("option inconnue : {other} (voir --help)"),
        }
    }

    let soak = match soak_secs {
        Some(secs) => Some(SoakOptions {
            duration: Duration::from_secs(secs),
            hostile,
            report: report.unwrap_or_else(|| PathBuf::from("soak-report.md")),
        }),
        None if hostile || report.is_some() => bail!("--hostile et --report exigent --soak"),
        None => None,
    };
    Ok(Options {
        spin_margin,
        soak,
        journal_dir,
        help,
    })
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
        assert_eq!(o.journal_dir, Some(PathBuf::from("logs")));
        assert!(!o.help);
        assert_eq!(
            parse_str("--journal-dir /tmp/j").unwrap().journal_dir,
            Some(PathBuf::from("/tmp/j"))
        );
        assert_eq!(parse_str("--no-journal").unwrap().journal_dir, None);
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
            })
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_str("--soak").is_err());
        assert!(parse_str("--soak abc").is_err());
        assert!(parse_str("--hostile").is_err());
        assert!(parse_str("--frobnicate").is_err());
    }
}
