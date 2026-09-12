//! Complete private payloads for the first executable domain. Unsupported is an error, not
//! a payload-free tag that could accidentally compare equal.
use crate::screens::registry::{AppFx, AppMsg};
use crate::ui::machine::{Delivery, Fx};
use crate::ui::screen::ScreenEvent;
use serde_json::{json, Value};

fn focus(key: crate::ui::machine::FocusKey<u32>) -> Value { json!([key.entry.0,key.elem]) }
fn home_command(command: &crate::screens::registry::HomeCmd) -> Value {
    use crate::screens::registry::{HomeCmd,HomeTab};
    match command {
        HomeCmd::FocusGrid { row,col } => json!({"focus_grid":[row,col]}),
        HomeCmd::Hero => json!("hero"),
        HomeCmd::FocusStrip(tab) => json!({"focus_strip":match tab {
            HomeTab::Home => "home", HomeTab::Movies => "movies", HomeTab::Shows => "shows", HomeTab::Search => "search" }}),
        HomeCmd::Flip(delta) => json!({"flip":delta}),
        HomeCmd::SelectHero(index) => json!({"select_hero":index}),
        HomeCmd::ItemMenu => json!("itemmenu"),
    }
}

pub(crate) fn input(event: &crate::ui::machine::InputEvent<u32>) -> Result<Value, &'static str> {
    use crate::ui::machine::{InputKind,Key,Edge,Source};
    let body = match &event.kind {
        InputKind::Key { key, sym, wcode, edge, at_edge } => json!({"key":match key {
            Key::Up => "Up", Key::Down => "Down", Key::Left => "Left", Key::Right => "Right",
            Key::Ok => "Ok", Key::Back => "Back", Key::Other => "Other" },
            "sym":sym,"wcode":wcode,"edge":match edge { Edge::Down => "Down", Edge::Repeat => "Repeat", Edge::Up => "Up" },"at_edge":at_edge}),
        InputKind::Pointer { x,y,hit } | InputKind::Click { x,y,hit } | InputKind::Drag { x,y,hit } =>
            json!({"kind":match event.kind { InputKind::Pointer {..} => "pointer", InputKind::Click {..} => "click", _ => "drag" },
                "x_bits":x.to_bits(),"y_bits":y.to_bits(),"hit":hit}),
        InputKind::Wheel { dy } => json!({"wheel_bits":dy.to_bits()}),
        InputKind::PointerHidden => json!({"pointer_hidden":true}),
        InputKind::SystemKeyboard(up) => json!({"keyboard":up}),
        InputKind::Text(_) => return Err("unsupported Home text input"),
    };
    Ok(json!({"kind":"owned", "ms":event.at.ms,"dt_us":event.at.dt_us,
        "source":match event.source { Source::Sdl => "Sdl", Source::RemoteFifo => "RemoteFifo", Source::Script => "Script", Source::Replay => "Replay" },"body":body}))
}

pub(crate) fn decode_input(value: &Value) -> Result<crate::ui::machine::InputEvent<u32>, &'static str> {
    use crate::ui::machine::{InputEvent, InputKind, Key, Edge, Source, Tick};
    let number = |value: &Value| value.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or("invalid input integer");
    if value["kind"] != "owned" { return Err("unsupported controlled input"); }
    let body = &value["body"];
    let key = match body["key"].as_str() {
        Some("Up") => Key::Up, Some("Down") => Key::Down, Some("Left") => Key::Left,
        Some("Right") => Key::Right, _ => return Err("unsupported Home key"),
    };
    let edge = match body["edge"].as_str() {
        Some("Up") => Edge::Up, Some("Down") => Edge::Down, Some("Repeat") => Edge::Repeat,
        _ => return Err("invalid Home key edge"),
    };
    let source = match value["source"].as_str() {
        Some("Sdl") => Source::Sdl, Some("RemoteFifo") => Source::RemoteFifo,
        Some("Script") => Source::Script, Some("Replay") => Source::Replay,
        _ => return Err("invalid input source"),
    };
    let event = InputEvent { at: Tick { ms:number(&value["ms"])?, dt_us:number(&value["dt_us"])? }, source,
        kind: InputKind::Key { key, sym:number(&body["sym"])?, wcode:number(&body["wcode"])?, edge,
            at_edge:body["at_edge"].as_bool().ok_or("invalid input edge state")? } };
    let state = match edge { Edge::Up => 0, Edge::Down => 1, Edge::Repeat => 0x101 };
    let physical = super::super::bridge::key_input(number(&body["sym"])?, number(&body["wcode"])?,
        state, event.at, source);
    if input(&physical)? != input(&event)? { return Err("incoherent physical input"); }
    let mut canonical = value.clone();
    if let Some(object) = canonical.as_object_mut() { object.remove("f"); object.remove("t"); }
    if input(&event)? != canonical { return Err("noncanonical controlled input"); }
    Ok(event)
}

fn wire(value: &impl serde::Serialize) -> Result<Value, &'static str> {
    serde_json::to_value(value).map_err(|_| "invalid effect encoding")
}

pub(crate) fn app(effect: &AppFx) -> Result<Value, &'static str> {
    Ok(match effect {
        AppFx::Session(command) => json!({"session":wire(command)?}),
        AppFx::SessionEffect(effect) => json!({"session_effect":wire(effect)?}),
        AppFx::Store(id, command) => json!({"store":id.ord().0,"command":store(command)?}),
        AppFx::StoreWork(work) => json!({"work":work_value(work)?}),
        AppFx::Home(crate::screens::registry::HomeReq::FoldToHero) => json!({"home":"fold"}),
        _ => return Err("unsupported Home application effect"),
    })
}

