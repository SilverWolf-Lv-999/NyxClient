use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod tray_icon;

use nyx_client::{
    event::{
        api::{EventBus, SharedEventBus},
        implementations::{
            EventKeyboard, EventLifecycle, EventTick, EventWindows, KeyState, WindowsHookPublisher,
            WindowsSessionAction, WindowsSessionPublisher,
        },
    },
    modules::{Category, ModuleHandler, ToggleResult},
};
use tray_icon::TrayIcon;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RSHIFT};

const KEY_STATE_DOWN_MASK: i16 = 0x8000_u16 as i16;

fn main() -> Result<(), String> {
    let mut module_handler = ModuleHandler::with_builtin_modules();
    configure_default_key_binds(&mut module_handler);
    load_default_config(&mut module_handler);
    print_startup_summary(&module_handler);

    let event_bus = EventBus::shared();
    let modules = Arc::new(Mutex::new(module_handler));
    let running = Arc::new(AtomicBool::new(true));
    let pressed_bind_keys = Arc::new(Mutex::new(HashSet::new()));
    let _tray_icon = TrayIcon::start(Arc::clone(&running))?;

    subscribe_runtime_handlers(
        &event_bus,
        Arc::clone(&modules),
        Arc::clone(&pressed_bind_keys),
        Arc::clone(&running),
    )?;

    let _session_publisher = WindowsSessionPublisher::start(Arc::clone(&event_bus))
        .map_err(|error| format!("failed to start Windows session publisher: {error:?}"))?;
    let _hook_publisher = WindowsHookPublisher::start(Arc::clone(&event_bus))
        .map_err(|error| format!("failed to start Windows hook publisher: {error:?}"))?;

    println!(
        "NyxClient running. Press Right Shift to toggle ClickGui. Right-click the tray icon and choose 退出 to exit."
    );
    run_client_loop(&event_bus, &modules, &pressed_bind_keys, &running);

    Ok(())
}

fn configure_default_key_binds(module_handler: &mut ModuleHandler) {
    if let Some(module) = module_handler.get_mut("ClickGui") {
        if module.state().key_bind().is_none() {
            module
                .state_mut()
                .set_key_bind(Some(u32::from(VK_RSHIFT.0)));
        }
    }
}

fn load_default_config(module_handler: &mut ModuleHandler) {
    match module_handler.load_default_config_if_exists() {
        Ok(true) => {
            println!(
                "Loaded default config from {}.",
                ModuleHandler::default_config_file().display()
            );
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!(
                "Failed to load default config from {}: {error}",
                ModuleHandler::default_config_file().display()
            );
        }
    }
}

fn print_startup_summary(module_handler: &ModuleHandler) {
    println!(
        "NyxClient initialized with {} module(s).",
        module_handler.len()
    );
    for category in Category::ALL {
        println!(
            "{}: {}",
            category,
            module_handler.by_category(category).count()
        );
    }
}

fn subscribe_runtime_handlers(
    event_bus: &SharedEventBus,
    modules: Arc<Mutex<ModuleHandler>>,
    pressed_bind_keys: Arc<Mutex<HashSet<u32>>>,
    running: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut event_bus = event_bus
        .lock()
        .map_err(|_| "event bus lock was poisoned during startup".to_owned())?;

    event_bus.subscribe::<EventKeyboard, _>(100, move |event| {
        if handle_key_bind_transition(&modules, &pressed_bind_keys, event.vk_code, event.state) {
            event.cancel();
        }
    });

    event_bus.subscribe::<EventWindows, _>(100, move |event| {
        if matches!(
            event.action,
            WindowsSessionAction::ConsoleCtrlC
                | WindowsSessionAction::ConsoleBreak
                | WindowsSessionAction::ConsoleClose
                | WindowsSessionAction::Logoff
                | WindowsSessionAction::Shutdown
                | WindowsSessionAction::EndSession
        ) {
            running.store(false, Ordering::Release);
            event.cancel();
        }
    });

    Ok(())
}

