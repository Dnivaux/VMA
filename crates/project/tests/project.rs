//! Project files (ADR 0013, `phase-5.md` E1, E2, E4).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vma_core::{FrameRate, Resolution};
use vma_engine::{
    Command, Format, InputSpec, OutputSpec, ProjectSnapshot, RtmpSpec, Secret, State, StateReader,
};
use vma_project::{
    AUTOSAVE_DELAY, Autosaver, ProjectError, Secrets, VERSION, autosaves, from_json,
    latest_autosave, open, save,
};

const HD50: Format = Format {
    rate: FrameRate::FPS_50,
    resolution: Resolution::HD_1080,
};

/// A fresh folder of its own under the system's temporary folder.
fn folder(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("vma-project-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn show(media: &Path) -> State {
    let mut s = State::new(HD50);
    for (spec, name) in [
        (
            InputSpec::File {
                path: media.join("clips/générique.mp4"),
            },
            "Générique",
        ),
        (
            InputSpec::Image {
                path: media.join("logo.png"),
            },
            "Logo",
        ),
    ] {
        s.apply(&Command::AddInput {
            spec,
            name: Some(name.into()),
        })
        .unwrap();
    }
    s.apply(&Command::AddOutput {
        spec: OutputSpec::Rtmp(RtmpSpec {
            url: "rtmp://live.example/app".into(),
            key: Secret::new("clé-secrète"),
            video_kbps: None,
        }),
        name: Some("Direct".into()),
    })
    .unwrap();
    s
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
}

fn rtmp_key(project: &ProjectSnapshot) -> &Secret {
    match &project.outputs[1].spec {
        OutputSpec::Rtmp(rtmp) => &rtmp.key,
        other => panic!("{other:?}"),
    }
}

/// E1: saved, opened again: the same project; the stream key is not in the file but comes
/// back from the keys of this machine, and not on another one.
#[test]
fn a_saved_project_opens_to_the_same_show_without_its_keys_in_the_file() {
    let dir = folder("roundtrip");
    let project = show(&dir).project();
    let mut secrets = Secrets::load(&dir.join("config/secrets.json"));
    let path = dir.join("émission.vma");
    save(&path, &project, &mut secrets).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("clé-secrète"), "{text}");
    assert!(text.contains(r#""format": "vma-project""#), "{text}");
    assert!(text.contains(&format!(r#""version": {VERSION}"#)), "{text}");
    assert!(
        !dir.join("émission.vma.tmp").exists(),
        "no temporary file left"
    );

    let keys = Secrets::load(&dir.join("config/secrets.json"));
    assert_eq!(open(&path, &keys).unwrap(), project);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join("config/secrets.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "keys readable by their owner only: {mode:o}"
        );
    }

    let elsewhere = Secrets::load(&dir.join("other-machine/secrets.json"));
    assert_eq!(
        *rtmp_key(&open(&path, &elsewhere).unwrap()),
        Secret::Redacted
    );
}

/// E2 (Q16): the show's folder moved, media and all: found by their relative paths.
#[test]
fn media_are_found_when_the_show_folder_moves() {
    let dir = folder("moved");
    let before = dir.join("avant");
    touch(&before.join("clips/générique.mp4"));
    touch(&before.join("logo.png"));
    let project = show(&before).project();
    let mut secrets = Secrets::default();
    save(&before.join("show.vma"), &project, &mut secrets).unwrap();
    let after = dir.join("après");
    std::fs::rename(&before, &after).unwrap();

    let opened = open(&after.join("show.vma"), &secrets).unwrap();
    let path = |n: usize| match &opened.inputs[n].spec {
        InputSpec::File { path } | InputSpec::Image { path } => path.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(path(0), after.join("clips/générique.mp4"));
    assert_eq!(path(1), after.join("logo.png"));

    // A medium missing from both places keeps its absolute path (signal lost at load).
    std::fs::remove_file(after.join("logo.png")).unwrap();
    let opened = open(&after.join("show.vma"), &secrets).unwrap();
    match &opened.inputs[1].spec {
        InputSpec::Image { path } => assert_eq!(*path, before.join("logo.png")),
        other => panic!("{other:?}"),
    }
}

/// E2: what is not a project, or a newer one, is refused with its reason.
#[test]
fn what_is_not_a_project_is_refused_with_its_reason() {
    let secrets = Secrets::default();
    let good =
        vma_project::to_json(&ProjectSnapshot::empty(HD50), None, &mut Secrets::default()).unwrap();
    assert!(from_json(&good, None, &secrets).is_ok());
    for (text, reason) in [
        ("{", "EOF"),
        ("[]", "objet"),
        (r#"{"format":"autre","version":1,"project":{}}"#, "format"),
        (r#"{"format":"vma-project","project":{}}"#, "version"),
        (
            &good.replacen("\"format\"", "\"surprise\": 1, \"format\"", 1),
            "surprise",
        ),
        (
            &good.replacen("\"ftb_ms\"", "\"surprise\": 1, \"ftb_ms\"", 1),
            "surprise",
        ),
    ] {
        let error = from_json(text, None, &secrets).unwrap_err().to_string();
        assert!(error.contains(reason), "{text}: {error}");
    }
    let newer = good.replacen(&format!("\"version\": {VERSION}"), "\"version\": 99", 1);
    let dir = folder("newer");
    let path = dir.join("futur.vma");
    std::fs::write(&path, newer).unwrap();
    match open(&path, &secrets) {
        Err(e @ ProjectError::Newer { version: 99, .. }) => {
            assert!(e.to_string().contains("mettre VMA à jour"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        open(&dir.join("absent.vma"), &secrets),
        Err(ProjectError::Io { .. })
    ));
}

/// E4: the show is saved 2 s after its last change and when VMA closes; nothing is written
/// while nothing changes; files turn.
#[test]
fn the_show_is_saved_after_a_change_and_at_the_close() {
    let dir = folder("autosave");
    let mut state = show(&dir);
    let (reader, publisher) = StateReader::standalone(state.clone());
    let secrets = Arc::new(Mutex::new(Secrets::load(&dir.join("secrets.json"))));
    let saver = Autosaver::spawn(reader, dir.clone(), Arc::clone(&secrets)).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(
        latest_autosave(&dir).is_none(),
        "the show as it started is not written"
    );

    state
        .apply(&Command::RenameInput {
            input: state.inputs[0].id,
            name: "Ouverture".into(),
        })
        .unwrap();
    publisher.publish(state.clone());
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        latest_autosave(&dir).is_none(),
        "not before {AUTOSAVE_DELAY:?} of quiet"
    );
    std::thread::sleep(AUTOSAVE_DELAY + Duration::from_millis(700));
    let newest = latest_autosave(&dir).expect("saved after the delay");
    let opened = open(&newest, &Secrets::load(&dir.join("secrets.json"))).unwrap();
    assert_eq!(
        opened,
        state.project(),
        "the key comes back from the machine's keys"
    );

    // A change just before closing is not lost.
    state
        .apply(&Command::RenameInput {
            input: state.inputs[1].id,
            name: "Logo 2".into(),
        })
        .unwrap();
    publisher.publish(state.clone());
    std::thread::sleep(Duration::from_millis(300));
    saver.finish();
    let opened = open(&latest_autosave(&dir).unwrap(), &Secrets::default()).unwrap();
    assert_eq!(opened.inputs[1].name, "Logo 2");
    assert_eq!(autosaves(&dir).len(), 1, "a young file is written over");
}
