//! Mobile runner for Iced

use crate::window_man::WindowManager;
use core::slice;
use iced::Point;
use iced::advanced::widget::operation;
use iced::advanced::{Renderer, renderer};
use iced::futures::channel::{mpsc, oneshot};
use iced::futures::{StreamExt, task};
use iced::touch::Finger;
use iced::{Error, Executor, Program, Task, theme};
use iced_renderer;
use iced_runtime::{Action, image, system};
use iced_wgpu::graphics::{self, Compositor, Shell, compositor};
use iced_winit::core::Size;
use iced_winit::core::mouse;
use iced_winit::core::time::Instant;
use iced_winit::core::window;
use iced_winit::futures::{self, Runtime, subscription};
use iced_winit::runtime::user_interface::{self, UserInterface};
use iced_winit::{Clipboard, conversion, program, runtime};
use iced_winit::{Proxy, winit};
use rustc_hash::FxHashMap;
use std::borrow::Cow;
use std::mem::ManuallyDrop;
use std::sync::Arc;
#[cfg(target_os = "android")]
extern crate android_activity;
#[cfg(target_os = "android")]
use android_activity::AndroidApp;
#[cfg(target_os = "android")]
use std::sync::OnceLock;

#[cfg(target_os = "android")]
pub static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

pub mod window_man;

#[derive(Debug)]
enum Event<Message: 'static> {
    WindowCreated {
        id: window::Id,
        window: Arc<winit::window::Window>,
        exit_on_close_request: bool,
        make_visible: bool,
        on_open: oneshot::Sender<window::Id>,
    },
    EventLoopAwakened(winit::event::Event<Message>),
    Resumed,
    Suspended,
    Exit,
}
#[derive(Debug)]
enum Control {
    ChangeFlow(winit::event_loop::ControlFlow),
    Exit,
    Crash(Error),
    CreateWindow {
        id: window::Id,
        settings: window::Settings,
        title: String,
        monitor: Option<winit::monitor::MonitorHandle>,
        on_open: oneshot::Sender<window::Id>,
        scale_factor: f32,
    },
    SetAutomaticWindowTabbing(bool),
}

/// A trait for running an Iced application on mobile.
pub trait MobileAppRunner {
    /// Runs the application on mobile.
    fn mobile_run(self) -> Result<(), Error>;
}