fn run_client_loop(
    event_bus: &SharedEventBus,
    modules: &Arc<Mutex<ModuleHandler>>,
    pressed_bind_keys: &Arc<Mutex<HashSet<u32>>>,
    running: &AtomicBool,
) {
    let started_at = Instant::now();
    let mut frame = 0_u64;
    let mut last_tick = Instant::now();

    let mut startup = EventLifecycle::startup();
    EventBus::dispatch_shared(event_bus, &mut startup);

    while running.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
        poll_key_binds(modules, pressed_bind_keys);

        frame = frame.wrapping_add(1);
        let now = Instant::now();
        let delta = now.duration_since(last_tick);
        last_tick = now;

        let mut tick = EventTick::client(frame, delta);
        EventBus::dispatch_shared(event_bus, &mut tick);

        let mut lifecycle = EventLifecycle::running(frame, started_at);
        EventBus::dispatch_shared(event_bus, &mut lifecycle);
    }

    let mut closing = EventLifecycle::closing(started_at);
    EventBus::dispatch_shared(event_bus, &mut closing);

    let mut closed = EventLifecycle::closed(started_at);
    EventBus::dispatch_shared(event_bus, &mut closed);
}

fn poll_key_binds(
    modules: &Arc<Mutex<ModuleHandler>>,
    pressed_bind_keys: &Arc<Mutex<HashSet<u32>>>,
) {
    let bind_keys = match modules.lock() {
        Ok(modules) => modules
            .iter()
            .filter_map(|module| module.state().key_bind())
            .collect::<Vec<_>>(),
        Err(_) => return,
    };

    for key_code in bind_keys {
        let state = if is_key_down(key_code) {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        handle_key_bind_transition(modules, pressed_bind_keys, key_code, state);
    }
}

fn handle_key_bind_transition(
    modules: &Arc<Mutex<ModuleHandler>>,
    pressed_bind_keys: &Arc<Mutex<HashSet<u32>>>,
    key_code: u32,
    state: KeyState,
) -> bool {
    match state {
        KeyState::Pressed => {
            let already_pressed = match pressed_bind_keys.lock() {
                Ok(mut pressed_bind_keys) => !pressed_bind_keys.insert(key_code),
                Err(_) => return false,
            };

            if already_pressed {
                return has_key_binding(modules, key_code);
            }

            toggle_bound_modules(modules, key_code)
        }
        KeyState::Released => {
            if let Ok(mut pressed_bind_keys) = pressed_bind_keys.lock() {
                pressed_bind_keys.remove(&key_code);
            }
            false
        }
    }
}

fn has_key_binding(modules: &Arc<Mutex<ModuleHandler>>, key_code: u32) -> bool {
    modules.lock().is_ok_and(|modules| {
        modules
            .iter()
            .any(|module| module.state().key_bind() == Some(key_code))
    })
}

fn toggle_bound_modules(modules: &Arc<Mutex<ModuleHandler>>, key_code: u32) -> bool {
    let Ok(mut modules) = modules.lock() else {
        return false;
    };

    let mut handled = false;
    let mut should_save_config = false;
    for module in modules.iter_mut() {
        if module.state().key_bind() != Some(key_code) {
            continue;
        }

        handled = true;
        let name = module.name();
        if let ToggleResult::Changed {
            enabled,
            notify,
            save_config,
        } = module.toggle()
        {
            should_save_config |= save_config;
            if notify {
                println!("{name} {}", if enabled { "enabled" } else { "disabled" });
            }
        }
    }

    if should_save_config {
        if let Err(error) = modules.save_default_config() {
            eprintln!(
                "Failed to save default config to {}: {error}",
                ModuleHandler::default_config_file().display()
            );
        }
    }

    handled
}

fn is_key_down(key_code: u32) -> bool {
    unsafe { GetAsyncKeyState(key_code as i32) & KEY_STATE_DOWN_MASK != 0 }
}
