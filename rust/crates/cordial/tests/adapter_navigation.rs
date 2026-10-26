use cordial::{
    controller::Event,
    sim::Desktop,
    ui::app::{Action, App, Live, Msg, Page, world::Conn},
};
use cordial_client::serial::PortInfo;
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

fn until(
    app: &mut App<Live>,
    rx: &mpsc::Receiver<Msg>,
    page: &Page,
    done: impl Fn(&App<Live>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(app) {
        let msg = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("application did not reach the expected state");
        app.update(msg);
        assert_eq!(&app.shown(), page);
    }
}

fn reconnect(world: Desktop, listed: impl Fn(&Desktop) -> Vec<PortInfo>) {
    let ports = listed(&world);
    let id = world.adapters()[0].0.lock().unwrap().status.id.clone();
    let other = world.adapters()[1].0.lock().unwrap().status.id.clone();
    assert!(ports[0].is_adapter(&id));
    assert!(!ports[0].is_adapter(&other));
    assert!(ports[1].is_adapter(&other));
    assert!(!ports[1].is_adapter(&id));

    let (tx, rx) = mpsc::channel();
    let mut app = App::new(world.live(Arc::new(move |msg| {
        let _ = tx.send(msg);
    })));
    until(&mut app, &rx, &Page::Overview, |app| {
        app.ready_adapters().len() == 2
    });
    app.enable_immediate();
    let page = Page::Adapter(id.clone());
    app.show(page.clone());
    for _ in 0..3 {
        app.act(Action::DisconnectAdapter(id.clone()));
        assert_eq!(app.shown(), page);
        assert_eq!(app.adapter(&id).unwrap().conn, Conn::Disconnected);

        app.update(Msg::Ports(Ok(listed(&world))));
        assert_eq!(app.shown(), page);
        assert_eq!(app.adapter(&id).unwrap().conn, Conn::Disconnected);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let msg = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("disconnected session did not close");
            let closed = matches!(&msg, Msg::Fleet(_, e) if matches!(**e, Event::Closed));
            app.update(msg);
            assert_eq!(app.shown(), page);
            assert_eq!(app.adapter(&id).unwrap().conn, Conn::Disconnected);
            if closed {
                break;
            }
        }
        app.act(Action::ConnectAdapter(id.clone()));
        assert_eq!(app.adapter(&id).unwrap().conn, Conn::Connecting);
        app.update(Msg::Ports(Ok(listed(&world))));
        assert_eq!(app.shown(), page);
        assert_eq!(app.adapter(&id).unwrap().conn, Conn::Connecting);
        until(&mut app, &rx, &page, |app| {
            app.adapter(&id).is_some_and(|a| a.ready)
        });
        assert!(app.adapter(&other).unwrap().ready);
    }
    assert_eq!(world.adapters()[0].sessions(), 4);
    assert_eq!(world.adapters()[1].sessions(), 1);

    app.act(Action::DisconnectAdapter(id.clone()));
    assert!(world.set_present(0, false));
    app.update(Msg::Ports(Ok(listed(&world))));
    assert!(app.adapter(&id).is_none());
    assert_eq!(app.shown(), Page::Overview);
    assert!(app.adapter(&other).unwrap().ready);

    assert!(world.set_present(0, true));
    app.update(Msg::Ports(Ok(listed(&world))));
    assert_eq!(app.adapter(&id).unwrap().conn, Conn::Disconnected);
    assert_eq!(app.shown(), Page::Overview);
    app.show(page.clone());
    app.act(Action::ConnectAdapter(id.clone()));
    until(&mut app, &rx, &page, |app| {
        app.adapter(&id).is_some_and(|a| a.ready)
    });
    assert_eq!(world.adapters()[0].sessions(), 5);
    assert_eq!(world.adapters()[1].sessions(), 1);
    app.quit();
    until(&mut app, &rx, &Page::Overview, App::done);
    assert!(
        world
            .adapters()
            .iter()
            .all(|adapter| adapter.handles() == 0)
    );
}

#[test]
fn simulated_adapter_navigation_survives_discovery_during_reconnect() {
    reconnect(Desktop::new(2), Desktop::ports);
}

#[test]
fn usb_serial_suffixes_preserve_adapter_navigation_and_unplug() {
    let world = Desktop::new(2);
    for (index, adapter) in world.adapters().iter().enumerate() {
        adapter.0.lock().unwrap().status.id = format!("{:016X}", 0xAB00 + index);
    }
    reconnect(world, |world| {
        world
            .ports()
            .into_iter()
            .map(|mut port| {
                port.serial = format!("{}-vial:12345678", port.serial.to_lowercase());
                port
            })
            .collect()
    });
}