impl<P> MobileAppRunner for P
where
    P: Program + 'static,
    P::Theme: iced::theme::Base,
{
    fn mobile_run(self) -> Result<(), Error> {
        run(self)
    }
}
/// Runs a [`Program`] with the provided settings.
pub fn run<P>(program: P) -> Result<(), Error>
where
    P: Program + 'static,
    P::Theme: iced::theme::Base,
{
    let settings = program.settings();
    let window_settings = program.window();
    let mut graphics_settings: iced_wgpu::graphics::Settings = settings.clone().into();
    graphics_settings.default_font = iced::Font::with_name("Fira Sans");

    use winit::event_loop::EventLoop;

    let mut builder = EventLoop::with_user_event();

    #[cfg(target_os = "android")]
    {
        use winit::platform::android::EventLoopBuilderExtAndroid;

        if let Some(app) = ANDROID_APP.get() {
            builder.with_android_app(app.clone());
        } else {
            panic!("AndroidApp not initialized in android_main");
        }
    }

    let event_loop = builder.build().expect("Create event loop");

    let display_handle = event_loop.owned_display_handle();
    let (proxy, worker) = Proxy::new(event_loop.create_proxy());
    let mut runtime = {
        let executor = P::Executor::new().map_err(Error::ExecutorCreationFailed)?;
        executor.spawn(worker);

        Runtime::new(executor, proxy.clone())
    };

    let (program, task) = runtime.enter(|| program::Instance::new(program));
    let is_daemon = window_settings.is_none();

    let task = if let Some(window_settings) = window_settings {
        let mut task = Some(task);

        let (_id, open) = runtime::window::open(window_settings);

        open.then(move |_| task.take().unwrap_or_else(Task::none))
    } else {
        task
    };

    if let Some(stream) = runtime::task::into_stream(task) {
        runtime.run(stream);
    }

    runtime.track(subscription::into_recipes(
        runtime.enter(|| program.subscription().map(iced_runtime::Action::Output)),
    ));

    let (event_sender, event_receiver) = mpsc::unbounded();
    let (control_sender, control_receiver) = mpsc::unbounded();

    let instance = Box::pin(run_instance::<P>(
        program,
        runtime,
        proxy.clone(),
        event_receiver,
        control_sender,
        display_handle,
        is_daemon,
        graphics_settings,
        settings.fonts,
    ));

    let context = task::Context::from_waker(task::noop_waker_ref());

    struct Runner<Message: 'static, F> {
        instance: std::pin::Pin<Box<F>>,
        context: task::Context<'static>,
        id: Option<String>,
        sender: mpsc::UnboundedSender<Event<iced_winit::runtime::Action<Message>>>,
        receiver: mpsc::UnboundedReceiver<Control>,
        error: Option<Error>,
        resumed: bool,
        pending_controls: Vec<Control>,
    }

    let runner = Runner {
        instance,
        context,
        id: settings.id,
        sender: event_sender,
        receiver: control_receiver,
        error: None,
        resumed: false,
        pending_controls: Vec::new(),
    };

    impl<Message, F> winit::application::ApplicationHandler<Action<Message>> for Runner<Message, F>
    where
        F: Future<Output = ()>,
    {
        fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            self.resumed = true;
            self.process_event(event_loop, Event::Resumed);

            let pending = std::mem::take(&mut self.pending_controls);

            for control in pending {
                self.handle_control(event_loop, control);
            }
        }

        fn suspended(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            self.resumed = false;
            self.process_event(event_loop, Event::Suspended);
        }

        fn new_events(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            cause: winit::event::StartCause,
        ) {
            #[cfg(target_os = "android")]
            if cause == winit::event::StartCause::Init {
                return;
            }

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::NewEvents(cause)),
            );
        }

        fn window_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            window_id: winit::window::WindowId,
            event: winit::event::WindowEvent,
        ) {
            #[cfg(target_os = "windows")]
            let is_move_or_resize = matches!(
                event,
                winit::event::WindowEvent::Resized(_) | winit::event::WindowEvent::Moved(_)
            );

            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::WindowEvent { window_id, event }),
            );

            // TODO: Remove when unnecessary
            // On Windows, we emulate an `AboutToWait` event after every `Resized` event
            // since the event loop does not resume during resize interaction.
            // More details: https://github.com/rust-windowing/winit/issues/3272
            #[cfg(target_os = "windows")]
            {
                if is_move_or_resize {
                    self.process_event(
                        event_loop,
                        Event::EventLoopAwakened(winit::event::Event::AboutToWait),
                    );
                }
            }
        }

        fn user_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            action: Action<Message>,
        ) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::UserEvent(action)),
            );
        }

        fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
            self.process_event(
                event_loop,
                Event::EventLoopAwakened(winit::event::Event::AboutToWait),
            );
        }
    }

    impl<Message, F> Runner<Message, F>
    where
        F: Future<Output = ()>,
    {
        fn process_event(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            event: Event<Action<Message>>,
        ) {
            if event_loop.exiting() {
                return;
            }

            self.sender.start_send(event).expect("Send event");

            loop {
                let poll = self.instance.as_mut().poll(&mut self.context);

                match poll {
                    task::Poll::Pending => match self.receiver.try_next() {
                        Ok(Some(control)) => {
                            #[cfg(target_os = "android")]
                            if matches!(control, Control::CreateWindow { .. }) && !self.resumed {
                                self.pending_controls.push(control);
                                continue;
                            }

                            self.handle_control(event_loop, control);
                        }
                        _ => {
                            break;
                        }
                    },
                    task::Poll::Ready(_) => {
                        event_loop.exit();
                        break;
                    }
                };
            }
        }

        fn handle_control(
            &mut self,
            event_loop: &winit::event_loop::ActiveEventLoop,
            control: Control,
        ) {
            match control {
                Control::ChangeFlow(flow) => {
                    use winit::event_loop::ControlFlow;

                    match (event_loop.control_flow(), flow) {
                        (ControlFlow::WaitUntil(current), ControlFlow::WaitUntil(new))
                            if current < new => {}
                        (ControlFlow::WaitUntil(target), ControlFlow::Wait)
                            if target > Instant::now() => {}
                        _ => {
                            event_loop.set_control_flow(flow);
                        }
                    }
                }
                Control::CreateWindow {
                    id,
                    settings,
                    title,
                    scale_factor,
                    monitor,
                    on_open,
                } => {
                    let exit_on_close_request = settings.exit_on_close_request;

                    let visible = settings.visible;

                    #[cfg(target_arch = "wasm32")]
                    let target = settings.platform_specific.target.clone();

                    #[allow(unused_mut)]
                    let mut window_attributes = conversion::window_attributes(
                        settings,
                        &title,
                        scale_factor,
                        monitor.or(event_loop.primary_monitor()),
                        self.id.clone(),
                    )
                    .with_visible(false);

                    #[cfg(target_os = "ios")]
                    {
                        let size: Option<winit::dpi::Size> = None;
                        use winit::platform::ios::WindowAttributesExtIOS;
                        window_attributes = window_attributes
                            .with_prefers_home_indicator_hidden(true)
                            .with_prefers_status_bar_hidden(true);
                        window_attributes.inner_size = size;
                        window_attributes.max_inner_size = size;
                        window_attributes.min_inner_size = size;
                    }

                    tracing::info!("Window attributes for id `{id:#?}`: {window_attributes:#?}");

                    // On macOS, the `position` in `WindowAttributes` represents the "inner"
                    // position of the window; while on other platforms it's the "outer" position.
                    // We fix the inconsistency on macOS by positioning the window after creation.
                    #[cfg(target_os = "macos")]
                    let mut window_attributes = window_attributes;

                    #[cfg(target_os = "macos")]
                    let position = window_attributes.position.take();

                    let window = event_loop
                        .create_window(window_attributes)
                        .expect("Create window");

                    #[cfg(target_os = "ios")]
                    {
                        use winit::platform::ios::{ValidOrientations, WindowExtIOS};
                        window.set_scale_factor(window.scale_factor());
                        window.set_valid_orientations(ValidOrientations::LandscapeAndPortrait);
                    }

                    #[cfg(target_os = "macos")]
                    if let Some(position) = position {
                        window.set_outer_position(position);
                    }

                    #[cfg(target_arch = "wasm32")]
                    {
                        use winit::platform::web::WindowExtWebSys;

                        let canvas = window.canvas().expect("Get window canvas");

                        let _ = canvas
                            .set_attribute("style", "display: block; width: 100%; height: 100%");

                        let window = web_sys::window().unwrap();
                        let document = window.document().unwrap();
                        let body = document.body().unwrap();

                        let target = target.and_then(|target| {
                            body.query_selector(&format!("#{target}"))
                                .ok()
                                .unwrap_or(None)
                        });

                        match target {
                            Some(node) => {
                                let _ = node
                                    .replace_with_with_node_1(&canvas)
                                    .expect(&format!("Could not replace #{}", node.id()));
                            }
                            None => {
                                let _ = body
                                    .append_child(&canvas)
                                    .expect("Append canvas to HTML body");
                            }
                        };
                    }

                    self.process_event(
                        event_loop,
                        Event::WindowCreated {
                            id,
                            window: Arc::new(window),
                            exit_on_close_request,
                            make_visible: visible,
                            on_open,
                        },
                    );
                }
                Control::Exit => {
                    self.process_event(event_loop, Event::Exit);
                    event_loop.exit();
                }
                Control::Crash(error) => {
                    self.error = Some(error);
                    event_loop.exit();
                }
                Control::SetAutomaticWindowTabbing(_enabled) => {
                    #[cfg(target_os = "macos")]
                    {
                        use winit::platform::macos::ActiveEventLoopExtMacOS;
                        event_loop.set_allows_automatic_window_tabbing(_enabled);
                    }
                }
            }
        }
    }

    let mut runner = runner;
    let _ = event_loop.run_app(&mut runner);

    runner.error.map(Err).unwrap_or(Ok(()))
}