fn work_value(work: &crate::stores::StoreWork) -> Result<Value, &'static str> {
    Ok(match work {
        crate::stores::StoreWork::Hubs => json!("hubs"),
        crate::stores::StoreWork::BrowseDiscovery => json!("discovery"),
        _ => return Err("unsupported Home store work"),
    })
}

fn store(command: &crate::stores::StoreCmd) -> Result<Value, &'static str> {
    use crate::stores::{StoreCmd, hubs::HubsCmd, browse::BrowseCmd};
    Ok(match command {
        StoreCmd::Hubs(HubsCmd::Reset) => json!({"hubs":"reset"}),
        StoreCmd::Hubs(HubsCmd::RefetchHubs) => json!({"hubs":"refetch"}),
        StoreCmd::Hubs(HubsCmd::Retry) => json!({"hubs":"retry"}),
        StoreCmd::Browse(BrowseCmd::Reset) => json!({"browse":"reset"}),
        StoreCmd::Browse(BrowseCmd::Discovery(result)) => crate::browse::record::encode(result),
        _ => return Err("unsupported Home store command"),
    })
}

pub(crate) fn message(message: &AppMsg) -> Result<Value, &'static str> {
    use crate::auth::owner::SessionEvent;
    Ok(match message {
        AppMsg::Session(event) => match event {
            SessionEvent::Command(command) => json!({"command":wire(command)?}),
            SessionEvent::Commit(reply) => json!({"commit":wire(reply)?}),
            SessionEvent::Pump => json!("pump"),
            _ => return Err("unsupported Home Session delivery"),
        },
        AppMsg::Store(command) => store(command)?,
        AppMsg::StoreWork(work) => work_value(work)?,
        AppMsg::HubsResult(result) => crate::pms::record::encode(result),
        AppMsg::Home(command) => json!({"home_command":home_command(command)}),
        _ => return Err("unsupported Home message"),
    })
}

pub(crate) fn encode(effect: &Fx<super::super::bridge::AppHost>) -> Result<Value, &'static str> {
    Ok(match effect {
        Fx::App(effect) => app(effect)?,
        Fx::Mount(id) => json!({"mount":id.0}),
        Fx::Unmount(id) => json!({"unmount":id.0}),
        Fx::Nav(crate::ui::machine::NavOp::Root(crate::screens::registry::AppArg::Home)) => json!({"root":"home"}),
        Fx::Timer { id, after_ms } => json!({"timer":id.0,"after_ms":after_ms}),
        Fx::CancelTimer(id) => json!({"cancel_timer":id.0}),
        Fx::Remember { group, elem } => json!({"group":group.0,"elem":elem}),
        Fx::Press(arm) => json!({"press":{"key":focus(arm.key),"from":match arm.from {
            crate::ui::machine::PressFrom::Key => "key", crate::ui::machine::PressFrom::Pointer => "pointer" },"holdable":arm.holdable}}),
        Fx::Log(line) => json!({"log":line.0}),
        Fx::Deliver(to, delivery) => {
            let body = match delivery {
                Delivery::Machine(msg) => message(msg)?,
                Delivery::Keyboard { up } => json!({"keyboard":up}),
                Delivery::Press { id, key, held } => json!({"press":id.0,"key":focus(*key),"held":held}),
                Delivery::Screen(event) => {
                    let body = match event {
                        ScreenEvent::Mount | ScreenEvent::Cover | ScreenEvent::Uncover |
                        ScreenEvent::Unmount | ScreenEvent::Suspend | ScreenEvent::Resume => Value::Null,
                        ScreenEvent::Enter(enter) => match enter {
                            crate::ui::screen::Enter::Restored => json!("restored"),
                            crate::ui::screen::Enter::Fresh { focus:target } => json!({"fresh":match target {
                                crate::ui::screen::FocusTarget::Elem(key) => json!({"elem":focus(*key)}),
                                crate::ui::screen::FocusTarget::ContainerGroup(group) => json!({"group":group.0}),
                            }}),
                        },
                        ScreenEvent::WillLeave(leave) => json!(match leave {
                            crate::ui::machine::Leave::ForGood => "for_good",
                            crate::ui::machine::Leave::Deeper => "deeper",
                        }),
                        ScreenEvent::Tick(tick) => json!({"ms":tick.ms,"dt_us":tick.dt_us}),
                        ScreenEvent::Timer(id) => json!(id.0),
                        ScreenEvent::PressHold(id) | ScreenEvent::PressCommit(id) => json!(id.0),
                        ScreenEvent::Activate(elem) => json!(elem),
                        ScreenEvent::Input(event) => input(event)?,
                        ScreenEvent::StoreChanged(store, generation) => json!([store.0,generation]),
                        ScreenEvent::FocusMoved { from, to, by } => json!({"from":from.map(focus),
                            "to":focus(*to),"by":match by { crate::ui::screen::By::Dir => "dir",
                                crate::ui::screen::By::Pointer => "pointer", crate::ui::screen::By::Restore => "restore",
                                crate::ui::screen::By::Reconcile => "reconcile" }}),
                        ScreenEvent::App(msg) => message(msg)?,
                        ScreenEvent::Async(req, msg) => json!({"req":req.0,"message":message(msg)?}),
                        _ => return Err("unsupported Home screen delivery"),
                    };
                    json!({"event":event.name(),"body":body})
                }
            };
            json!({"to":super::super::recorder::machine_name(*to),"delivery":body})
        }
        _ => return Err("unsupported Home library effect"),
    })
}
