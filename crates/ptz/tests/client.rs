//! G1 and G2 of `phase-7.md`: the client against the simulated camera, over UDP and TCP.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use vma_ptz::sim::SimCamera;
use vma_ptz::visca::MAX_MESSAGE;
use vma_ptz::{
    CameraAddress, CameraError, Direction, LensMove, LinkEvent, Preset, PtzClient, Speed, Timing,
    Transport, ViscaCommand,
};

const WAIT: Duration = Duration::from_secs(3);

fn timing() -> Timing {
    Timing {
        reply: Duration::from_millis(100),
        connect: Duration::from_millis(200),
        keepalive: Duration::from_millis(100),
        retries: [100, 150, 200, 250].map(Duration::from_millis),
    }
}

fn client(address: CameraAddress) -> (PtzClient, Receiver<LinkEvent>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let client = PtzClient::spawn("vma-ptz-test".into(), address, timing(), move |event| {
        let _ = tx.send(event);
    })
    .unwrap();
    (client, rx)
}

fn next_event(events: &Receiver<LinkEvent>) -> LinkEvent {
    events.recv_timeout(WAIT).expect("an event")
}

fn wait_reachable(events: &Receiver<LinkEvent>) {
    assert_eq!(next_event(events), LinkEvent::Reachable);
}

fn pan(direction: Direction) -> ViscaCommand {
    canon(ViscaCommand::PanTilt {
        direction,
        speed: Speed::DEFAULT,
    })
}

/// A command as the camera reads it back: speeds on the motors' scales.
fn canon(command: ViscaCommand) -> ViscaCommand {
    ViscaCommand::decode(command.encode(&mut [0; MAX_MESSAGE])).unwrap()
}

fn preset(n: u8) -> Preset {
    Preset::new(n).unwrap()
}

fn every_command() -> Vec<ViscaCommand> {
    let fast = Speed::FASTEST;
    vec![
        pan(Direction::UpLeft),
        pan(Direction::Stop),
        ViscaCommand::Zoom(LensMove::In(fast)),
        ViscaCommand::Zoom(LensMove::Stop),
        ViscaCommand::Focus(LensMove::Out(Speed::SLOWEST)),
        ViscaCommand::Focus(LensMove::Stop),
        ViscaCommand::FocusMode(vma_ptz::FocusMode::Manual),
        ViscaCommand::FocusOnePush,
        ViscaCommand::PresetSave(preset(4)),
        ViscaCommand::PresetRecall(preset(4)),
        ViscaCommand::Home,
    ]
}

/// Each command reaches the camera once, in order, to the byte; no event but the first.
fn commands_arrive_in_order(camera: &SimCamera) {
    let (client, events) = client(camera.address());
    wait_reachable(&events);
    let commands = every_command();
    for command in &commands {
        client.send(*command).unwrap();
        // One at a time: the queue would merge the moves (tested below).
        assert!(camera.wait_for(WAIT, |got| got.last() == Some(&canon(*command))));
    }
    assert_eq!(
        camera.commands(),
        commands.iter().map(|c| canon(*c)).collect::<Vec<_>>()
    );
    let raw: Vec<Vec<u8>> = camera
        .raw()
        .into_iter()
        .filter(|m| m.get(1) != Some(&0x09))
        .collect();
    let expected: Vec<Vec<u8>> = commands
        .iter()
        .map(|c| c.encode(&mut [0; MAX_MESSAGE]).to_vec())
        .collect();
    assert_eq!(raw, expected);
    assert_eq!(camera.sequence_errors(), 0);
    client.join();
    assert!(events.try_recv().is_err(), "nothing else to report");
}

#[test]
fn udp_commands_reach_the_camera_in_order() {
    let camera = SimCamera::udp().unwrap();
    commands_arrive_in_order(&camera);
    assert!(camera.resets() >= 1, "the sequence starts from a reset");
}

#[test]
fn tcp_commands_reach_the_camera_in_order() {
    let camera = SimCamera::tcp().unwrap();
    commands_arrive_in_order(&camera);
    assert_eq!(camera.resets(), 0, "no sequence over TCP");
}

#[test]
fn an_idle_camera_is_probed() {
    let camera = SimCamera::udp().unwrap();
    let (client, events) = client(camera.address());
    wait_reachable(&events);
    std::thread::sleep(Duration::from_millis(450));
    let probes = camera.received().len();
    assert!(
        (2..=8).contains(&probes),
        "{probes} probes in 450 ms at 100 ms"
    );
    assert!(camera.commands().is_empty());
    client.join();
}

#[test]
fn a_refused_command_is_reported_and_the_camera_stays_reachable() {
    for camera in [SimCamera::udp().unwrap(), SimCamera::tcp().unwrap()] {
        let (client, events) = client(camera.address());
        wait_reachable(&events);
        camera.refuse_with(Some(CameraError::NotExecutable));
        let focus = canon(ViscaCommand::Focus(LensMove::In(Speed::DEFAULT)));
        client.send(focus).unwrap();
        assert_eq!(
            next_event(&events),
            LinkEvent::Refused {
                command: focus,
                error: CameraError::NotExecutable
            }
        );
        camera.refuse_with(None);
        client.send(ViscaCommand::Home).unwrap();
        assert!(camera.wait_for(WAIT, |got| got.last() == Some(&ViscaCommand::Home)));
        // Refused once, not resent.
        assert_eq!(camera.commands(), [focus, ViscaCommand::Home]);
        client.join();
        assert!(events.try_recv().is_err());
    }
}