async fn run_instance<P>(
    mut program: program::Instance<P>,
    mut runtime: Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    mut proxy: Proxy<P::Message>,
    mut event_receiver: mpsc::UnboundedReceiver<Event<Action<P::Message>>>,
    mut control_sender: mpsc::UnboundedSender<Control>,
    display_handle: winit::event_loop::OwnedDisplayHandle,
    is_daemon: bool,
    graphics_settings: graphics::Settings,
    default_fonts: Vec<Cow<'static, [u8]>>,
) where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    use rustc_hash::FxHashMap;
    use winit::event;
    use winit::event_loop::ControlFlow;

    let mut windowager = WindowManager::new();
    let mut is_window_opening = !is_daemon;
    let mut system_theme = theme::Mode::Dark;
    let mut compositor = None;
    let mut events = Vec::new();
    let mut messages = Vec::new();
    let mut actions = 0;

    let mut ui_caches = FxHashMap::default();
    let mut user_interfaces = ManuallyDrop::new(FxHashMap::default());
    let mut clipboard = Clipboard::unconnected();

    tracing::info!("default_fonts amount: {}", default_fonts.len());
    if let Ok(mut s) = graphics::text::font_system().write() {
        tracing::info!("font_system amount: {}", s.raw().db().len());
    } else {
        tracing::info!("Could not access font_system");
    }
    'next_event: loop {
        // Empty the queue if possible
        let event = if let Ok(event) = event_receiver.try_next() {
            event
        } else {
            event_receiver.next().await
        };

        let Some(event) = event else {
            break;
        };

        match event {
            Event::WindowCreated {
                id,
                window,
                exit_on_close_request,
                make_visible,
                on_open,
            } => {
                tracing::info!(
                    "Window created: id={:?}, exit_on_close_request={}, make_visible={}",
                    id,
                    exit_on_close_request,
                    make_visible
                );

                if compositor.is_none() {
                    let (compositor_sender, compositor_receiver) = oneshot::channel();

                    let create_compositor = {
                        let window = window.clone();
                        let display_handle = display_handle.clone();
                        let proxy = proxy.clone();
                        let default_fonts = default_fonts.clone();

                        async move {
                            let shell = Shell::new(proxy.clone());
                            use iced_renderer::graphics::Compositor;
                            unsafe {
                                _ = std::env::set_var("WGPU_BACKEND", "Metal");
                            }
                            let mut compositor =
                                <P::Renderer as compositor::Default>::Compositor::new(
                                    graphics_settings,
                                    display_handle,
                                    window,
                                    shell,
                                )
                                .await;

                            if let Ok(compositor) = &mut compositor {
                                for font in default_fonts {
                                    compositor.load_font(font.clone());
                                }
                            }

                            compositor_sender
                                .send(compositor)
                                .ok()
                                .expect("Send compositor");

                            // HACK! Send a proxy event on completion to trigger
                            // a runtime re-poll
                            // TODO: Send compositor through proxy (?)
                            {
                                let (sender, _receiver) = oneshot::channel();

                                proxy.send_action(Action::Window(
                                    runtime::window::Action::GetLatest(sender),
                                ));
                            }
                        }
                    };

                    runtime.block_on(create_compositor);

                    match compositor_receiver.await.expect("Wait for compositor") {
                        Ok(new_compositor) => {
                            compositor = Some(new_compositor);
                        }
                        Err(error) => {
                            let _ = control_sender.start_send(Control::Crash(
                                Error::WindowCreationFailed(Box::new(error)),
                            ));
                            continue;
                        }
                    }
                }

                let window_theme = window
                    .theme()
                    .map(conversion::theme_mode)
                    .unwrap_or_default();

                let is_first = windowager.is_empty();
                let window = windowager.insert(
                    id,
                    window,
                    &program,
                    compositor.as_mut().expect("Compositor must be initialized"),
                    exit_on_close_request,
                    system_theme,
                );

                window
                    .raw
                    .set_theme(conversion::window_theme(window.state.theme_mode()));

                let logical_size = window.state.logical_size();

                let _ = user_interfaces.insert(
                    id,
                    build_user_interface(
                        &program,
                        user_interface::Cache::default(),
                        &mut window.renderer,
                        logical_size,
                        id,
                    ),
                );
                let _ = ui_caches.insert(id, user_interface::Cache::default());

                if make_visible {
                    window.raw.set_visible(true);
                }

                events.push((
                    id,
                    iced::Event::Window(window::Event::Opened {
                        position: window.position(),
                        size: window.logical_size(),
                    }),
                ));

                if clipboard.window_id().is_none() {
                    clipboard = Clipboard::connect(window.raw.clone());
                }

                if let Ok(mut s) = graphics::text::font_system().write() {
                    tracing::info!("font_system amount: {}", s.raw().db().len());
                } else {
                    tracing::info!("Could not access font_system");
                }
                let _ = on_open.send(id);
                is_window_opening = false;
            }
            Event::Resumed => {
                tracing::info!("Application resumed");
            }
            Event::Suspended => {
                tracing::info!("Application suspended");
            }
            Event::EventLoopAwakened(event) => {
                match event {
                    event::Event::NewEvents(event::StartCause::Init) => {
                        for (_id, window) in windowager.iter_mut() {
                            window.raw.request_redraw();
                        }
                    }
                    event::Event::NewEvents(event::StartCause::ResumeTimeReached { .. }) => {
                        let now = Instant::now();

                        for (_id, window) in windowager.iter_mut() {
                            if let Some(redraw_at) = window.redraw_at
                                && redraw_at <= now
                            {
                                window.raw.request_redraw();
                                window.redraw_at = None;
                            }
                        }

                        if let Some(redraw_at) = windowager.redraw_at() {
                            let _ = control_sender
                                .start_send(Control::ChangeFlow(ControlFlow::WaitUntil(redraw_at)));
                        } else {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(ControlFlow::Wait));
                        }
                    }
                    event::Event::UserEvent(action) => {
                        run_action(
                            action,
                            &program,
                            &mut runtime,
                            &mut compositor,
                            &mut events,
                            &mut messages,
                            &mut clipboard,
                            &mut control_sender,
                            &mut user_interfaces,
                            &mut windowager,
                            &mut ui_caches,
                            &mut is_window_opening,
                            &mut system_theme,
                        );
                        actions += 1;
                    }
                    event::Event::WindowEvent {
                        window_id: id,
                        event: event::WindowEvent::RedrawRequested,
                        ..
                    } => {
                        tracing::debug!("RedrawRequested for window id={:?}", id);

                        let Some(mut current_compositor) = compositor.as_mut() else {
                            continue;
                        };

                        let Some((id, mut window)) = windowager.get_mut_alias(id) else {
                            continue;
                        };

                        let physical_size = window.state.physical_size();
                        let mut logical_size = window.state.logical_size();

                        tracing::info!(
                            "Redraw: physical_size={:?}, logical_size={:?}, viewport={:?}",
                            physical_size,
                            logical_size,
                            window.state.viewport()
                        );

                        if physical_size.width == 0 || physical_size.height == 0 {
                            continue;
                        }

                        if window.surface_version != window.state.surface_version() {
                            tracing::debug!(
                                "Surface reconfiguration needed for window id={:?}",
                                id
                            );

                            let ui = user_interfaces.remove(&id).expect("Remove user interface");

                            let _ = user_interfaces
                                .insert(id, ui.relayout(logical_size, &mut window.renderer));

                            current_compositor.configure_surface(
                                &mut window.surface,
                                physical_size.width,
                                physical_size.height,
                            );

                            window.surface_version = window.state.surface_version();
                        }

                        let redraw_event =
                            iced::Event::Window(window::Event::RedrawRequested(Instant::now()));

                        let cursor = window.state.cursor();

                        let mut interface =
                            user_interfaces.get_mut(&id).expect("Get user interface");

                        // let interact_span = debug::interact(id);
                        let mut redraw_count = 0;

                        let state = loop {
                            let message_count = messages.len();
                            let (state, _) = interface.update(
                                slice::from_ref(&redraw_event),
                                cursor,
                                &mut window.renderer,
                                &mut clipboard,
                                &mut messages,
                            );

                            if message_count == messages.len() && !state.has_layout_changed() {
                                break state;
                            }

                            if redraw_count >= 2 {
                                tracing::warn!(
                                    "More than 3 consecutive RedrawRequested events \
                                    produced layout invalidation"
                                );

                                break state;
                            }

                            redraw_count += 1;

                            if !messages.is_empty() {
                                let caches: FxHashMap<_, _> =
                                    ManuallyDrop::into_inner(user_interfaces)
                                        .into_iter()
                                        .map(|(id, interface)| (id, interface.into_cache()))
                                        .collect();

                                let actions = update(&mut program, &mut runtime, &mut messages);

                                user_interfaces = ManuallyDrop::new(build_user_interfaces(
                                    &program,
                                    &mut windowager,
                                    caches,
                                ));

                                for action in actions {
                                    // Defer all window actions to avoid compositor
                                    // race conditions while redrawing
                                    if let Action::Window(_) = action {
                                        proxy.send_action(action);
                                        continue;
                                    }

                                    run_action(
                                        action,
                                        &program,
                                        &mut runtime,
                                        &mut compositor,
                                        &mut events,
                                        &mut messages,
                                        &mut clipboard,
                                        &mut control_sender,
                                        &mut user_interfaces,
                                        &mut windowager,
                                        &mut ui_caches,
                                        &mut is_window_opening,
                                        &mut system_theme,
                                    );
                                }

                                for (window_id, window) in windowager.iter_mut() {
                                    // We are already redrawing this window
                                    if window_id == id {
                                        continue;
                                    }

                                    window.raw.request_redraw();
                                }

                                let Some(next_compositor) = compositor.as_mut() else {
                                    continue 'next_event;
                                };

                                current_compositor = next_compositor;
                                window = windowager.get_mut(id).unwrap();

                                // Window scale factor changed during a redraw request
                                if logical_size != window.state.logical_size() {
                                    logical_size = window.state.logical_size();

                                    tracing::debug!(
                                        "Window scale factor changed during a redraw request"
                                    );

                                    let ui =
                                        user_interfaces.remove(&id).expect("Remove user interface");

                                    // let layout_span = debug::layout(id);
                                    let _ = user_interfaces.insert(
                                        id,
                                        ui.relayout(logical_size, &mut window.renderer),
                                    );
                                    // layout_span.finish();
                                }

                                interface = user_interfaces.get_mut(&id).unwrap();
                            }
                        };
                        // interact_span.finish();

                        // let draw_span = debug::draw(id);
                        interface.draw(
                            &mut window.renderer,
                            window.state.theme(),
                            &renderer::Style {
                                text_color: window.state.text_color(),
                            },
                            cursor,
                        );
                        // draw_span.finish();

                        if let user_interface::State::Updated {
                            redraw_request,
                            input_method,
                            mouse_interaction,
                            ..
                        } = state
                        {
                            window.request_redraw(redraw_request);
                            window.request_input_method(input_method);
                            window.update_mouse(mouse_interaction);
                        }

                        runtime.broadcast(subscription::Event::Interaction {
                            window: id,
                            event: redraw_event,
                            status: iced::event::Status::Ignored,
                        });

                        window.draw_preedit();

                        // let present_span = debug::present(id);
                        match current_compositor.present(
                            &mut window.renderer,
                            &mut window.surface,
                            window.state.viewport(),
                            window.state.background_color(),
                            || window.raw.pre_present_notify(),
                        ) {
                            Ok(()) => {
                                tracing::debug!("Present successful for window id={:?}", id);
                            }
                            Err(error) => match error {
                                compositor::SurfaceError::OutOfMemory => {
                                    panic!("{error:?}");
                                }
                                compositor::SurfaceError::Outdated
                                | compositor::SurfaceError::Lost => {
                                    tracing::warn!(
                                        "Surface lost/outdated for window id={:?}, reconfiguring",
                                        id
                                    );
                                    // present_span.finish();

                                    // Reconfigure surface and try redrawing
                                    let physical_size = window.state.physical_size();

                                    if error == compositor::SurfaceError::Lost {
                                        window.surface = current_compositor.create_surface(
                                            window.raw.clone(),
                                            physical_size.width,
                                            physical_size.height,
                                        );
                                    } else {
                                        current_compositor.configure_surface(
                                            &mut window.surface,
                                            physical_size.width,
                                            physical_size.height,
                                        );
                                    }

                                    window.raw.request_redraw();
                                }
                                _ => {
                                    tracing::error!(
                                        "Present error {:?} for window id={:?}",
                                        error,
                                        id
                                    );

                                    // Try rendering all windows again next frame.
                                    for (_id, window) in windowager.iter_mut() {
                                        window.raw.request_redraw();
                                    }
                                }
                            },
                        }
                    }
                    event::Event::WindowEvent {
                        event: window_event,
                        window_id,
                    } => {
                        if !is_daemon
                            && matches!(window_event, winit::event::WindowEvent::Destroyed)
                            && !is_window_opening
                            && windowager.is_empty()
                        {
                            control_sender
                                .start_send(Control::Exit)
                                .expect("Send control action");

                            continue;
                        }

                        let Some((id, window)) = windowager.get_mut_alias(window_id) else {
                            continue;
                        };

                        match window_event {
                            winit::event::WindowEvent::Resized(_) => {
                                window.raw.request_redraw();
                            }
                            winit::event::WindowEvent::ThemeChanged(theme) => {
                                let mode = conversion::theme_mode(theme);

                                if mode != system_theme {
                                    system_theme = mode;

                                    runtime
                                        .broadcast(subscription::Event::SystemThemeChanged(mode));
                                }
                            }
                            winit::event::WindowEvent::Touch(touch) => {
                                let finger_id = Finger(touch.id);
                                let position = iced::Point::new(
                                    touch.location.x as f32,
                                    touch.location.y as f32,
                                );
                                let touch_event = match touch.phase {
                                    winit::event::TouchPhase::Started => {
                                        iced::touch::Event::FingerPressed {
                                            id: finger_id,
                                            position,
                                        }
                                    }
                                    winit::event::TouchPhase::Ended
                                    | winit::event::TouchPhase::Cancelled => {
                                        iced::touch::Event::FingerLifted {
                                            id: finger_id,
                                            position,
                                        }
                                    }
                                    winit::event::TouchPhase::Moved => {
                                        iced::touch::Event::FingerMoved {
                                            id: finger_id,
                                            position,
                                        }
                                    }
                                };

                                tracing::debug!(
                                    "Touch event: id={:?}, phase={:?}, position={:?}",
                                    finger_id,
                                    touch.phase,
                                    position
                                );
                                events.push((id, iced::Event::Touch(touch_event)));

                                window.state.update(&program, &window.raw, &window_event);
                            }
                            _ => {}
                        }

                        if matches!(window_event, winit::event::WindowEvent::CloseRequested)
                            && window.exit_on_close_request
                        {
                            run_action(
                                Action::Window(runtime::window::Action::Close(id)),
                                &program,
                                &mut runtime,
                                &mut compositor,
                                &mut events,
                                &mut messages,
                                &mut clipboard,
                                &mut control_sender,
                                &mut user_interfaces,
                                &mut windowager,
                                &mut ui_caches,
                                &mut is_window_opening,
                                &mut system_theme,
                            );
                        } else {
                            window.state.update(&program, &window.raw, &window_event);

                            if let Some(event) = conversion::window_event(
                                window_event,
                                window.state.scale_factor(),
                                window.state.modifiers(),
                            ) {
                                events.push((id, event));
                            }
                        }
                    }
                    event::Event::AboutToWait => {
                        if actions > 0 {
                            proxy.free_slots(actions);
                            actions = 0;
                        }

                        if events.is_empty() && messages.is_empty() && windowager.is_idle() {
                            continue;
                        }

                        let mut uis_stale = false;

                        for (id, window) in windowager.iter_mut() {
                            // let interact_span = debug::interact(id);
                            let mut window_events = vec![];

                            events.retain(|(window_id, event)| {
                                if *window_id == id {
                                    window_events.push(event.clone());
                                    false
                                } else {
                                    true
                                }
                            });

                            if window_events.is_empty() && messages.is_empty() {
                                continue;
                            }

                            let (ui_state, statuses) = user_interfaces
                                .get_mut(&id)
                                .expect("Get user interface")
                                .update(
                                    &window_events,
                                    window.state.cursor(),
                                    &mut window.renderer,
                                    &mut clipboard,
                                    &mut messages,
                                );

                            #[cfg(target_os = "ios")]
                            let had_events = !window_events.is_empty();

                            #[cfg(feature = "unconditional-rendering")]
                            window.request_redraw(window::RedrawRequest::NextFrame);

                            match ui_state {
                                user_interface::State::Updated {
                                    redraw_request: _redraw_request,
                                    mouse_interaction,
                                    ..
                                } => {
                                    window.update_mouse(mouse_interaction);

                                    #[cfg(not(feature = "unconditional-rendering"))]
                                    window.request_redraw(_redraw_request);

                                    #[cfg(target_os = "ios")]
                                    if had_events
                                        && matches!(_redraw_request, window::RedrawRequest::Wait)
                                    {
                                        tracing::debug!(
                                            "iOS redraw fallback: forcing redraw after events"
                                        );
                                        window.raw.request_redraw();
                                    }
                                }
                                user_interface::State::Outdated => {
                                    uis_stale = true;

                                    #[cfg(target_os = "ios")]
                                    if had_events {
                                        tracing::debug!(
                                            "iOS redraw fallback: forcing redraw for outdated UI"
                                        );
                                        window.raw.request_redraw();
                                    }
                                }
                            }

                            for (event, status) in
                                window_events.into_iter().zip(statuses.into_iter())
                            {
                                runtime.broadcast(subscription::Event::Interaction {
                                    window: id,
                                    event,
                                    status,
                                });
                            }

                            // interact_span.finish();
                        }

                        for (id, event) in events.drain(..) {
                            runtime.broadcast(subscription::Event::Interaction {
                                window: id,
                                event,
                                status: iced::event::Status::Ignored,
                            });
                        }

                        if !messages.is_empty() || uis_stale {
                            let cached_interfaces: FxHashMap<_, _> =
                                ManuallyDrop::into_inner(user_interfaces)
                                    .into_iter()
                                    .map(|(id, ui)| (id, ui.into_cache()))
                                    .collect();

                            let actions = update(&mut program, &mut runtime, &mut messages);

                            user_interfaces = ManuallyDrop::new(build_user_interfaces(
                                &program,
                                &mut windowager,
                                cached_interfaces,
                            ));

                            for action in actions {
                                run_action(
                                    action,
                                    &program,
                                    &mut runtime,
                                    &mut compositor,
                                    &mut events,
                                    &mut messages,
                                    &mut clipboard,
                                    &mut control_sender,
                                    &mut user_interfaces,
                                    &mut windowager,
                                    &mut ui_caches,
                                    &mut is_window_opening,
                                    &mut system_theme,
                                );
                            }

                            for (_id, window) in windowager.iter_mut() {
                                window.raw.request_redraw();
                            }

                            #[cfg(target_os = "ios")]
                            tracing::debug!(
                                "iOS redraw fallback: requesting redraw after processing events/messages"
                            );
                        }

                        if let Some(redraw_at) = windowager.redraw_at() {
                            let _ = control_sender
                                .start_send(Control::ChangeFlow(ControlFlow::WaitUntil(redraw_at)));
                        } else {
                            let _ =
                                control_sender.start_send(Control::ChangeFlow(ControlFlow::Wait));
                        }
                    }
                    _ => {}
                }
            }
            Event::Exit => break,
        }
    }

    let _ = ManuallyDrop::into_inner(user_interfaces);
}

/// Builds a window's [`UserInterface`] for the [`Program`].
fn build_user_interface<'a, P: Program>(
    program: &'a program::Instance<P>,
    cache: user_interface::Cache,
    renderer: &mut P::Renderer,
    size: Size,
    id: window::Id,
) -> UserInterface<'a, P::Message, P::Theme, P::Renderer>
where
    P::Theme: theme::Base,
{
    // let view_span = debug::view(id);
    let view = program.view(id);
    // view_span.finish();

    // let layout_span = debug::layout(id);
    let user_interface = UserInterface::build(view, size, cache, renderer);
    // layout_span.finish();

    user_interface
}

fn update<P: Program, E: Executor>(
    program: &mut program::Instance<P>,
    runtime: &mut Runtime<E, Proxy<P::Message>, Action<P::Message>>,
    messages: &mut Vec<P::Message>,
) -> Vec<Action<P::Message>>
where
    P::Theme: theme::Base,
{
    use futures::futures;

    let mut actions = Vec::new();

    for message in messages.drain(..) {
        let task = runtime.enter(|| program.update(message));

        if let Some(mut stream) = runtime::task::into_stream(task) {
            let waker = futures::task::noop_waker_ref();
            let mut context = futures::task::Context::from_waker(waker);

            // Run immediately available actions synchronously (e.g. widget operations)
            loop {
                match runtime.enter(|| stream.poll_next_unpin(&mut context)) {
                    futures::task::Poll::Ready(Some(action)) => {
                        actions.push(action);
                    }
                    futures::task::Poll::Ready(None) => {
                        break;
                    }
                    futures::task::Poll::Pending => {
                        runtime.run(stream);
                        break;
                    }
                }
            }
        }
    }

    let subscription = runtime.enter(|| program.subscription());
    let recipes = subscription::into_recipes(subscription.map(Action::Output));

    runtime.track(recipes);

    actions
}