#[test]
fn a_lost_datagram_is_resent_after_a_reset() {
    let camera = SimCamera::udp().unwrap();
    let (client, events) = client(camera.address());
    wait_reachable(&events);
    let resets = camera.resets();
    camera.lose_next(1);
    let recall = ViscaCommand::PresetRecall(preset(2));
    client.send(recall).unwrap();
    assert!(camera.wait_for(WAIT, |got| got == [recall]));
    assert_eq!(camera.resets(), resets + 1);
    client.join();
    assert!(events.try_recv().is_err(), "a single loss is no silence");
}

#[test]
fn a_sequence_error_resets_the_sequence_and_resends() {
    let camera = SimCamera::udp().unwrap();
    let (client, events) = client(camera.address());
    wait_reachable(&events);
    let resets = camera.resets();
    camera.desync();
    client.send(ViscaCommand::Home).unwrap();
    assert!(camera.wait_for(WAIT, |got| got == [ViscaCommand::Home]));
    assert_eq!(camera.sequence_errors(), 1);
    assert_eq!(camera.resets(), resets + 1);
    client.join();
    assert!(events.try_recv().is_err());
}

#[test]
fn a_silent_camera_is_unreachable_then_found_again() {
    for camera in [SimCamera::udp().unwrap(), SimCamera::tcp().unwrap()] {
        let transport = camera.address().transport;
        let (client, events) = client(camera.address());
        wait_reachable(&events);
        camera.set_silent(true);
        let lost_at = Instant::now();
        let LinkEvent::Unreachable { reason } = next_event(&events) else {
            panic!("{transport:?}: unreachable expected");
        };
        // Next probe within 100 ms, then two silent attempts of 100 ms (slack for CI).
        assert!(lost_at.elapsed() < Duration::from_secs(1), "{transport:?}");
        assert!(reason.contains("pas de réponse"), "{transport:?}: {reason}");
        // A gesture meanwhile is dropped, not sent late.
        client.send(pan(Direction::Left)).unwrap();
        assert_eq!(
            next_event(&events),
            LinkEvent::Dropped {
                command: pan(Direction::Left)
            }
        );
        camera.set_silent(false);
        let back = Instant::now();
        wait_reachable(&events);
        assert!(
            back.elapsed() < Duration::from_millis(1500),
            "{transport:?}"
        );
        client.send(ViscaCommand::Home).unwrap();
        assert!(camera.wait_for(WAIT, |got| got.last() == Some(&ViscaCommand::Home)));
        assert!(!camera.commands().contains(&pan(Direction::Left)));
        client.join();
    }
}

#[test]
fn nothing_listening_is_unreachable() {
    for transport in [Transport::Udp, Transport::Tcp] {
        // A port just freed: nothing listens there.
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let address = CameraAddress {
            transport,
            host: "127.0.0.1".into(),
            port,
        };
        let (client, events) = client(address);
        assert!(
            matches!(next_event(&events), LinkEvent::Unreachable { .. }),
            "{transport:?}"
        );
        client.join();
    }
}

#[test]
fn moves_waiting_are_replaced_by_the_latest() {
    let camera = SimCamera::udp().unwrap();
    let (client, events) = client(camera.address());
    wait_reachable(&events);
    // The first command is lost: the client waits for its reply while the gestures pile up.
    camera.lose_next(1);
    client.send(ViscaCommand::Home).unwrap();
    for command in [
        pan(Direction::Left),
        pan(Direction::Right),
        ViscaCommand::Zoom(LensMove::In(Speed::DEFAULT)),
        pan(Direction::Stop),
        ViscaCommand::PresetRecall(preset(3)),
        ViscaCommand::Zoom(LensMove::Stop),
    ] {
        client.send(command).unwrap();
    }
    let expected = [
        ViscaCommand::Home,
        pan(Direction::Stop),
        ViscaCommand::PresetRecall(preset(3)),
        ViscaCommand::Zoom(LensMove::Stop),
    ];
    assert!(
        camera.wait_for(WAIT, |got| got.len() >= expected.len()),
        "{:?}",
        camera.commands()
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(camera.commands(), expected);
    client.join();
}

#[test]
fn late_completions_do_not_stand_for_an_accuse() {
    for camera in [SimCamera::udp().unwrap(), SimCamera::tcp().unwrap()] {
        let (client, events) = client(camera.address());
        wait_reachable(&events);
        // A recall ends 40 ms after its accusé; the commands after it go on meanwhile.
        camera.set_completion_delay(Duration::from_millis(40));
        let commands = [
            ViscaCommand::PresetRecall(preset(1)),
            ViscaCommand::PresetRecall(preset(2)),
            ViscaCommand::Home,
        ];
        for command in commands {
            client.send(command).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(camera.wait_for(WAIT, |got| got == commands));
        std::thread::sleep(Duration::from_millis(150));
        client.join();
        assert!(events.try_recv().is_err(), "no silence, no refusal");
    }
}