fn run_action<'a, P, C>(
    action: Action<P::Message>,
    program: &'a program::Instance<P>,
    runtime: &mut Runtime<P::Executor, Proxy<P::Message>, Action<P::Message>>,
    compositor: &mut Option<C>,
    events: &mut Vec<(window::Id, iced::Event)>,
    messages: &mut Vec<P::Message>,
    clipboard: &mut Clipboard,
    control_sender: &mut mpsc::UnboundedSender<Control>,
    interfaces: &mut FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>,
    window_manager: &mut WindowManager<P, C>,
    ui_caches: &mut FxHashMap<window::Id, user_interface::Cache>,
    is_window_opening: &mut bool,
    system_theme: &mut theme::Mode,
) where
    P: Program,
    C: iced_winit::graphics::Compositor<Renderer = P::Renderer> + 'static,
    P::Theme: theme::Base,
{
    use iced_runtime::clipboard;
    use iced_runtime::window;

    match action {
        Action::Output(message) => {
            messages.push(message);
        }
        Action::Clipboard(action) => match action {
            clipboard::Action::Read { target, channel } => {
                let _ = channel.send(clipboard.read(target));
            }
            clipboard::Action::Write { target, contents } => {
                clipboard.write(target, contents);
            }
        },
        Action::Window(action) => match action {
            window::Action::Open(id, settings, channel) => {
                let monitor = window_manager.last_monitor();

                control_sender
                    .start_send(Control::CreateWindow {
                        id,
                        settings,
                        title: program.title(id),
                        scale_factor: program.scale_factor(id),
                        monitor,
                        on_open: channel,
                    })
                    .expect("Send control action");

                *is_window_opening = true;
            }
            window::Action::Close(id) => {
                let _ = ui_caches.remove(&id);
                let _ = interfaces.remove(&id);

                if let Some(window) = window_manager.remove(id) {
                    if clipboard.window_id() == Some(window.raw.id()) {
                        *clipboard = window_manager
                            .first()
                            .map(|window| window.raw.clone())
                            .map(Clipboard::connect)
                            .unwrap_or_else(Clipboard::unconnected);
                    }

                    events.push((id, iced::Event::Window(iced::window::Event::Closed)));
                }

                if window_manager.is_empty() {
                    *compositor = None;
                }
            }
            window::Action::GetOldest(channel) => {
                let id = window_manager.iter_mut().next().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::GetLatest(channel) => {
                let id = window_manager.iter_mut().last().map(|(id, _window)| id);

                let _ = channel.send(id);
            }
            window::Action::Drag(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.drag_window();
                }
            }
            window::Action::DragResize(id, direction) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window
                        .raw
                        .drag_resize_window(conversion::resize_direction(direction));
                }
            }
            window::Action::Resize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.request_inner_size(
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor())),
                    );
                }
            }
            window::Action::SetMinSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_min_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetMaxSize(id, size) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_max_inner_size(size.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetResizeIncrements(id, increments) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resize_increments(increments.map(|size| {
                        winit::dpi::LogicalSize {
                            width: size.width,
                            height: size.height,
                        }
                        .to_physical::<f32>(f64::from(window.state.scale_factor()))
                    }));
                }
            }
            window::Action::SetResizable(id, resizable) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_resizable(resizable);
                }
            }
            window::Action::GetSize(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let size = window.logical_size();
                    let _ = channel.send(Size::new(size.width, size.height));
                }
            }
            window::Action::GetMaximized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_maximized());
                }
            }
            window::Action::Maximize(id, maximized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(maximized);
                }
            }
            window::Action::GetMinimized(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.is_minimized());
                }
            }
            window::Action::Minimize(id, minimized) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_minimized(minimized);
                }
            }
            window::Action::GetPosition(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let position = window
                        .raw
                        .outer_position()
                        .map(|position| {
                            let position = position.to_logical::<f32>(window.raw.scale_factor());

                            Point::new(position.x, position.y)
                        })
                        .ok();

                    let _ = channel.send(position);
                }
            }
            window::Action::GetScaleFactor(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let scale_factor = window.raw.scale_factor();

                    let _ = channel.send(scale_factor as f32);
                }
            }
            window::Action::Move(id, position) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_outer_position(winit::dpi::LogicalPosition {
                        x: position.x,
                        y: position.y,
                    });
                }
            }
            window::Action::SetMode(id, mode) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_visible(conversion::visible(mode));
                    window
                        .raw
                        .set_fullscreen(conversion::fullscreen(window.raw.current_monitor(), mode));
                }
            }
            window::Action::SetIcon(id, icon) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_window_icon(conversion::icon(icon));
                }
            }
            window::Action::GetMode(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let mode = if window.raw.is_visible().unwrap_or(true) {
                        conversion::mode(window.raw.fullscreen())
                    } else {
                        iced::window::Mode::Hidden
                    };

                    let _ = channel.send(mode);
                }
            }
            window::Action::ToggleMaximize(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_maximized(!window.raw.is_maximized());
                }
            }
            window::Action::ToggleDecorations(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_decorations(!window.raw.is_decorated());
                }
            }
            window::Action::RequestUserAttention(id, attention_type) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window
                        .raw
                        .request_user_attention(attention_type.map(conversion::user_attention));
                }
            }
            window::Action::GainFocus(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.focus_window();
                }
            }
            window::Action::SetLevel(id, level) => {
                if let Some(window) = window_manager.get_mut(id) {
                    window.raw.set_window_level(conversion::window_level(level));
                }
            }
            window::Action::ShowSystemMenu(id) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let mouse::Cursor::Available(point) = window.state.cursor()
                {
                    window.raw.show_window_menu(winit::dpi::LogicalPosition {
                        x: point.x,
                        y: point.y,
                    });
                }
            }
            window::Action::GetRawId(id, channel) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = channel.send(window.raw.id().into());
                }
            }
            window::Action::Run(id, f) => {
                if let Some(window) = window_manager.get_mut(id) {
                    f(window);
                }
            }
            window::Action::Screenshot(id, channel) => {
                if let Some(window) = window_manager.get_mut(id)
                    && let Some(compositor) = compositor
                {
                    let bytes = compositor.screenshot(
                        &mut window.renderer,
                        window.state.viewport(),
                        window.state.background_color(),
                    );

                    let _ = channel.send(iced::window::Screenshot::new(
                        bytes,
                        window.state.physical_size(),
                        window.state.scale_factor(),
                    ));
                }
            }
            window::Action::EnableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(false);
                }
            }
            window::Action::DisableMousePassthrough(id) => {
                if let Some(window) = window_manager.get_mut(id) {
                    let _ = window.raw.set_cursor_hittest(true);
                }
            }
            window::Action::GetMonitorSize(id, channel) => {
                if let Some(window) = window_manager.get(id) {
                    let size = window.raw.current_monitor().map(|monitor| {
                        let scale = window.state.scale_factor();
                        let size = monitor.size().to_logical(f64::from(scale));

                        Size::new(size.width, size.height)
                    });

                    let _ = channel.send(size);
                }
            }
            window::Action::SetAllowAutomaticTabbing(enabled) => {
                control_sender
                    .start_send(Control::SetAutomaticWindowTabbing(enabled))
                    .expect("Send control action");
            }
            window::Action::RedrawAll => {
                for (_id, window) in window_manager.iter_mut() {
                    window.raw.request_redraw();
                }
            }
            window::Action::RelayoutAll => {
                for (id, window) in window_manager.iter_mut() {
                    if let Some(ui) = interfaces.remove(&id) {
                        let _ = interfaces.insert(
                            id,
                            ui.relayout(window.state.logical_size(), &mut window.renderer),
                        );
                    }

                    window.raw.request_redraw();
                }
            }
        },
        Action::System(action) => match action {
            system::Action::GetInformation(_channel) => {
                #[cfg(feature = "sysinfo")]
                {
                    if let Some(compositor) = compositor {
                        let graphics_info = compositor.information();

                        let _ = std::thread::spawn(move || {
                            let information = system_information(graphics_info);

                            let _ = _channel.send(information);
                        });
                    }
                }
            }
            system::Action::GetTheme(channel) => {
                let _ = channel.send(*system_theme);
            }
            system::Action::NotifyTheme(mode) => {
                if mode != *system_theme {
                    *system_theme = mode;

                    runtime.broadcast(subscription::Event::SystemThemeChanged(mode));
                }

                let Some(theme) = conversion::window_theme(mode) else {
                    return;
                };

                for (_id, window) in window_manager.iter_mut() {
                    window.state.update(
                        program,
                        &window.raw,
                        &winit::event::WindowEvent::ThemeChanged(theme),
                    );
                }
            }
        },
        Action::Widget(operation) => {
            let mut current_operation = Some(operation);

            while let Some(mut operation) = current_operation.take() {
                for (id, ui) in interfaces.iter_mut() {
                    if let Some(window) = window_manager.get_mut(*id) {
                        ui.operate(&window.renderer, operation.as_mut());
                    }
                }

                match operation.finish() {
                    operation::Outcome::None => {}
                    operation::Outcome::Some(()) => {}
                    operation::Outcome::Chain(next) => {
                        current_operation = Some(next);
                    }
                }
            }
        }
        Action::Image(action) => match action {
            image::Action::Allocate(handle, sender) => {
                use iced::Renderer as _;

                // TODO: Shared image cache in compositor
                if let Some((_id, window)) = window_manager.iter_mut().next() {
                    window.renderer.allocate_image(&handle, move |allocation| {
                        let _ = sender.send(allocation);
                    });
                }
            }
        },
        Action::LoadFont { bytes, channel } => {
            if let Some(compositor) = compositor {
                // TODO: Error handling (?)
                compositor.load_font(bytes.clone());

                let _ = channel.send(Ok(()));
            }
        }
        Action::Reload => {
            for (id, window) in window_manager.iter_mut() {
                let Some(ui) = interfaces.remove(&id) else {
                    continue;
                };

                let cache = ui.into_cache();
                let size = window.logical_size();

                let _ = interfaces.insert(
                    id,
                    build_user_interface(program, cache, &mut window.renderer, size, id),
                );

                window.raw.request_redraw();
            }
        }
        Action::Exit => {
            control_sender
                .start_send(Control::Exit)
                .expect("Send control action");
        }
    }
}

/// Build the user interface for every window.
pub fn build_user_interfaces<'a, P: Program, C>(
    program: &'a program::Instance<P>,
    window_manager: &mut WindowManager<P, C>,
    mut cached_user_interfaces: FxHashMap<window::Id, user_interface::Cache>,
) -> FxHashMap<window::Id, UserInterface<'a, P::Message, P::Theme, P::Renderer>>
where
    C: iced_winit::graphics::Compositor<Renderer = P::Renderer>,
    P::Theme: theme::Base,
{
    for (id, window) in window_manager.iter_mut() {
        window.state.synchronize(program, id, &window.raw);
    }

    // debug::theme_changed(|| {
    window_manager
        .first()
        .and_then(|window| theme::Base::palette(window.state.theme()));
    // });

    cached_user_interfaces
        .drain()
        .filter_map(|(id, cache)| {
            let window = window_manager.get_mut(id)?;

            Some((
                id,
                build_user_interface(
                    program,
                    cache,
                    &mut window.renderer,
                    window.state.logical_size(),
                    id,
                ),
            ))
        })
        .collect()
}

/// Returns true if the provided event should cause a [`Program`] to
/// exit.
pub fn user_force_quit(
    event: &winit::event::WindowEvent,
    _modifiers: winit::keyboard::ModifiersState,
) -> bool {
    match event {
        #[cfg(target_os = "macos")]
        winit::event::WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Character(c),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } if c == "q" && _modifiers.super_key() => true,
        _ => false,
    }
}
